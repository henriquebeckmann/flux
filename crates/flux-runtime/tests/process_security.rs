//! Commands run for an extension are confined; `process.kill` only reaches what the extension started.

mod common;

use std::time::Duration;

use common::*;
use flux_api::{wire::SandboxMode, *};

fn mode() -> SandboxMode {
    if flux_runtime::confine::landlock_abi() == 0 { SandboxMode::BestEffort } else { SandboxMode::Strict }
}

/// One extension `x`; `manifest_extra` is spliced into the manifest, `body` is the command.
fn setup(manifest_extra: &str, body: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(&d.path().join("x"), "manifest.lua", &format!("return {{ name = 'x', api_version = 1, commands = {{ {{ name = 't', mode = 'no-view' }} }}, {manifest_extra} }}"));
    write(&d.path().join("x"), "commands/t.lua", &format!("function main()\nlocal out = {{}}\n{body}\nflux.storage.set('out', out)\nend"));
    d
}

fn run(h: &Harness) -> serde_json::Value {
    h.run("x:t");
    serde_json::from_str(&h.wait_local("x", "out")).unwrap()
}

fn skip() -> bool {
    if flux_runtime::confine::landlock_abi() == 0 {
        eprintln!("skipped: no Landlock");
        return true;
    }
    false
}

#[test]
fn a_confined_command_cannot_read_the_home_folder_or_write_outside_its_data_dir() {
    if skip() {
        return;
    }
    let home = std::env::var("HOME").unwrap();
    let d = setup(
        "permissions = { 'process.execute' }, grants = { exec = { 'sh', 'cat' } }",
        &format!(
            "local r = flux.process.exec('sh', {{ '-c', 'ls {home} >/dev/null 2>&1; echo home=$?; echo x > /tmp/flux-confined-evil 2>/dev/null; echo tmp=$?; cat /etc/hostname >/dev/null; echo etc=$?' }})\nout.text = r.stdout"
        ),
    );
    let h = start_host(d.path(), mode(), |_| {});
    h.next_commands();
    let o = run(&h);
    let t = o["text"].as_str().unwrap();
    assert!(t.contains("etc=0"), "ordinary system files stay readable: {t}");
    assert!(!t.contains("home=0") && t.contains("home="), "{t}");
    assert!(!t.contains("tmp=0"), "{t}");
    assert!(!std::path::Path::new("/tmp/flux-confined-evil").exists());
}

#[test]
fn a_confined_command_can_use_the_extensions_data_dir() {
    if skip() {
        return;
    }
    let d = setup(
        "permissions = { 'process.execute' }, grants = { exec = { 'sh' } }",
        "local dir = flux.environment.data_dir\nlocal r = flux.process.exec('sh', { '-c', 'echo hi > \"$0/note\" && cat \"$0/note\"', dir })\nout.text = r.stdout",
    );
    let h = start_host(d.path(), mode(), |_| {});
    h.next_commands();
    assert_eq!(run(&h)["text"], "hi\n");
}

#[test]
fn a_command_listed_as_unconfined_runs_with_the_users_access() {
    if skip() {
        return;
    }
    let home = std::env::var("HOME").unwrap();
    let d = setup(
        "permissions = { 'process.execute' }, grants = { exec = { 'ls' }, exec_unconfined = { 'ls' } }",
        &format!("local r = flux.process.exec('ls', {{ '{home}' }})\nout.ok = r.success"),
    );
    let h = start_host(d.path(), mode(), |_| {});
    h.next_commands();
    assert_eq!(run(&h)["ok"], true);
}

#[test]
fn a_command_that_cannot_be_confined_is_not_run_outside_the_launcher_path() {
    // PATH lookup is the launcher's: a program the extension could plant in its own folder is not found by name.
    let d = setup("permissions = { 'process.execute' }, grants = { exec = { 'planted' } }", "local ok, e = pcall(flux.process.exec, 'planted')\nout.err = tostring(e)");
    write(&d.path().join("x"), "planted", "#!/bin/sh\necho owned\n");
    let h = start_host(d.path(), mode(), |_| {});
    h.next_commands();
    assert!(run(&h)["err"].as_str().unwrap().contains("not found in PATH"));
}

#[test]
fn kill_reaches_only_what_the_extension_started_unless_it_holds_grants_kill_user() {
    // `sleep` started by the test, not by the extension
    let mut foreign = std::process::Command::new("sleep").arg("41.41").spawn().unwrap();
    let pid = foreign.id();
    let body = format!(
        "local mine = flux.process.spawn('sleep', {{ '41.42' }})\nout.own = flux.process.kill(mine)\nlocal ok, e = pcall(flux.process.kill, {pid})\nout.foreign = tostring(e)"
    );
    let d = setup("permissions = { 'process.execute', 'process.kill' }, grants = { exec = { 'sleep' } }", &body);
    let h = start_host(d.path(), mode(), |_| {});
    h.next_commands();
    let o = run(&h);
    assert_eq!(o["own"], true);
    assert!(o["foreign"].as_str().unwrap().contains("was not started by this extension"), "{o}");
    assert!(foreign.try_wait().unwrap().is_none(), "the foreign process is still alive");
    drop(h);

    let d = setup("permissions = { 'process.kill' }, grants = { kill = 'user' }", &format!("out.foreign = flux.process.kill({pid})"));
    let h = start_host(d.path(), mode(), |_| {});
    h.next_commands();
    assert_eq!(run(&h)["foreign"], true);
    let end = std::time::Instant::now() + Duration::from_secs(5);
    while foreign.try_wait().unwrap().is_none() && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(foreign.try_wait().unwrap().is_some(), "grants.kill = user allowed it");
}

#[test]
fn flux_processes_are_never_signalled_even_with_grants_kill_user() {
    // the launcher (this test process), and Flux's own hosts by program name
    let d = setup(
        "permissions = { 'process.kill' }, grants = { kill = 'user' }",
        &format!("local ok, e = pcall(flux.process.kill, {})\nout.launcher = tostring(e)\nlocal ok2, e2 = pcall(flux.process.kill, tonumber(flux.storage.get('host')))\nout.host = tostring(e2)", std::process::id()),
    );
    let h = start_host(d.path(), mode(), |_| {});
    h.next_commands();
    let host_pid = loop {
        if let Some(p) = h.handle.stats().into_iter().find(|s| s.name == "x").and_then(|s| s.pid) {
            break p;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    h.storage.local_set("x", "host", &host_pid.to_string());
    let o = run(&h);
    assert!(o["launcher"].as_str().unwrap().contains("Flux's own processes"), "{o}");
    assert!(o["host"].as_str().unwrap().contains("Flux's own processes"), "{o}");
}
