use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use super::*;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

#[derive(Clone, Default)]
struct Recorder {
    sent: Arc<Mutex<Vec<DaemonCommand>>>,
    running: bool,
}

impl Daemon for Recorder {
    fn send(&self, cmd: &DaemonCommand) -> std::io::Result<()> {
        if !self.running {
            return Err(std::io::Error::other("not running"));
        }
        self.sent.lock().unwrap().push(cmd.clone());
        Ok(())
    }

    fn query(&self, what: &str) -> std::io::Result<String> {
        if !self.running {
            return Err(std::io::Error::other("not running"));
        }
        assert_eq!(what, "status");
        Ok(r#"[{"name":"a","state":"running","isolation":"process, confined","generation":3,"pid":4242,"uptime_s":12,"recent_crashes":1,"rss_kb":20480,"cpu_ms":1500,"tasks":2,"http_in_flight":1,"children":0,"lua_memory_kb":300}]"#.into())
    }
}

struct T {
    root: tempfile::TempDir,
    out: Capture,
    err: Capture,
    daemon: Recorder,
}

impl T {
    fn new(daemon_running: bool) -> T {
        T { root: tempfile::tempdir().unwrap(), out: Capture::default(), err: Capture::default(), daemon: Recorder { running: daemon_running, ..Default::default() } }
    }

    fn env(&self) -> Env {
        let data = self.root.path().join("data");
        Env {
            cwd: self.root.path().join("work"),
            installed_dir: data.join("installed"),
            data_dir: data.clone(),
            log_dir: data.join("logs"),
            db_path: data.join("flux.db"),
            config_dir: self.root.path().join("config"),
            extension_dirs: vec![self.root.path().join("work")],
            daemon: Box::new(self.daemon.clone()),
            out: Box::new(self.out.clone()),
            err: Box::new(self.err.clone()),
        }
    }

    fn run(&self, args: &[&str]) -> Outcome {
        std::fs::create_dir_all(self.root.path().join("work")).unwrap();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        run(&args, &mut self.env())
    }

    fn code(&self, args: &[&str]) -> i32 {
        match self.run(args) {
            Outcome::Done(c) => c,
            other => panic!("expected a finished command, got {other:?}"),
        }
    }

    fn work(&self) -> PathBuf {
        self.root.path().join("work")
    }

    fn sent(&self) -> Vec<DaemonCommand> {
        self.daemon.sent.lock().unwrap().clone()
    }
}

fn write(dir: &Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

#[test]
fn every_template_creates_a_valid_project() {
    let t = T::new(false);
    for (i, tpl) in ["list", "grid", "form", "detail"].iter().enumerate() {
        let name = format!("my-{tpl}-{i}");
        assert_eq!(t.code(&["create", &name, "--template", tpl]), OK, "{tpl}: {}", t.err.text());
        let dir = t.work().join(&name);
        for f in ["manifest.lua", "commands/main.lua", "README.md", ".luarc.json", "flux.d.lua"] {
            assert!(dir.join(f).is_file(), "{tpl}: missing {f}");
        }
        let manifest = std::fs::read_to_string(dir.join("manifest.lua")).unwrap();
        assert!(manifest.contains(&format!("name = \"{name}\"")) && !manifest.contains("{{"), "{manifest}");
        assert_eq!(t.code(&["validate", dir.to_str().unwrap()]), OK, "{tpl}: {}", t.err.text());
        assert!(!t.err.text().contains("warning"), "{tpl}: a fresh project has no warnings: {}", t.err.text());
    }
    assert!(t.out.text().contains("Created"));
}

#[test]
fn create_refuses_bad_ids_unknown_templates_and_existing_folders() {
    let t = T::new(false);
    assert_eq!(t.code(&["create", "Bad Name"]), FAILED);
    assert!(t.err.text().contains("not a valid extension id"));
    assert_eq!(t.code(&["create", "ok-name", "--template", "nope"]), FAILED);
    assert!(t.err.text().contains("available: list, grid, form, detail"));
    write(&t.work().join("taken"), "x.txt", "occupied");
    assert_eq!(t.code(&["create", "taken"]), FAILED);
    assert!(t.err.text().contains("already exists"));
    assert!(!t.work().join("taken/manifest.lua").exists(), "an existing folder is never touched");
    assert_eq!(t.code(&["create", "empty-dir-ok", "--dir", t.work().to_str().unwrap()]), OK);
}

#[test]
fn validate_reports_lua_errors_with_file_and_line_and_supports_json() {
    let t = T::new(false);
    assert_eq!(t.code(&["create", "proj"]), OK);
    let dir = t.work().join("proj");
    write(&dir, "commands/main.lua", "function main()\n  local = 1\nend\n");
    assert_eq!(t.code(&["validate", dir.to_str().unwrap()]), FAILED);
    let err = t.err.text();
    assert!(err.contains("commands/main.lua:2") && err.contains("syntax error"), "{err}");
    assert!(t.out.text().contains("invalid"));

    let t2 = T::new(false);
    write(&t2.work().join("bad"), "manifest.lua", "return { name = 'x', version = 'nope' }");
    assert_eq!(t2.code(&["validate", t2.work().join("bad").to_str().unwrap(), "--json"]), FAILED);
    let v: serde_json::Value = serde_json::from_str(&t2.out.text()).unwrap();
    assert_eq!(v["valid"], false);
    assert!(v["report"]["errors"][0]["message"].as_str().unwrap().contains("version"));

    assert_eq!(t.code(&["validate", t.root.path().join("nowhere").to_str().unwrap()]), FAILED);
}

#[test]
fn validate_defaults_to_the_current_folder() {
    let t = T::new(false);
    t.run(&["create", "here"]);
    let mut env = t.env();
    env.cwd = t.work().join("here");
    assert!(matches!(run(&["validate".into()], &mut env), Outcome::Done(OK)));
}

#[test]
fn package_install_list_disable_enable_remove_round_trip() {
    let t = T::new(true);
    assert_eq!(t.code(&["create", "roundtrip"]), OK);
    let src = t.work().join("roundtrip");
    assert_eq!(t.code(&["package", src.to_str().unwrap()]), OK, "{}", t.err.text());
    let pkg = t.work().join("roundtrip-0.1.0.fluxext");
    assert!(pkg.is_file(), "{}", t.out.text());

    assert_eq!(t.code(&["install", pkg.to_str().unwrap()]), OK, "{}", t.err.text());
    assert!(t.out.text().contains("Installed roundtrip 0.1.0"));
    assert!(t.sent().contains(&DaemonCommand::ReloadExtension("roundtrip".into())));
    assert!(t.root.path().join("data/installed/roundtrip/manifest.lua").is_file());

    assert_eq!(t.code(&["install", pkg.to_str().unwrap()]), FAILED, "same version again needs --force");
    assert!(t.err.text().contains("--force"));
    assert_eq!(t.code(&["install", pkg.to_str().unwrap(), "--force"]), OK);

    assert_eq!(t.code(&["list", "--json"]), OK);
    let json = t.out.text();
    let rows: serde_json::Value = serde_json::from_str(&json[json.rfind("[\n").unwrap()..]).unwrap();
    assert_eq!(rows[0]["name"], "roundtrip");
    assert_eq!((rows[0]["source"].as_str(), rows[0]["status"].as_str(), rows[0]["enabled"].as_bool()), (Some("installed"), Some("ok"), Some(true)));
    assert!(rows.as_array().unwrap().iter().any(|r| r["source"] == "dev"), "the project folder in the working dir is listed as a dev extension");

    assert_eq!(t.code(&["disable", "roundtrip"]), OK);
    assert!(t.sent().contains(&DaemonCommand::Disable("roundtrip".into())));
    let before = t.out.text().len();
    assert_eq!(t.code(&["list"]), OK);
    let table = t.out.text()[before..].to_string();
    assert!(table.contains("NAME") && table.lines().any(|l| l.starts_with("roundtrip") && l.contains("disabled") && l.contains("installed")), "{table}");

    assert_eq!(t.code(&["enable", "roundtrip"]), OK);
    assert!(t.sent().contains(&DaemonCommand::Enable("roundtrip".into())));

    assert_eq!(t.code(&["remove", "roundtrip"]), OK);
    assert!(!t.root.path().join("data/installed/roundtrip").exists());
    assert!(t.sent().contains(&DaemonCommand::Reload));
    assert_eq!(t.code(&["remove", "roundtrip"]), FAILED);
    assert!(t.err.text().contains("not installed"));
}

#[test]
fn installing_an_invalid_extension_prints_the_report_and_changes_nothing() {
    let t = T::new(true);
    t.run(&["create", "broken"]);
    let dir = t.work().join("broken");
    write(&dir, "commands/main.lua", "function main(");
    assert_eq!(t.code(&["install", dir.to_str().unwrap()]), FAILED);
    assert!(t.err.text().contains("commands/main.lua"));
    assert_eq!(t.code(&["package", dir.to_str().unwrap()]), FAILED);
    assert!(!t.root.path().join("data/installed/broken").exists());
    assert!(t.sent().is_empty());
    assert!(std::fs::read_dir(&t.work()).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".fluxext")));
}

#[test]
fn changes_made_without_a_running_launcher_are_applied_later_and_say_so() {
    let t = T::new(false);
    t.run(&["create", "offline"]);
    assert_eq!(t.code(&["install", t.work().join("offline").to_str().unwrap()]), OK);
    assert!(t.err.text().contains("launcher is not running"));
    assert_eq!(t.code(&["reload"]), FAILED, "an explicit reload with no launcher is an error");
    assert!(t.err.text().contains("not running"));
}

#[test]
fn reload_targets_one_extension_or_all() {
    let t = T::new(true);
    assert_eq!(t.code(&["reload"]), OK);
    assert_eq!(t.code(&["reload", "demo"]), OK);
    assert_eq!(t.sent(), vec![DaemonCommand::Reload, DaemonCommand::ReloadExtension("demo".into())]);
}

#[test]
fn logs_show_the_tail_of_the_extension_log() {
    let t = T::new(false);
    assert_eq!(t.code(&["logs", "ghost"]), OK);
    assert!(t.out.text().contains("No log lines"));
    let log = flux_core::log_path(&t.root.path().join("data/logs"), "chatty");
    write(log.parent().unwrap(), "chatty.log", &(1..=100).map(|i| format!("line {i}\n")).collect::<String>());
    assert_eq!(t.code(&["logs", "chatty", "-n", "3"]), OK);
    let text = t.out.text();
    assert!(text.ends_with("line 98\nline 99\nline 100\n"), "{text}");
    assert!(!text.contains("line 97"));
    assert_eq!(t.code(&["logs", "../etc/passwd"]), FAILED, "names cannot point outside the log directory");
}

#[test]
fn usage_errors_exit_2_and_help_exits_0() {
    let t = T::new(false);
    assert_eq!(t.code(&["frobnicate"]), USAGE);
    assert!(t.err.text().contains("unrecognized subcommand"));
    assert_eq!(t.code(&["install"]), USAGE, "missing argument");
    assert_eq!(t.code(&["create", "x", "--wat"]), USAGE);
    assert_eq!(t.code(&["--help"]), OK);
    let help = t.out.text();
    for c in ["create", "dev", "validate", "package", "install", "list", "remove"] {
        assert!(help.contains(c), "help mentions {c}");
    }
    assert_eq!(t.code(&["package", "--help"]), OK);
}

#[test]
fn application_commands_are_handed_back_to_the_caller() {
    let t = T::new(false);
    assert_eq!(t.run(&[]), Outcome::Gui(Gui::Toggle));
    assert_eq!(t.run(&["toggle"]), Outcome::Gui(Gui::Toggle));
    assert_eq!(t.run(&["daemon"]), Outcome::Gui(Gui::Daemon));
    assert_eq!(t.run(&["--dev", "daemon"]), Outcome::Gui(Gui::Daemon));
    assert_eq!(t.run(&["quit"]), Outcome::Gui(Gui::Quit));
    assert_eq!(t.run(&["install-desktop"]), Outcome::Gui(Gui::InstallDesktop));
    t.run(&["create", "devproj"]);
    let dir = t.work().join("devproj");
    match t.run(&["dev", dir.to_str().unwrap()]) {
        Outcome::Gui(Gui::Dev { dir: d }) => assert_eq!(d, std::fs::canonicalize(&dir).unwrap()),
        other => panic!("{other:?}"),
    }
    assert!(t.out.text().contains("valid"), "dev validates first");
    assert_eq!(t.code(&["dev", t.root.path().join("work").to_str().unwrap()]), FAILED, "a folder without a manifest is not an extension");
    assert!(t.err.text().contains("no manifest.lua"));
}

#[test]
fn dev_keeps_going_when_the_extension_has_problems_but_shows_them() {
    let t = T::new(false);
    t.run(&["create", "wip"]);
    let dir = t.work().join("wip");
    write(&dir, "commands/main.lua", "function main() return end end");
    assert!(matches!(t.run(&["dev", dir.to_str().unwrap()]), Outcome::Gui(Gui::Dev { .. })));
    assert!(t.err.text().contains("commands/main.lua"), "{}", t.err.text());
}

#[test]
fn permissions_shows_revokes_and_restores_and_tells_the_launcher() {
    let t = T::new(true);
    assert_eq!(t.code(&["create", "perm"]), OK);
    assert_eq!(t.code(&["permissions", "perm"]), OK, "{}", t.err.text());
    assert!(t.out.text().contains("clipboard.write") && t.out.text().contains("allowed"), "{}", t.out.text());

    assert_eq!(t.code(&["permissions", "perm", "--revoke", "clipboard.write"]), OK, "{}", t.err.text());
    assert!(t.out.text().contains("revoked"));
    let file = t.root.path().join("config/permissions.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("[extensions.perm]") && text.contains("clipboard.write"), "{text}");
    assert!(t.sent().iter().any(|c| matches!(c, DaemonCommand::ReloadExtension(n) if n == "perm")));

    assert_eq!(t.code(&["permissions", "perm", "--restore", "clipboard.write", "--json"]), OK);
    let after = std::fs::read_to_string(&file).unwrap();
    assert!(!after.contains("clipboard.write"), "an empty override is removed: {after}");
}

#[test]
fn permissions_rejects_unknown_names_and_things_the_extension_never_asked_for() {
    let t = T::new(false);
    assert_eq!(t.code(&["create", "perm"]), OK);
    assert_eq!(t.code(&["permissions", "perm", "--revoke", "teleport"]), FAILED);
    assert!(t.err.text().contains("is not a permission"));
    assert_eq!(t.code(&["permissions", "perm", "--revoke", "network"]), FAILED);
    assert!(t.err.text().contains("does not declare"));
    assert_eq!(t.code(&["permissions", "nope"]), FAILED);
    assert!(t.err.text().contains("no extension named"));
}

#[test]
fn status_prints_a_table_or_the_raw_json_and_needs_a_running_launcher() {
    let t = T::new(true);
    assert_eq!(t.code(&["status"]), OK, "{}", t.err.text());
    let out = t.out.text();
    assert!(out.contains("NAME") && out.contains("4242") && out.contains("20.0 MB") && out.contains("process, confined"), "{out}");
    assert_eq!(t.code(&["status", "--json"]), OK);
    assert!(t.out.text().contains(r#""pid":4242"#));
    let off = T::new(false);
    assert_eq!(off.code(&["status"]), FAILED);
    assert!(off.err.text().contains("is the launcher running"));
}
