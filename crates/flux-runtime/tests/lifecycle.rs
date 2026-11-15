//! Extension lifecycle: reload/disable/crash close the screens the extension owned, commands
//! report cancellation, crashes restart with backoff and eventually stop.

mod common;

use std::time::{Duration, Instant};

use common::*;
use flux_api::*;

const LIST: &str = "function main() return flux.ui.List { flux.ui.List.Item { title = 'A' } } end";
/// Exponential backtracking inside a C function: cannot be interrupted by the instruction hook.
const HANG: &str = "function main() string.find(string.rep('a', 40), string.rep('a?', 40) .. string.rep('a', 40)) end";

fn one_ext(cmds: &[(&str, &str)]) -> (tempfile::TempDir, Vec<(String, String)>) {
    let d = tempfile::tempdir().unwrap();
    let commands: Vec<(&str, &str)> = cmds.iter().map(|(n, _)| (*n, "view")).collect();
    let files: Vec<(String, &str)> = cmds.iter().map(|(n, src)| (format!("commands/{n}.lua"), *src)).collect();
    let files: Vec<(&str, &str)> = files.iter().map(|(p, s)| (p.as_str(), *s)).collect();
    ext(d.path(), "x", &[], &commands, &files);
    (d, vec![])
}

/// The next pushed screen; republished command lists in between are ignored.
fn next_view(h: &Harness) -> (u64, ViewNode) {
    h.wait_event(|e| if let RuntimeEvent::Nav(NavEvent::Push { view_id, view }) = e { Some((*view_id, view.clone())) } else { None })
}

fn fast_policy() -> RestartPolicy {
    RestartPolicy { base_ms: 100, max_ms: 400, max_crashes: 3, window_s: 60, stable_s: 1 }
}

fn status_of(infos: &[ExtensionInfo], name: &str) -> ExtensionStatus {
    infos.iter().find(|i| i.name == name).unwrap().status.clone()
}

#[test]
fn loading_an_extension_announces_it_with_a_generation() {
    let (d, _) = one_ext(&[("main", LIST)]);
    let h = start(d.path());
    let generation = h.wait_event(|e| if let RuntimeEvent::ExtensionLoaded { extension, generation } = e { (extension == "x").then_some(*generation) } else { None });
    assert!(generation >= 1);
}

#[test]
fn reloading_reports_the_screens_the_old_worker_owned() {
    let (d, _) = one_ext(&[("main", LIST)]);
    let h = start(d.path());
    h.next_commands();
    h.run("x:main");
    let (view_id, _) = next_view(&h);
    h.handle.send(RuntimeRequest::ReloadExtension { name: "x".into() });
    let views = h.wait_event(|e| if let RuntimeEvent::ExtensionReloaded { extension, views, .. } = e { (extension == "x").then(|| views.clone()) } else { None });
    assert_eq!(views, vec![view_id]);
    // The new worker serves the extension again with a different view id.
    h.run("x:main");
    let (new_id, _) = next_view(&h);
    assert_ne!(new_id, view_id);
}

#[test]
fn requests_for_a_reloaded_screen_are_ignored_not_misrouted() {
    let (d, _) = one_ext(&[("main", LIST)]);
    let h = start(d.path());
    h.next_commands();
    h.run("x:main");
    let (old, _) = next_view(&h);
    h.handle.send(RuntimeRequest::ReloadExtension { name: "x".into() });
    h.wait_event(|e| matches!(e, RuntimeEvent::ExtensionReloaded { .. }).then_some(()));
    h.run("x:main");
    let _ = next_view(&h);
    h.handle.send(RuntimeRequest::InvokeAction { view_id: old, action_id: 1 });
    assert!(h.drain_for(Duration::from_millis(300)).iter().all(|e| !matches!(e, RuntimeEvent::ExtensionError { .. })));
}

#[test]
fn disabling_unloads_and_reports_views() {
    let (d, _) = one_ext(&[("main", LIST)]);
    let h = start(d.path());
    h.next_commands();
    h.run("x:main");
    let (view_id, _) = next_view(&h);
    h.handle.send(RuntimeRequest::SetEnabled { name: "x".into(), enabled: false });
    let views = h.wait_event(|e| if let RuntimeEvent::ExtensionUnloaded { views, .. } = e { Some(views.clone()) } else { None });
    assert_eq!(views, vec![view_id]);
}

#[test]
fn commands_report_started_finished_and_cancelled() {
    let (d, _) = one_ext(&[("quick", "function main() end"), ("slow", "function main() flux.task.sleep(30) end")]);
    let h = start(d.path());
    h.next_commands();
    h.run("x:quick");
    h.wait_event(|e| matches!(e, RuntimeEvent::CommandStarted { command_id } if command_id == "x:quick").then_some(()));
    h.wait_event(|e| matches!(e, RuntimeEvent::CommandFinished { command_id } if command_id == "x:quick").then_some(()));
    h.run("x:slow");
    h.wait_event(|e| matches!(e, RuntimeEvent::CommandStarted { command_id } if command_id == "x:slow").then_some(()));
    h.handle.send(RuntimeRequest::CancelCommand { command_id: "x:slow".into() });
    h.wait_event(|e| matches!(e, RuntimeEvent::CommandCancelled { command_id } if command_id == "x:slow").then_some(()));
}

#[test]
fn a_hung_worker_is_detected_without_any_request_and_restarted_with_backoff() {
    let (d, _) = one_ext(&[("hang", HANG), ("main", LIST)]);
    let h = start_restart(d.path(), Limits { slice_ms: 50, ..Limits::default() }, fast_policy());
    h.next_commands();
    h.run("x:hang");
    let t = Instant::now();
    let reason = h.wait_event(|e| if let RuntimeEvent::ExtensionCrashed { reason, .. } = e { Some(reason.clone()) } else { None });
    assert!(reason.contains("unresponsive"), "{reason}");
    assert!(t.elapsed() < Duration::from_secs(8), "found by the periodic check, not by the next request");
    let infos = h.next_infos();
    assert!(matches!(status_of(&infos, "x"), ExtensionStatus::Crashed { .. }));
    // After the backoff the extension runs again.
    h.wait_event(|e| matches!(e, RuntimeEvent::ExtensionLoaded { generation, .. } if *generation >= 2).then_some(()));
    h.run("x:main");
    let _ = next_view(&h);
}

#[test]
fn repeated_crashes_stop_the_extension_until_it_is_reloaded() {
    let (d, _) = one_ext(&[("hang", HANG), ("main", LIST)]);
    let policy = RestartPolicy { max_crashes: 2, stable_s: 3600, ..fast_policy() };
    let h = start_restart(d.path(), Limits { slice_ms: 50, ..Limits::default() }, policy);
    h.next_commands();
    for _ in 0..2 {
        h.run("x:hang");
        h.wait_event(|e| matches!(e, RuntimeEvent::ExtensionCrashed { .. }).then_some(()));
        std::thread::sleep(Duration::from_millis(400)); // let the backoff expire
    }
    h.wait_event(|e| match e {
        RuntimeEvent::ExtensionsChanged(infos) if matches!(status_of(infos, "x"), ExtensionStatus::Failed { .. }) => Some(()),
        _ => None,
    });
    h.run("x:main");
    let msg = h.wait_event(|e| if let RuntimeEvent::ExtensionError { message, .. } = e { Some(message.clone()) } else { None });
    assert!(msg.contains("stopped after repeated crashes"), "{msg}");
    // A manual reload gives it a clean slate.
    h.handle.send(RuntimeRequest::ReloadExtension { name: "x".into() });
    h.run("x:main");
    let _ = next_view(&h);
}
