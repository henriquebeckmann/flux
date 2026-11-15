//! Starting programs for an extension.
//!
//! In a host-process deployment every command runs through `<exe> host --exec …`, which confines
//! itself (no_new_privs, Landlock, seccomp, no sockets unless the extension may use the network) and
//! then `exec`s the program, so the program and everything it starts inherit the restrictions. A
//! command the manifest lists in `grants.exec_unconfined` runs directly instead. Without a wrapper
//! (thread mode, tests) commands run directly.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use flux_api::{Capability, PermissionSet};

#[derive(Debug, Clone)]
pub struct ExecWrapper {
    pub exe: PathBuf,
}

/// `PATH` lookup done by the launcher (the extension never influences it).
pub fn resolve_program(cmd: &str) -> Result<PathBuf, String> {
    if cmd.contains('/') {
        return Ok(PathBuf::from(cmd));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|d| d.join(cmd))
        .find(|p| p.is_file() && is_executable(p))
        .ok_or_else(|| format!("'{cmd}' was not found in PATH"))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// `~/…` and absolute grant entries to paths.
pub fn expand_grant(raw: &str) -> Option<PathBuf> {
    match raw.strip_prefix("~/").map(Path::new).or_else(|| (raw == "~").then(|| Path::new(""))) {
        Some(rel) => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rel)),
        None => Some(PathBuf::from(raw)),
    }
}

/// The command to start for `cmd`, confined unless it is listed as unconfined.
pub fn command_for(wrapper: Option<&ExecWrapper>, perms: &PermissionSet, data_dir: &Path, cmd: &str, args: &[String], cwd: Option<&str>) -> Result<Command, String> {
    let confined = wrapper.filter(|_| perms.command_confined(cmd));
    let Some(w) = confined else {
        let mut c = Command::new(cmd);
        c.args(args);
        if let Some(d) = cwd {
            c.current_dir(d);
        }
        return Ok(c);
    };
    let mut c = Command::new(&w.exe);
    c.arg("host").arg("--exec");
    if perms.has(Capability::Network) {
        c.arg("--net");
    }
    c.arg("--write").arg(data_dir).arg("--read").arg(data_dir);
    for d in perms.grants.fs_read.iter().flatten().filter_map(|d| expand_grant(d)) {
        c.arg("--read").arg(d);
    }
    if perms.has(Capability::FilesystemWrite) {
        for d in perms.grants.fs_write.iter().flatten().filter_map(|d| expand_grant(d)) {
            c.arg("--write").arg(d);
        }
    }
    if let Some(d) = cwd {
        c.arg("--cwd").arg(d);
    }
    c.arg("--").arg(resolve_program(cmd)?).args(args);
    // The launcher's environment (tokens, session variables) is not for extensions' commands.
    c.env_clear();
    for key in ["PATH", "HOME", "LANG", "LC_ALL", "TZ"] {
        if let Some(v) = std::env::var_os(key) {
            c.env(key, v);
        }
    }
    Ok(c)
}

/// Programs that belong to Flux itself (the launcher, hosts, exec wrappers: all named `flux…`) are
/// never signalled, whatever the extension is allowed to kill.
pub fn is_flux_process(pid: u32) -> bool {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().starts_with("flux"))).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_are_resolved_by_the_launcher_and_grants_expand() {
        assert!(resolve_program("sh").unwrap().is_absolute());
        assert!(resolve_program("flux-no-such-program").is_err());
        assert_eq!(resolve_program("/bin/true").unwrap(), PathBuf::from("/bin/true"));
        assert_eq!(expand_grant("/a/b"), Some(PathBuf::from("/a/b")));
    }

    #[test]
    fn processes_not_named_flux_are_not_protected_by_name() {
        let mut sleeper = std::process::Command::new("sleep").arg("5").spawn().unwrap();
        assert!(!is_flux_process(sleeper.id()));
        assert!(is_flux_process(std::process::id()), "this test binary is called flux_runtime-…");
        let _ = sleeper.kill();
        let _ = sleeper.wait();
    }
}
