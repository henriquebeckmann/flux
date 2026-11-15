//! Attempts to escape the execution limits from Lua. Every one must end with an error within the
//! CPU slice, leave the extension usable, and never delay other extensions.

mod common;

use std::{sync::Arc, time::{Duration, Instant}};

use common::*;
use flux_api::*;

fn limits() -> Limits {
    Limits { memory_mb: 32, slice_ms: 100, command_timeout_s: 20, ..Limits::default() }
}

fn setup(body: &str) -> (tempfile::TempDir, Harness) {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "evil", &[], &[("attack", "no-view"), ("ping", "no-view")], &[
        ("commands/attack.lua", body),
        ("commands/ping.lua", "function main() flux.storage.set('alive', os.clock()) end"),
    ]);
    ext(d.path(), "bystander", &[], &[("ping", "no-view")], &[("commands/ping.lua", "function main() flux.storage.set('pong', 1) end")]);
    let h = start_with(d.path(), limits(), Arc::new(MemSecretStore::default()));
    let _ = h.next();
    (d, h)
}

/// Runs the attack; asserts it errors with `needle` quickly, that a bystander extension answers
/// while it runs, and that the attacked extension still works afterwards.
fn attack(body: &str, needle: &str) {
    let (_d, h) = setup(body);
    let t = Instant::now();
    h.run("evil:attack");
    h.run("bystander:ping");
    assert_eq!(h.wait_local("bystander", "pong"), "1");
    let msg = h.next_error();
    assert!(msg.contains(needle), "expected '{needle}' in: {msg}");
    if needle.is_empty() {
        assert!(msg.contains("memory") || msg.contains("execution limit"), "{msg}");
    }
    assert!(t.elapsed() < Duration::from_secs(3), "took {:?}", t.elapsed());
    h.run("evil:ping");
    h.wait_local("evil", "alive");
}

#[test]
fn plain_infinite_loop() {
    attack("function main() while true do end end", "execution limit");
}

#[test]
fn pcall_cannot_swallow_the_limit() {
    attack("function main() while true do pcall(function() while true do end end) end end", "execution limit");
}

#[test]
fn nested_pcall_cannot_swallow_the_limit() {
    attack(
        "function main() while true do pcall(pcall, pcall, function() while true do pcall(function() while true do end end) end end) end end",
        "execution limit",
    );
}

#[test]
fn xpcall_with_a_looping_handler() {
    attack("function main() xpcall(function() error('x') end, function() while true do end end) while true do end end", "execution limit");
}

#[test]
fn xpcall_swallowing_loop() {
    attack("function main() while true do xpcall(function() while true do end end, function(e) return e end) end end", "execution limit");
}

#[test]
fn coroutine_resume_cannot_swallow_the_limit() {
    attack(
        "function main() local co = coroutine.create(function() while true do end end) while true do coroutine.resume(co) end end",
        "execution limit",
    );
}

#[test]
fn coroutine_wrap_inside_pcall() {
    attack("function main() while true do pcall(coroutine.wrap(function() while true do end end)) end end", "execution limit");
}

#[test]
fn callbacks_called_from_c_functions_are_limited_too() {
    attack("function main() local s = ('x'):rep(1000) while true do pcall(string.gsub, s, '.', function() while true do end end) end end", "execution limit");
}

#[test]
fn bare_yield_in_the_command_thread_is_refused() {
    attack("function main() while true do coroutine.yield() end end", "only be used inside coroutines");
}

#[test]
fn memory_bomb_inside_pcall() {
    attack("function main() while true do pcall(function() local t = {} for i = 1, 1e9 do t[i] = ('x'):rep(1000) .. i end end) end end", "");
}

#[test]
fn string_rep_bomb() {
    attack("function main() local s = ('x'):rep(1e10) end", "too large");
}

#[test]
fn legitimate_pcall_and_coroutines_still_work() {
    let d = tempfile::tempdir().unwrap();
    ext(d.path(), "ok", &[], &[("t", "no-view")], &[(
        "commands/t.lua",
        r#"function main()
  local ok, err = pcall(error, "boom")
  assert(not ok and err == "boom")
  local ok2, v = pcall(function() return 42 end)
  assert(ok2 and v == 42)
  local ok3, e3 = xpcall(function() error("x") end, function(m) return "handled: " .. m end)
  assert(not ok3 and e3:find("handled"))
  local gen = coroutine.wrap(function() for i = 1, 3 do coroutine.yield(i) end end)
  assert(gen() == 1 and gen() == 2 and gen() == 3)
  local co = coroutine.create(function(a) local b = coroutine.yield(a + 1) return b * 2 end)
  local _, x = coroutine.resume(co, 1)
  local _, y = coroutine.resume(co, 10)
  assert(x == 2 and y == 20)
  local so = pcall(function() local function f() return 1 + f() end return f() end) -- stack overflow is an ordinary error
  assert(so == false)
  flux.storage.set("done", "yes")
end"#,
    )]);
    let h = start_with(d.path(), Limits { slice_ms: 3000, ..limits() }, Arc::new(MemSecretStore::default()));
    let _ = h.next();
    h.run("ok:t");
    let end = Instant::now() + Duration::from_secs(5);
    while h.storage.local_get("ok", "done").is_none() {
        if let Some(RuntimeEvent::ExtensionError { message, .. }) = h.recv(Duration::from_millis(20)) {
            panic!("{message}");
        }
        assert!(Instant::now() < end, "timed out");
    }
}

#[test]
fn a_new_slice_starts_clean_after_a_limit_error() {
    // The guard is re-armed for every command: an earlier overrun must not poison later runs.
    let (_d, h) = setup("function main() while true do end end");
    for _ in 0..3 {
        h.run("evil:attack");
        assert!(h.next_error().contains("execution limit"));
        h.run("evil:ping");
        h.wait_local("evil", "alive");
        h.storage.local_remove("evil", "alive");
    }
}
