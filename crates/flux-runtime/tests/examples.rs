//! The example extensions, run for real against the runtime: nothing here is mocked except the
//! network (a local server) and the process table (real `sleep` children).

mod common;

use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use common::*;
use flux_api::*;

fn start_root(root: &Path) -> Harness {
    let h = start_with(root, Limits { slice_ms: 2000, ..Limits::default() }, Arc::new(MemSecretStore::default()));
    let _ = h.next();
    h
}

/// Declares an extra no-view command (the driver files in these tests live next to the real ones).
fn declare_driver(root: &Path, ext: &str) {
    let path = root.join(ext).join("manifest.lua");
    let m = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, m.replace("commands = {", "commands = { { name = \"driver\", mode = \"no-view\" },")).unwrap();
}

fn set_pref(h: &Harness, ext: &str, name: &str, json: &str) {
    h.storage.pref_set(ext, name, json);
}

// -------------------------------------------------------------- calculator

#[test]
fn the_calculator_evaluates_correctly_and_safely() {
    let d = tempfile::tempdir().unwrap();
    install_example(d.path(), "calculator");
    // a driver command next to the module under test
    write(
        &d.path().join("calculator"),
        "commands/driver.lua",
        r#"local calc = require("calc")
function main()
  local out = {}
  local cases = {
    "2+3*4", "(2+3)*4", "2^3^2", "-2^2", "2**10", "10/4", "10 % 3", "10 mod 3", "7 - -3", "1_000 * 2", ".5 + .25", "1e3 + 1",
    "9223372036854775807 * 2", "sqrt(16)", "sqrt(2)^2", "abs(-5)", "max(1, 9, 3)", "min(4, 2)", "round(3.14159, 2)", "floor(2.9) + ceil(2.1)",
    "pi", "2pi", "3(4+1)", "5!", "ln(e)", "log(1000)", "log2(8)", "sin(0)", "cos(0)", "((((1+1))))", "  42  ",
    -- errors
    "", "1/0", "5 % 0", "2 +", "(1", "1)", "foo", "foo(1)", "sqrt(-1)", "ln(0)", "(-1)!", "1.5!", "2 $ 3", "os.exit()", "load('x')()", "1 2 3 4 +",
    string.rep("(", 60) .. "1" .. string.rep(")", 60), string.rep("1+", 400) .. "1",
  }
  for i, c in ipairs(cases) do
    local v, err = calc.eval(c)
    out[i] = { src = c:sub(1, 30), value = v and calc.format(v) or nil, err = err }
  end
  flux.storage.set("out", { cases = out, huge = calc.format(calc.eval("170!")), inf = calc.format(calc.eval("171!")) })
end"#,
    );
    declare_driver(d.path(), "calculator");
    let h = start_root(d.path());
    h.run("calculator:driver");
    let out: serde_json::Value = serde_json::from_str(&h.wait_local("calculator", "out")).unwrap();
    let v = |src: &str| out["cases"].as_array().unwrap().iter().find(|r| r["src"] == src).unwrap_or_else(|| panic!("no case {src}")).clone();
    let ok = |src: &str, want: &str| assert_eq!(v(src)["value"], want, "{src}: {}", v(src));
    let bad = |src: &str, needle: &str| {
        let r = v(src);
        assert!(r["value"].is_null() && r["err"].as_str().unwrap_or("").contains(needle), "{src}: {r}");
    };
    ok("2+3*4", "14");
    ok("(2+3)*4", "20");
    ok("2^3^2", "512");
    ok("-2^2", "-4");
    ok("2**10", "1024");
    ok("10/4", "2.5");
    ok("10 % 3", "1");
    ok("10 mod 3", "1");
    ok("7 - -3", "10");
    ok("1_000 * 2", "2000");
    ok(".5 + .25", "0.75");
    ok("1e3 + 1", "1001");
    ok("9223372036854775807 * 2", "1.84467440737e+19"); // no integer wrap-around
    ok("sqrt(16)", "4");
    ok("max(1, 9, 3)", "9");
    ok("round(3.14159, 2)", "3.14");
    ok("floor(2.9) + ceil(2.1)", "5");
    ok("pi", "3.14159265359");
    ok("2pi", "6.28318530718");
    ok("3(4+1)", "15");
    ok("5!", "120");
    ok("ln(e)", "1");
    ok("log(1000)", "3");
    ok("log2(8)", "3");
    ok("((((1+1))))", "2");
    ok("  42  ", "42");
    bad("", "empty");
    bad("1/0", "division by zero");
    bad("5 % 0", "division by zero");
    bad("2 +", "unexpected end");
    bad("(1", "missing ')'");
    bad("1)", "unexpected ')'");
    bad("foo", "unknown name 'foo'");
    bad("foo(1)", "unknown function 'foo'");
    bad("sqrt(-1)", "negative");
    bad("ln(0)", "non-positive");
    bad("(-1)!", "non-negative");
    bad("1.5!", "non-negative integer");
    bad("2 $ 3", "unexpected '$'");
    bad("os.exit()", "unexpected");
    bad("load('x')()", "unexpected");
    assert_eq!(out["huge"], "7.25741561531e+306");
    assert_eq!(out["inf"], "∞");
    // pathological inputs fail cleanly instead of hanging or crashing
    assert!(out["cases"].as_array().unwrap().iter().any(|r| r["err"].as_str().is_some_and(|e| e.contains("nested") || e.contains("too long"))));
}

#[test]
fn the_calculator_view_updates_as_you_type_and_keeps_a_history() {
    let d = tempfile::tempdir().unwrap();
    install_example(d.path(), "calculator");
    let h = start_root(d.path());
    h.run("calculator:main");
    let (id, mut view) = h.next_push();
    assert!(titles(&view).is_empty(), "nothing to show before typing");
    let ViewNode::List(l) = &view else { panic!() };
    assert!(!l.filtering && l.events.search_change);

    h.handle.send(RuntimeRequest::SearchChanged { view_id: id, text: "2 * (3 + 4) ^ 2".into() });
    h.wait_view(id, &mut view, |v| titles(v).first().is_some_and(|t| t == "98"));
    h.handle.send(RuntimeRequest::SearchChanged { view_id: id, text: "2 +".into() });
    h.wait_view(id, &mut view, |v| titles(v).first().is_some_and(|t| t == "Cannot evaluate"));
    h.handle.send(RuntimeRequest::SearchChanged { view_id: id, text: "6*7".into() });
    h.wait_view(id, &mut view, |v| titles(v).first().is_some_and(|t| t == "42"));

    // "Save to history" (second action of the result row)
    let save = list(view.clone()).item(0).unwrap().actions[0].actions[1].id;
    h.handle.send(RuntimeRequest::InvokeAction { view_id: id, action_id: save });
    h.wait_view(id, &mut view, |v| titles(v).len() == 2);
    let stored: serde_json::Value = serde_json::from_str(&h.wait_local("calculator", "history")).unwrap();
    assert_eq!(stored[0], serde_json::json!({ "expr": "6*7", "result": "42" }));
    // saving the same expression again does not duplicate it
    h.handle.send(RuntimeRequest::InvokeAction { view_id: id, action_id: save });
    h.handle.send(RuntimeRequest::SearchChanged { view_id: id, text: "".into() });
    h.wait_view(id, &mut view, |v| titles(v) == ["42"]);

    // a fresh run restores the history from storage
    h.handle.send(RuntimeRequest::ViewClosed { view_id: id });
    h.run("calculator:main");
    let (_, again) = h.next_push();
    assert_eq!(titles(&again), ["42"]);
    let l = list(again);
    assert_eq!(l.sections[0].title.as_deref(), Some("History"));
    let clear = &l.actions[0].actions[0];
    assert!(clear.confirm.is_some() && clear.style == ActionStyle::Destructive, "clearing asks first");
}

// ------------------------------------------------------------------- files

fn tree() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let p = t.path();
    std::fs::create_dir_all(p.join("docs/deep")).unwrap();
    std::fs::create_dir_all(p.join("src")).unwrap();
    std::fs::write(p.join("README.md"), "# Title\n\nSome *markdown*.").unwrap();
    std::fs::write(p.join("notes.txt"), "line one\nline two\n").unwrap();
    std::fs::write(p.join("big.log"), "x".repeat(100_000)).unwrap();
    std::fs::write(p.join("blob.bin"), [0u8, 1, 2, 0, 255]).unwrap();
    std::fs::write(p.join("photo.png"), b"\x89PNG\r\n\x1a\n").unwrap();
    std::fs::write(p.join(".secret"), "hidden").unwrap();
    std::fs::write(p.join("docs/guide.md"), "guide").unwrap();
    t
}

fn open_files(h: &Harness, start: &Path) -> (u64, ViewNode) {
    set_pref(h, "files", "start_dir", &serde_json::to_string(&start.to_string_lossy()).unwrap());
    h.run("files:browse");
    h.next_push()
}

fn find_action(v: &ViewNode, item: &str, action: &str) -> u64 {
    let it = match v {
        ViewNode::List(l) => l.items().find(|i| i.title == item).map(|i| i.actions.clone()),
        ViewNode::Grid(g) => g.items().find(|i| i.title.as_deref() == Some(item)).map(|i| i.actions.clone()),
        _ => None,
    }
    .unwrap_or_else(|| panic!("no item {item}: {:?}", titles(v)));
    it.iter().flat_map(|s| &s.actions).find(|a| a.title == action).unwrap_or_else(|| panic!("no action {action} on {item}")).id
}

fn global_action(v: &ViewNode, title: &str) -> u64 {
    v.global_actions().iter().flat_map(|s| &s.actions).find(|a| a.title == title).unwrap_or_else(|| panic!("no global action {title}")).id
}

#[test]
fn the_file_explorer_lists_navigates_previews_and_toggles_hidden_files() {
    let t = tree();
    let d = tempfile::tempdir().unwrap();
    install_example(d.path(), "files");
    let h = start_root(d.path());
    let (id, mut view) = open_files(&h, t.path());

    let ViewNode::List(l) = &view else { panic!() };
    assert_eq!(l.title.as_deref(), Some(t.path().to_str().unwrap()));
    assert_eq!(l.sections[0].title.as_deref(), Some("Folders"));
    assert_eq!(titles(&view), ["docs", "src", "big.log", "blob.bin", "notes.txt", "photo.png", "README.md"], "folders first, hidden files excluded");

    // enter a folder
    let open = find_action(&view, "docs", "Open folder");
    h.handle.send(RuntimeRequest::InvokeAction { view_id: id, action_id: open });
    h.wait_view(id, &mut view, |v| titles(v) == ["deep", "guide.md"]);
    let ViewNode::List(l) = &view else { panic!() };
    assert!(l.title.as_deref().unwrap().ends_with("/docs"));

    // up again, via the global action
    h.handle.send(RuntimeRequest::InvokeAction { view_id: id, action_id: global_action(&view, "Go to parent folder") });
    h.wait_view(id, &mut view, |v| titles(v).len() == 7);

    // hidden files
    h.handle.send(RuntimeRequest::InvokeAction { view_id: id, action_id: global_action(&view, "Show hidden files") });
    h.wait_view(id, &mut view, |v| titles(v).contains(&".secret".to_string()));
    h.handle.send(RuntimeRequest::InvokeAction { view_id: id, action_id: global_action(&view, "Hide hidden files") });
    h.wait_view(id, &mut view, |v| !titles(v).contains(&".secret".to_string()));

    // new files appear after a refresh
    std::fs::write(t.path().join("zzz-new.txt"), "n").unwrap();
    h.handle.send(RuntimeRequest::InvokeAction { view_id: id, action_id: global_action(&view, "Refresh") });
    h.wait_view(id, &mut view, |v| titles(v).contains(&"zzz-new.txt".to_string()));

    // accessories: size and modified time
    let ViewNode::List(l) = &view else { panic!() };
    let notes = l.items().find(|i| i.title == "notes.txt").unwrap();
    assert_eq!(notes.accessories[0].text.as_deref(), Some("18 B"));
    assert!(notes.accessories[1].text.as_deref().unwrap().starts_with("20"));

    // details of a text file: preview in a code fence + metadata
    h.handle.send(RuntimeRequest::InvokeAction { view_id: id, action_id: find_action(&view, "notes.txt", "Show details") });
    let (_, detail) = h.next_push();
    let ViewNode::Detail(dv) = detail else { panic!() };
    let DetailBlock::Markdown(md) = &dv.blocks[0] else { panic!() };
    assert!(md.contains("line one\nline two") && md.contains("```"), "{md}");
    assert!(dv.metadata.iter().any(|m| matches!(m, MetaItem::Label { title, text, .. } if title == "Size" && text == "18 B")));

    // markdown files render as markdown, large files are cut, binary files are not dumped
    for (name, needle) in [("README.md", "# Title"), ("big.log", "first 64 KB"), ("blob.bin", "binary file")] {
        h.handle.send(RuntimeRequest::InvokeAction { view_id: id, action_id: find_action(&view, name, "Show details") });
        let (_, d) = h.next_push();
        let ViewNode::Detail(dv) = d else { panic!() };
        let DetailBlock::Markdown(md) = &dv.blocks[0] else { panic!() };
        assert!(md.contains(needle), "{name}: {}", &md[..md.len().min(200)]);
        assert!(!md.contains('\0'));
    }
}

#[test]
fn the_file_explorer_grid_shows_image_thumbnails_and_its_errors_are_friendly() {
    let t = tree();
    let d = tempfile::tempdir().unwrap();
    install_example(d.path(), "files");
    let h = start_root(d.path());
    set_pref(&h, "files", "view", "\"grid\"");
    let (id, mut view) = open_files(&h, t.path());
    let ViewNode::Grid(g) = &view else { panic!("expected a grid, got {view:?}") };
    let photo = g.items().find(|i| i.title.as_deref() == Some("photo.png")).unwrap();
    assert!(matches!(&photo.content, GridContent::Image(Image { source: ImageSource::Path(p), .. }) if p.ends_with("photo.png")), "{:?}", photo.content);
    let docs = g.items().find(|i| i.title.as_deref() == Some("docs")).unwrap();
    assert!(matches!(&docs.content, GridContent::Icon(_)));

    h.handle.send(RuntimeRequest::InvokeAction { view_id: id, action_id: global_action(&view, "Switch to list") });
    h.wait_view(id, &mut view, |v| matches!(v, ViewNode::List(_)));

    // a start folder that does not exist falls back to home instead of failing
    let h2 = start_root(d.path());
    set_pref(&h2, "files", "start_dir", "\"/definitely/not/here\"");
    h2.run("files:browse");
    let (_, v) = h2.next_push();
    let ViewNode::List(l) = v else { panic!() };
    assert_eq!(l.title.as_deref(), std::env::var("HOME").ok().as_deref());
}

#[test]
fn the_file_explorer_cannot_read_credential_folders() {
    let d = tempfile::tempdir().unwrap();
    install_example(d.path(), "files");
    let h = start_root(d.path());
    set_pref(&h, "files", "start_dir", "\"~/.ssh\"");
    h.run("files:browse");
    let (_, v) = h.next_push();
    // either ~/.ssh does not exist (falls back to home) or the API refused it: it never lists keys
    let ViewNode::List(l) = v else { panic!() };
    assert!(!l.items().any(|i| i.title.starts_with("id_")), "private keys must never be listed");
}

// --------------------------------------------------------------- processes

fn child_pid(arg: &str) -> u32 {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(p) = pid_of(&format!("sleep {arg}")) {
            return p;
        }
        assert!(Instant::now() < end, "child did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn ps_output_is_parsed_defensively() {
    let d = tempfile::tempdir().unwrap();
    install_example(d.path(), "processes");
    write(
        &d.path().join("processes"),
        "commands/driver.lua",
        r#"local ps = require("ps")
function main()
  local rows = ps.parse(table.concat({
    "    1  0.0  11520 root     systemd         /sbin/init splash",
    " 4242 12.5 204800 alice    firefox         /usr/lib/firefox/firefox --new-window https://x.test",
    "  777  0.3      0 bob      kworker/0:1     kworker/0:1",
    "garbage line",
    "abc 1 2 u c a",
    "",
  }, "\n"))
  flux.storage.set("out", { rows = rows, mem = { ps.mem(512), ps.mem(2048), ps.mem(3 * 1048576) } })
end"#,
    );
    declare_driver(d.path(), "processes");
    let h = start_root(d.path());
    h.run("processes:driver");
    let out: serde_json::Value = serde_json::from_str(&h.wait_local("processes", "out")).unwrap();
    let rows = out["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "garbage lines are skipped");
    assert_eq!((rows[1]["pid"].as_i64(), rows[1]["user"].as_str(), rows[1]["name"].as_str()), (Some(4242), Some("alice"), Some("firefox")));
    assert_eq!(rows[1]["cpu"], 12.5);
    assert_eq!(rows[1]["command"], "/usr/lib/firefox/firefox --new-window https://x.test");
    assert_eq!(rows[2]["command"], "kworker/0:1", "kernel threads have no arguments");
    assert_eq!(out["mem"], serde_json::json!(["512 KB", "2 MB", "3.0 GB"]));
}

#[test]
fn the_process_manager_lists_live_processes_and_terminates_one() {
    let d = tempfile::tempdir().unwrap();
    install_example(d.path(), "processes");
    let h = start_root(d.path());
    let mut child = std::process::Command::new("sleep").arg("41.37").spawn().unwrap();
    let pid = child.id();
    h.run("processes:list");
    let (id, mut view) = h.next_push();
    let key = pid.to_string();
    h.wait_view(id, &mut view, |v| matches!(v, ViewNode::List(l) if l.items().any(|i| i.key == key)));

    let ViewNode::List(l) = &view else { panic!() };
    let item = l.items().find(|i| i.key == key).unwrap();
    assert_eq!(item.title, "sleep");
    assert!(item.subtitle.as_deref().unwrap().contains("41.37"));
    assert!(item.accessories.iter().any(|a| a.text.as_deref() == Some(&format!("pid {pid}"))));
    let term = item.actions.iter().flat_map(|s| &s.actions).find(|a| a.title.starts_with("Terminate")).unwrap();
    assert_eq!(term.style, ActionStyle::Destructive);
    assert!(term.confirm.as_ref().unwrap().title.contains("sleep"), "the core asks before anything is signalled");

    // live detail page
    h.handle.send(RuntimeRequest::InvokeAction { view_id: id, action_id: item.actions[0].actions[0].id });
    let (_, detail) = h.next_push();
    let ViewNode::Detail(dv) = detail else { panic!() };
    assert!(dv.blocks.iter().any(|b| matches!(b, DetailBlock::ProgressBar { .. })));
    assert!(dv.metadata.iter().any(|m| matches!(m, MetaItem::Label { title, text, .. } if title == "PID" && *text == key)));

    // terminate it for real
    h.handle.send(RuntimeRequest::InvokeAction { view_id: id, action_id: term.id });
    let status = child.wait().unwrap(); // reaps it ourselves: we are its parent
    assert!(!status.success(), "killed by a signal");
    // the next refresh drops it from the list
    h.wait_view(id, &mut view, |v| matches!(v, ViewNode::List(l) if !l.items().any(|i| i.key == key)));

    // closing the screen stops the refresh loop (no more patches, no leaked task)
    h.handle.send(RuntimeRequest::ViewClosed { view_id: id });
    let _ = h.drain_for(Duration::from_millis(200));
    assert!(h.drain_for(Duration::from_millis(3000)).iter().all(|e| !matches!(e, RuntimeEvent::Nav(NavEvent::Update { .. }))));
    let _ = child_pid; // helper kept for symmetry with the other process tests
}

// ------------------------------------------------------------------ github

struct Api {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
}

/// A tiny fake GitHub: `/search/repositories`, `/repos/<o>/<r>/readme`, and rate-limit errors for `q=limited`.
fn fake_github() -> Api {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let log = log.clone();
            std::thread::spawn(move || {
                let mut s = stream;
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let first = head.lines().next().unwrap_or("").to_string();
                let auth = head.lines().find(|l| l.to_lowercase().starts_with("authorization:")).map(|l| l.to_string());
                log.lock().unwrap().push(format!("{first} | {}", auth.unwrap_or_default()));
                let target = first.split(' ').nth(1).unwrap_or("/");
                let (status, ctype, body, extra) = if target.starts_with("/search/repositories") {
                    if target.contains("q=limited") {
                        ("403 Forbidden", "application/json", r#"{"message":"API rate limit exceeded"}"#.to_string(), "X-RateLimit-Remaining: 0\r\n")
                    } else {
                        let q = target.split("q=").nth(1).unwrap_or("").split('&').next().unwrap_or("");
                        let items: Vec<_> = (1..=3)
                            .map(|i| {
                                serde_json::json!({
                                    "full_name": format!("octo/{q}-{i}"), "description": format!("Repo {i} about {q}"),
                                    "html_url": format!("https://github.com/octo/{q}-{i}"), "clone_url": format!("https://github.com/octo/{q}-{i}.git"),
                                    "stargazers_count": 1500 * i, "forks_count": i, "open_issues_count": 7, "language": "Lua",
                                    "topics": ["launcher", "lua"], "license": { "spdx_id": "MIT" }, "pushed_at": "2026-01-02T03:04:05Z",
                                    "homepage": "https://example.com", "owner": { "avatar_url": "https://avatars.githubusercontent.com/u/1" }
                                })
                            })
                            .collect();
                        ("200 OK", "application/json", serde_json::json!({ "total_count": 3, "items": items }).to_string(), "")
                    }
                } else if target.ends_with("/readme") {
                    if target.contains("norepo") {
                        ("404 Not Found", "application/json", r#"{"message":"Not Found"}"#.to_string(), "")
                    } else {
                        ("200 OK", "text/markdown", "# README\n\nHello from the fake API.\n".to_string(), "")
                    }
                } else {
                    ("404 Not Found", "text/plain", "?".to_string(), "")
                };
                let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            });
        }
    });
    Api { port, requests }
}

fn github_root(port: u16) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    install_example(d.path(), "github");
    // the same extension, allowed to talk to the local fake instead of api.github.com
    let m = std::fs::read_to_string(d.path().join("github/manifest.lua")).unwrap();
    let _ = port;
    write(&d.path().join("github"), "manifest.lua", &m.replace(r#"{ "api.github.com", "avatars.githubusercontent.com" }"#, r#"{ "127.0.0.1" }"#));
    d
}

fn github_harness(api: &Api, d: &tempfile::TempDir) -> Harness {
    let h = start_root(d.path());
    set_pref(&h, "github", "api_url", &format!("\"http://127.0.0.1:{}\"", api.port));
    set_pref(&h, "github", "avatars", "false");
    h
}

#[test]
fn github_search_debounces_shows_results_and_loads_the_readme() {
    let api = fake_github();
    let d = github_root(api.port);
    let h = github_harness(&api, &d);
    h.run("github:search");
    let (id, mut view) = h.next_push();
    let ViewNode::List(l) = &view else { panic!() };
    assert!(!l.filtering && l.events.search_change && l.empty.title.as_deref() == Some("Search GitHub"));

    // typing fast: only the last query reaches the server
    for q in ["l", "lu", "lua", "luau"] {
        h.handle.send(RuntimeRequest::SearchChanged { view_id: id, text: q.into() });
    }
    h.wait_view(id, &mut view, |v| titles(v).len() == 3);
    let ViewNode::List(l) = &view else { panic!() };
    assert!(!l.is_loading);
    assert_eq!(titles(&view), ["octo/luau-1", "octo/luau-2", "octo/luau-3"]);
    assert_eq!(l.sections[0].title.as_deref(), Some("3 repositories"));
    let first = l.item(0).unwrap();
    assert_eq!(first.accessories.last().unwrap().text.as_deref(), Some("1.5k"));
    assert_eq!(first.accessories[0].tag.as_ref().unwrap().text, "Lua");
    let search_calls = api.requests.lock().unwrap().iter().filter(|r| r.contains("/search/repositories")).count();
    assert_eq!(search_calls, 1, "debounce + latest-wins: {:?}", api.requests.lock().unwrap());
    assert!(api.requests.lock().unwrap().iter().any(|r| r.contains("q=luau") && r.contains("per_page=30")));

    // details: the page opens at once and the README arrives later
    h.handle.send(RuntimeRequest::InvokeAction { view_id: id, action_id: first.actions[0].actions[0].id });
    let (did, mut detail) = h.next_push();
    let ViewNode::Detail(dv) = &detail else { panic!() };
    assert!(dv.is_loading);
    h.wait_view(did, &mut detail, |v| matches!(v, ViewNode::Detail(d) if matches!(&d.blocks[0], DetailBlock::Markdown(m) if m.contains("Hello from the fake API"))));
    let ViewNode::Detail(dv) = &detail else { panic!() };
    assert!(!dv.is_loading);
    let label = |t: &str| dv.metadata.iter().find_map(|m| match m { MetaItem::Label { title, text, .. } if title == t => Some(text.clone()), _ => None }).unwrap();
    assert_eq!((label("Stars").as_str(), label("Language").as_str(), label("License").as_str(), label("Updated").as_str()), ("1500", "Lua", "MIT", "2026-01-02"));
    assert!(dv.metadata.iter().any(|m| matches!(m, MetaItem::TagList { title, tags } if title == "Topics" && tags.len() == 2)));
    assert!(dv.metadata.iter().any(|m| matches!(m, MetaItem::Link { url, .. } if url == "https://github.com/octo/luau-1")));
    let titles_a: Vec<_> = dv.actions.iter().flat_map(|s| &s.actions).map(|a| a.title.clone()).collect();
    assert_eq!(titles_a, ["Open in browser", "Copy URL", "Copy clone command"]);

    // clearing the search empties the list without another request
    let before = api.requests.lock().unwrap().len();
    h.handle.send(RuntimeRequest::SearchChanged { view_id: id, text: "".into() });
    h.wait_view(id, &mut view, |v| titles(v).is_empty());
    assert_eq!(api.requests.lock().unwrap().len(), before);
}

#[test]
fn github_reports_rate_limits_missing_readmes_and_sends_the_token_only_when_set() {
    let api = fake_github();
    let d = github_root(api.port);
    let h = github_harness(&api, &d);
    h.run("github:search");
    let (id, mut view) = h.next_push();

    h.handle.send(RuntimeRequest::SearchChanged { view_id: id, text: "limited".into() });
    h.wait_view(id, &mut view, |v| matches!(v, ViewNode::List(l) if l.empty.title.as_deref() == Some("Search failed")));
    let ViewNode::List(l) = &view else { panic!() };
    assert!(l.empty.description.as_deref().unwrap().contains("rate limit"), "{:?}", l.empty);
    assert!(api.requests.lock().unwrap().iter().all(|r| !r.to_lowercase().contains("bearer")), "no token configured, none sent");

    // with a token (kept in the Secret Service) it is sent as a bearer token
    h.secrets.set("github", "pref:token", "ghp_secret").await_blocking();
    h.handle.send(RuntimeRequest::ViewClosed { view_id: id });
    h.run("github:search");
    let (id2, mut view2) = h.next_push();
    h.handle.send(RuntimeRequest::SearchChanged { view_id: id2, text: "lua".into() });
    h.wait_view(id2, &mut view2, |v| titles(v).len() == 3);
    assert!(api.requests.lock().unwrap().iter().any(|r| r.contains("q=lua") && r.contains("Bearer ghp_secret")), "{:?}", api.requests.lock().unwrap());
    assert!(!h.storage.prefs.lock().unwrap().values().any(|v| v.contains("ghp_secret")), "tokens never touch the database");
}

trait Block {
    fn await_blocking(self);
}
impl<F: std::future::Future<Output = Result<(), SecretError>>> Block for F {
    fn await_blocking(self) {
        let rt = tokio_rt();
        rt.block_on(self).unwrap();
    }
}
fn tokio_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

// -------------------------------------------------------------------- demo

#[test]
fn the_demo_covers_form_grid_and_detail() {
    let d = tempfile::tempdir().unwrap();
    install_example(d.path(), "demo");
    let h = start_root(d.path());
    h.run("demo:form");
    let (_, form) = h.next_push();
    let ViewNode::Form(f) = form else { panic!() };
    let kinds: std::collections::HashSet<&str> = f
        .fields
        .iter()
        .map(|x| match x.kind {
            FieldKind::Text => "text",
            FieldKind::Password => "password",
            FieldKind::TextArea { .. } => "textarea",
            FieldKind::Checkbox { .. } => "checkbox",
            FieldKind::Dropdown { .. } => "dropdown",
            FieldKind::TagPicker { .. } => "tags",
            FieldKind::DatePicker { .. } => "date",
            FieldKind::FilePicker { .. } => "file",
            FieldKind::Separator => "separator",
            FieldKind::Description { .. } => "description",
        })
        .collect();
    assert_eq!(kinds.len(), 10, "every field kind appears in the demo: {kinds:?}");

    h.run("demo:grid");
    let (_, grid) = h.next_push();
    assert!(matches!(&grid, ViewNode::Grid(g) if g.sections.len() == 2 && g.item_count_total() > 20));
    h.run("demo:detail");
    let (id, mut detail) = h.next_push();
    assert!(matches!(&detail, ViewNode::Detail(d) if d.is_loading));
    h.wait_view(id, &mut detail, |v| matches!(v, ViewNode::Detail(d) if !d.is_loading));
}

trait CountItems {
    fn item_count_total(&self) -> usize;
}
impl CountItems for GridView {
    fn item_count_total(&self) -> usize {
        self.items().count()
    }
}
