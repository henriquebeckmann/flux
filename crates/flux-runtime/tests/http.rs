//! `flux.http` against an in-process HTTP server (no external network).

mod common;

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use common::*;
use flux_api::*;

struct Server {
    port: u16,
    hits: Arc<AtomicUsize>,
    /// The client hung up while `/hang` was waiting.
    hangup_seen: Arc<AtomicBool>,
}

fn read_request(s: &mut TcpStream) -> Option<(String, String, Vec<(String, String)>, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?.to_string(), first.next()?.to_string());
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string())).collect();
    let len: usize = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < len {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    Some((method, target, headers, body))
}

fn respond(s: &mut TcpStream, status: &str, headers: &[(&str, String)], body: &[u8]) {
    let mut out = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let _ = s.write_all(out.as_bytes());
    let _ = s.write_all(body);
}

fn serve() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (hits, hangup_seen) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
    let (h2, hs2) = (hits.clone(), hangup_seen.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (hits, hangup) = (h2.clone(), hs2.clone());
            std::thread::spawn(move || {
                let mut s = stream;
                let Some((method, target, headers, body)) = read_request(&mut s) else { return };
                hits.fetch_add(1, Ordering::SeqCst);
                let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
                match path {
                    "/echo" => {
                        let h: serde_json::Map<String, serde_json::Value> = headers.iter().map(|(k, v)| (k.clone(), v.clone().into())).collect();
                        let j = serde_json::json!({ "method": method, "query": query, "headers": h, "body": String::from_utf8_lossy(&body) });
                        respond(&mut s, "200 OK", &[("Content-Type", "application/json".into())], j.to_string().as_bytes());
                    }
                    "/slow" => {
                        std::thread::sleep(Duration::from_secs(3));
                        respond(&mut s, "200 OK", &[], b"late");
                    }
                    "/hang" => {
                        // wait until the client goes away
                        s.set_read_timeout(Some(Duration::from_millis(50))).ok();
                        let end = Instant::now() + Duration::from_secs(5);
                        let mut b = [0u8; 1];
                        while Instant::now() < end {
                            match s.read(&mut b) {
                                Ok(0) => {
                                    hangup.store(true, Ordering::SeqCst);
                                    return;
                                }
                                Ok(_) => {}
                                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                                Err(_) => {
                                    hangup.store(true, Ordering::SeqCst);
                                    return;
                                }
                            }
                        }
                    }
                    "/missing" => respond(&mut s, "404 Not Found", &[], b"nope"),
                    "/big" => respond(&mut s, "200 OK", &[], &vec![b'x'; 2 * 1024 * 1024]),
                    "/redirect-in" => respond(&mut s, "302 Found", &[("Location", "/echo?via=redirect".into())], b""),
                    "/redirect-out" => {
                        let host = headers.iter().find(|(k, _)| k == "host").map(|(_, v)| v.clone()).unwrap_or_default();
                        let port = host.rsplit(':').next().unwrap_or("0").to_string();
                        respond(&mut s, "302 Found", &[("Location", format!("http://localhost:{port}/echo"))], b"");
                    }
                    _ => respond(&mut s, "404 Not Found", &[], b"?"),
                }
            });
        }
    });
    Server { port, hits, hangup_seen }
}

/// Extension `net` with the given permissions/allowed_hosts and commands (name -> lua).
fn setup(perms: &str, allowed: &str, cmds: &[(&str, String)]) -> (tempfile::TempDir, Harness) {
    let d = tempfile::tempdir().unwrap();
    let command_list: String = cmds.iter().map(|(n, _)| format!("{{name='{n}',title='{n}',mode='view'}},")).collect();
    write(
        &d.path().join("net"),
        "manifest.lua",
        &format!("return {{ name='net', commands={{{command_list}}}, permissions={{{perms}}}, allowed_hosts={{{allowed}}} }}"),
    );
    for (n, src) in cmds {
        write(&d.path().join("net"), &format!("commands/{n}.lua"), src);
    }
    let h = start(d.path());
    let _ = h.next();
    (d, h)
}

/// A command that runs `body` (Lua, sees `base`) and stores a JSON result under "out".
fn script(body: &str, port: u16) -> String {
    format!("local base = 'http://127.0.0.1:{port}'\nfunction main()\n{body}\nend")
}

fn run_json(h: &Harness, cmd: &str) -> serde_json::Value {
    h.run(&format!("net:{cmd}"));
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(v) = h.storage.local_get("net", "out") {
            h.storage.local_remove("net", "out");
            return serde_json::from_str(&v).unwrap();
        }
        if let Some(RuntimeEvent::ExtensionError { message, .. }) = h.recv(Duration::from_millis(20)) {
            panic!("extension error: {message}");
        }
        assert!(Instant::now() < end, "timed out");
    }
}

fn host_list(s: &Server) -> String {
    let _ = s;
    "'127.0.0.1'".to_string()
}

#[test]
fn all_methods_headers_query_json_form_and_body() {
    let s = serve();
    let (_d, h) = setup("'network'", &host_list(&s), &[(
        "t",
        script(
            r#"
  local out = {}
  local r = assert(flux.http.get(base .. '/echo', { query = { q = 'a b&c', n = 3, list = { 'x', 'y' } }, headers = { ['X-Token'] = 'abc' } }))
  out.get = r:json(); out.status = r.status; out.ok = r.ok; out.ctype = r.headers['content-type']
  out.post = assert(flux.http.post(base .. '/echo', { json = { a = 1, b = { 'x' } } })):json()
  out.put = assert(flux.http.put(base .. '/echo', { body = 'raw-body', headers = { ['Content-Type'] = 'text/plain' } })):json()
  out.patch = assert(flux.http.patch(base .. '/echo', { form = { k = 'v w', z = '1' } })):json()
  out.delete = assert(flux.http.delete(base .. '/echo')):json()
  out.req = assert(flux.http.request { method = 'post', url = base .. '/echo', json = { via = 'request' } }):json()
  flux.storage.set('out', out)"#,
            s.port,
        ),
    )]);
    let o = run_json(&h, "t");
    assert_eq!((o["status"].as_i64(), o["ok"].as_bool()), (Some(200), Some(true)));
    assert_eq!(o["ctype"], "application/json");
    assert_eq!(o["get"]["method"], "GET");
    assert_eq!(o["get"]["query"], "list=x&list=y&n=3&q=a+b%26c");
    assert_eq!(o["get"]["headers"]["x-token"], "abc");
    assert!(o["get"]["headers"]["user-agent"].as_str().unwrap().starts_with("Flux/"));
    assert_eq!(o["post"]["method"], "POST");
    assert_eq!(o["post"]["headers"]["content-type"], "application/json");
    let sent: serde_json::Value = serde_json::from_str(o["post"]["body"].as_str().unwrap()).unwrap();
    assert_eq!(sent, serde_json::json!({ "a": 1, "b": ["x"] }));
    assert_eq!((o["put"]["method"].as_str(), o["put"]["body"].as_str()), (Some("PUT"), Some("raw-body")));
    assert_eq!(o["put"]["headers"]["content-type"], "text/plain");
    assert_eq!(o["patch"]["method"], "PATCH");
    assert_eq!(o["patch"]["body"], "k=v+w&z=1");
    assert_eq!(o["patch"]["headers"]["content-type"], "application/x-www-form-urlencoded");
    assert_eq!(o["delete"]["method"], "DELETE");
    assert_eq!(o["req"]["method"], "POST");
}

#[test]
fn non_2xx_is_a_response_not_an_error_and_redirects_are_followed() {
    let s = serve();
    let (_d, h) = setup("'network'", &host_list(&s), &[(
        "t",
        script(
            r#"
  local r = flux.http.get(base .. '/missing')
  local f = flux.http.get(base .. '/redirect-in')
  local nf = flux.http.get(base .. '/redirect-in', { follow_redirects = false })
  flux.storage.set('out', { status = r.status, ok = r.ok, text = r:text(), redirected = f.redirected, final = f.url, nofollow = nf.status })"#,
            s.port,
        ),
    )]);
    let o = run_json(&h, "t");
    assert_eq!((o["status"].as_i64(), o["ok"].as_bool(), o["text"].as_str()), (Some(404), Some(false), Some("nope")));
    assert_eq!(o["redirected"], true);
    assert!(o["final"].as_str().unwrap().ends_with("/echo?via=redirect"));
    assert_eq!(o["nofollow"], 302);
}

#[test]
fn errors_are_values_with_a_kind() {
    let s = serve();
    let (_d, h) = setup("'network'", &format!("{}, 'localhost'", host_list(&s)), &[(
        "t",
        script(
            r#"
  local k = {}
  local function kind(...) local r, e = ...; return r and 'ok' or e.kind end
  k.timeout = kind(flux.http.get(base .. '/slow', { timeout = 0.3 }))
  k.big = kind(flux.http.get(base .. '/big', { max_bytes = 1000 }))
  k.bigok = kind(flux.http.get(base .. '/big'))
  k.badurl = kind(flux.http.get('not a url'))
  k.scheme = kind(flux.http.get('ftp://127.0.0.1/x'))
  k.refused = kind(flux.http.get('http://127.0.0.1:1/'))
  k.redirect_ok = kind(flux.http.get(base .. '/redirect-out'))
  local _, e = flux.http.get('http://127.0.0.1:1/')
  k.tostring = tostring(e):sub(1, 5)
  flux.storage.set('out', k)"#,
            s.port,
        ),
    )]);
    let o = run_json(&h, "t");
    assert_eq!(o["timeout"], "timeout");
    assert_eq!(o["big"], "too_large");
    assert_eq!(o["bigok"], "ok");
    assert_eq!(o["badurl"], "invalid");
    assert_eq!(o["scheme"], "invalid");
    assert_eq!(o["refused"], "network");
    assert_eq!(o["redirect_ok"], "ok", "localhost is listed, so the redirect is allowed");
    assert!(!o["tostring"].as_str().unwrap().is_empty(), "errors print as their message");
}

#[test]
fn hosts_are_restricted_by_allowed_hosts_on_every_redirect_hop() {
    let s = serve();
    // 127.0.0.1 is listed, `localhost` (the redirect target) is not
    let (_d, h) = setup("'network'", &host_list(&s), &[
        (
            "redirect",
            script("local _, e = flux.http.get(base .. '/redirect-out') flux.storage.set('out', { kind = e and e.kind, msg = e and e.message })", s.port),
        ),
        ("other", script("local _, e = flux.http.get('http://example.com/') flux.storage.set('out', { kind = e.kind })", s.port)),
    ]);
    let o = run_json(&h, "redirect");
    assert_eq!(o["kind"], "denied", "{o}");
    assert!(o["msg"].as_str().unwrap().contains("allowed_hosts"));
    assert_eq!(run_json(&h, "other")["kind"], "denied");
}

#[test]
fn private_addresses_are_denied_unless_explicitly_listed() {
    let s = serve();
    let (_d, h) = setup("'network'", "", &[(
        "t",
        script(
            r#"
  local out = {}
  for _, u in ipairs { base .. '/echo', 'http://localhost:1/', 'http://[::1]:1/', 'http://10.0.0.1/', 'http://192.168.1.1/', 'http://169.254.169.254/latest/meta-data/', 'http://0.0.0.0/' } do
    local _, e = flux.http.get(u)
    out[#out + 1] = e and e.kind or 'ok'
  end
  flux.storage.set('out', out)"#,
            s.port,
        ),
    )]);
    let o = run_json(&h, "t");
    assert!(o.as_array().unwrap().iter().all(|k| k == "denied"), "{o}");
    assert_eq!(s.hits.load(Ordering::SeqCst), 0, "no request may reach the server");
}

#[test]
fn wildcard_hosts_match_subdomains_only() {
    use flux_runtime::HostPolicy;
    let p = HostPolicy { allowed: vec!["*.example.com".into(), "api.github.com".into()] };
    assert!(p.host_allowed("a.example.com") && p.host_allowed("a.b.example.com") && p.host_allowed("api.github.com"));
    assert!(!p.host_allowed("example.com") && !p.host_allowed("evilexample.com") && !p.host_allowed("github.com"));
    assert!(!p.explicitly_listed("a.example.com") && p.explicitly_listed("api.github.com"));
}

#[test]
fn network_permission_is_required_and_options_are_validated() {
    let s = serve();
    let (_d, h) = setup("", "", &[("t", script("flux.http.get(base .. '/echo')", s.port))]);
    h.run("net:t");
    assert!(h.next_error().contains("'network' permission"));
    let (_d2, h2) = setup("'network'", &host_list(&s), &[
        ("opt", script("flux.http.get(base .. '/echo', { tiemout = 1 })", s.port)),
        ("both", script("flux.http.post(base .. '/echo', { json = {}, body = 'x' })", s.port)),
        ("hdr", script("flux.http.get(base .. '/echo', { headers = { ['bad name'] = 'x' } })", s.port)),
        ("tmo", script("flux.http.get(base .. '/echo', { timeout = -1 })", s.port)),
    ]);
    for (cmd, needle) in [("opt", "unknown option 'tiemout'"), ("both", "only one of"), ("hdr", "invalid header name"), ("tmo", "timeout must be")] {
        h2.run(&format!("net:{cmd}"));
        let m = h2.next_error();
        assert!(m.contains(needle), "{cmd}: {m}");
    }
    assert_eq!(s.hits.load(Ordering::SeqCst), 0);
}

#[test]
fn bad_allowed_hosts_in_the_manifest_are_rejected() {
    let d = tempfile::tempdir().unwrap();
    write(&d.path().join("x"), "manifest.lua", "return { name='x', permissions={'network'}, allowed_hosts={'https://api.example.com/v1'} }");
    let h = start(d.path());
    assert!(h.next_error().contains("allowed_hosts: invalid entry"));
}

const VIEW_WITH_REQUEST: &str = r#"
local base = 'http://127.0.0.1:%PORT%'
function main()
  return flux.ui.view {
    init = function(self)
      local token = flux.task.token()
      self.token = token
      self:task('req', function()
        local res, err = flux.http.get(base .. '%PATH%', { signal = token, timeout = 20 })
        flux.storage.set('out', err and err.kind or ('status ' .. res.status))
      end)
    end,
    render = function(self)
      return flux.ui.List { flux.ui.List.Item { key = 'k', title = 'x',
        actions = { flux.ui.Action { title = 'cancel', on_action = function() self.token:cancel() end } } } }
    end,
  }
end"#;

fn view_script(port: u16, path: &str) -> String {
    VIEW_WITH_REQUEST.replace("%PORT%", &port.to_string()).replace("%PATH%", path)
}

#[test]
fn a_request_is_cancelled_by_its_token() {
    let s = serve();
    let (_d, h) = setup("'network'", &host_list(&s), &[("t", view_script(s.port, "/hang"))]);
    h.run("net:t");
    let (view_id, v) = h.next_push();
    let action = list(v).item(0).unwrap().actions[0].actions[0].id;
    std::thread::sleep(Duration::from_millis(300)); // request in flight
    h.handle.send(RuntimeRequest::InvokeAction { view_id, action_id: action });
    assert_eq!(h.wait_local("net", "out"), "\"cancelled\"");
    let end = Instant::now() + Duration::from_secs(3);
    while !s.hangup_seen.load(Ordering::SeqCst) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(s.hangup_seen.load(Ordering::SeqCst), "the connection must be closed when the request is cancelled");
}

#[test]
fn closing_the_view_aborts_an_in_flight_request() {
    let s = serve();
    let (_d, h) = setup("'network'", &host_list(&s), &[("t", view_script(s.port, "/hang"))]);
    h.run("net:t");
    let (view_id, _) = h.next_push();
    std::thread::sleep(Duration::from_millis(300));
    h.handle.send(RuntimeRequest::ViewClosed { view_id });
    let end = Instant::now() + Duration::from_secs(3);
    while !s.hangup_seen.load(Ordering::SeqCst) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(s.hangup_seen.load(Ordering::SeqCst), "dropping the task must drop the connection");
    assert!(h.storage.local_get("net", "out").is_none(), "an aborted task never reports");
}

#[test]
fn a_slow_request_blocks_neither_other_extensions_nor_the_same_extension() {
    let s = serve();
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "other", &[], &[("ping", "no-view")], &[("commands/ping.lua", "function main() flux.storage.set('pong', 1) end")]);
    write(
        &d.path().join("net"),
        "manifest.lua",
        &format!("return {{ name='net', commands={{{{name='t',title='t',mode='view'}},{{name='ping',title='p',mode='no-view'}}}}, permissions={{'network'}}, allowed_hosts={{'127.0.0.1'}} }}"),
    );
    write(&d.path().join("net"), "commands/t.lua", &view_script(s.port, "/slow"));
    write(&d.path().join("net"), "commands/ping.lua", "function main() flux.storage.set('same', 1) end");
    let h = start(d.path());
    let _ = h.next();
    h.run("net:t");
    let _ = h.next_push();
    std::thread::sleep(Duration::from_millis(200)); // the 3 s request is now running
    let t = Instant::now();
    h.run("other:ping");
    h.run("net:ping"); // same extension, same thread: the request must only be *awaiting*
    assert_eq!(h.wait_local("other", "pong"), "1");
    assert_eq!(h.wait_local("net", "same"), "1");
    assert!(t.elapsed() < Duration::from_secs(1), "took {:?}", t.elapsed());
}
