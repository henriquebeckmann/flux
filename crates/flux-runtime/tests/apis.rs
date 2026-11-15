//! filesystem, process.kill and the permission matrix.

mod common;

use std::time::{Duration, Instant};

use common::*;
use flux_api::*;

fn one(perms: &[&str], files: &[(&str, &str)]) -> (tempfile::TempDir, Harness) {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "e", perms, &[("t", "no-view")], files);
    let h = start(d.path());
    let _ = h.next();
    (d, h)
}

fn stored(h: &Harness, cmd: &str) -> serde_json::Value {
    h.run(&format!("e:{cmd}"));
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(v) = h.storage.local_get("e", "out") {
            h.storage.local_remove("e", "out");
            return serde_json::from_str(&v).unwrap();
        }
        if let Some(RuntimeEvent::ExtensionError { message, .. }) = h.recv(Duration::from_millis(20)) {
            panic!("extension error: {message}");
        }
        assert!(Instant::now() < end, "timed out");
    }
}

#[test]
fn filesystem_lists_stats_reads_and_orders_directories_first() {
    let tree = tempfile::tempdir().unwrap();
    let t = tree.path();
    std::fs::create_dir_all(t.join("b_dir")).unwrap();
    std::fs::create_dir_all(t.join("A_dir")).unwrap();
    std::fs::write(t.join("z.txt"), "hello world").unwrap();
    std::fs::write(t.join("a.txt"), "x".repeat(100)).unwrap();
    std::fs::write(t.join(".hidden"), "h").unwrap();
    std::os::unix::fs::symlink(t.join("a.txt"), t.join("link")).unwrap();
    let root = t.to_string_lossy().to_string();
    let src = format!(
        r#"function main()
  local fs = flux.filesystem
  local root = '{root}'
  local names = function(l) local n = {{}} for _, e in ipairs(l) do n[#n + 1] = e.name end return n end
  local out = {{}}
  out.list = names(fs.list(root))
  out.hidden = names(fs.list(root, {{ hidden = true }}))
  local st = fs.stat(root .. '/z.txt')
  out.stat = {{ size = st.size, is_file = st.is_file, is_dir = st.is_dir, mod = st.modified ~= nil }}
  out.link = fs.stat(root .. '/link').is_symlink
  out.text, out.truncated = fs.read_text(root .. '/z.txt')
  out.cut, out.cut_flag = fs.read_text(root .. '/a.txt', {{ max_bytes = 10 }})
  out.exists = {{ fs.exists(root), fs.exists(root .. '/nope') }}
  out.dotdot = fs.exists(root .. '/b_dir/../z.txt')
  out.missing = {{ fs.list(root .. '/nope') }}
  out.helpers = {{ fs.join('/a/', '/b', 'c.txt'), fs.basename('/a/b/c.txt'), fs.dirname('/a/b/c.txt'), fs.extension('/a/b/c.tar.gz'), fs.dirname('c') }}
  out.home = type(fs.home())
  flux.storage.set('out', out)
end"#
    );
    let (_d, h) = one(&["filesystem"], &[("commands/t.lua", &src)]);
    let o = stored(&h, "t");
    assert_eq!(o["list"], serde_json::json!(["A_dir", "b_dir", "a.txt", "link", "z.txt"]), "directories first, case-insensitive");
    assert_eq!(o["hidden"].as_array().unwrap().len(), 6);
    assert_eq!(o["stat"], serde_json::json!({ "size": 11, "is_file": true, "is_dir": false, "mod": true }));
    assert_eq!(o["link"], true);
    assert_eq!((o["text"].as_str(), o["truncated"].as_bool()), (Some("hello world"), Some(false)));
    assert_eq!((o["cut"].as_str(), o["cut_flag"].as_bool()), (Some("xxxxxxxxxx"), Some(true)));
    assert_eq!(o["exists"], serde_json::json!([true, false]));
    assert_eq!(o["dotdot"], true);
    assert!(o["missing"][0].is_null() && o["missing"][1].as_str().unwrap().contains("No such file"), "failures return nil, reason: {}", o["missing"]);
    assert_eq!(o["helpers"], serde_json::json!(["/a/b/c.txt", "c.txt", "/a/b", "gz", "."]));
    assert_eq!(o["home"], "string");
}

#[test]
fn filesystem_refuses_relative_paths_and_protected_locations() {
    let d = tempfile::tempdir().unwrap();
    // two extensions: `e` (the attacker) and `victim`; their data dirs live under <root>/.data/
    ext(d.path(), "e", &["filesystem"], &[("t", "no-view"), ("own", "no-view")], &[("commands/t.lua", ""), ("commands/own.lua", "")]);
    ext(d.path(), "victim", &[], &[("save", "no-view")], &[("commands/save.lua", "function main() flux.storage.set('k', 1) end")]);
    let victim_data = d.path().join(".data/victim");
    std::fs::create_dir_all(&victim_data).unwrap();
    std::fs::write(victim_data.join("secret.txt"), "top secret").unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(&victim_data, outside.path().join("sneaky")).unwrap();
    let cases = [
        ("relative".to_string(), "relative/path.txt".to_string()),
        ("victim".to_string(), victim_data.join("secret.txt").to_string_lossy().to_string()),
        ("dotdot".to_string(), format!("{}/e/../.data/victim/secret.txt", d.path().display())),
        ("symlink".to_string(), outside.path().join("sneaky/secret.txt").to_string_lossy().to_string()),
        ("ssh".to_string(), "~/.ssh/id_ed25519".to_string()),
        ("flux_config".to_string(), "~/.config/flux/config.toml".to_string()),
    ];
    let body: String = cases.iter().map(|(n, p)| format!("  r['{n}'] = tostring(select(2, pcall(flux.filesystem.read_text, '{p}')))\n")).collect();
    write(&d.path().join("e"), "commands/t.lua", &format!("function main()\n  local r = {{}}\n{body}  flux.storage.set('out', r)\nend"));
    write(&d.path().join("e"), "commands/own.lua", "function main() local f = flux.filesystem; flux.storage.set('out', { f.exists(flux.environment.data_dir) }) end");
    let h = start(d.path());
    let _ = h.next();
    let o = stored(&h, "t");
    assert!(o["relative"].as_str().unwrap().contains("not absolute"), "{o}");
    for k in ["victim", "dotdot", "symlink", "ssh", "flux_config"] {
        assert!(o[k].as_str().unwrap_or("").contains("protected"), "{k}: {o}");
    }
    assert!(!o.to_string().contains("top secret"));
    // an extension's own data directory stays reachable
    assert_eq!(stored(&h, "own"), serde_json::json!([true]));
}

#[test]
fn process_kill_terminates_and_refuses_dangerous_targets() {
    let (_d, h) = one(&["process"], &[(
        "commands/t.lua",
        r#"function main()
  local pid = flux.process.spawn('sleep', { '37.25' })
  local out = { pid = pid }
  out.refuse1 = tostring(select(2, pcall(flux.process.kill, 1)))
  out.refuse0 = tostring(select(2, pcall(flux.process.kill, 0)))
  out.refuse_neg = tostring(select(2, pcall(flux.process.kill, -5)))
  out.badsig = tostring(select(2, pcall(flux.process.kill, pid, 'NOPE')))
  out.ok = flux.process.kill(pid, 'TERM')
  local okk, why = pcall(flux.process.kill, 2147483000)
  out.nosuch = tostring(why)
  flux.storage.set('out', out)
end"#,
    )]);
    let o = stored(&h, "t");
    let pid = o["pid"].as_u64().unwrap() as u32;
    assert!(o["refuse1"].as_str().unwrap().contains("refusing pid"));
    assert!(o["refuse0"].as_str().unwrap().contains("refusing pid"));
    assert!(o["refuse_neg"].as_str().unwrap().contains("refusing pid"));
    assert!(o["badsig"].as_str().unwrap().contains("unknown signal"));
    assert_eq!(o["ok"], true);
    assert!(o["nosuch"].as_str().unwrap().contains("was not started by this extension"), "a pid the extension never started is refused: {o}");
    wait_until_gone(pid); // killed and reaped
}

/// Every privileged call, without its permission, fails with a message naming the permission.
#[test]
fn permission_matrix_every_privileged_api_is_gated() {
    let calls: &[(&str, &str)] = &[
        ("clipboard.read", "flux.clipboard.getText()"),
        ("clipboard.write", "flux.clipboard.setText('x')"),
        ("clipboard.write", "flux.clipboard.clear()"),
        ("process.execute", "flux.process.exec('true')"),
        ("process.execute", "flux.process.spawn('true')"),
        ("process.kill", "flux.process.kill(99999)"),
        ("browser.open", "flux.browser.open('https://example.com')"),
        ("filesystem.read", "flux.filesystem.home()"),
        ("filesystem.read", "flux.filesystem.list('/tmp')"),
        ("filesystem.read", "flux.filesystem.stat('/tmp')"),
        ("filesystem.read", "flux.filesystem.exists('/tmp')"),
        ("filesystem.read", "flux.filesystem.read_text('/etc/hostname')"),
        ("filesystem.read", "flux.filesystem.open('/tmp')"),
        ("filesystem.write", "flux.filesystem.write_text('/tmp/flux-never', 'x')"),
        ("network", "flux.http.get('http://127.0.0.1:1/')"),
        ("network", "flux.http.request { method = 'POST', url = 'http://127.0.0.1:1/' }"),
    ];
    let d = tempfile::tempdir().unwrap();
    let names: Vec<String> = (0..calls.len()).map(|i| format!("c{i}")).collect();
    let cmds: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "no-view")).collect();
    let files: Vec<(String, String)> = calls.iter().enumerate().map(|(i, (_, c))| (format!("commands/c{i}.lua"), format!("function main() {c} end"))).collect();
    let files: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    ext(d.path(), "none", &[], &cmds, &files);
    let h = start(d.path());
    let _ = h.next();
    for (i, (perm, call)) in calls.iter().enumerate() {
        h.run(&format!("none:c{i}"));
        let m = h.next_error();
        assert!(m.contains(&format!("'{perm}' permission")), "{call}: {m}");
    }
}

#[test]
fn json_encode_and_decode_roundtrip() {
    let (_d, h) = one(&[], &[(
        "commands/t.lua",
        r#"function main()
  local j = flux.json
  local s = j.encode({ a = 1, b = { 1, 2, 3 }, c = 'x' })
  local back = j.decode(s)
  local out = { sum = back.a + #back.b, c = back.c, nul = j.decode('null'), arr = j.decode('[1,[2]]')[2][1] }
  out.bad = tostring(select(2, pcall(j.decode, '{oops')))
  out.pretty = j.encode({ 1 }, true) ~= nil
  flux.storage.set('out', out)
end"#,
    )]);
    let o = stored(&h, "t");
    assert_eq!((o["sum"].as_i64(), o["c"].as_str(), o["arr"].as_i64()), (Some(4), Some("x"), Some(2)));
    assert!(o["bad"].as_str().unwrap().contains("invalid JSON"));
}
