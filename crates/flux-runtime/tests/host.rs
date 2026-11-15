//! Extensions in real host processes: the protocol, the broker, confinement and failure handling.

mod common;

use std::time::{Duration, Instant};

use common::*;
use flux_api::{wire::SandboxMode, *};

/// Tests that look for host processes by parent pid must not run beside others that start hosts.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|p| p.into_inner())
}

const LIST: &str = "function main() return flux.ui.List { flux.ui.List.Item { title = 'A' } } end";

fn one(manifest_extra: &str, commands: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let cmds: String = commands.iter().map(|(n, _)| format!("{{ name = '{n}', mode = 'view' }},")).collect();
    write(&d.path().join("x"), "manifest.lua", &format!("return {{ name = 'x', api_version = 1, commands = {{ {cmds} }}, {manifest_extra} }}"));
    for (n, src) in commands {
        write(&d.path().join("x"), &format!("commands/{n}.lua"), src);
    }
    d
}

fn push(h: &Harness) -> (u64, ViewNode) {
    h.wait_event(|e| if let RuntimeEvent::Nav(NavEvent::Push { view_id, view }) = e { Some((*view_id, view.clone())) } else { None })
}

fn error(h: &Harness) -> String {
    h.wait_event(|e| if let RuntimeEvent::ExtensionError { message, .. } = e { Some(message.clone()) } else { None })
}

fn isolation(h: &Harness) -> String {
    loop {
        let infos = h.next_infos();
        let i = infos.iter().find(|i| i.name == "x").unwrap();
        if !i.isolation.contains("starting") && !i.isolation.contains("not running") {
            return i.isolation.clone();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_command_runs_in_its_own_process_and_its_view_arrives() {
    let _serial = serial();
    let d = one("", &[("main", LIST)]);
    let h = start_host(d.path(), SandboxMode::Off, |_| {});
    h.run("x:main");
    let (_, view) = push(&h);
    assert!(matches!(view, ViewNode::List(_)));
}

#[test]
fn storage_and_secrets_are_served_by_the_launcher() {
    let _serial = serial();
    let d = one(
        "",
        &[("main", "function main()\n  flux.storage.set('k', { n = 7 })\n  flux.storage.secure.set('tok', 'abc')\n  flux.storage.set('back', { got = flux.storage.secure.get('tok'), n = flux.storage.get('k').n })\nend")],
    );
    let h = start_host(d.path(), SandboxMode::Off, |_| {});
    h.run("x:main");
    let v: serde_json::Value = serde_json::from_str(&h.wait_local("x", "back")).unwrap();
    assert_eq!(v, serde_json::json!({ "got": "abc", "n": 7 }));
    assert_eq!(h.wait_local("x", "k"), r#"{"n":7}"#);
}

#[test]
fn privileged_calls_are_performed_by_the_launcher_after_its_own_check() {
    let _serial = serial();
    let d = one(
        "permissions = { 'process.execute' }, grants = { exec = { 'echo' } }",
        &[("main", "function main()\n  local r = flux.process.exec('echo', { 'hi' })\n  flux.storage.set('out', { out = r.stdout, ok = r.success })\nend"),
          ("bad", "function main() flux.process.exec('true') end")],
    );
    let h = start_host(d.path(), SandboxMode::Off, |_| {});
    h.run("x:main");
    let v: serde_json::Value = serde_json::from_str(&h.wait_local("x", "out")).unwrap();
    assert_eq!(v, serde_json::json!({ "out": "hi\n", "ok": true }));
    h.run("x:bad");
    assert!(error(&h).contains("not one of the commands"));
}

#[test]
fn a_host_that_lies_about_its_permissions_is_still_refused_by_the_broker() {
    let _serial = serial();
    // The extension declares nothing. Even if its host skipped the first check, the launcher's
    // own policy decides: simulate that by asking the broker directly through the wire.
    use std::os::unix::net::UnixStream;
    use flux_api::wire::*;
    let (a, b) = UnixStream::pair().unwrap();
    let sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let s2 = sink.clone();
    let broker = flux_runtime::broker::spawn(flux_runtime::broker::BrokerEnv {
        extension: "x".into(),
        dir: std::env::temp_dir(),
        data_dir: std::env::temp_dir().join("x-data"),
        policy: std::sync::Arc::new(PermissionSet::default()),
        storage: std::sync::Arc::new(MemStorage::default()),
        secrets: std::sync::Arc::new(MemSecretStore::default()),
        limits: Limits::default(),
        app_version: "t".into(),
        stats: Default::default(),
        exec_exe: None,
        reply: std::sync::Arc::new(move |corr, r| s2.lock().unwrap().push((corr, r))),
    });
    drop((a, b));
    for (i, (ns, f, args)) in [
        ("process", "exec", serde_json::json!(["true"])),
        ("clipboard", "getText", serde_json::json!([])),
        ("filesystem", "read_text", serde_json::json!(["/etc/hostname"])),
        ("browser", "open", serde_json::json!(["https://example.com"])),
        ("process", "kill", serde_json::json!([999999])),
    ]
    .into_iter()
    .enumerate()
    {
        broker.call(i as u64 + 1, HostCall::Privileged { namespace: ns.into(), function: f.into(), args: args.as_array().unwrap().clone() });
    }
    // Something that is not brokered at all:
    broker.call(100, HostCall::Privileged { namespace: "lifecycle".into(), function: "on_unload".into(), args: vec![] });
    let end = Instant::now() + Duration::from_secs(5);
    while sink.lock().unwrap().len() < 6 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    let got = sink.lock().unwrap().clone();
    assert_eq!(got.len(), 6, "{got:?}");
    for (corr, r) in got {
        let err = r.expect_err("must be refused");
        assert!(err.contains("permission") || err.contains("cannot be requested"), "call {corr}: {err}");
    }
    broker.stop();
}

#[test]
fn killing_the_host_is_a_crash_and_the_extension_comes_back() {
    let _serial = serial();
    let d = one("", &[("main", LIST)]);
    let h = start_host(d.path(), SandboxMode::Off, |_| {});
    h.run("x:main");
    push(&h);
    // find and kill the host process
    let out = std::process::Command::new("pgrep").args(["-f", "flux-host"]).output().unwrap();
    let pids: Vec<i32> = String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect();
    assert!(!pids.is_empty());
    // only our child: match by parent pid
    let me = std::process::id();
    let mine: Vec<i32> = pids.into_iter().filter(|p| std::fs::read_to_string(format!("/proc/{p}/stat")).ok().and_then(|s| s.rsplit(')').next().and_then(|r| r.split_whitespace().nth(1).map(String::from))).is_some_and(|ppid| ppid == me.to_string())).collect();
    assert_eq!(mine.len(), 1, "{mine:?}");
    unsafe { libc::kill(mine[0], libc::SIGKILL) };
    let reason = h.wait_event(|e| if let RuntimeEvent::ExtensionCrashed { reason, .. } = e { Some(reason.clone()) } else { None });
    assert!(reason.contains("signal 9"), "{reason}");
    h.wait_event(|e| matches!(e, RuntimeEvent::ExtensionLoaded { generation, .. } if *generation >= 2).then_some(()));
    h.run("x:main");
    push(&h);
}

#[test]
fn a_strictly_confined_host_works_and_says_so() {
    let _serial = serial();
    if flux_runtime::confine::landlock_abi() == 0 {
        eprintln!("skipped: this kernel has no Landlock");
        return;
    }
    let d = one("", &[("main", LIST)]);
    let h = start_host(d.path(), SandboxMode::Strict, |_| {});
    h.run("x:main");
    push(&h);
    assert_eq!(isolation(&h), "process, confined");
}


// ------------------------------------------------------------------ helpers for process checks

/// Pids of this process's children whose name contains `needle`.
fn children(needle: &str) -> Vec<i32> {
    let me = std::process::id().to_string();
    let mut out = Vec::new();
    for e in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else { continue };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { continue };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else { continue };
        let comm = &stat[open + 1..close];
        let ppid = stat[close + 2..].split_whitespace().nth(1).unwrap_or("");
        if ppid == me && comm.contains(needle) {
            out.push(pid);
        }
    }
    out
}

fn alive(pid: i32) -> bool {
    // a zombie still has a /proc entry; count it as gone
    std::fs::read_to_string(format!("/proc/{pid}/stat")).map(|s| !s[s.rfind(')').unwrap() + 2..].starts_with('Z')).unwrap_or(false)
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for: {what}");
}

/// Processes on the machine running `sleep <marker>` (our marker for a child of a brokered command).
fn sleepers(marker: &str) -> Vec<i32> {
    let out = std::process::Command::new("pgrep").args(["-f", &format!("sleep {marker}")]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect()
}

// ------------------------------------------------------------------ confinement (real kernel)

fn selftest(network: &str) -> std::collections::HashMap<String, String> {
    let d = tempfile::tempdir().unwrap();
    let (ext, data) = (d.path().join("ext"), d.path().join("data"));
    std::fs::create_dir_all(&ext).unwrap();
    std::fs::write(ext.join("manifest.lua"), "return {}").unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_flux-host"))
        .args(["--selftest-sandbox", ext.to_str().unwrap(), data.to_str().unwrap(), network, "strict"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("apply=ok"), "{text}\n{}", String::from_utf8_lossy(&out.stderr));
    text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn the_kernel_confines_a_host_without_the_network_capability() {
    if flux_runtime::confine::landlock_abi() == 0 {
        eprintln!("skipped: no Landlock on this kernel");
        return;
    }
    let r = selftest("none");
    let report: flux_api::wire::SandboxReport = serde_json::from_str(&r["report"]).unwrap();
    assert!(report.fully_confined(), "{report:?}");
    // what it may do
    for ok in ["read_own_dir", "write_own_data", "read_usr_lib", "thread"] {
        assert_eq!(r[ok], "ok", "{ok}");
    }
    // what it must not
    for denied in [
        "write_own_dir", "write_tmp", "read_etc_passwd", "read_home", "read_ssh", "read_other_proc_environ", "socket_unix", "socket_inet",
        "socket_packet", "execve", "fork", "ptrace", "kill_parent", "tgkill_other", "unshare_userns", "bpf",
    ] {
        assert!(r[denied].starts_with("errno"), "{denied} was allowed: {}", r[denied]);
    }
}

#[test]
fn with_the_network_capability_only_inet_sockets_and_web_ports_open_up() {
    if flux_runtime::confine::landlock_abi() == 0 {
        return;
    }
    let r = selftest("network");
    assert_eq!(r["socket_inet"], "ok");
    assert!(r["socket_unix"].starts_with("errno"), "unix sockets would reach the launcher's control socket");
    assert!(r["socket_packet"].starts_with("errno"));
    assert_eq!(r["connect_port_443"], "errno 111", "443 is allowed through (nothing listens: connection refused)");
    let report: flux_api::wire::SandboxReport = serde_json::from_str(&r["report"]).unwrap();
    if report.landlock_net {
        assert_eq!(r["connect_port_22"], "errno 13", "other ports are refused by Landlock");
    }
}

#[test]
fn strict_mode_refuses_to_run_what_it_cannot_confine_best_effort_runs_and_admits_it() {
    let _serial = serial();
    let d = one("", &[("main", LIST)]);
    // The data folder cannot be created, so the filesystem restriction cannot be built.
    let bad_data = std::path::PathBuf::from("/proc/flux-no-such-dir");
    let h = start_host_in(d.path(), &bad_data, env!("CARGO_BIN_EXE_flux-host").into(), SandboxMode::Strict, |_| {});
    let status = loop {
        let infos = h.next_infos();
        if let ExtensionStatus::Failed { message } = &infos.iter().find(|i| i.name == "x").unwrap().status {
            break message.clone();
        }
    };
    assert!(status.contains("cannot confine"), "{status}");
    drop(h);

    let h = start_host_in(d.path(), &bad_data, env!("CARGO_BIN_EXE_flux-host").into(), SandboxMode::BestEffort, |_| {});
    h.run("x:main");
    push(&h);
    let iso = isolation(&h);
    assert!(iso.starts_with("process, partly confined"), "{iso}");
}

// ------------------------------------------------------------------ failure handling

#[test]
fn a_hung_host_is_killed_for_real() {
    let _serial = serial();
    let d = one("", &[("hang", "function main() string.find(string.rep('a', 40), string.rep('a?', 40) .. string.rep('a', 40)) end"), ("main", LIST)]);
    let h = start_host(d.path(), SandboxMode::Off, |_| {});
    h.run("x:main");
    push(&h);
    let before = children("flux-host");
    assert_eq!(before.len(), 1, "{before:?}");
    h.run("x:hang");
    let reason = h.wait_event(|e| if let RuntimeEvent::ExtensionCrashed { reason, .. } = e { Some(reason.clone()) } else { None });
    assert!(reason.contains("unresponsive"), "{reason}");
    wait_until("the stuck process to be gone", || !alive(before[0]));
    // and the extension recovers in a new process
    h.wait_event(|e| matches!(e, RuntimeEvent::ExtensionLoaded { generation, .. } if *generation >= 2).then_some(()));
    h.run("x:main");
    push(&h);
}

#[test]
fn a_host_that_stops_answering_pings_is_killed() {
    let _serial = serial();
    let d = one("", &[("main", LIST)]);
    let h = start_host(d.path(), SandboxMode::Off, |o| {
        o.ping_ms = 100;
        o.ping_timeout_ms = 1000;
    });
    h.run("x:main");
    push(&h);
    let pids = children("flux-host");
    assert_eq!(pids.len(), 1);
    unsafe { libc::kill(pids[0], libc::SIGSTOP) }; // frozen: alive but silent
    let reason = h.wait_event(|e| if let RuntimeEvent::ExtensionCrashed { reason, .. } = e { Some(reason.clone()) } else { None });
    assert!(reason.contains("stopped answering"), "{reason}");
    wait_until("the frozen process to be gone", || !alive(pids[0]));
}

#[test]
fn cancelling_a_command_kills_the_process_the_launcher_started_for_it() {
    let _serial = serial();
    let d = one(
        "permissions = { 'process.execute' }, grants = { exec = { 'sleep' } }",
        &[("main", "function main() flux.process.exec('sleep', { '31.11' }, { timeout = 60 }) end")],
    );
    let h = start_host(d.path(), SandboxMode::Off, |_| {});
    h.run("x:main");
    wait_until("the command's child to start", || !sleepers("31.11").is_empty());
    let pids = sleepers("31.11");
    h.handle.send(RuntimeRequest::CancelCommand { command_id: "x:main".into() });
    wait_until("the child to be killed", || pids.iter().all(|p| !alive(*p)));
}

#[test]
fn disabling_an_extension_and_crashing_it_leave_no_children_behind() {
    let _serial = serial();
    let d = one(
        "permissions = { 'process.execute' }, grants = { exec = { 'sleep' } }",
        &[("main", "function main() flux.process.exec('sleep', { '32.22' }, { timeout = 60 }) end")],
    );
    let h = start_host(d.path(), SandboxMode::Off, |_| {});
    h.run("x:main");
    wait_until("child", || !sleepers("32.22").is_empty());
    let kids = sleepers("32.22");
    h.handle.send(RuntimeRequest::SetEnabled { name: "x".into(), enabled: false });
    wait_until("children to be killed after unload", || kids.iter().all(|p| !alive(*p)));

    h.handle.send(RuntimeRequest::SetEnabled { name: "x".into(), enabled: true });
    h.wait_event(|e| matches!(e, RuntimeEvent::ExtensionLoaded { .. }).then_some(()));
    h.run("x:main");
    wait_until("child again", || !sleepers("32.22").is_empty());
    let kids = sleepers("32.22");
    let host = children("flux-host");
    unsafe { libc::kill(host[0], libc::SIGKILL) };
    wait_until("children to be killed after a crash", || kids.iter().all(|p| !alive(*p)));
}

#[test]
fn a_cgroup_scope_carries_the_limits_when_systemd_is_available() {
    let _serial = serial();
    let ok = std::process::Command::new("systemd-run").args(["--user", "--scope", "--quiet", "--collect", "true"]).status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        eprintln!("skipped: no systemd user scope available");
        return;
    }
    let d = one("", &[("main", LIST)]);
    let h = start_host(d.path(), SandboxMode::Off, |o| {
        o.cgroup = true;
        o.memory_mb = 300;
        o.max_tasks = 40;
    });
    h.run("x:main");
    push(&h);
    let pids = children("flux-host");
    assert_eq!(pids.len(), 1, "{pids:?}");
    let cg = std::fs::read_to_string(format!("/proc/{}/cgroup", pids[0])).unwrap();
    let path = cg.trim().rsplit("::").next().unwrap().to_string();
    assert!(path.ends_with(".scope"), "{path}");
    let read = |f: &str| std::fs::read_to_string(format!("/sys/fs/cgroup{path}/{f}")).unwrap().trim().to_string();
    assert_eq!(read("memory.max"), (300u64 * 1024 * 1024).to_string());
    assert_eq!(read("pids.max"), "40");
}


// ------------------------------------------------------------------ a misbehaving host

/// Writes an executable script that stands in for the host program (`<script> host`, fd 3 is the
/// connection). `body` is shell code.
fn fake_host(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-host.sh");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Shell command that writes one frame (length prefix + `json`) to the launcher.
fn send_frame(json: &str) -> String {
    let len = (json.len() as u32).to_le_bytes();
    format!("printf '\\{:03o}\\{:03o}\\{:03o}\\{:03o}%s' '{json}' >&3", len[0], len[1], len[2], len[3])
}

fn crash_reason_of_fake(body: &str) -> String {
    let _serial = serial();
    let d = one("", &[("main", LIST)]);
    let exe = fake_host(d.path(), body);
    let h = start_host_in(d.path(), &d.path().join(".data"), exe, SandboxMode::Off, |_| {});
    h.wait_event(|e| if let RuntimeEvent::ExtensionCrashed { reason, .. } = e { Some(reason.clone()) } else { None })
}

#[test]
fn a_host_that_exits_at_once_is_reported_with_its_exit_code() {
    let r = crash_reason_of_fake("exit 7");
    assert!(r.contains("exit code 7"), "{r}");
}

#[test]
fn a_frame_that_claims_to_be_enormous_is_a_protocol_error() {
    let r = crash_reason_of_fake("printf '\\377\\377\\377\\177' >&3\nsleep 30");
    assert!(r.contains("protocol error") && r.contains("exceeds"), "{r}");
}

#[test]
fn a_frame_that_is_not_json_is_a_protocol_error_and_the_host_is_killed() {
    let _serial = serial();
    let d = one("", &[("main", LIST)]);
    let exe = fake_host(d.path(), "printf '\\005\\000\\000\\000nope!' >&3\nsleep 30");
    let h = start_host_in(d.path(), &d.path().join(".data"), exe, SandboxMode::Off, |_| {});
    let before = Instant::now();
    let r = h.wait_event(|e| if let RuntimeEvent::ExtensionCrashed { reason, .. } = e { Some(reason.clone()) } else { None });
    assert!(r.contains("malformed frame"), "{r}");
    assert!(before.elapsed() < Duration::from_secs(5), "the 30 s sleeper was killed, not waited for");
}

#[test]
fn a_host_speaking_an_unsupported_protocol_version_is_refused() {
    let r = crash_reason_of_fake(&format!("{}\nsleep 30", send_frame(r#"{"v":0,"id":1,"body":{"kind":"unloaded"}}"#)));
    assert!(r.contains("speaks protocol 0"), "{r}");
}

#[test]
fn unknown_message_kinds_from_a_newer_host_are_ignored() {
    let _serial = serial();
    let d = one("", &[("main", LIST)]);
    let body = format!("{}\n{}\nsleep 30", send_frame(r#"{"v":7,"id":1,"body":{"kind":"telemetry","x":1}}"#), send_frame(r#"{"v":1,"id":2,"body":{"kind":"call","call":{"op":"brand_new"}}}"#));
    let exe = fake_host(d.path(), &body);
    let h = start_host_in(d.path(), &d.path().join(".data"), exe, SandboxMode::Off, |_| {});
    // Nothing crashes: the launcher answers the unknown call with an error and carries on.
    let events = h.drain_for(Duration::from_millis(1500));
    assert!(!events.iter().any(|e| matches!(e, RuntimeEvent::ExtensionCrashed { .. })), "{events:?}");
}

#[test]
fn a_host_claiming_to_be_confined_when_it_is_not_is_refused_in_strict_mode() {
    let _serial = serial();
    let d = one("", &[("main", LIST)]);
    let ready = r#"{"v":1,"id":1,"body":{"kind":"ready","proto":1,"sandbox":{"mode":"strict","no_new_privs":true,"rlimits":true,"landlock_abi":null,"landlock_net":false,"seccomp":false,"missing":[]}}}"#;
    let exe = fake_host(d.path(), &format!("{}\nsleep 30", send_frame(ready)));
    let h = start_host_in(d.path(), &d.path().join(".data"), exe, SandboxMode::Strict, |_| {});
    let r = h.wait_event(|e| if let RuntimeEvent::ExtensionCrashed { reason, .. } = e { Some(reason.clone()) } else { None });
    assert!(r.contains("not fully confined"), "{r}");
}

#[test]
fn status_reports_the_host_process_its_memory_and_its_state() {
    let _serial = serial();
    let d = one("", &[("main", LIST)]);
    let h = start_host(d.path(), SandboxMode::Off, |_| {});
    h.run("x:main");
    push(&h);
    wait_until("the first pong", || h.handle.stats().iter().any(|s| s.name == "x" && s.lua_memory_kb == 0 && s.pid.is_some()));
    let s = h.handle.stats().into_iter().find(|s| s.name == "x").unwrap();
    assert_eq!(s.state, "running");
    assert_eq!(s.pid, Some(children("flux-host")[0] as u32));
    assert!(s.rss_kb.unwrap() > 1000 && s.cpu_ms.is_some(), "{s:?}");
    assert!(s.generation >= 1 && s.recent_crashes == 0);
}

#[test]
fn status_counts_background_processes_started_for_the_extension() {
    let _serial = serial();
    let d = one(
        "permissions = { 'process.execute' }, grants = { exec = { 'sleep' } }",
        &[("main", "function main() flux.process.spawn('sleep', { '33.33' }) end")],
    );
    let h = start_host(d.path(), SandboxMode::Off, |_| {});
    h.run("x:main");
    wait_until("one tracked child", || h.handle.stats().iter().any(|s| s.name == "x" && s.children == 1));
}

#[test]
fn http_is_performed_by_the_launcher_and_binary_bodies_survive() {
    use std::io::{Read, Write};
    let _serial = serial();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let body: Vec<u8> = (0..=255u8).cycle().take(5000).collect(); // not valid UTF-8
    let served = body.clone();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = s.write_all(format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: application/octet-stream\r\nconnection: close\r\n\r\n", served.len()).as_bytes());
            let _ = s.write_all(&served);
        }
    });
    let d = one(
        &format!("permissions = {{ 'network' }}, allowed_hosts = {{ '127.0.0.1' }}"),
        &[("main", &format!("function main()\n  local r = flux.http.get('http://127.0.0.1:{port}/bin')\n  local sum = 0\n  for i = 1, #r.body do sum = sum + r.body:byte(i) end\n  flux.storage.set('out', {{ status = r.status, len = #r.body, sum = sum }})\nend"))],
    );
    let h = start_host(d.path(), SandboxMode::Off, |_| {});
    h.run("x:main");
    let v: serde_json::Value = serde_json::from_str(&h.wait_local("x", "out")).unwrap();
    assert_eq!(v["status"], 200);
    assert_eq!(v["len"], 5000, "{v}");
    assert_eq!(v["sum"].as_u64().unwrap(), body.iter().map(|b| *b as u64).sum::<u64>());
}

#[test]
fn a_confined_host_has_no_network_even_with_the_network_capability() {
    if flux_runtime::confine::landlock_abi() == 0 {
        return;
    }
    let _serial = serial();
    let d = one("permissions = { 'network' }, allowed_hosts = { 'example.com' }", &[("main", LIST)]);
    let h = start_host(d.path(), SandboxMode::Strict, |_| {});
    h.run("x:main");
    push(&h);
    assert_eq!(isolation(&h), "process, confined");
    // The host process cannot create an internet socket: look at its seccomp-visible behaviour through /proc.
    let pid = children("flux-host")[0];
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    assert!(status.contains("Seccomp:\t2"), "a seccomp filter is installed: {status}");
}
