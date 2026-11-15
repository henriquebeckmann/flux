mod common;
#[allow(unused_imports)]
use common::*;
use std::{path::Path, sync::Arc, time::{Duration, Instant}};
use flux_api::*;
use flux_runtime::*;

// ------------------------------------------------------------- demo extension

#[test]
fn demo_registers_commands_and_runs_view_and_actions() {
    let d = demo_dir();
    let h = start(d.path());
    let RuntimeEvent::CommandsRegistered(cmds) = h.next() else { panic!() };
    let ids: Vec<_> = cmds.iter().map(|c| c.id.as_str()).collect();
    assert!(ids.contains(&"demo:hello") && ids.contains(&"demo:sync") && ids.contains(&"demo:__preferences"), "{ids:?}");

    h.run("demo:hello");
    let (view_id, view) = h.next_push();
    let l = list(view);
    assert!(l.item_count() > 0);
    assert!(l.items().any(|i| i.actions.len() >= 1));
    let _ = view_id;
}

// ------------------------------------------------------------------ runtime basics

#[test]
fn no_view_command_uses_isolated_storage() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "a", &[], &[("x", "no-view")], &[("commands/x.lua", "function main() flux.storage.set('runs', 1) end")]);
    ext(d.path(), "b", &[], &[("x", "no-view")], &[("commands/x.lua", "function main() flux.storage.set('runs', 99) end")]);
    let h = start(d.path());
    let _ = h.next();
    h.run("a:x");
    h.run("b:x");
    assert_eq!(h.wait_local("a", "runs"), "1");
    assert_eq!(h.wait_local("b", "runs"), "99");
}

#[test]
fn permission_denied_and_lua_errors_do_not_crash() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "bad", &[], &[("perm", "no-view"), ("boom", "no-view"), ("stub", "no-view"), ("sandbox", "no-view"), ("secret", "no-view")], &[
        ("commands/perm.lua", "function main() flux.clipboard.setText('x') end"),
        ("commands/boom.lua", "function main() error('kaboom') end"),
        ("commands/stub.lua", "function main() flux.notification.show('x') end"),
        ("commands/sandbox.lua", "function main() return io.open('/etc/passwd') end"),
        ("commands/secret.lua", "function main() flux.process.exec('true') end"),
    ]);
    let h = start(d.path());
    let _ = h.next();
    for (cmd, needle) in [("perm", "permission"), ("boom", "kaboom"), ("stub", "not implemented"), ("sandbox", "nil value"), ("secret", "permission")] {
        h.run(&format!("bad:{cmd}"));
        let msg = h.next_error();
        assert!(msg.contains(needle), "{cmd}: {msg}");
    }
}

#[test]
fn invalid_manifest_is_reported_and_others_still_load() {
    let d = demo_dir();
    write(&d.path().join("broken"), "manifest.lua", "return { name='Broken Name' }");
    let h = start(d.path());
    let mut saw_error = false;
    loop {
        match h.next() {
            RuntimeEvent::ExtensionError { .. } => saw_error = true,
            RuntimeEvent::CommandsRegistered(c) => { assert!(c.iter().any(|c| c.id == "demo:hello")); break }
            _ => {}
        }
    }
    assert!(saw_error);
}

// ------------------------------------------------------- actions, sections, shortcuts

const ACTIONS_EXT: &str = r#"
function main()
  return flux.ui.list {
    title = "Things",
    items = {{
      title = "One",
      actions = {
        flux.action.new { title = "Open", description = "opens it", on_action = function() flux.storage.set("did", "open") end },
        flux.action.new { title = "Copy", shortcut = "ctrl+shift+c", on_action = function() flux.storage.set("did", "copy") end },
        flux.action.section { title = "Danger", actions = {
          flux.action.new { title = "Delete", style = "destructive", on_action = function() flux.storage.set("did", "delete") end },
        }},
      },
    }},
    actions = { flux.action.new { title = "Global", on_action = function() flux.storage.set("did", "global") end } },
  }
end
"#;

#[test]
fn actions_are_registered_with_sections_shortcuts_and_global_actions_and_callbacks_run() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "acts", &[], &[("main", "view")], &[("commands/main.lua", ACTIONS_EXT)]);
    let h = start(d.path());
    let _ = h.next();
    h.run("acts:main");
    let (view_id, view) = h.next_push();
    let l = list(view);
    assert_eq!(l.title.as_deref(), Some("Things"));

    let item = &l.item(0).unwrap();
    assert_eq!(item.actions.len(), 2, "loose actions form one section, then 'Danger'");
    assert_eq!(item.actions[1].title.as_deref(), Some("Danger"));
    let open = &item.actions[0].actions[0];
    assert_eq!(open.description.as_deref(), Some("opens it"));
    let copy = &item.actions[0].actions[1];
    assert_eq!(copy.shortcut.as_ref().unwrap().to_string(), "ctrl+shift+c");
    let del = &item.actions[1].actions[0];
    assert_eq!(del.style, ActionStyle::Destructive);
    assert_eq!(l.actions[0].actions[0].title, "Global");

    // resolved shortcuts: primary = enter, secondary (no explicit shortcut) stays None here
    let sections = l.sections_for(0);
    let resolved = resolve_actions(&sections);
    assert_eq!(resolved.len(), 4);
    assert_eq!(resolved[0].shortcut, Some(Shortcut::new("enter")));

    // callbacks really execute (item action + global action)
    for (spec, expect) in [(open, "\"open\""), (del, "\"delete\""), (&l.actions[0].actions[0], "\"global\"")] {
        h.storage.local_remove("acts", "did");
        h.handle.send(RuntimeRequest::InvokeAction { view_id, action_id: spec.id });
        assert_eq!(h.wait_local("acts", "did"), expect);
    }
}

#[test]
fn invalid_action_definitions_are_reported_without_opening_a_view() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "bad", &[], &[("shortcut", "view"), ("style", "view"), ("nocb", "view")], &[
        ("commands/shortcut.lua", "function main() return flux.ui.list{items={{title='x',actions={flux.action.new{title='a',shortcut='ctrl+',on_action=function() end}}}}} end"),
        ("commands/style.lua", "function main() return flux.ui.list{items={{title='x',actions={flux.action.new{title='a',style='loud',on_action=function() end}}}}} end"),
        ("commands/nocb.lua", "function main() return flux.ui.list{items={{title='x',actions={flux.action.new{title='a'}}}}} end"),
    ]);
    let h = start(d.path());
    let _ = h.next();
    for (cmd, needle) in [("shortcut", "shortcut"), ("style", "unknown style"), ("nocb", "on_action")] {
        h.run(&format!("bad:{cmd}"));
        let m = h.next_error();
        assert!(m.contains(needle), "{cmd}: {m}");
    }
    assert!(h.drain_for(Duration::from_millis(200)).iter().all(|e| matches!(e, RuntimeEvent::CommandFinished { .. })));
}

// ------------------------------------------------------------------------ navigation

const NAV_EXT: &str = r#"
local function second()
  return flux.ui.list { title = "Second", items = {{ title = "inner", actions = {
    flux.action.new { title = "Back", on_action = function() flux.navigation.pop() end },
    flux.action.new { title = "Swap", on_action = function() flux.navigation.replace(flux.ui.list { title = "Third", items = {} }) end },
  }}}}
end

function main()
  return flux.ui.list { title = "First", items = {{ title = "go", actions = {
    flux.action.new { title = "Push", on_action = function() flux.navigation.push({ component = second() }) end },
    flux.action.new { title = "Bad", on_action = function() flux.navigation.push({ nothing = true }) end },
    flux.action.new { title = "Replace", on_action = function() return flux.ui.list { title = "Replaced", items = {} } end },
  }}}}
end
"#;

#[test]
fn navigation_push_pop_replace_and_errors_leave_the_stack_unchanged() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "nav", &[], &[("main", "view")], &[("commands/main.lua", NAV_EXT)]);
    let h = start(d.path());
    let _ = h.next();
    h.run("nav:main");
    let (first, v) = h.next_push();
    let l = list(v);
    let [push, bad, replace] = [&l.item(0).unwrap().actions[0].actions[0], &l.item(0).unwrap().actions[0].actions[1], &l.item(0).unwrap().actions[0].actions[2]];

    // push (accepts { component = view })
    h.handle.send(RuntimeRequest::InvokeAction { view_id: first, action_id: push.id });
    let (second, v) = h.next_push();
    assert_eq!(list(v.clone()).title.as_deref(), Some("Second"));

    // replace the top screen from inside the pushed view
    let sl = list(v);
    h.handle.send(RuntimeRequest::InvokeAction { view_id: second, action_id: sl.item(0).unwrap().actions[0].actions[1].id });
    let RuntimeEvent::Nav(NavEvent::Replace { old_view_id, view, .. }) = h.next() else { panic!() };
    assert_eq!((old_view_id, list(view).title.as_deref().map(str::to_string)), (second, Some("Third".into())));

    // an invalid view is an error and emits no navigation event
    h.handle.send(RuntimeRequest::InvokeAction { view_id: first, action_id: bad.id });
    assert!(h.next_error().contains("view"));

    // returning a view from an action replaces the screen
    h.handle.send(RuntimeRequest::InvokeAction { view_id: first, action_id: replace.id });
    let RuntimeEvent::Nav(NavEvent::Replace { view, .. }) = h.next() else { panic!() };
    assert_eq!(list(view).title.as_deref(), Some("Replaced"));
}

#[test]
fn navigation_pop_emits_event_and_closed_views_drop_their_callbacks() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "nav", &[], &[("main", "view")], &[("commands/main.lua", NAV_EXT)]);
    let h = start(d.path());
    let _ = h.next();
    h.run("nav:main");
    let (first, v) = h.next_push();
    let push = list(v).item(0).unwrap().actions[0].actions[0].id;
    h.handle.send(RuntimeRequest::InvokeAction { view_id: first, action_id: push });
    let (second, v) = h.next_push();
    let back = list(v).item(0).unwrap().actions[0].actions[0].id;

    h.handle.send(RuntimeRequest::InvokeAction { view_id: second, action_id: back });
    let RuntimeEvent::Nav(NavEvent::Pop { view_id }) = h.next() else { panic!() };
    assert_eq!(view_id, second);

    // the popped view is gone: invoking it again does nothing
    h.handle.send(RuntimeRequest::InvokeAction { view_id: second, action_id: back });
    // and a view the UI closed also forgets its callbacks
    h.handle.send(RuntimeRequest::ViewClosed { view_id: first });
    h.handle.send(RuntimeRequest::InvokeAction { view_id: first, action_id: push });
    assert!(h.drain_for(Duration::from_millis(300)).is_empty());
}

// ----------------------------------------------------------------------- cancellation

#[test]
fn closing_a_view_cancels_its_in_flight_work_and_kills_child_processes() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "slow", &["process"], &[("main", "view")], &[("commands/main.lua", r#"
function main()
  return flux.ui.list { items = {{ title = "slow", actions = { flux.action.new { title = "Run", on_action = function()
    flux.storage.set("started", true)
    flux.process.exec("sleep", { "31.7" })
    flux.storage.set("finished", true)
  end }}}}}
end"#)]);
    let h = start(d.path());
    let _ = h.next();
    h.run("slow:main");
    let (view_id, v) = h.next_push();
    h.handle.send(RuntimeRequest::InvokeAction { view_id, action_id: list(v).item(0).unwrap().actions[0].actions[0].id });
    h.wait_local("slow", "started");
    std::thread::sleep(Duration::from_millis(300));
    let pid = pid_of("sleep 31.7").expect("child process should be running");

    h.handle.send(RuntimeRequest::ViewClosed { view_id });
    wait_until_gone(pid);
    std::thread::sleep(Duration::from_millis(200));
    assert!(h.storage.local_get("slow", "finished").is_none(), "cancelled work must not continue");
}

#[test]
fn cancel_command_stops_a_running_command() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "slow", &["process"], &[("run", "no-view")], &[("commands/run.lua", r#"
function main()
  flux.storage.set("started", true)
  flux.process.exec("sleep", { "32.9" })
  flux.storage.set("finished", true)
end"#)]);
    let h = start(d.path());
    let _ = h.next();
    h.run("slow:run");
    h.wait_local("slow", "started");
    std::thread::sleep(Duration::from_millis(300));
    let pid = pid_of("sleep 32.9").expect("child process should be running");
    h.handle.send(RuntimeRequest::CancelCommand { command_id: "slow:run".into() });
    wait_until_gone(pid);
    assert!(h.storage.local_get("slow", "finished").is_none());
}

// ---------------------------------------------------------------------------- limits

#[test]
fn infinite_loops_are_interrupted_and_other_extensions_stay_responsive() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "loop", &[], &[("spin", "no-view")], &[("commands/spin.lua", "function main() while true do end end")]);
    ext(d.path(), "ok", &[], &[("ping", "no-view")], &[("commands/ping.lua", "function main() flux.storage.set('pong', 1) end")]);
    let h = start(d.path());
    let _ = h.next();

    let t = Instant::now();
    h.run("loop:spin");
    h.run("ok:ping"); // different worker: must not wait for the loop
    assert_eq!(h.wait_local("ok", "pong"), "1");
    let msg = h.next_error();
    assert!(msg.contains("execution limit"), "{msg}");
    assert!(t.elapsed() < Duration::from_secs(3), "took {:?}", t.elapsed());

    // the looping extension itself still works afterwards
    h.run("loop:spin");
    assert!(h.next_error().contains("execution limit"));
}

#[test]
fn memory_limit_stops_memory_bombs() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "hog", &[], &[("eat", "no-view"), ("fine", "no-view")], &[
        ("commands/eat.lua", "function main() local t = {} for i = 1, 1e9 do t[i] = string.rep('x', 1000) .. i end end"),
        ("commands/fine.lua", "function main() flux.storage.set('alive', 1) end"),
    ]);
    let h = start_with(d.path(), Limits { memory_mb: 16, slice_ms: 5000, command_timeout_s: 60, ..Limits::default() }, Arc::new(MemSecretStore::default()));
    let _ = h.next();
    h.run("hog:eat");
    let msg = h.next_error();
    assert!(msg.to_lowercase().contains("memory"), "{msg}");
    h.run("hog:fine"); // the state survives the failed allocation
    assert_eq!(h.wait_local("hog", "alive"), "1");
}

#[test]
fn wall_clock_timeout_cancels_slow_async_work() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "slow", &["process"], &[("run", "no-view")], &[("commands/run.lua", "function main() flux.process.exec('sleep', {'34.1'}) end")]);
    let h = start_with(d.path(), Limits { memory_mb: 64, slice_ms: 200, command_timeout_s: 1, ..Limits::default() }, Arc::new(MemSecretStore::default()));
    let _ = h.next();
    h.run("slow:run");
    std::thread::sleep(Duration::from_millis(300));
    let pid = pid_of("sleep 34.1").expect("child process should be running");
    assert!(h.next_error().contains("timed out"));
    wait_until_gone(pid);
}

// --------------------------------------------------------------------------- isolation

#[test]
fn extensions_do_not_share_globals_or_stdlib_state() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "a", &[], &[("tamper", "no-view")], &[
        ("init.lua", "shared_global = 'from a'"),
        ("commands/tamper.lua", r#"function main()
          string.upper = nil; table.insert = nil; math.floor = nil
          flux.storage.set('mt', tostring(getmetatable('')))
        end"#),
    ]);
    ext(d.path(), "b", &[], &[("look", "no-view")], &[("commands/look.lua", r#"function main()
        flux.storage.set('global', tostring(shared_global))
        flux.storage.set('upper', string.upper('x') .. tostring(math.floor(1.5)))
      end"#)]);
    let h = start(d.path());
    let _ = h.next();
    h.run("a:tamper");
    assert_eq!(h.wait_local("a", "mt"), "\"nil\"", "the string metatable must not be exposed");
    h.run("b:look");
    assert_eq!(h.wait_local("b", "global"), "\"nil\"");
    assert_eq!(h.wait_local("b", "upper"), "\"X1\"");
}

#[test]
fn require_cannot_escape_the_extension_directory() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "secret.lua", "return 'leaked'");
    ext(d.path(), "a", &[], &[("x", "no-view")], &[("commands/x.lua", r#"function main()
      local ok, err = pcall(require, '..secret')
      flux.storage.set('ok', tostring(ok))
    end"#)]);
    let h = start(d.path());
    let _ = h.next();
    h.run("a:x");
    assert_eq!(h.wait_local("a", "ok"), "\"false\"");
}

// ------------------------------------------------------------------------ lifecycle

#[test]
fn on_unload_hooks_run_when_extensions_reload() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "life", &[], &[("x", "no-view")], &[("init.lua", "flux.lifecycle.on_unload(function() flux.storage.set('unloaded', true) end)")]);
    let h = start(d.path());
    let _ = h.next();
    std::thread::sleep(Duration::from_millis(300)); // init.lua finished
    h.handle.send(RuntimeRequest::Reload);
    assert_eq!(h.wait_local("life", "unloaded"), "true");
    assert!(matches!(h.next(), RuntimeEvent::CommandsRegistered(_)));
}

// -------------------------------------------------------------- preferences and secrets

const PREFS: &str = r#"return {
  { name = "username", title = "User", type = "text", default = "anon" },
  { name = "token", title = "Token", type = "password", description = "API token" },
  { name = "verbose", title = "Verbose", type = "checkbox", default = false },
  { name = "theme", title = "Theme", type = "dropdown", default = "dark", options = {{ title = "Dark", value = "dark" }, { title = "Light", value = "light" }} },
}"#;

fn prefs_ext(root: &Path, main: &str, required_token: bool) {
    let prefs = if required_token { PREFS.replace("type = \"password\"", "type = \"password\", required = true") } else { PREFS.to_string() };
    ext(root, "p", &[], &[("main", "no-view")], &[("commands/main.lua", main), ("preferences.lua", &prefs)]);
}

#[test]
fn preferences_defaults_set_get_persist_and_isolate_per_extension() {
    let d = tempfile::tempdir().unwrap();
    prefs_ext(d.path(), r#"function main()
      flux.storage.set('default', flux.preferences.get('username'))
      flux.preferences.set('username', 'ana')
      flux.preferences.set('verbose', true)
      flux.preferences.set('theme', 'light')
      local all = flux.preferences.getAll()
      flux.storage.set('all', all)
      local ok, err = pcall(flux.preferences.set, 'theme', 'neon')
      flux.storage.set('bad_option', tostring(ok))
      local ok2 = pcall(flux.preferences.set, 'verbose', 'yes')
      flux.storage.set('bad_type', tostring(ok2))
      local ok3 = pcall(flux.preferences.get, 'nope')
      flux.storage.set('unknown', tostring(ok3))
    end"#, false);
    ext(d.path(), "other", &[], &[("x", "no-view")], &[("commands/x.lua", "function main() flux.storage.set('p', tostring(pcall(flux.preferences.get, 'username'))) end")]);
    let h = start(d.path());
    let _ = h.next();
    h.run("p:main");
    assert_eq!(h.wait_local("p", "unknown"), "\"false\"");
    assert_eq!(h.wait_local("p", "default"), "\"anon\"");
    assert_eq!(h.wait_local("p", "bad_option"), "\"false\"");
    assert_eq!(h.wait_local("p", "bad_type"), "\"false\"");
    let all: serde_json::Value = serde_json::from_str(&h.wait_local("p", "all")).unwrap();
    assert_eq!(all, serde_json::json!({ "username": "ana", "verbose": true, "theme": "light" }));
    // persisted through the Storage backend, scoped by extension
    assert_eq!(h.storage.pref_get("p", "username").as_deref(), Some("\"ana\""));
    assert_eq!(h.storage.pref_get("other", "username"), None);
    h.run("other:x");
    assert_eq!(h.wait_local("other", "p"), "\"false\"", "another extension cannot read p's preferences");
}

#[test]
fn password_preferences_and_secure_storage_never_touch_plain_storage() {
    let d = tempfile::tempdir().unwrap();
    prefs_ext(d.path(), r#"function main()
      flux.preferences.set('token', 's3cret-token')
      flux.storage.secure.set('refresh', 'r3fresh')
      flux.storage.set('read_back', #flux.preferences.get('token') .. '/' .. #flux.storage.secure.get('refresh'))
      flux.storage.secure.remove('refresh')
      flux.storage.set('removed', tostring(flux.storage.secure.get('refresh')))
    end"#, false);
    let h = start(d.path());
    let _ = h.next();
    h.run("p:main");
    assert_eq!(h.wait_local("p", "removed"), "\"nil\"");
    assert_eq!(h.wait_local("p", "read_back"), "\"12/7\"");
    // secrets live in the SecretStore ...
    let secret = futures_block(h.secrets.get("p", "pref:token"));
    assert_eq!(secret.unwrap().as_deref(), Some("s3cret-token"));
    // ... and nowhere in the SQLite-backed storage
    let everything = format!("{:?}{:?}", h.storage.local.lock().unwrap(), h.storage.prefs.lock().unwrap());
    assert!(!everything.contains("s3cret") && !everything.contains("r3fresh"), "{everything}");
    // and they are isolated per extension
    assert_eq!(futures_block(h.secrets.get("other", "pref:token")).unwrap(), None);
}

#[test]
fn secure_storage_fails_loudly_without_a_secret_service() {
    let d = tempfile::tempdir().unwrap();
    prefs_ext(d.path(), "function main() flux.storage.secure.set('k', 'v') end", false);
    let h = start_with(d.path(), Limits::default(), Arc::new(UnavailableSecretStore("no Secret Service".into())));
    let _ = h.next();
    h.run("p:main");
    let m = h.next_error();
    assert!(m.contains("secure storage unavailable") && m.contains("no Secret Service"), "{m}");
    assert!(h.storage.local.lock().unwrap().is_empty() && h.storage.prefs.lock().unwrap().is_empty());
}

#[test]
fn missing_required_preferences_open_the_settings_screen_instead_of_running() {
    let d = tempfile::tempdir().unwrap();
    prefs_ext(d.path(), "function main() flux.storage.set('ran', true) end", true);
    let h = start(d.path());
    let RuntimeEvent::CommandsRegistered(cmds) = h.next() else { panic!() };
    assert!(cmds.iter().any(|c| c.id == "p:__preferences" && c.title.starts_with("Configure")));
    h.run("p:main");
    assert!(h.next_error().contains("required preference"));
    let (_, v) = h.next_push();
    assert_eq!(list(v).title.as_deref(), Some("Preferences"));
    assert!(h.storage.local_get("p", "ran").is_none(), "the command must not run");
}

#[test]
fn preferences_screens_edit_values_through_input_and_list_views() {
    let d = tempfile::tempdir().unwrap();
    prefs_ext(d.path(), "function main() end", false);
    let h = start(d.path());
    let _ = h.next();
    h.run("p:__preferences");
    let (view, v) = h.next_push();
    let l = list(v);
    let titles: Vec<_> = l.items().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["User", "Token", "Verbose", "Theme"]);
    assert_eq!(l.item(0).unwrap().subtitle.as_deref(), Some("anon")); // default shown
    assert_eq!(l.item(1).unwrap().subtitle.as_deref(), Some("Not set"));
    let act = |l: &ListView, i: usize| l.item(i).unwrap().actions[0].actions[0].id;

    // text -> Input screen -> submit -> back on a refreshed list
    h.handle.send(RuntimeRequest::InvokeAction { view_id: view, action_id: act(&l, 0) });
    let (input_view, v) = h.next_push();
    let ViewNode::Input(input) = v else { panic!("expected an input view") };
    assert!(!input.secure);
    h.handle.send(RuntimeRequest::SubmitInput { view_id: input_view, text: "maria".into() });
    assert!(matches!(h.next(), RuntimeEvent::Nav(NavEvent::Pop { .. })));
    let RuntimeEvent::Nav(NavEvent::Replace { view, .. }) = h.next() else { panic!() };
    assert_eq!(list(view).item(0).unwrap().subtitle.as_deref(), Some("maria"));
    assert_eq!(h.storage.pref_get("p", "username").as_deref(), Some("\"maria\""));
}

#[test]
fn password_and_checkbox_and_dropdown_screens_work() {
    let d = tempfile::tempdir().unwrap();
    prefs_ext(d.path(), "function main() end", false);
    let h = start(d.path());
    let _ = h.next();
    h.run("p:__preferences");
    let (mut view, v) = h.next_push();
    let mut l = list(v);
    let act = |l: &ListView, i: usize| l.item(i).unwrap().actions[0].actions[0].id;

    // checkbox toggles in place (action returns a view => Replace)
    h.handle.send(RuntimeRequest::InvokeAction { view_id: view, action_id: act(&l, 2) });
    let RuntimeEvent::Nav(NavEvent::Replace { view_id, view: nv, .. }) = h.next() else { panic!() };
    assert_eq!(list(nv.clone()).item(2).unwrap().subtitle.as_deref(), Some("Enabled"));
    (view, l) = (view_id, list(nv));

    // dropdown pushes an options list; selecting pops it and refreshes
    h.handle.send(RuntimeRequest::InvokeAction { view_id: view, action_id: act(&l, 3) });
    let (opts_view, ov) = h.next_push();
    let ol = list(ov);
    assert_eq!(ol.item_count(), 2);
    h.handle.send(RuntimeRequest::InvokeAction { view_id: opts_view, action_id: ol.item(1).unwrap().actions[0].actions[0].id });
    assert!(matches!(h.next(), RuntimeEvent::Nav(NavEvent::Pop { .. })));
    let RuntimeEvent::Nav(NavEvent::Replace { view_id, view: nv, .. }) = h.next() else { panic!() };
    assert_eq!(list(nv.clone()).item(3).unwrap().subtitle.as_deref(), Some("Light"));
    (view, l) = (view_id, list(nv));

    // password: masked input, value goes to the secret store
    h.handle.send(RuntimeRequest::InvokeAction { view_id: view, action_id: act(&l, 1) });
    let (input_view, iv) = h.next_push();
    let ViewNode::Input(input) = iv else { panic!() };
    assert!(input.secure && input.value.is_none());
    h.handle.send(RuntimeRequest::SubmitInput { view_id: input_view, text: "tok-123".into() });
    let _ = h.next(); // Pop
    let RuntimeEvent::Nav(NavEvent::Replace { view: nv, .. }) = h.next() else { panic!() };
    assert_eq!(list(nv).item(1).unwrap().subtitle.as_deref(), Some("••••••••"));
    assert_eq!(futures_block(h.secrets.get("p", "pref:token")).unwrap().as_deref(), Some("tok-123"));
    assert!(!format!("{:?}", h.storage.prefs.lock().unwrap()).contains("tok-123"));
}

/// Minimal executor for the (immediately ready) in-memory secret store futures.
fn futures_block<T>(fut: flux_api::BoxFuture<'_, T>) -> T {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(fut)
}

#[test]
fn preferences_screen_survives_an_unavailable_secret_service() {
    let d = tempfile::tempdir().unwrap();
    prefs_ext(d.path(), "function main() end", false);
    let h = start_with(d.path(), Limits::default(), Arc::new(UnavailableSecretStore("no Secret Service".into())));
    let _ = h.next();
    h.run("p:__preferences");
    let (view, v) = h.next_push();
    let l = list(v);
    assert_eq!(l.item(1).unwrap().subtitle.as_deref(), Some("Keyring unavailable"), "other preferences keep working");
    assert_eq!(l.item(0).unwrap().subtitle.as_deref(), Some("anon"));

    // saving a password reports the problem and leaves the screens untouched
    h.handle.send(RuntimeRequest::InvokeAction { view_id: view, action_id: l.item(1).unwrap().actions[0].actions[0].id });
    let (input_view, _) = h.next_push();
    h.handle.send(RuntimeRequest::SubmitInput { view_id: input_view, text: "tok".into() });
    assert!(h.next_error().contains("secure storage unavailable"));
    assert!(h.drain_for(Duration::from_millis(200)).is_empty(), "no Pop/Replace after a failed save");
}
