//! Resource limits: refused clearly, released when the work ends.

mod common;

use std::time::Duration;

use common::*;
use flux_api::*;

fn one(perms: &[&str], grants: &str, commands: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let cmds: String = commands.iter().map(|(n, _)| format!("{{ name = '{n}', mode = 'view' }},")).collect();
    let perms: String = perms.iter().map(|p| format!("'{p}',")).collect();
    write(&d.path().join("x"), "manifest.lua", &format!("return {{ name = 'x', api_version = 1, commands = {{ {cmds} }}, permissions = {{ {perms} }}, {grants} }}"));
    for (n, src) in commands {
        write(&d.path().join("x"), &format!("commands/{n}.lua"), src);
    }
    d
}

fn started(d: &tempfile::TempDir, limits: Limits) -> Harness {
    let h = start_with(d.path(), limits, std::sync::Arc::new(MemSecretStore::default()));
    h.next_commands();
    h
}

fn out(h: &Harness) -> serde_json::Value {
    serde_json::from_str(&h.wait_local("x", "out")).unwrap()
}

#[test]
fn coroutines_are_limited_but_finished_ones_do_not_count() {
    let d = one(&[], "", &[(
        "main",
        "function main()\n  local out = {}\n  for i = 1, 500 do\n    local co = coroutine.wrap(function() return i end)\n    co()\n  end\n  out.finished_ok = true\n  local kept = {}\n  local ok, err = pcall(function() for i = 1, 100 do kept[i] = coroutine.create(function() coroutine.yield() end) coroutine.resume(kept[i]) end end)\n  out.over = tostring(err)\n  flux.storage.set('out', out)\nend",
    )]);
    let h = started(&d, Limits { max_coroutines: 50, ..Limits::default() });
    h.run("x:main");
    let o = out(&h);
    assert_eq!(o["finished_ok"], true, "500 short-lived coroutines are fine");
    assert!(o["over"].as_str().unwrap().contains("too many coroutines (max 50)"), "{o}");
}

#[test]
fn background_processes_are_limited_and_released_when_they_end() {
    let d = one(
        &["process.execute"],
        "grants = { exec = { 'sleep' } }",
        &[("main", "function main()\n  local out = {}\n  out.a = flux.process.spawn('sleep', { '1.4' })\n  out.b = flux.process.spawn('sleep', { '1.4' })\n  local ok, err = pcall(flux.process.spawn, 'sleep', { '1.4' })\n  out.third = tostring(err)\n  flux.task.sleep(2.2)\n  out.again = flux.process.spawn('sleep', { '0.1' })\n  flux.storage.set('out', out)\nend")],
    );
    let h = started(&d, Limits { max_children: 2, ..Limits::default() });
    h.run("x:main");
    let o = out(&h);
    assert!(o["third"].as_str().unwrap().contains("too many background processes (max 2)"), "{o}");
    assert!(o["again"].is_number(), "slots are free again once the children have exited: {o}");
}

#[test]
fn exec_output_is_capped_and_says_so() {
    let d = one(
        &["process.execute"],
        "grants = { exec = { 'head' } }",
        &[("main", "function main()\n  local r = flux.process.exec('head', { '-c', '50000', '/dev/zero' })\n  flux.storage.set('out', { len = #r.stdout, truncated = r.truncated, ok = r.success })\nend")],
    );
    let h = started(&d, Limits { exec_output_kb: 2, ..Limits::default() });
    h.run("x:main");
    let o = out(&h);
    assert_eq!((o["len"].as_u64(), o["truncated"].as_bool(), o["ok"].as_bool()), (Some(2048), Some(true), Some(true)));
}

#[test]
fn exec_timeouts_are_capped() {
    let d = one(
        &["process.execute"],
        "grants = { exec = { 'sleep' } }",
        &[("main", "function main()\n  local ok, err = pcall(flux.process.exec, 'sleep', { '30' }, { timeout = 99999 })\n  flux.storage.set('out', { err = tostring(err) })\nend")],
    );
    let h = started(&d, Limits { exec_timeout_max_s: 1, ..Limits::default() });
    h.run("x:main");
    let o = out(&h);
    assert!(o["err"].as_str().unwrap().contains("exceeded the 1s timeout"), "{o}");
}

#[test]
fn too_many_concurrent_things_are_refused_with_a_message() {
    let d = one(
        &[],
        "",
        &[("main", "function main()\n  return flux.ui.List { flux.ui.List.Item { title = 'A', actions = { flux.ui.Action { title = 'Slow', on_action = function() flux.task.sleep(5) end } } } }\nend")],
    );
    let h = started(&d, Limits { max_tasks: 4, ..Limits::default() });
    h.run("x:main");
    let (view_id, node) = h.wait_event(|e| if let RuntimeEvent::Nav(NavEvent::Push { view_id, view }) = e { Some((*view_id, view.clone())) } else { None });
    let ViewNode::List(l) = node else { panic!() };
    let action = l.items().next().unwrap().actions[0].actions[0].id;
    for _ in 0..6 {
        h.handle.send(RuntimeRequest::InvokeAction { view_id, action_id: action });
    }
    let msg = h.wait_event(|e| if let RuntimeEvent::ExtensionError { message, .. } = e { Some(message.clone()) } else { None });
    assert!(msg.contains("too many things running at once (max 4)"), "{msg}");
    let _ = h.drain_for(Duration::from_millis(100));
}
