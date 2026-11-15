//! The Extension Host: a process that runs exactly one extension.
//!
//! Started by the launcher as `<exe> host` with the connection to the launcher on file descriptor
//! 3. It confines itself (see `confine.rs`), loads the extension in its own Lua state, and from
//! then on only talks over that connection: requests in, events and brokered calls out.

use std::{
    os::unix::{io::FromRawFd, net::UnixStream},
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

use flux_api::wire::{self, FromHost, Init, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION, Stats, ToHost};
use tokio::sync::mpsc;

use crate::{
    confine::{self, Plan},
    limits::Heartbeat,
    remote::{Link, RemoteSecrets, RemoteStorage},
    sandbox::Sandbox,
    supervisor::load_extension,
    worker::{self, Shared, WorkerMsg},
};

/// Entry point of `<exe> host`. Returns the process exit code.
pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--exec") {
        return exec_main(&args[i + 1..]);
    }
    if let Some(i) = args.iter().position(|a| a == "--eval") {
        return crate::eval::child_main(&args[i + 1..]);
    }
    if let Some(i) = args.iter().position(|a| a == "--selftest-sandbox") {
        return selftest(&args[i + 1..]);
    }
    // SAFETY: the launcher passes the connection as fd 3 by contract; nothing else uses it.
    let stream = unsafe { UnixStream::from_raw_fd(3) };
    match run(stream) {
        Ok(()) => 0,
        Err(code) => code,
    }
}

fn run(stream: UnixStream) -> Result<(), i32> {
    let mut reader = stream.try_clone().map_err(|_| 2)?;
    let link = Link::new(stream);
    let fail = |message: String, code: i32| {
        let _ = link.send(None, &FromHost::Failed { message });
        code
    };

    // 1. the first message must be Init
    let _ = reader.set_read_timeout(Some(Duration::from_secs(10)));
    let init: Init = match wire::read_frame::<_, ToHost>(&mut reader) {
        Ok(Some(f)) => match f.body {
            ToHost::Init(i) => i,
            _ => return Err(fail("the first message was not Init".into(), 2)),
        },
        Ok(None) => return Err(2),
        Err(e) => return Err(fail(format!("bad first message: {e}"), 2)),
    };
    let proto = init.proto_max.min(PROTOCOL_VERSION);
    if proto < init.proto_min.max(MIN_PROTOCOL_VERSION) {
        return Err(fail(format!("no common protocol version (launcher {}–{}, host {MIN_PROTOCOL_VERSION}–{PROTOCOL_VERSION})", init.proto_min, init.proto_max), 2));
    }
    let _ = reader.set_read_timeout(None);

    // 2. confine before touching any extension code
    let dir = PathBuf::from(&init.dir);
    let data_dir = PathBuf::from(&init.data_dir);
    let plan = Plan { dir: dir.clone(), data_dir: Some(data_dir.clone()), network: false, max_open_files: init.max_open_files, mode: init.sandbox };
    let report = match confine::apply(&plan) {
        Ok(r) => r,
        Err(e) => return Err(fail(e, 3)),
    };

    // 3. load the extension (the launcher's policy is authoritative)
    let scratch = match Sandbox::new(init.limits, Heartbeat::default()) {
        Ok(s) => s,
        Err(e) => return Err(fail(format!("could not create a Lua state: {e}"), 4)),
    };
    let mut ext = match load_extension(&crate::eval::Evaluator::Local(scratch), &dir, &Default::default()) {
        Ok(e) => e,
        Err(errors) => return Err(fail(errors.join("; "), 4)),
    };
    ext.policy = Arc::new(init.policy.clone());
    if ext.manifest.name != init.extension {
        return Err(fail(format!("the manifest names '{}', expected '{}'", ext.manifest.name, init.extension), 4));
    }
    let _ = link.send(None, &FromHost::Ready { proto, sandbox: report });

    // 4. wiring: events out, requests in
    let (events_tx, events_rx) = async_channel::unbounded();
    let out = link.clone();
    std::thread::Builder::new()
        .name("flux-host-events".into())
        .spawn(move || {
            while let Ok(event) = events_rx.recv_blocking() {
                if out.send(None, &FromHost::Event { event }).is_err() {
                    std::process::exit(1);
                }
            }
        })
        .map_err(|_| 5)?;

    let hb = Heartbeat::default();
    let stats: Arc<crate::resources::LiveStats> = Default::default();
    let (wtx, wrx) = mpsc::unbounded_channel::<WorkerMsg>();
    let shared = Shared {
        events: events_tx,
        storage: Arc::new(RemoteStorage(link.clone())),
        secrets: Arc::new(RemoteSecrets(link.clone())),
        limits: init.limits,
        data_root: data_dir.parent().map(PathBuf::from).unwrap_or_else(|| data_dir.clone()),
        app_version: init.app_version.clone(),
        view_ids: Arc::new(AtomicU64::new(init.view_id_base)),
        owners: Default::default(),
        generation: init.generation,
        link: Some(link.clone()),
        stats: stats.clone(),
    };

    let unloading = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let unloading_rx = unloading.clone();
    let (pong_link, pong_hb, pong_stats) = (link.clone(), hb.clone(), stats.clone());
    std::thread::Builder::new()
        .name("flux-host-rx".into())
        .spawn(move || {
            loop {
                let frame = match wire::read_frame::<_, ToHost>(&mut reader) {
                    Ok(Some(f)) => f,
                    // The launcher is gone or sent garbage: nothing left to serve.
                    Ok(None) | Err(_) => std::process::exit(1),
                };
                let msg = match frame.body {
                    ToHost::Run { command_id, name } => WorkerMsg::Run { command_id, name },
                    ToHost::Invoke { view_id, action_id } => WorkerMsg::Invoke { view_id, action_id },
                    ToHost::Submit { view_id, text } => WorkerMsg::Submit { view_id, text },
                    ToHost::Search { view_id, text } => WorkerMsg::Search { view_id, text },
                    ToHost::Selection { view_id, key } => WorkerMsg::Selection { view_id, key },
                    ToHost::FormChanged { view_id, field, value } => WorkerMsg::FormChanged { view_id, field, value },
                    ToHost::FormSubmit { view_id, values } => WorkerMsg::FormSubmit { view_id, values },
                    ToHost::ViewClosed { view_id } => WorkerMsg::ViewClosed(view_id),
                    ToHost::CancelCommand { command_id } => WorkerMsg::CancelCommand(command_id),
                    ToHost::Unload => {
                        unloading_rx.store(true, std::sync::atomic::Ordering::Release);
                        let (done_tx, done_rx) = std::sync::mpsc::channel();
                        if wtx.send(WorkerMsg::Unload { done: done_tx }).is_err() {
                            std::process::exit(0);
                        }
                        let link = pong_link.clone();
                        std::thread::spawn(move || {
                            let _ = done_rx.recv();
                            let _ = link.send(None, &FromHost::Unloaded);
                            std::process::exit(0);
                        });
                        continue;
                    }
                    ToHost::Ping => {
                        let busy_ms = pong_hb.busy_for().map(|d| d.as_millis() as u64);
                        let _ = pong_link.send(frame.corr, &FromHost::Pong { busy_ms, stats: pong_stats.snapshot() });
                        continue;
                    }
                    ToHost::Reply { result } => {
                        if let Some(corr) = frame.corr {
                            pong_link.complete(corr, result);
                        }
                        continue;
                    }
                    ToHost::Init(_) | ToHost::Unknown => continue,
                };
                if wtx.send(msg).is_err() {
                    std::process::exit(0);
                }
            }
        })
        .map_err(|_| 5)?;

    // 5. run the extension on this thread until it is unloaded
    let result = catch_unwind(AssertUnwindSafe(|| worker::main(ext, shared, wrx, hb)));
    link.fail_all("the extension host is shutting down");
    match result {
        // Unloaded on request: the thread that confirms it also ends the process.
        Ok(()) if unloading.load(std::sync::atomic::Ordering::Acquire) => loop {
            std::thread::park_timeout(Duration::from_secs(5));
        },
        // The worker ended without being asked to (it could not start): that is a failure.
        Ok(()) => Err(4),
        Err(_) => Err(101),
    }
}

// ---------------------------------------------------------------------------
// Self-test of the confinement
// ---------------------------------------------------------------------------

/// `<exe> host --selftest-sandbox <ext-dir> <data-dir> <network|none> <strict|best-effort>`:
/// applies the confinement the way a real host does, then tries things an extension must not be
/// able to do and prints one `name=outcome` line each. The integration tests read these lines;
/// they exercise the real kernel, not a model of it.
pub fn selftest(args: &[String]) -> i32 {
    use flux_api::wire::SandboxMode;
    let [dir, data, net, mode] = args else {
        eprintln!("usage: --selftest-sandbox <ext-dir> <data-dir> <network|none> <strict|best-effort>");
        return 2;
    };
    let mode = if mode == "strict" { SandboxMode::Strict } else { SandboxMode::BestEffort };
    let plan = Plan { dir: dir.into(), data_dir: Some(data.into()), network: net == "network", max_open_files: 64, mode };
    let report = match confine::apply(&plan) {
        Ok(r) => r,
        Err(e) => {
            println!("apply=refused: {e}");
            return 3;
        }
    };
    println!("apply=ok");
    println!("report={}", serde_json::to_string(&report).unwrap_or_default());

    fn errno() -> String {
        std::io::Error::last_os_error().raw_os_error().map(|e| e.to_string()).unwrap_or_default()
    }
    fn open(path: &str, flags: i32) -> String {
        let c = std::ffi::CString::new(path).unwrap();
        // SAFETY: plain open(2) on a NUL-terminated path.
        let fd = unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC, 0o600) };
        if fd >= 0 {
            // SAFETY: closing the descriptor we just opened.
            unsafe { libc::close(fd) };
            "ok".into()
        } else {
            format!("errno {}", errno())
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut put = |k: &str, v: String| out.push((k.to_string(), v));

    put("read_own_dir", open(&format!("{dir}/manifest.lua"), libc::O_RDONLY));
    put("write_own_data", open(&format!("{data}/note"), libc::O_WRONLY | libc::O_CREAT));
    put("write_own_dir", open(&format!("{dir}/evil"), libc::O_WRONLY | libc::O_CREAT));
    put("write_tmp", open("/tmp/flux-selftest-evil", libc::O_WRONLY | libc::O_CREAT));
    put("read_etc_passwd", open("/etc/passwd", libc::O_RDONLY));
    put("read_home", open(&home, libc::O_RDONLY | libc::O_DIRECTORY));
    put("read_ssh", open(&format!("{home}/.ssh"), libc::O_RDONLY | libc::O_DIRECTORY));
    put("read_other_proc_environ", open(&format!("/proc/{}/environ", unsafe { libc::getppid() }), libc::O_RDONLY));
    put("read_usr_lib", open("/usr/lib", libc::O_RDONLY | libc::O_DIRECTORY));

    // sockets
    let sock = |domain: i32| -> String {
        // SAFETY: plain socket(2).
        let fd = unsafe { libc::socket(domain, libc::SOCK_STREAM, 0) };
        if fd >= 0 {
            unsafe { libc::close(fd) };
            "ok".into()
        } else {
            format!("errno {}", errno())
        }
    };
    put("socket_unix", sock(libc::AF_UNIX));
    put("socket_inet", sock(libc::AF_INET));
    put("socket_packet", sock(libc::AF_PACKET));
    // connect to a local port: Landlock (ABI >= 4) refuses ports other than 80/443 with EACCES
    let connect = |port: u16| -> String {
        // SAFETY: socket/connect on a loopback address, closed before returning.
        unsafe {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
            if fd < 0 {
                return format!("no socket (errno {})", errno());
            }
            let addr = libc::sockaddr_in { sin_family: libc::AF_INET as u16, sin_port: port.to_be(), sin_addr: libc::in_addr { s_addr: u32::from_be_bytes([127, 0, 0, 1]).to_be() }, sin_zero: [0; 8] };
            let r = libc::connect(fd, &addr as *const _ as *const libc::sockaddr, std::mem::size_of::<libc::sockaddr_in>() as u32);
            let e = errno();
            libc::close(fd);
            if r == 0 { "connected".into() } else { format!("errno {e}") }
        }
    };
    put("connect_port_22", connect(22));
    put("connect_port_443", connect(443));

    // processes and kernel interfaces
    // SAFETY: each call is made with harmless arguments; the point is that the kernel refuses.
    unsafe {
        let c = std::ffi::CString::new("/bin/true").unwrap();
        let argv = [c.as_ptr(), std::ptr::null()];
        let r = libc::execv(c.as_ptr(), argv.as_ptr());
        put("execve", if r == 0 { "ran".into() } else { format!("errno {}", errno()) });
        let r = libc::fork();
        if r == 0 {
            libc::_exit(0);
        }
        put("fork", if r > 0 { "forked".into() } else { format!("errno {}", errno()) });
        let r = libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0);
        put("ptrace", if r == 0 { "ok".into() } else { format!("errno {}", errno()) });
        let r = libc::kill(libc::getppid(), 0);
        put("kill_parent", if r == 0 { "ok".into() } else { format!("errno {}", errno()) });
        let r = libc::syscall(libc::SYS_tgkill, libc::getppid() as i64, libc::getppid() as i64, 0);
        put("tgkill_other", if r == 0 { "ok".into() } else { format!("errno {}", errno()) });
        let r = libc::unshare(libc::CLONE_NEWUSER);
        put("unshare_userns", if r == 0 { "ok".into() } else { format!("errno {}", errno()) });
        let r = libc::syscall(libc::SYS_bpf, 0, 0, 0);
        put("bpf", if r == 0 { "ok".into() } else { format!("errno {}", errno()) });
    }
    put("thread", if std::thread::Builder::new().spawn(|| 1).and_then(|h| h.join().map_err(|_| std::io::Error::other("join"))).is_ok() { "ok".into() } else { "failed".into() });
    for (k, v) in out {
        println!("{k}={v}");
    }
    0
}

/// `<exe> host --exec [--net] [--read P]… [--write P]… [--cwd D] -- <program> [args…]`: confines
/// itself, then becomes the program. Never runs the program unconfined: if the confinement cannot
/// be applied it exits with 126 and says why on stderr.
pub fn exec_main(args: &[String]) -> i32 {
    use std::os::unix::process::CommandExt;
    let (mut net, mut read, mut write, mut cwd) = (false, Vec::new(), Vec::new(), None);
    let mut it = args.iter();
    let mut program: Vec<String> = Vec::new();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--net" => net = true,
            "--read" => read.extend(it.next().map(PathBuf::from)),
            "--write" => write.extend(it.next().map(PathBuf::from)),
            "--cwd" => cwd = it.next().cloned(),
            "--" => {
                program = it.cloned().collect();
                break;
            }
            other => {
                eprintln!("flux: unknown option {other}");
                return 2;
            }
        }
    }
    let Some((prog, rest)) = program.split_first() else {
        eprintln!("flux: nothing to run");
        return 2;
    };
    if let Some(d) = &cwd {
        if let Err(e) = std::env::set_current_dir(d) {
            eprintln!("flux: cannot enter {d}: {e}");
            return 126;
        }
    }
    if let Err(e) = confine::apply_exec(&confine::ExecPlan { read, write, network: net, max_open_files: 256 }) {
        eprintln!("flux: this command was not run because it cannot be confined: {e}");
        return 126;
    }
    let err = std::process::Command::new(prog).args(rest).exec();
    eprintln!("flux: could not run {prog}: {err}");
    127
}
