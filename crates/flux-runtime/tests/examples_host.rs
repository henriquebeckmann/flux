//! The example extensions, each in its own strictly confined host process: everything that needs
//! the outside world (listing files, running `ps`) goes through the broker.

mod common;

use std::time::Duration;

use common::*;
use flux_api::{wire::SandboxMode, *};

fn run_example(ext: &str, command: &str) -> ViewNode {
    let d = tempfile::tempdir().unwrap();
    install_example(d.path(), ext);
    let strict = if flux_runtime::confine::landlock_abi() == 0 { SandboxMode::BestEffort } else { SandboxMode::Strict };
    let h = start_host_in(d.path(), &d.path().join(".data"), env!("CARGO_BIN_EXE_flux-host").into(), strict, |o| o.ping_timeout_ms = 20_000);
    h.run(&format!("{ext}:{command}"));
    let end = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        assert!(std::time::Instant::now() < end, "{ext}:{command} produced no screen");
        match h.recv(Duration::from_millis(50)) {
            Some(RuntimeEvent::Nav(NavEvent::Push { view, .. })) => return view,
            Some(RuntimeEvent::ExtensionError { extension, message }) => panic!("{extension}: {message}"),
            Some(RuntimeEvent::ExtensionCrashed { reason, .. }) => panic!("crashed: {reason}"),
            _ => {}
        }
    }
}

#[test]
fn calculator_in_a_host() {
    assert!(matches!(run_example("calculator", "main"), ViewNode::List(_) | ViewNode::Input(_)));
}

#[test]
fn files_lists_the_home_folder_through_the_broker() {
    let ViewNode::List(l) = run_example("files", "browse") else { panic!("expected a list") };
    let items: usize = l.sections.iter().map(|s| s.items.len()).sum();
    assert!(items > 0, "the home folder has entries ({items})");
}

#[test]
fn processes_runs_ps_through_the_broker() {
    let d = tempfile::tempdir().unwrap();
    install_example(d.path(), "processes");
    let strict = if flux_runtime::confine::landlock_abi() == 0 { SandboxMode::BestEffort } else { SandboxMode::Strict };
    let h = start_host_in(d.path(), &d.path().join(".data"), env!("CARGO_BIN_EXE_flux-host").into(), strict, |o| o.ping_timeout_ms = 20_000);
    h.run("processes:list");
    let (id, mut node) = h.wait_event(|e| if let RuntimeEvent::Nav(NavEvent::Push { view_id, view }) = e { Some((*view_id, view.clone())) } else { None });
    // The list opens empty and fills in when `ps` (run by the launcher on its behalf) answers.
    h.wait_view(id, &mut node, |n| matches!(n, ViewNode::List(l) if l.items().count() > 3));
}

#[test]
fn the_demo_screens_open_in_a_host() {
    for cmd in ["form", "grid", "detail"] {
        run_example("demo", cmd);
    }
}

#[test]
fn github_starts_in_a_host() {
    // The search itself needs the network; opening the screen does not.
    run_example("github", "search");
}
