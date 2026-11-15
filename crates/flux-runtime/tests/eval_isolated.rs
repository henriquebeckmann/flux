//! Declarations (`manifest.lua`) are evaluated in a confined child process, not in the launcher.

mod common;

use std::time::{Duration, Instant};

use common::*;
use flux_api::wire::SandboxMode;
use flux_runtime::{eval::evaluate_isolated, hostproc::HostOptions};

fn opts(mode: SandboxMode) -> HostOptions {
    let mut o = HostOptions::new(env!("CARGO_BIN_EXE_flux-host").into());
    o.sandbox = mode;
    o
}

fn mode() -> SandboxMode {
    if flux_runtime::confine::landlock_abi() == 0 { SandboxMode::BestEffort } else { SandboxMode::Strict }
}

fn ext(manifest: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "manifest.lua", manifest);
    d
}

#[test]
fn a_good_manifest_comes_back_as_data() {
    let d = ext("return { name = 'ok', api_version = 1, permissions = { 'network' }, allowed_hosts = { 'a.com' }, commands = { { name = 'c' } }, weird = 1 }");
    write(d.path(), "preferences.lua", "return { { name = 'who', type = 'text', title = 'Who' } }");
    let decl = evaluate_isolated(&opts(mode()), d.path()).unwrap();
    assert_eq!(decl.manifest.name, "ok");
    assert_eq!(decl.manifest.commands.len(), 1);
    assert!(decl.manifest.permissions.has(flux_api::Capability::Network));
    assert_eq!(decl.prefs.len(), 1);
    assert_eq!(decl.unknown_keys, ["weird"]);
}

#[test]
fn invalid_manifests_report_their_errors() {
    let d = ext("return { name = 'BAD NAME', api_version = 1 }");
    let e = evaluate_isolated(&opts(mode()), d.path()).unwrap_err().join("\n");
    assert!(e.contains("manifest.lua") && e.contains("name:"), "{e}");
}

#[test]
fn an_endless_manifest_is_stopped_and_the_caller_is_fine() {
    let d = ext("while true do end return {}");
    let t = Instant::now();
    let e = evaluate_isolated(&opts(mode()), d.path()).unwrap_err().join("\n");
    assert!(t.elapsed() < Duration::from_secs(6), "{:?}", t.elapsed());
    assert!(!e.is_empty(), "an error, not a hang: {e}");
}

#[test]
fn a_manifest_that_hangs_in_c_code_is_killed() {
    let d = ext("local x = string.find(string.rep('a', 40), string.rep('a?', 40) .. string.rep('a', 40)) return {}");
    let t = Instant::now();
    let e = evaluate_isolated(&opts(mode()), d.path()).unwrap_err().join("\n");
    assert!(t.elapsed() < Duration::from_secs(6));
    assert!(e.contains("longer than") || e.contains("failed"), "{e}");
}

#[test]
fn a_memory_bomb_and_forbidden_apis_are_refused() {
    let d = ext("local t = {} for i = 1, 1e9 do t[i] = string.rep('x', 1000) end return {}");
    assert!(evaluate_isolated(&opts(mode()), d.path()).is_err());
    let d = ext("io.open('/etc/passwd') return { name = 'x' }");
    assert!(evaluate_isolated(&opts(mode()), d.path()).is_err());
    let d = ext("os.execute('touch /tmp/flux-eval-pwned') return { name = 'x' }");
    assert!(evaluate_isolated(&opts(mode()), d.path()).is_err());
    assert!(!std::path::Path::new("/tmp/flux-eval-pwned").exists());
}

#[test]
fn results_are_cached_until_the_file_changes() {
    let d = ext("return { name = 'one', api_version = 1 }");
    let o = opts(mode());
    assert_eq!(evaluate_isolated(&o, d.path()).unwrap().manifest.name, "one");
    // same files: the cached answer (an unrunnable evaluator would otherwise fail)
    let broken = HostOptions::new("/nonexistent/flux-host".into());
    assert_eq!(evaluate_isolated(&broken, d.path()).unwrap().manifest.name, "one");
    // changed file: evaluated again
    std::thread::sleep(Duration::from_millis(20));
    write(d.path(), "manifest.lua", "return { name = 'two', api_version = 1, version = '1.0.0' }");
    assert_eq!(evaluate_isolated(&o, d.path()).unwrap().manifest.name, "two");
    assert!(evaluate_isolated(&broken, tempfile::tempdir().unwrap().path()).is_err());
}

#[test]
fn the_launcher_loads_extensions_through_the_evaluator_in_host_mode() {
    let d = tempfile::tempdir().unwrap();
    write(&d.path().join("x"), "manifest.lua", "return { name = 'x', api_version = 1, commands = { { name = 'main', mode = 'view' } } }");
    write(&d.path().join("x"), "commands/main.lua", "function main() return flux.ui.List { flux.ui.List.Item { title = 'A' } } end");
    let h = start_host(d.path(), mode(), |_| {});
    assert!(h.next_commands().iter().any(|c| c.id == "x:main"));
}
