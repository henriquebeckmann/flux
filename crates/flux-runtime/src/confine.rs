//! OS-level confinement of an Extension Host process (Linux).
//!
//! Applied by the host to itself, before it reads any extension code:
//!
//! 1. `no_new_privs`, core dumps off, file-size and open-file limits;
//! 2. **Landlock**: the host can read its extension's folder, a minimal set of system files,
//!    and read/write its own data folder. Nothing else in the filesystem. With ABI ≥ 4 TCP
//!    `connect` is limited to ports 80/443 (and not at all without the `network` capability);
//! 3. **seccomp** (default-allow with a deny list): no `execve`, no process creation except
//!    threads, no `ptrace`/`mount`/`bpf`/namespaces/kernel modules, no signals to other
//!    processes, and sockets only for the families the permissions need.
//!
//! The returned [`SandboxReport`] says what is *really* in effect. Nothing here claims more than
//! the kernel enforced; `docs/security.md` lists what remains possible.

use std::path::{Path, PathBuf};

use flux_api::wire::{SandboxMode, SandboxReport};
use landlock::{ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, NetPort, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus};
use seccompiler::{SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter, SeccompRule};

pub struct Plan {
    pub dir: PathBuf,
    /// Where the process may write (`None`: nowhere).
    pub data_dir: Option<PathBuf>,
    pub network: bool,
    pub max_open_files: u64,
    pub mode: SandboxMode,
}

/// System paths a Rust + Lua + rustls process needs to read. Missing ones are skipped.
const READ_ONLY: &[&str] = &[
    "/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc/ssl", "/etc/ca-certificates", "/etc/pki", "/etc/resolv.conf", "/etc/hosts",
    "/etc/nsswitch.conf", "/etc/host.conf", "/etc/gai.conf", "/etc/localtime", "/etc/ld.so.cache", "/etc/ld.so.conf", "/etc/ld.so.conf.d",
    "/sys/devices/system/cpu", "/sys/fs/cgroup", "/proc/self", "/proc/cpuinfo", "/proc/meminfo", "/proc/sys/vm", "/dev/urandom", "/dev/random",
    "/dev/zero",
];

/// Kernel's Landlock ABI (`0` = unavailable).
pub fn landlock_abi() -> u32 {
    // SAFETY: a documented probe: no ruleset pointer, size 0, VERSION flag.
    let r = unsafe { libc::syscall(444, std::ptr::null::<u8>(), 0usize, 1u32) };
    if r < 0 { 0 } else { r as u32 }
}

pub fn apply(plan: &Plan) -> Result<SandboxReport, String> {
    let mut report = SandboxReport { mode: Some(plan.mode), ..Default::default() };
    if plan.mode == SandboxMode::Off {
        report.missing.push("sandboxing is turned off".into());
        return Ok(report);
    }
    let strict = plan.mode == SandboxMode::Strict;
    let missing = |report: &mut SandboxReport, what: &str| -> Result<(), String> {
        if strict {
            Err(format!("cannot confine the extension host: {what}"))
        } else {
            report.missing.push(what.to_string());
            Ok(())
        }
    };

    // 1. no_new_privs + resource limits
    // SAFETY: plain prctl/setrlimit calls with valid arguments.
    unsafe {
        report.no_new_privs = libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0;
        let lim = |resource, n: u64| libc::setrlimit(resource, &libc::rlimit { rlim_cur: n, rlim_max: n }) == 0;
        report.rlimits = lim(libc::RLIMIT_CORE, 0) && lim(libc::RLIMIT_NOFILE, plan.max_open_files) && lim(libc::RLIMIT_FSIZE, 256 * 1024 * 1024);
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0); // die with the launcher
    }
    if !report.no_new_privs {
        missing(&mut report, "PR_SET_NO_NEW_PRIVS failed")?;
    }
    if !report.rlimits {
        missing(&mut report, "resource limits could not be set")?;
    }

    // 2. Landlock
    let kernel_abi = landlock_abi();
    match landlock_rules(plan, strict) {
        Ok(status) => {
            if status == RulesetStatus::NotEnforced || kernel_abi == 0 {
                missing(&mut report, "the kernel does not support Landlock")?;
            } else {
                report.landlock_abi = Some(kernel_abi);
                report.landlock_net = kernel_abi >= 4;
                if !report.landlock_net {
                    report.notes.push("TCP connections are not limited to ports 80/443 (needs Landlock ABI 4, Linux 6.7)".into());
                }
            }
        }
        Err(e) => missing(&mut report, &format!("Landlock: {e}"))?,
    }

    // 3. seccomp
    match seccomp(plan.network, false) {
        Ok(()) => report.seccomp = true,
        Err(e) => missing(&mut report, &format!("seccomp: {e}"))?,
    }
    Ok(report)
}

fn landlock_rules(plan: &Plan, strict: bool) -> Result<RulesetStatus, String> {
    let mut read: Vec<PathBuf> = READ_ONLY.iter().map(PathBuf::from).collect();
    read.push(plan.dir.clone());
    landlock_apply(&read, plan.data_dir.as_slice(), plan.network, strict)
}

/// Landlock: `read` is readable, `write` readable and writable (created if missing), `/dev/null`
/// writable, and with `web_ports` TCP connect is limited to 80/443 (without: no TCP at all).
fn landlock_apply(read: &[PathBuf], write: &[PathBuf], web_ports: bool, strict: bool) -> Result<RulesetStatus, String> {
    let abi = ABI::V5;
    for d in write {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let fs_level = if strict { CompatLevel::HardRequirement } else { CompatLevel::BestEffort };
    let mut ruleset = Ruleset::default().set_compatibility(fs_level).handle_access(AccessFs::from_all(abi)).map_err(|e| e.to_string())?;
    // Network restriction is desirable but not required: older kernels simply lack it.
    ruleset = ruleset.set_compatibility(CompatLevel::BestEffort).handle_access(AccessNet::from_all(abi)).map_err(|e| e.to_string())?;
    let mut created = ruleset.create().map_err(|e| e.to_string())?;
    let ro = AccessFs::from_read(abi);
    for p in read.iter().filter(|p| p.exists()) {
        created = created.add_rule(PathBeneath::new(PathFd::new(p).map_err(|e| e.to_string())?, ro)).map_err(|e| e.to_string())?;
    }
    for d in write {
        created = created.add_rule(PathBeneath::new(PathFd::new(d).map_err(|e| e.to_string())?, AccessFs::from_all(abi))).map_err(|e| e.to_string())?;
    }
    if Path::new("/dev/null").exists() {
        created = created.add_rule(PathBeneath::new(PathFd::new("/dev/null").map_err(|e| e.to_string())?, AccessFs::from_all(abi))).map_err(|e| e.to_string())?;
    }
    if web_ports {
        for port in [80u16, 443] {
            created = created.add_rule(NetPort::new(port, AccessNet::ConnectTcp)).map_err(|e| e.to_string())?;
        }
    }
    Ok(created.restrict_self().map_err(|e| e.to_string())?.ruleset)
}

macro_rules! sys {
    ($($name:ident),* $(,)?) => { &[$(libc::$name),*] };
}

/// Calls that are refused with `EPERM` whatever the arguments.
fn denied_syscalls(processes: bool) -> Vec<i64> {
    let mut v: Vec<i64> = sys![
        SYS_execve, SYS_execveat, SYS_ptrace, SYS_process_vm_readv, SYS_process_vm_writev, SYS_mount, SYS_umount2, SYS_pivot_root, SYS_chroot,
        SYS_setns, SYS_unshare, SYS_bpf, SYS_keyctl, SYS_add_key, SYS_request_key, SYS_perf_event_open, SYS_kexec_load, SYS_init_module,
        SYS_finit_module, SYS_delete_module, SYS_reboot, SYS_swapon, SYS_swapoff, SYS_acct, SYS_quotactl, SYS_userfaultfd, SYS_io_uring_setup,
        SYS_io_uring_enter, SYS_io_uring_register, SYS_open_by_handle_at, SYS_name_to_handle_at, SYS_kill, SYS_tkill, SYS_rt_sigqueueinfo,
        SYS_rt_tgsigqueueinfo, SYS_pidfd_send_signal, SYS_pidfd_open, SYS_pidfd_getfd, SYS_mount_setattr, SYS_move_mount, SYS_open_tree,
        SYS_fsopen, SYS_fsconfig, SYS_fsmount, SYS_fspick, SYS_setuid, SYS_setgid, SYS_setreuid, SYS_setregid, SYS_setresuid, SYS_setresgid,
        SYS_setgroups, SYS_capset, SYS_settimeofday, SYS_clock_settime, SYS_sethostname, SYS_setdomainname,
    ]
    .iter()
    .map(|n| *n as i64)
    .collect();
    #[cfg(target_arch = "x86_64")]
    v.extend([libc::SYS_fork, libc::SYS_vfork, libc::SYS_iopl, libc::SYS_ioperm, libc::SYS_kexec_file_load, libc::SYS_modify_ldt].map(|n| n as i64));
    #[cfg(target_arch = "aarch64")]
    v.extend([libc::SYS_kexec_file_load].map(|n| n as i64));
    if processes {
        // A command may start programs: only the rest of the list applies.
        let allowed = [libc::SYS_execve, libc::SYS_execveat];
        #[cfg(target_arch = "x86_64")]
        let allowed = [allowed[0], allowed[1], libc::SYS_fork, libc::SYS_vfork];
        v.retain(|n| !allowed.contains(n));
    }
    v
}

fn seccomp(network: bool, processes: bool) -> Result<(), String> {
    let arch = std::env::consts::ARCH.try_into().map_err(|e| format!("unsupported architecture: {e:?}"))?;
    let cond = |idx: u8, op: SeccompCmpOp, v: u64| SeccompCondition::new(idx, SeccompCmpArgLen::Dword, op, v).map_err(|e| e.to_string());
    let rule = |c: Vec<SeccompCondition>| SeccompRule::new(c).map_err(|e| e.to_string());

    let mut rules: std::collections::BTreeMap<i64, Vec<SeccompRule>> = Default::default();
    for n in denied_syscalls(processes) {
        rules.insert(n, vec![]); // no condition = always
    }
    // Only threads: a clone that does not share the thread group would create a process.
    if !processes {
        rules.insert(libc::SYS_clone, vec![rule(vec![cond(0, SeccompCmpOp::MaskedEq(libc::CLONE_THREAD as u64), 0)?])?]);
    }
    // Signals only to itself.
    rules.insert(libc::SYS_tgkill, vec![rule(vec![cond(0, SeccompCmpOp::Ne, std::process::id() as u64)?])?]);
    // Sockets: none unless the network permission is held; then IPv4/IPv6 (+ netlink for resolving).
    let allowed_domains: &[i32] = if network { &[libc::AF_INET, libc::AF_INET6, libc::AF_NETLINK] } else { &[] };
    let mut socket_cond = Vec::new();
    for d in allowed_domains {
        socket_cond.push(cond(0, SeccompCmpOp::Ne, *d as u64)?);
    }
    if socket_cond.is_empty() {
        rules.insert(libc::SYS_socket, vec![]);
    } else {
        rules.insert(libc::SYS_socket, vec![rule(socket_cond)?]);
    }
    let filter = SeccompFilter::new(rules, SeccompAction::Allow, SeccompAction::Errno(libc::EPERM as u32), arch).map_err(|e| e.to_string())?;
    let prog: seccompiler::BpfProgram = filter.try_into().map_err(|e: seccompiler::BackendError| e.to_string())?;
    seccompiler::apply_filter(&prog).map_err(|e| e.to_string())?;

    if processes {
        return Ok(());
    }
    // `clone3` takes its flags from memory, so it cannot be filtered: answering ENOSYS makes libc
    // fall back to `clone`, which can.
    let mut r2: std::collections::BTreeMap<i64, Vec<SeccompRule>> = Default::default();
    r2.insert(libc::SYS_clone3, vec![]);
    let f2 = SeccompFilter::new(r2, SeccompAction::Allow, SeccompAction::Errno(libc::ENOSYS as u32), arch).map_err(|e| e.to_string())?;
    let p2: seccompiler::BpfProgram = f2.try_into().map_err(|e: seccompiler::BackendError| e.to_string())?;
    seccompiler::apply_filter(&p2).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Commands started on behalf of an extension
// ---------------------------------------------------------------------------

/// Confinement of a command run for an extension (`<exe> host --exec …`, see `host::exec_main`).
/// Always strict: a command that cannot be confined is not run.
pub struct ExecPlan {
    /// Extra readable folders (`grants.fs_read`, the extension's data folder).
    pub read: Vec<PathBuf>,
    /// Writable folders: the extension's data folder and `grants.fs_write`.
    pub write: Vec<PathBuf>,
    pub network: bool,
    pub max_open_files: u64,
}

/// What ordinary programs (`ps`, `uname`, `head`…) read. Not `$HOME`, not other users' data.
const EXEC_READ: &[&str] = &["/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc", "/proc", "/sys", "/dev/urandom", "/dev/random", "/dev/zero"];

pub fn apply_exec(plan: &ExecPlan) -> Result<(), String> {
    // SAFETY: plain prctl/setrlimit calls with valid arguments.
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err("PR_SET_NO_NEW_PRIVS failed".into());
        }
        let lim = |resource, n: u64| libc::setrlimit(resource, &libc::rlimit { rlim_cur: n, rlim_max: n }) == 0;
        if !(lim(libc::RLIMIT_CORE, 0) && lim(libc::RLIMIT_NOFILE, plan.max_open_files)) {
            return Err("resource limits could not be set".into());
        }
    }
    let mut read: Vec<PathBuf> = EXEC_READ.iter().map(PathBuf::from).collect();
    read.extend(plan.read.iter().cloned());
    let status = landlock_apply(&read, &plan.write, plan.network, true).map_err(|e| format!("Landlock: {e}"))?;
    if status == RulesetStatus::NotEnforced {
        return Err("the kernel does not support Landlock".into());
    }
    seccomp(plan.network, true).map_err(|e| format!("seccomp: {e}"))
}
