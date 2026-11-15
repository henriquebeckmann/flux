//! Builds the `flux` object for a command run.
//!
//! To add an API: create an `install_<ns>`-style function here (or put the
//! idiomatic part in `prelude.lua`), register it in [`build_flux`] and, if it
//! needs a permission, call `ctx.require(Capability::X)?` inside the function.

use std::{process::Stdio, rc::Rc, time::Duration};

use flux_api::{Capability, PrefValue, PreferenceKind, PreferenceSpec, RuntimeEvent};
use mlua::{Lua, LuaSerdeExt, Table, Value, serde::SerializeOptions};
use tokio::{io::AsyncWriteExt, process::Command};

use crate::ctx::CmdCtx;

type R<T> = mlua::Result<T>;

pub fn build_flux(lua: &Lua, ctx: &Rc<CmdCtx>, extend: &mlua::Function) -> R<Table> {
    let flux = lua.create_table()?;
    flux.set("ui", ui(lua, ctx)?)?;
    flux.set("action", lua.create_table()?)?;
    flux.set("clipboard", clipboard(lua, ctx)?)?;
    flux.set("storage", storage(lua, ctx)?)?;
    flux.set("process", process(lua, ctx)?)?;
    flux.set("logger", logger(lua, ctx)?)?;
    flux.set("browser", browser(lua, ctx)?)?;
    flux.set("environment", environment(lua, ctx)?)?;
    flux.set("navigation", navigation(lua, ctx)?)?;
    flux.set("preferences", preferences(lua, ctx)?)?;
    flux.set("lifecycle", lifecycle(lua, ctx)?)?;
    flux.set("http", crate::http::install(lua, ctx)?)?;
    flux.set("filesystem", filesystem(lua, ctx)?)?;
    flux.set("task", task(lua)?)?;
    flux.set("json", json(lua)?)?;
    // Stubs first: the Lua helpers added by `extend` capture the functions they wrap.
    crate::remote::install_stubs(lua, ctx, &flux)?;
    // `extend` hides the raw request function from extensions; the launcher's broker still needs it.
    if let Ok(f) = flux.get::<Table>("http").and_then(|h: Table| h.get::<mlua::Function>("_request")) {
        lua.set_named_registry_value("flux.http._request", f)?;
    }
    extend.call::<()>(flux.clone())?;
    Ok(flux)
}

fn ui(lua: &Lua, ctx: &Rc<CmdCtx>) -> R<Table> {
    let t = lua.create_table()?;
    let c = ctx.clone();
    t.set("close", lua.create_function(move |_, ()| {
        c.emit(RuntimeEvent::CloseLauncher);
        Ok(())
    })?)?;
    Ok(t)
}

/// Caps what one extension can push through the event channel: 200 lines per second, 4 KB each.
#[derive(Default)]
pub struct LogLimiter {
    window: std::cell::Cell<Option<std::time::Instant>>,
    count: std::cell::Cell<u32>,
    dropped: std::cell::Cell<u32>,
}

impl LogLimiter {
    const PER_SECOND: u32 = 200;
    const MAX_LEN: usize = 4096;

    /// `Some(extra)` when the line may be sent (with a note about dropped lines), `None` to drop it.
    fn admit(&self) -> Option<Option<String>> {
        let now = std::time::Instant::now();
        let fresh = self.window.get().is_none_or(|w| now.duration_since(w) >= Duration::from_secs(1));
        if fresh {
            self.window.set(Some(now));
            self.count.set(0);
            let dropped = self.dropped.replace(0);
            self.count.set(1);
            return Some((dropped > 0).then(|| format!("{dropped} log line(s) dropped (more than {} per second)", Self::PER_SECOND)));
        }
        if self.count.get() >= Self::PER_SECOND {
            self.dropped.set(self.dropped.get() + 1);
            return None;
        }
        self.count.set(self.count.get() + 1);
        Some(None)
    }
}

fn logger(lua: &Lua, ctx: &Rc<CmdCtx>) -> R<Table> {
    let t = lua.create_table()?;
    let c = ctx.clone();
    t.set("_log", lua.create_function(move |_, (level, msg): (String, String)| {
        let tag = c.tag();
        let Some(note) = c.log_limiter.admit() else { return Ok(()) };
        let msg: String = if msg.len() > LogLimiter::MAX_LEN { format!("{}… (truncated)", msg.chars().take(LogLimiter::MAX_LEN).collect::<String>()) } else { msg };
        let lvl = match level.as_str() {
            "debug" => flux_api::LogLevel::Debug,
            "warn" => flux_api::LogLevel::Warn,
            "error" => flux_api::LogLevel::Error,
            _ => flux_api::LogLevel::Info,
        };
        match lvl {
            flux_api::LogLevel::Debug => log::debug!("[{tag}] {msg}"),
            flux_api::LogLevel::Warn => log::warn!("[{tag}] {msg}"),
            flux_api::LogLevel::Error => log::error!("[{tag}] {msg}"),
            flux_api::LogLevel::Info => log::info!("[{tag}] {msg}"),
        }
        if let Some(n) = note {
            c.emit(RuntimeEvent::Log { extension: c.extension.clone(), level: flux_api::LogLevel::Warn, message: n });
        }
        c.emit(RuntimeEvent::Log { extension: c.extension.clone(), level: lvl, message: msg });
        Ok(())
    })?)?;
    Ok(t)
}

fn environment(lua: &Lua, ctx: &Rc<CmdCtx>) -> R<Table> {
    let t = lua.create_table()?;
    t.set("app_version", ctx.app_version.as_str())?;
    t.set("extension_id", ctx.extension.as_str())?;
    t.set("command_id", ctx.command.as_str())?;
    t.set("extension_dir", ctx.dir.to_string_lossy().as_ref())?;
    t.set("data_dir", ctx.data_dir.to_string_lossy().as_ref())?;
    t.set("os", std::env::consts::OS)?;
    Ok(t)
}

// ---------------------------------------------------------------- storage

fn storage(lua: &Lua, ctx: &Rc<CmdCtx>) -> R<Table> {
    let t = lua.create_table()?;

    // LocalStorage lives directly in `flux.storage` (`local` is a reserved word in Lua).
    let local = t.clone();
    let c = ctx.clone();
    local.set("get", lua.create_function(move |lua, key: String| {
        match c.storage.local_get(&c.extension, &key) {
            Some(json) => from_json(lua, &json),
            None => Ok(Value::Nil),
        }
    })?)?;
    let c = ctx.clone();
    local.set("set", lua.create_function(move |lua, (key, value): (String, Value)| {
        c.storage.local_set(&c.extension, &key, &to_json(lua, value)?);
        Ok(())
    })?)?;
    let c = ctx.clone();
    local.set("remove", lua.create_function(move |_, key: String| {
        c.storage.local_remove(&c.extension, &key);
        Ok(())
    })?)?;
    let c = ctx.clone();
    local.set("clear", lua.create_function(move |_, ()| {
        c.storage.local_clear(&c.extension);
        Ok(())
    })?)?;

    let cache = lua.create_table()?;
    let c = ctx.clone();
    cache.set("get", lua.create_function(move |lua, key: String| {
        match c.storage.cache_get(&c.extension, &key) {
            Some(json) => from_json(lua, &json),
            None => Ok(Value::Nil),
        }
    })?)?;
    let c = ctx.clone();
    cache.set("set", lua.create_function(move |lua, (key, value, ttl): (String, Value, f64)| {
        c.storage.cache_set(&c.extension, &key, &to_json(lua, value)?, ttl.max(0.0) as u64);
        Ok(())
    })?)?;
    t.set("cache", cache)?;
    t.set("secure", secure_storage(lua, ctx)?)?;
    Ok(t)
}

fn to_json(lua: &Lua, value: Value) -> R<String> {
    let v: serde_json::Value = lua.from_value(value)?;
    serde_json::to_string(&v).map_err(mlua::Error::external)
}

/// Serializes to Lua with JSON `null`/`None` becoming `nil` (the default is a truthy lightuserdata).
pub fn to_lua<T: serde::Serialize + ?Sized>(lua: &Lua, v: &T) -> R<Value> {
    lua.to_value_with(v, SerializeOptions::new().serialize_none_to_null(false).serialize_unit_to_null(false))
}

fn from_json(lua: &Lua, json: &str) -> R<Value> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(mlua::Error::external)?;
    to_lua(lua, &v)
}

// -------------------------------------------------------------- clipboard

/// `grants.exec`: the extension may only start the commands it listed (exact match).
fn allow_command(c: &CmdCtx, cmd: &str) -> R<()> {
    if c.permissions.command_allowed(cmd) {
        Ok(())
    } else {
        Err(mlua::Error::runtime(format!("flux.process: '{cmd}' is not one of the commands this extension may run (grants.exec)")))
    }
}

fn clipboard(lua: &Lua, ctx: &Rc<CmdCtx>) -> R<Table> {
    let t = lua.create_table()?;

    let c = ctx.clone();
    t.set("getText", lua.create_async_function(move |_, ()| {
        let c = c.clone();
        async move {
            c.require(Capability::ClipboardRead)?;
            let out = run_capture("wl-paste", &["--no-newline"]).await
                .or(run_capture("xclip", &["-o", "-selection", "clipboard"]).await);
            Ok(out.ok())
        }
    })?)?;

    let c = ctx.clone();
    t.set("setText", lua.create_async_function(move |_, text: String| {
        let c = c.clone();
        async move {
            c.require(Capability::ClipboardWrite)?;
            feed("wl-copy", &[], &text).await
                .or(feed("xclip", &["-selection", "clipboard"], &text).await)
                .map_err(mlua::Error::runtime)
        }
    })?)?;

    let c = ctx.clone();
    t.set("clear", lua.create_async_function(move |_, ()| {
        let c = c.clone();
        async move {
            c.require(Capability::ClipboardWrite)?;
            feed("wl-copy", &["--clear"], "").await.map_err(mlua::Error::runtime)
        }
    })?)?;

    for name in ["getImage", "setImage"] {
        t.set(name, lua.create_function(move |_, ()| -> R<()> {
            Err(mlua::Error::runtime(format!("flux.clipboard.{name} is not implemented yet")))
        })?)?;
    }
    Ok(t)
}

async fn run_capture(bin: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(bin).args(args).stdin(Stdio::null()).output().await.map_err(|e| format!("{bin}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("{bin} failed"))
    }
}

async fn feed(bin: &str, args: &[&str], input: &str) -> Result<(), String> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{bin}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes()).await.map_err(|e| e.to_string())?;
    }
    let status = child.wait().await.map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("{bin} failed")) }
}

// ---------------------------------------------------------------- process

/// Owns a running child. If the future that awaits it is dropped (cancelled command, closed
/// view, timeout) the child is killed *and reaped*; `kill_on_drop` alone leaves a zombie behind
/// until some later spawn happens.
struct ChildGuard(Option<tokio::process::Child>, Option<crate::resources::PidGuard>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.start_kill();
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
    }
}

/// Reads a pipe to its end but keeps at most `cap` bytes (the rest is read and dropped, so the
/// child never blocks on a full pipe). The flag says whether anything was dropped.
async fn read_all<R: tokio::io::AsyncRead + Unpin>(pipe: &mut Option<R>, cap: usize) -> (Vec<u8>, bool) {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let mut truncated = false;
    if let Some(p) = pipe {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match p.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = cap.saturating_sub(buf.len());
                    buf.extend_from_slice(&chunk[..n.min(room)]);
                    truncated |= n > room;
                }
            }
        }
    }
    (buf, truncated)
}

fn process(lua: &Lua, ctx: &Rc<CmdCtx>) -> R<Table> {
    let t = lua.create_table()?;

    // flux.process.exec(cmd, args?, { cwd?, timeout? }) -> { code, success, stdout, stderr }
    let c = ctx.clone();
    t.set("exec", lua.create_async_function(move |lua, (cmd, args, opts): (String, Option<Vec<String>>, Option<Table>)| {
        let c = c.clone();
        async move {
            c.require(Capability::ProcessExecute)?;
            allow_command(&c, &cmd)?;
            let mut timeout = 30.0_f64;
            let mut signal: Option<crate::http::Token> = None;
            let mut cwd: Option<String> = None;
            if let Some(o) = opts {
                cwd = o.get::<Option<String>>("cwd")?;
                if let Some(t) = o.get::<Option<f64>>("timeout")? { timeout = t.clamp(0.001, c.res.exec_timeout_max); }
                signal = o.get::<Option<mlua::UserDataRef<crate::http::Token>>>("signal")?.map(|t| t.clone());
            }
            let args = args.unwrap_or_default();
            let mut command = crate::execwrap::command_for(c.exec_wrapper.as_ref(), &c.permissions, &c.data_dir, &cmd, &args, cwd.as_deref())
                .map_err(|e| mlua::Error::runtime(format!("could not run '{cmd}': {e}")))?;
            command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
            let child = tokio::process::Command::from(command).spawn().map_err(|e| mlua::Error::runtime(format!("could not run '{cmd}': {e}")))?;
            let own = child.id().map(|pid| c.res.own_pid(pid));
            let mut guard = ChildGuard(Some(child), own);
            let run = async {
                let child = guard.0.as_mut().expect("child is present until completion");
                let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
                let cap = c.res.exec_output_cap;
                let (status, stdout, stderr) = tokio::join!(child.wait(), read_all(&mut out, cap), read_all(&mut err, cap));
                status.map(|s| (s, stdout, stderr))
            };
            let timed = tokio::time::timeout(Duration::from_secs_f64(timeout), run);
            let outcome = match signal {
                // Dropping `guard` (on the error path) kills and reaps the child.
                Some(token) => tokio::select! {
                    r = timed => r,
                    _ = token.cancelled() => return Err(mlua::Error::runtime(format!("'{cmd}' was cancelled"))),
                },
                None => timed.await,
            };
            let (status, stdout, stderr) = outcome
                .map_err(|_| mlua::Error::runtime(format!("'{cmd}' exceeded the {timeout}s timeout")))?
                .map_err(|e| mlua::Error::runtime(format!("could not run '{cmd}': {e}")))?;
            guard.0 = None; // finished normally: nothing to kill
            guard.1 = None;
            let ((stdout, out_cut), (stderr, err_cut)) = (stdout, stderr);
            let out = std::process::Output { status, stdout, stderr };
            let r = lua.create_table()?;
            r.set("code", out.status.code())?;
            r.set("success", out.status.success())?;
            r.set("stdout", lua.create_string(&out.stdout)?)?;
            r.set("stderr", lua.create_string(&out.stderr)?)?;
            r.set("truncated", out_cut || err_cut)?;
            Ok(r)
        }
    })?)?;

    // flux.process.spawn(cmd, args?) -> pid  (in the background, without waiting)
    let c = ctx.clone();
    t.set("spawn", lua.create_function(move |_, (cmd, args): (String, Option<Vec<String>>)| {
        c.require(Capability::ProcessExecute)?;
        allow_command(&c, &cmd)?;
        let permit = c.res.child().map_err(|e| mlua::Error::runtime(format!("flux.process.spawn: {e}")))?;
        let args = args.unwrap_or_default();
        let mut command = crate::execwrap::command_for(c.exec_wrapper.as_ref(), &c.permissions, &c.data_dir, &cmd, &args, None)
            .map_err(|e| mlua::Error::runtime(format!("could not run '{cmd}': {e}")))?;
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let child = tokio::process::Command::from(command).spawn().map_err(|e| mlua::Error::runtime(format!("could not run '{cmd}': {e}")))?;
        let pid = child.id().unwrap_or(0);
        let own = c.res.own_pid(pid);
        tokio::task::spawn_local(async move { let _permit = permit; let _own = own; let mut child = child; let _ = child.wait().await; });
        Ok(pid)
    })?)?;

    // flux.process.kill(pid, signal?) -> true | nil, reason. The kernel only lets the extension
    // signal processes of the same user; pid <= 1 and Flux itself are refused outright.
    let c = ctx.clone();
    t.set("kill", lua.create_function(move |_, (pid, signal): (i64, Option<Value>)| {
        c.require(Capability::ProcessKill)?;
        if pid <= 1 || pid > i32::MAX as i64 {
            return Err(mlua::Error::runtime(format!("flux.process.kill: refusing pid {pid}")));
        }
        let own = c.res.owns(pid as u32);
        if pid as u32 == std::process::id() || (!own && crate::execwrap::is_flux_process(pid as u32)) {
            return Err(mlua::Error::runtime("flux.process.kill: an extension cannot signal Flux's own processes"));
        }
        if !own && !c.permissions.grants.kill_any {
            return Err(mlua::Error::runtime(format!(
                "flux.process.kill: pid {pid} was not started by this extension (grants.kill = \"user\" would let it signal other processes of yours)"
            )));
        }
        let sig = match signal {
            None | Some(Value::Nil) => libc::SIGTERM,
            Some(Value::Integer(n)) if (1..=64).contains(&n) => n as i32,
            Some(Value::String(s)) => match s.to_string_lossy().trim_start_matches("SIG").to_uppercase().as_str() {
                "TERM" => libc::SIGTERM,
                "KILL" => libc::SIGKILL,
                "INT" => libc::SIGINT,
                "HUP" => libc::SIGHUP,
                "QUIT" => libc::SIGQUIT,
                "STOP" => libc::SIGSTOP,
                "CONT" => libc::SIGCONT,
                "USR1" => libc::SIGUSR1,
                "USR2" => libc::SIGUSR2,
                other => return Err(mlua::Error::runtime(format!("flux.process.kill: unknown signal '{other}'"))),
            },
            Some(other) => return Err(mlua::Error::runtime(format!("flux.process.kill: signal must be a name or number, got {}", other.type_name()))),
        };
        // SAFETY: plain syscall with validated arguments.
        let r = unsafe { libc::kill(pid as i32, sig) };
        if r == 0 { Ok((Some(true), None)) } else { Ok((None, Some(std::io::Error::last_os_error().to_string()))) }
    })?)?;

    for name in ["getOutput", "getRunningProcesses"] {
        t.set(name, lua.create_function(move |_, ()| -> R<()> {
            Err(mlua::Error::runtime(format!("flux.process.{name} is not implemented yet")))
        })?)?;
    }
    Ok(t)
}

// ---------------------------------------------------------------- browser

fn browser(lua: &Lua, ctx: &Rc<CmdCtx>) -> R<Table> {
    let t = lua.create_table()?;
    let c = ctx.clone();
    t.set("open", lua.create_function(move |_, url: String| {
        c.require(Capability::BrowserOpen)?;
        if !(url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:")) {
            return Err(mlua::Error::runtime("flux.browser.open only accepts http://, https:// and mailto:"));
        }
        let child = Command::new("xdg-open")
            .arg(&url)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn()
            .map_err(|e| mlua::Error::runtime(format!("xdg-open: {e}")))?;
        tokio::task::spawn_local(async move { let mut child = child; let _ = child.wait().await; });
        Ok(())
    })?)?;
    Ok(t)
}

// -------------------------------------------------------------- navigation

/// `flux.navigation.push/replace/pop`: the runtime keeps the extension's screen stack and
/// tells the core what changed; the UI never decides navigation on its own.
fn navigation(lua: &Lua, ctx: &Rc<CmdCtx>) -> R<Table> {
    let t = lua.create_table()?;
    let c = ctx.clone();
    t.set("push", lua.create_function(move |_, view: Value| {
        c.views()?.push(&c.tag(), view).map_err(mlua::Error::runtime)
    })?)?;
    let c = ctx.clone();
    t.set("replace", lua.create_function(move |_, view: Value| {
        c.views()?.replace(&c.tag(), view).map_err(mlua::Error::runtime)
    })?)?;
    let c = ctx.clone();
    t.set("pop", lua.create_function(move |_, ()| {
        c.views()?.pop();
        Ok(())
    })?)?;
    Ok(t)
}

// -------------------------------------------------------------- lifecycle

fn lifecycle(lua: &Lua, ctx: &Rc<CmdCtx>) -> R<Table> {
    let t = lua.create_table()?;
    let c = ctx.clone();
    // Runs (with a 1 s budget) when the extension is reloaded or the app quits.
    t.set("on_unload", lua.create_function(move |_, f: mlua::Function| {
        c.on_unload.borrow_mut().push(f);
        Ok(())
    })?)?;
    Ok(t)
}

// ------------------------------------------------------------ preferences

fn secret_err(e: flux_api::SecretError) -> mlua::Error {
    mlua::Error::runtime(format!("secure storage unavailable: {e}"))
}

fn pref_key(name: &str) -> String {
    format!("pref:{name}")
}

fn find_spec<'a>(ctx: &'a CmdCtx, name: &str) -> R<&'a PreferenceSpec> {
    ctx.prefs.iter().find(|p| p.name == name).ok_or_else(|| {
        mlua::Error::runtime(format!("unknown preference '{name}' (declare it in preferences.lua)"))
    })
}

/// Current value of a preference (default applied). Password values come from the secret store.
pub async fn pref_value(ctx: &CmdCtx, spec: &PreferenceSpec) -> R<Option<PrefValue>> {
    if spec.kind == PreferenceKind::Password {
        let v = ctx.secrets.get(&ctx.extension, &pref_key(&spec.name)).await.map_err(secret_err)?;
        return Ok(v.map(PrefValue::Text));
    }
    let stored = ctx.storage.pref_get(&ctx.extension, &spec.name).and_then(|j| serde_json::from_str::<PrefValue>(&j).ok());
    Ok(stored.or_else(|| spec.default.clone()))
}

/// Names of required preferences that have no value yet.
pub async fn missing_required(ctx: &CmdCtx) -> R<Vec<String>> {
    let mut missing = Vec::new();
    for spec in ctx.prefs.iter().filter(|p| p.required) {
        match pref_value(ctx, spec).await? {
            None => missing.push(spec.title.clone()),
            Some(PrefValue::Text(t)) if t.is_empty() => missing.push(spec.title.clone()),
            _ => {}
        }
    }
    Ok(missing)
}

fn preferences(lua: &Lua, ctx: &Rc<CmdCtx>) -> R<Table> {
    let t = lua.create_table()?;

    let c = ctx.clone();
    t.set("get", lua.create_async_function(move |lua, name: String| {
        let c = c.clone();
        async move {
            let spec = find_spec(&c, &name)?;
            match pref_value(&c, spec).await? {
                Some(v) => to_lua(&lua, &v),
                None => Ok(Value::Nil),
            }
        }
    })?)?;

    let c = ctx.clone();
    t.set("getAll", lua.create_async_function(move |lua, ()| {
        let c = c.clone();
        async move {
            let out = lua.create_table()?;
            for spec in c.prefs.iter() {
                if let Some(v) = pref_value(&c, spec).await? {
                    out.set(spec.name.as_str(), to_lua(&lua, &v)?)?;
                }
            }
            Ok(out)
        }
    })?)?;

    let c = ctx.clone();
    t.set("set", lua.create_async_function(move |lua, (name, value): (String, Value)| {
        let c = c.clone();
        async move {
            let spec = find_spec(&c, &name)?;
            let v: PrefValue = lua.from_value(value).map_err(|_| mlua::Error::runtime(format!("preference '{name}': expected a string or boolean")))?;
            match (&spec.kind, &v) {
                (PreferenceKind::Checkbox, PrefValue::Bool(_)) => {}
                (PreferenceKind::Dropdown, PrefValue::Text(s)) if spec.options.iter().any(|o| &o.value == s) => {}
                (PreferenceKind::Dropdown, _) => return Err(mlua::Error::runtime(format!("preference '{name}': value must be one of the declared options"))),
                (PreferenceKind::Text | PreferenceKind::Password, PrefValue::Text(_)) => {}
                (PreferenceKind::Checkbox, _) => return Err(mlua::Error::runtime(format!("preference '{name}' is a checkbox: expected a boolean"))),
                _ => return Err(mlua::Error::runtime(format!("preference '{name}': expected a string"))),
            }
            if spec.kind == PreferenceKind::Password {
                let PrefValue::Text(text) = &v else { unreachable!() };
                c.secrets.set(&c.extension, &pref_key(&name), text).await.map_err(secret_err)?;
            } else {
                c.storage.pref_set(&c.extension, &name, &serde_json::to_string(&v).map_err(mlua::Error::external)?);
            }
            Ok(())
        }
    })?)?;

    // Declared preferences, used by the built-in "Configure" command.
    let c = ctx.clone();
    t.set("spec", lua.create_function(move |lua, ()| {
        let list: Vec<serde_json::Value> = c.prefs.iter().map(|p| serde_json::json!({
            "name": p.name,
            "title": p.title,
            "description": p.description,
            "type": match p.kind {
                PreferenceKind::Text => "text",
                PreferenceKind::Password => "password",
                PreferenceKind::Checkbox => "checkbox",
                PreferenceKind::Dropdown => "dropdown",
            },
            "required": p.required,
            "options": p.options.iter().map(|o| serde_json::json!({"title": o.title, "value": o.value})).collect::<Vec<_>>(),
        })).collect();
        to_lua(lua, &list)
    })?)?;
    Ok(t)
}

fn secure_storage(lua: &Lua, ctx: &Rc<CmdCtx>) -> R<Table> {
    let t = lua.create_table()?;
    let key = |k: &str| format!("secure:{k}");

    let c = ctx.clone();
    t.set("get", lua.create_async_function(move |_, k: String| {
        let c = c.clone();
        async move {
            c.require(Capability::CredentialsRead)?;
            c.secrets.get(&c.extension, &key(&k)).await.map_err(secret_err)
        }
    })?)?;
    let c = ctx.clone();
    t.set("set", lua.create_async_function(move |_, (k, v): (String, String)| {
        let c = c.clone();
        async move {
            c.require(Capability::CredentialsWrite)?;
            c.secrets.set(&c.extension, &key(&k), &v).await.map_err(secret_err)
        }
    })?)?;
    let c = ctx.clone();
    t.set("remove", lua.create_async_function(move |_, k: String| {
        let c = c.clone();
        async move {
            c.require(Capability::CredentialsWrite)?;
            c.secrets.remove(&c.extension, &key(&k)).await.map_err(secret_err)
        }
    })?)?;
    Ok(t)
}

// ------------------------------------------------------------------- task

/// `flux.task.token()`: a cancellation token accepted as `signal` by `flux.http` and `flux.process.exec`.
fn task(lua: &Lua) -> R<Table> {
    let t = lua.create_table()?;
    t.set("token", lua.create_function(|lua, ()| lua.create_userdata(crate::http::Token::default()))?)?;
    // flux.task.sleep(seconds): waits without blocking anything (debouncing, polling loops).
    // The wait is cancelled with the task that made it.
    t.set("sleep", lua.create_async_function(|_, secs: f64| async move {
        if !secs.is_finite() || !(0.0..=3600.0).contains(&secs) {
            return Err(mlua::Error::runtime("flux.task.sleep: seconds must be between 0 and 3600"));
        }
        tokio::time::sleep(Duration::from_secs_f64(secs)).await;
        Ok(())
    })?)?;
    Ok(t)
}

// ------------------------------------------------------------------- json

fn json(lua: &Lua) -> R<Table> {
    let t = lua.create_table()?;
    t.set("encode", lua.create_function(|lua, (value, pretty): (Value, Option<bool>)| {
        let v: serde_json::Value = lua.from_value(value)?;
        if pretty.unwrap_or(false) { serde_json::to_string_pretty(&v) } else { serde_json::to_string(&v) }.map_err(mlua::Error::external)
    })?)?;
    t.set("decode", lua.create_function(|lua, s: mlua::LuaString| {
        let v: serde_json::Value = serde_json::from_slice(&s.as_bytes()).map_err(|e| mlua::Error::runtime(format!("invalid JSON: {e}")))?;
        to_lua(lua, &v)
    })?)?;
    Ok(t)
}

// ------------------------------------------------------------- filesystem

/// Read-only file access (`filesystem` permission). Flux's own data (database, other extensions'
/// data, logs, config) and common credential directories are refused even though the extension
/// holds the permission: a granted capability is not a licence to read other extensions' secrets.
/// (Extensions that also hold `process` can still run `cat`; see docs/security.md.)
pub(crate) mod fs {
    use std::{
        path::{Component, Path, PathBuf},
        time::UNIX_EPOCH,
    };

    use mlua::{Lua, Table};

    use crate::ctx::CmdCtx;

    pub fn home() -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }

    fn lexical(p: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for c in p.components() {
            match c {
                Component::ParentDir => {
                    out.pop();
                }
                Component::CurDir => {}
                other => out.push(other.as_os_str()),
            }
        }
        out
    }

    /// Canonical form even for paths that do not exist yet (symlinks of the existing prefix resolved).
    fn resolve(p: &Path) -> PathBuf {
        let mut existing = p.to_path_buf();
        let mut rest = Vec::new();
        while !existing.exists() {
            match existing.file_name() {
                Some(n) => rest.push(n.to_owned()),
                None => break,
            }
            if !existing.pop() {
                break;
            }
        }
        let mut base = std::fs::canonicalize(&existing).unwrap_or(existing);
        for n in rest.into_iter().rev() {
            base.push(n);
        }
        base
    }

    fn protected(ctx: &CmdCtx) -> Vec<PathBuf> {
        let mut v = Vec::new();
        if let Some(root) = ctx.data_dir.parent() {
            v.push(root.to_path_buf()); // every extension's data dir
            if let Some(app) = root.parent() {
                for f in ["flux.db", "flux.db-wal", "flux.db-shm"] {
                    v.push(app.join(f));
                }
            }
        }
        if let Some(h) = home() {
            for rel in [".config/flux", ".local/state/flux", ".ssh", ".gnupg", ".aws", ".kube", ".local/share/keyrings"] {
                v.push(h.join(rel));
            }
        }
        if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
            v.push(PathBuf::from(rt).join("flux.sock"));
        }
        v.into_iter().map(|p| resolve(&p)).collect()
    }

    /// Validates and resolves a path given by an extension for reading.
    pub fn check(ctx: &CmdCtx, raw: &str) -> mlua::Result<PathBuf> {
        check_for(ctx, raw, false)
    }

    /// Like [`check`] for writing: only the extension's own data directory and `grants.fs_write`.
    pub fn check_write(ctx: &CmdCtx, raw: &str) -> mlua::Result<PathBuf> {
        check_for(ctx, raw, true)
    }

    fn expand(raw: &str) -> Option<PathBuf> {
        match raw.strip_prefix("~/").map(Path::new).or_else(|| (raw == "~").then(|| Path::new(""))) {
            Some(rel) => home().map(|h| h.join(rel)),
            None => Some(PathBuf::from(raw)),
        }
    }

    fn check_for(ctx: &CmdCtx, raw: &str, write: bool) -> mlua::Result<PathBuf> {
        let expanded = match raw.strip_prefix("~/").map(Path::new).or_else(|| (raw == "~").then(|| Path::new(""))) {
            Some(rel) => home().ok_or_else(|| mlua::Error::runtime("flux.filesystem: HOME is not set"))?.join(rel),
            None => PathBuf::from(raw),
        };
        if !expanded.is_absolute() {
            return Err(mlua::Error::runtime(format!("flux.filesystem: '{raw}' is not absolute (use an absolute path or ~/…)")));
        }
        // Decide on the fully resolved path (symlinks followed), but operate on the lexical one so
        // that `stat` can still report a symlink as a symlink.
        let path = lexical(&expanded);
        let resolved = resolve(&path);
        let own = resolve(&ctx.data_dir);
        let in_own = resolved.starts_with(&own);
        if !in_own && protected(ctx).iter().any(|p| resolved.starts_with(p)) {
            return Err(mlua::Error::runtime(format!("flux.filesystem: '{raw}' is protected (Flux data or credentials)")));
        }
        // Grants narrow what the capability covers. The extension's own directories always stay reachable.
        let granted = if write { &ctx.permissions.grants.fs_write } else { &ctx.permissions.grants.fs_read };
        let restricted = write || granted.is_some();
        if restricted && !in_own && !(!write && resolved.starts_with(resolve(&ctx.dir))) {
            let inside = granted.iter().flatten().filter_map(|d| expand(d)).any(|d| resolved.starts_with(resolve(&d)));
            if !inside {
                let what = if write { "writing" } else { "reading" };
                return Err(mlua::Error::runtime(format!("flux.filesystem: '{raw}' is outside the directories this extension may use for {what} (grants.{})", if write { "fs_write" } else { "fs_read" })));
            }
        }
        Ok(path)
    }

    /// Image paths from views use the same rules as `flux.filesystem` reads.
    pub fn check_image(permissions: &flux_api::PermissionSet, ext_dir: &Path, data_dir: &Path, raw: &str) -> Result<(), String> {
        let path = lexical(Path::new(raw));
        let resolved = resolve(&path);
        let (own, dir) = (resolve(data_dir), resolve(ext_dir));
        if resolved.starts_with(&own) || resolved.starts_with(&dir) {
            return Ok(());
        }
        let mut protected = Vec::new();
        if let Some(root) = data_dir.parent() {
            protected.push(root.to_path_buf());
        }
        if let Some(h) = home() {
            for rel in [".config/flux", ".local/state/flux", ".ssh", ".gnupg", ".aws", ".kube", ".local/share/keyrings"] {
                protected.push(h.join(rel));
            }
        }
        if protected.into_iter().map(|p| resolve(&p)).any(|p| resolved.starts_with(p)) {
            return Err("that path is protected (Flux data or credentials)".into());
        }
        if let Some(dirs) = &permissions.grants.fs_read {
            if !dirs.iter().filter_map(|d| expand(d)).any(|d| resolved.starts_with(resolve(&d))) {
                return Err("that path is outside the directories this extension may read (grants.fs_read)".into());
            }
        }
        Ok(())
    }

    pub fn entry(lua: &Lua, path: &Path, meta: &std::fs::Metadata, lmeta: &std::fs::Metadata) -> mlua::Result<Table> {
        let t = lua.create_table()?;
        t.set("name", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())?;
        t.set("path", path.to_string_lossy().as_ref())?;
        t.set("is_dir", meta.is_dir())?;
        t.set("is_file", meta.is_file())?;
        t.set("is_symlink", lmeta.file_type().is_symlink())?;
        t.set("size", meta.len())?;
        t.set("modified", meta.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()))?;
        t.set("readonly", meta.permissions().readonly())?;
        Ok(t)
    }
}

fn filesystem(lua: &Lua, ctx: &Rc<CmdCtx>) -> R<Table> {
    use std::io::Read;
    let t = lua.create_table()?;
    const MAX_ENTRIES: usize = 20_000;

    let c = ctx.clone();
    t.set("home", lua.create_function(move |_, ()| {
        c.require(Capability::FilesystemRead)?;
        Ok(fs::home().map(|h| h.to_string_lossy().into_owned()))
    })?)?;

    // list(path, { hidden = false }) -> entries (directories first), or nil, reason
    let c = ctx.clone();
    t.set("list", lua.create_function(move |lua, (path, opts): (String, Option<Table>)| {
        c.require(Capability::FilesystemRead)?;
        let dir = fs::check(&c, &path)?;
        let hidden = opts.map(|o| o.get::<Option<bool>>("hidden")).transpose()?.flatten().unwrap_or(false);
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => return Ok((Value::Nil, Value::String(lua.create_string(e.to_string())?))),
        };
        let mut rows: Vec<(bool, String, Table)> = Vec::new();
        for entry in rd.flatten().take(MAX_ENTRIES) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !hidden && name.starts_with('.') {
                continue;
            }
            let p = entry.path();
            let Ok(lmeta) = std::fs::symlink_metadata(&p) else { continue };
            let meta = std::fs::metadata(&p).unwrap_or_else(|_| lmeta.clone()); // dangling links show as themselves
            rows.push((!meta.is_dir(), name.to_lowercase(), fs::entry(lua, &p, &meta, &lmeta)?));
        }
        rows.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        let out = lua.create_table()?;
        for (i, (_, _, e)) in rows.into_iter().enumerate() {
            out.set(i + 1, e)?;
        }
        Ok((Value::Table(out), Value::Nil))
    })?)?;

    let c = ctx.clone();
    t.set("stat", lua.create_function(move |lua, path: String| {
        c.require(Capability::FilesystemRead)?;
        let p = fs::check(&c, &path)?;
        match (std::fs::metadata(&p), std::fs::symlink_metadata(&p)) {
            (Ok(m), Ok(l)) => Ok((Value::Table(fs::entry(lua, &p, &m, &l)?), Value::Nil)),
            (Err(e), _) | (_, Err(e)) => Ok((Value::Nil, Value::String(lua.create_string(e.to_string())?))),
        }
    })?)?;

    let c = ctx.clone();
    t.set("exists", lua.create_function(move |_, path: String| {
        c.require(Capability::FilesystemRead)?;
        Ok(fs::check(&c, &path)?.exists())
    })?)?;

    // read_text(path, { max_bytes = 65536 }) -> text, truncated   |  nil, reason
    let c = ctx.clone();
    t.set("read_text", lua.create_function(move |lua, (path, opts): (String, Option<Table>)| {
        c.require(Capability::FilesystemRead)?;
        let p = fs::check(&c, &path)?;
        let max = opts.map(|o| o.get::<Option<usize>>("max_bytes")).transpose()?.flatten().unwrap_or(64 * 1024).min(8 * 1024 * 1024);
        let file = match std::fs::File::open(&p) {
            Ok(f) => f,
            Err(e) => return Ok((Value::Nil, Value::String(lua.create_string(e.to_string())?))),
        };
        let mut buf = Vec::new();
        if let Err(e) = file.take(max as u64 + 1).read_to_end(&mut buf) {
            return Ok((Value::Nil, Value::String(lua.create_string(e.to_string())?)));
        }
        let truncated = buf.len() > max;
        buf.truncate(max);
        Ok((Value::String(lua.create_string(String::from_utf8_lossy(&buf).as_bytes())?), Value::Boolean(truncated)))
    })?)?;

    // write_text(path, text, { append = false }) -> true | nil, reason. Only the extension's data
    // directory and `grants.fs_write` are writable; the file must be at most 8 MB.
    let c = ctx.clone();
    t.set("write_text", lua.create_function(move |lua, (path, text, opts): (String, mlua::LuaString, Option<Table>)| {
        c.require(Capability::FilesystemWrite)?;
        let p = fs::check_write(&c, &path)?;
        if text.as_bytes().len() > 8 * 1024 * 1024 {
            return Err(mlua::Error::runtime("flux.filesystem.write_text: the text is larger than 8 MB"));
        }
        let append = opts.map(|o| o.get::<Option<bool>>("append")).transpose()?.flatten().unwrap_or(false);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let result = std::fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(&p).and_then(|mut f| {
            use std::io::Write;
            f.write_all(&text.as_bytes())
        });
        match result {
            Ok(()) => Ok((Value::Boolean(true), Value::Nil)),
            Err(e) => Ok((Value::Nil, Value::String(lua.create_string(e.to_string())?))),
        }
    })?)?;

    // open(path): hand the file or folder to the desktop (xdg-open)
    let c = ctx.clone();
    t.set("open", lua.create_function(move |_, path: String| {
        c.require(Capability::FilesystemRead)?;
        let p = fs::check(&c, &path)?;
        let child = Command::new("xdg-open")
            .arg(&p)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn()
            .map_err(|e| mlua::Error::runtime(format!("xdg-open: {e}")))?;
        tokio::task::spawn_local(async move { let mut child = child; let _ = child.wait().await; });
        Ok(())
    })?)?;
    Ok(t)
}
