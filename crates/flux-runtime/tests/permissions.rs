//! Granular permissions (`grants`), user overrides and manifest versioning, end to end.

mod common;

use std::time::Duration;

use common::*;
use flux_api::*;

/// One extension `e` whose manifest is `manifest_body` (the part after `name = 'e',`) with a single
/// no-view command `t` running `body`.
fn setup(manifest_body: &str, body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    write(&d.path().join("e"), "manifest.lua", &format!("return {{ name = 'e', api_version = 1, commands = {{ {{ name = 't', mode = 'no-view' }} }}, {manifest_body} }}"));
    write(&d.path().join("e"), "commands/t.lua", &format!("function main()\n{body}\nflux.storage.set('out', out)\nend"));
    let root = d.path().to_path_buf();
    (d, root)
}

/// Runs `t` and returns what it stored in `out`, or the extension error.
fn run(h: &Harness) -> Result<serde_json::Value, String> {
    h.run("e:t");
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < end {
        if let Some(v) = h.storage.local_get("e", "out") {
            return Ok(serde_json::from_str(&v).unwrap());
        }
        if let Some(RuntimeEvent::ExtensionError { message, .. }) = h.recv(Duration::from_millis(20)) {
            return Err(message);
        }
    }
    panic!("timed out");
}

fn start_plain(root: &std::path::Path) -> Harness {
    let h = start(root);
    h.next_commands();
    h
}

#[test]
fn exec_is_limited_to_the_granted_commands() {
    let (_d, root) = setup("permissions = { 'process.execute' }, grants = { exec = { 'true' } }", "local out = {}\nout.ok = flux.process.exec('true').success\nflux.process.exec('false')");
    let h = start_plain(&root);
    let err = run(&h).unwrap_err();
    assert!(err.contains("'false' is not one of the commands") && err.contains("grants.exec"), "{err}");
}

#[test]
fn a_path_cannot_pretend_to_be_a_granted_command() {
    let (_d, root) = setup("permissions = { 'process.execute' }, grants = { exec = { 'true' } }", "local out = {}\nflux.process.exec('/bin/true')");
    let h = start_plain(&root);
    assert!(run(&h).unwrap_err().contains("not one of the commands"));
}

#[test]
fn without_a_grant_list_any_command_runs_but_validate_warns() {
    let (_d, root) = setup("permissions = { 'process.execute' }", "local out = { ok = flux.process.exec('true').success }");
    let h = start_plain(&root);
    assert_eq!(run(&h).unwrap()["ok"], true);
    let r = flux_runtime::validate::validate_dir(&root.join("e"));
    assert!(r.warnings.iter().any(|w| w.message.contains("without grants.exec")), "{r:?}");
}

#[test]
fn reading_is_limited_to_granted_directories_but_own_files_stay_readable() {
    let allowed = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    std::fs::write(allowed.path().join("a.txt"), "yes").unwrap();
    std::fs::write(other.path().join("b.txt"), "no").unwrap();
    let body = format!(
        "local out = {{}}\nout.a = flux.filesystem.read_text('{}/a.txt')\nlocal ok, e = pcall(flux.filesystem.read_text, '{}/b.txt')\nout.denied = tostring(e)",
        allowed.path().display(),
        other.path().display()
    );
    let (_d, root) = setup(&format!("permissions = {{ 'filesystem.read' }}, grants = {{ fs_read = {{ '{}' }} }}", allowed.path().display()), &body);
    let h = start_plain(&root);
    let o = run(&h).unwrap();
    assert_eq!(o["a"], "yes");
    assert!(o["denied"].as_str().unwrap().contains("grants.fs_read"), "{o}");
}

#[test]
fn writing_needs_the_capability_and_stays_in_data_dir_or_granted_dirs() {
    let granted = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let body = format!(
        "local out = {{}}\nout.own = flux.filesystem.write_text(flux.environment.data_dir .. '/note.txt', 'hi')\nout.granted = flux.filesystem.write_text('{g}/x.txt', 'yo')\nlocal ok, e = pcall(flux.filesystem.write_text, '{o}/x.txt', 'no')\nout.denied = tostring(e)",
        g = granted.path().display(),
        o = other.path().display()
    );
    let (_d, root) = setup(&format!("permissions = {{ 'filesystem.write' }}, grants = {{ fs_write = {{ '{}' }} }}", granted.path().display()), &body);
    let h = start_plain(&root);
    let r = run(&h);
    let o = r.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(o["own"], true);
    assert_eq!(std::fs::read_to_string(granted.path().join("x.txt")).unwrap(), "yo");
    assert!(!other.path().join("x.txt").exists());
    assert!(o["denied"].as_str().unwrap().contains("grants.fs_write"), "{o}");
}

#[test]
fn credentials_become_strict_once_either_is_declared() {
    let (_d, root) = setup("permissions = { 'credentials.read' }", "local out = {}\nlocal ok, e = pcall(flux.storage.secure.set, 'k', 'v')\nout.err = tostring(e)");
    let h = start_plain(&root);
    let o = run(&h).unwrap();
    assert!(o["err"].as_str().unwrap().contains("credentials.write"), "{o}");
}

#[test]
fn the_user_can_revoke_a_declared_permission() {
    let (_d, root) = setup("permissions = { 'process.execute' }", "local out = { ok = flux.process.exec('true').success }");
    let file = root.join("permissions.toml");
    std::fs::write(&file, "[extensions.e]\nrevoke = [\"process.execute\"]\n").unwrap();
    let h = start_user_permissions(&root, &file);
    let infos = h.next_infos();
    h.next_commands();
    let err = run(&h).unwrap_err();
    assert!(err.contains("process.execute") && err.contains("flux permissions"), "{err}");
    assert!(!infos.iter().find(|i| i.name == "e").unwrap().permissions.contains(&"process.execute".to_string()));
}

#[test]
fn the_user_can_narrow_but_not_widen_commands() {
    let (_d, root) = setup("permissions = { 'process.execute' }, grants = { exec = { 'true', 'false' } }", "local out = { ok = flux.process.exec('true').success }");
    let file = root.join("permissions.toml");
    std::fs::write(&file, "[extensions.e]\nexec = [\"false\", \"rm\"]\n").unwrap();
    let h = start_user_permissions(&root, &file);
    h.next_commands();
    assert!(run(&h).unwrap_err().contains("not one of the commands"), "'true' was dropped by the user, 'rm' was never granted");
}

#[test]
fn a_broken_permissions_file_is_reported_and_ignored() {
    let (_d, root) = setup("permissions = { 'process.execute' }", "local out = { ok = flux.process.exec('true').success }");
    let file = root.join("permissions.toml");
    std::fs::write(&file, "this is = not [valid").unwrap();
    let h = start_user_permissions(&root, &file);
    let ev = h.wait_event(|e| if let RuntimeEvent::ExtensionError { message, .. } = e { Some(message.clone()) } else { None });
    assert!(ev.contains("permissions.toml") && ev.contains("ignoring"), "{ev}");
    h.next_commands();
    assert_eq!(run(&h).unwrap()["ok"], true, "extensions still load");
}

// ------------------------------------------------------------------ versioning

fn infos_with(manifest: &str) -> Vec<ExtensionInfo> {
    let d = tempfile::tempdir().unwrap();
    write(&d.path().join("v"), "manifest.lua", manifest);
    start(d.path()).next_infos()
}

#[test]
fn an_unsupported_api_version_says_what_is_supported() {
    for (v, hint) in [("2", "Update Flux"), ("0", "Update the extension")] {
        let infos = infos_with(&format!("return {{ name = 'v', api_version = {v} }}"));
        let ExtensionStatus::Invalid { errors } = &infos[0].status else { panic!("{:?}", infos[0].status) };
        let all = errors.join("\n");
        assert!(all.contains(&format!("targets API {v}")) && all.contains("supports API 1") && all.contains(hint), "{all}");
    }
}

#[test]
fn api_version_may_be_a_string_and_min_flux_version_is_checked() {
    assert_eq!(infos_with("return { name = 'v', api_version = '1' }")[0].status, ExtensionStatus::Loaded);
    let infos = infos_with("return { name = 'v', api_version = 1, min_flux_version = '99.0.0' }");
    let ExtensionStatus::Invalid { errors } = &infos[0].status else { panic!() };
    assert!(errors.join(" ").contains("needs Flux 99.0.0"), "{errors:?}");
}

#[test]
fn reserved_and_unknown_permissions_are_rejected_legacy_ones_still_load() {
    let infos = infos_with("return { name = 'v', api_version = 1, permissions = { 'window.control' } }");
    let ExtensionStatus::Invalid { errors } = &infos[0].status else { panic!() };
    assert!(errors.join(" ").contains("reserved"), "{errors:?}");
    let infos = infos_with("return { name = 'v', api_version = 1, permissions = { 'clipboard', 'window' } }");
    assert_eq!(infos[0].status, ExtensionStatus::Loaded);
    assert!(infos[0].permissions.contains(&"clipboard.read".to_string()) && infos[0].permissions.contains(&"clipboard.write".to_string()));
}

#[test]
fn deprecated_names_warn_in_validate() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "manifest.lua", "return { name = 'v', api_version = 1, permissions = { 'process' }, commands = { { name = 'c' } } }");
    write(d.path(), "commands/c.lua", "function main() return flux.ui.list { } end");
    let w: Vec<String> = flux_runtime::validate::validate_dir(d.path()).warnings.iter().map(|w| w.to_string()).collect();
    assert!(w.iter().any(|m| m.contains("'process' is deprecated") && m.contains("process.execute")), "{w:?}");
    assert!(w.iter().any(|m| m.contains("flux.ui.list is deprecated") && m.contains("flux.ui.List") && m.contains("API 2")), "{w:?}");
}
