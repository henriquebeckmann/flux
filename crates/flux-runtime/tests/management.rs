//! Discovery, status reporting, granular reload, enable/disable and logs.

mod common;

use std::{collections::HashSet, sync::Arc, time::Duration};

use common::*;
use flux_api::*;

fn find<'a>(infos: &'a [ExtensionInfo], name: &str) -> &'a ExtensionInfo {
    infos.iter().find(|i| i.name == name).unwrap_or_else(|| panic!("no extension '{name}' in {:?}", infos.iter().map(|i| &i.name).collect::<Vec<_>>()))
}

#[test]
fn an_invalid_extension_is_reported_without_blocking_the_others() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "good", &[], &[("hi", "no-view")], &[("commands/hi.lua", "function main() flux.storage.set('ok', 1) end")]);
    write(&d.path().join("broken"), "manifest.lua", "return { name = 'Not Valid!', permissions = { 'bogus' } }");
    write(&d.path().join("syntax"), "manifest.lua", "return {");
    let h = start(d.path());
    let infos = h.next_infos();
    assert_eq!(find(&infos, "good").status, ExtensionStatus::Loaded);
    let ExtensionStatus::Invalid { errors } = &find(&infos, "broken").status else { panic!() };
    assert!(errors.len() >= 2 && errors.iter().all(|e| e.starts_with("manifest.lua:")), "{errors:?}");
    assert!(matches!(find(&infos, "syntax").status, ExtensionStatus::Invalid { .. }));
    h.run("good:hi");
    assert_eq!(h.wait_local("good", "ok"), "1");
}

#[test]
fn duplicate_names_keep_the_first_and_flag_the_second() {
    let d = tempfile::tempdir().unwrap();
    write(&d.path().join("a"), "manifest.lua", "return { name = 'same', version = '1.0.0' }");
    write(&d.path().join("b"), "manifest.lua", "return { name = 'same', version = '2.0.0' }");
    let h = start(d.path());
    let infos = h.next_infos();
    assert_eq!(infos.len(), 2);
    assert_eq!(infos.iter().filter(|i| i.status == ExtensionStatus::Loaded).count(), 1);
    let bad = infos.iter().find(|i| matches!(i.status, ExtensionStatus::Invalid { .. })).unwrap();
    let ExtensionStatus::Invalid { errors } = &bad.status else { unreachable!() };
    assert!(errors[0].contains("duplicate extension 'same'"), "{errors:?}");
}

#[test]
fn info_carries_version_permissions_hosts_commands_and_source() {
    let d = tempfile::tempdir().unwrap();
    let installed = d.path().join("installed");
    write(
        &installed.join("info"),
        "manifest.lua",
        "return { name='info', title='Info', version='2.3.4', author='Ann', license='MIT', description='d', permissions={'process','network'}, allowed_hosts={'api.x.com'}, commands={{name='c'}} }",
    );
    write(&installed.join("info"), "commands/c.lua", "function main() end");
    ext(&d.path().join("dev"), "devext", &[], &[], &[]);
    let h = start_dirs(
        vec![installed.clone(), d.path().join("dev")],
        d.path(),
        Limits::default(),
        Arc::new(MemSecretStore::default()),
        Some(installed),
        HashSet::new(),
    );
    let infos = h.next_infos();
    let i = find(&infos, "info");
    assert_eq!((i.version.as_str(), i.author.as_deref(), i.license.as_deref()), ("2.3.4", Some("Ann"), Some("MIT")));
    // legacy "process" expands to execute + kill; secure storage is implicit until credentials.* is declared
    assert_eq!(i.permissions, vec!["network", "process.execute", "process.kill", "credentials.read", "credentials.write"]);
    assert_eq!(i.allowed_hosts, vec!["api.x.com"]);
    assert_eq!(i.commands, vec!["c"]);
    assert_eq!(i.source, ExtensionSource::Installed);
    assert_eq!(find(&infos, "devext").source, ExtensionSource::Dev);
}

#[test]
fn disabled_extensions_load_but_do_not_run_and_can_be_enabled_again() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "off", &[], &[("hi", "no-view")], &[("commands/hi.lua", "function main() flux.storage.set('ran', 1) end")]);
    ext(d.path(), "on", &[], &[("hi", "no-view")], &[("commands/hi.lua", "function main() end")]);
    let h = start_full(d.path(), Limits::default(), Arc::new(MemSecretStore::default()), None, HashSet::from(["off".to_string()]));
    let infos = h.next_infos();
    assert_eq!(find(&infos, "off").status, ExtensionStatus::Disabled);
    assert!(!find(&infos, "off").enabled && find(&infos, "on").enabled);
    let cmds = h.next_commands();
    assert!(cmds.iter().all(|c| c.extension.as_deref() != Some("off")), "disabled commands are not announced");

    h.run("off:hi");
    assert!(h.next_error().contains("does not exist or its extension is disabled"));
    assert!(h.storage.local_get("off", "ran").is_none());

    h.handle.send(RuntimeRequest::SetEnabled { name: "off".into(), enabled: true });
    let infos = h.next_infos();
    assert_eq!(find(&infos, "off").status, ExtensionStatus::Loaded);
    assert!(h.next_commands().iter().any(|c| c.id == "off:hi"));
    h.run("off:hi");
    assert_eq!(h.wait_local("off", "ran"), "1");

    h.handle.send(RuntimeRequest::SetEnabled { name: "off".into(), enabled: false });
    assert_eq!(find(&h.next_infos(), "off").status, ExtensionStatus::Disabled);
    let _ = h.next_commands();
    h.storage.local_remove("off", "ran");
    h.run("off:hi");
    assert!(h.next_error().contains("disabled"));
}

#[test]
fn reloading_one_extension_leaves_the_others_running() {
    let d = tempfile::tempdir().unwrap();
    let hook = "flux.lifecycle.on_unload(function() flux.storage.set('unloaded', 1) end)";
    ext(d.path(), "a", &[], &[("hi", "no-view")], &[("init.lua", hook), ("commands/hi.lua", "function main() flux.storage.set('v', 1) end")]);
    ext(d.path(), "b", &[], &[("hi", "no-view")], &[("init.lua", hook), ("commands/hi.lua", "function main() end")]);
    let h = start(d.path());
    let _ = h.next_infos();
    // edit `a`: new code and a new command
    write(&d.path().join("a"), "commands/hi.lua", "function main() flux.storage.set('v', 2) end");
    write(&d.path().join("a"), "commands/extra.lua", "function main() end");
    write(&d.path().join("a"), "manifest.lua", "return { name='a', version='1.1.0', commands={{name='hi',mode='no-view'},{name='extra',mode='no-view'}} }");
    h.handle.send(RuntimeRequest::ReloadExtension { name: "a".into() });
    let infos = h.next_infos();
    assert_eq!(find(&infos, "a").version, "1.1.0");
    let cmds = h.next_commands(); // announced right before? (order: commands then infos) -- both are sent; fetch from a fresh reload below
    let _ = cmds;
    h.run("a:hi");
    assert_eq!(h.wait_local("a", "v"), "2");
    assert_eq!(h.wait_local("a", "unloaded"), "1", "the reloaded extension ran its on_unload hook");
    assert!(h.storage.local_get("b", "unloaded").is_none(), "the other extension was not touched");
    h.run("a:extra");
    let _ = h.drain_for(Duration::from_millis(100));

    // by folder name, and a manifest that became invalid
    write(&d.path().join("a"), "manifest.lua", "return { name = 'a', version = 'oops' }");
    h.handle.send(RuntimeRequest::ReloadExtension { name: "a".into() });
    let infos = h.next_infos();
    assert!(matches!(find(&infos, "a").status, ExtensionStatus::Invalid { .. }));
    assert_eq!(find(&infos, "b").status, ExtensionStatus::Loaded);
    // fix it again
    write(&d.path().join("a"), "manifest.lua", "return { name = 'a', commands={{name='hi',mode='no-view'}} }");
    h.handle.send(RuntimeRequest::ReloadExtension { name: "a".into() });
    assert_eq!(find(&h.next_infos(), "a").status, ExtensionStatus::Loaded);
}

#[test]
fn logger_lines_become_log_events_and_floods_are_rate_limited() {
    let d = tempfile::tempdir().unwrap();
    ext(
        d.path(),
        "chatty",
        &[],
        &[("few", "no-view"), ("flood", "no-view")],
        &[
            ("commands/few.lua", "function main() flux.logger.info('hello', 42) flux.logger.error('bad') flux.logger.warn(('x'):rep(10000)) end"),
            ("commands/flood.lua", "function main() for i = 1, 5000 do flux.logger.info('line ' .. i) end end"),
        ],
    );
    let h = start(d.path());
    let _ = h.next_infos();
    h.run("chatty:few");
    let mut got = Vec::new();
    while got.len() < 3 {
        if let RuntimeEvent::Log { extension, level, message } = h.wait_event(|e| matches!(e, RuntimeEvent::Log { .. }).then(|| e.clone())) {
            assert_eq!(extension, "chatty");
            got.push((level, message));
        }
    }
    assert_eq!(got[0], (LogLevel::Info, "hello 42".to_string()));
    assert_eq!(got[1], (LogLevel::Error, "bad".to_string()));
    assert_eq!(got[2].0, LogLevel::Warn);
    assert!(got[2].1.len() < 4200 && got[2].1.ends_with("(truncated)"));

    h.run("chatty:flood");
    let events = h.drain_for(Duration::from_millis(800));
    let lines = events.iter().filter(|e| matches!(e, RuntimeEvent::Log { .. })).count();
    assert!((190..=260).contains(&lines), "expected about 200 lines, got {lines}");
}
