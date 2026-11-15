//! The host side of the link to the launcher: calls that cross the process boundary.
//!
//! Storage and secrets are plain requests. Privileged `flux.*` functions (processes, clipboard,
//! browser, files outside the extension's own folders) are replaced by stubs that check the
//! permission (first layer), then ask the launcher to run the real function (second layer, in
//! `broker.rs`, with a policy the host cannot change).

use std::{
    collections::HashMap,
    os::unix::net::UnixStream,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use flux_api::{
    BoxFuture, Capability, SecretError, SecretStore, Storage,
    wire::{self, FromHost, HostCall},
};
use mlua::{Lua, MultiValue, Table, Value};

use crate::ctx::CmdCtx;

enum Waiter {
    Async(tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>),
    Sync(std::sync::mpsc::SyncSender<Result<serde_json::Value, String>>),
}

/// Outgoing half of the connection plus the table of calls waiting for an answer.
pub struct Link {
    writer: Mutex<UnixStream>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, Waiter>>,
}

impl Link {
    pub fn new(writer: UnixStream) -> Arc<Self> {
        Arc::new(Self { writer: Mutex::new(writer), next_id: AtomicU64::new(1), pending: Mutex::new(HashMap::new()) })
    }

    pub fn send(&self, corr: Option<u64>, body: &FromHost) -> std::io::Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut w = self.writer.lock().unwrap();
        wire::write_frame(&mut *w, id, corr, body)
    }

    /// Routes an answer from the launcher to whoever is waiting for it.
    pub fn complete(&self, corr: u64, result: Result<serde_json::Value, String>) {
        match self.pending.lock().unwrap().remove(&corr) {
            Some(Waiter::Async(tx)) => {
                let _ = tx.send(result);
            }
            Some(Waiter::Sync(tx)) => {
                let _ = tx.send(result);
            }
            None => log::debug!("answer {corr} arrived after its call was abandoned"),
        }
    }

    /// The connection is gone: every waiting call fails instead of hanging.
    pub fn fail_all(&self, why: &str) {
        for (_, w) in self.pending.lock().unwrap().drain() {
            match w {
                Waiter::Async(tx) => {
                    let _ = tx.send(Err(why.to_string()));
                }
                Waiter::Sync(tx) => {
                    let _ = tx.send(Err(why.to_string()));
                }
            }
        }
    }

    fn register(&self, w: Waiter) -> std::io::Result<u64> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.pending.lock().unwrap().insert(id, w);
        Ok(id)
    }

    fn send_call(&self, id: u64, call: HostCall) -> Result<(), String> {
        let mut w = self.writer.lock().unwrap();
        // The call's own `id` doubles as its correlation key: replies carry it back as `corr`.
        wire::write_frame(&mut *w, id, None, &FromHost::Call { call }).map_err(|e| {
            self.pending.lock().unwrap().remove(&id);
            format!("the launcher is not reachable: {e}")
        })
    }

    /// Blocking call (storage). Used from the worker thread, never from the reader.
    pub fn call_blocking(&self, call: HostCall) -> Result<serde_json::Value, String> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let id = self.register(Waiter::Sync(tx)).map_err(|e| e.to_string())?;
        self.send_call(id, call)?;
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(r) => r,
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err("the launcher did not answer in time".into())
            }
        }
    }

    /// Async call. Dropping the future (the task was cancelled) tells the launcher to abort it.
    pub async fn call(self: &Arc<Self>, call: HostCall) -> Result<serde_json::Value, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let id = self.register(Waiter::Async(tx)).map_err(|e| e.to_string())?;
        self.send_call(id, call)?;
        let mut guard = CancelOnDrop { link: self.clone(), id, done: false };
        let r = rx.await.unwrap_or_else(|_| Err("the launcher is not reachable".into()));
        guard.done = true;
        r
    }
}

struct CancelOnDrop {
    link: Arc<Link>,
    id: u64,
    done: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if !self.done {
            self.link.pending.lock().unwrap().remove(&self.id);
            let _ = self.link.send(None, &FromHost::CancelCall { call: self.id });
        }
    }
}

// ---------------------------------------------------------------- storage and secrets

fn json_string(v: serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s),
        _ => None,
    }
}

pub struct RemoteStorage(pub Arc<Link>);

impl RemoteStorage {
    fn get(&self, call: HostCall) -> Option<String> {
        self.0.call_blocking(call).ok().and_then(json_string)
    }
    fn put(&self, call: HostCall) {
        if let Err(e) = self.0.call_blocking(call) {
            log::warn!("storage write failed: {e}");
        }
    }
}

impl Storage for RemoteStorage {
    fn local_get(&self, _: &str, key: &str) -> Option<String> {
        self.get(HostCall::LocalGet { key: key.into() })
    }
    fn local_set(&self, _: &str, key: &str, json: &str) {
        self.put(HostCall::LocalSet { key: key.into(), json: json.into() })
    }
    fn local_remove(&self, _: &str, key: &str) {
        self.put(HostCall::LocalRemove { key: key.into() })
    }
    fn local_clear(&self, _: &str) {
        self.put(HostCall::LocalClear)
    }
    fn cache_get(&self, _: &str, key: &str) -> Option<String> {
        self.get(HostCall::CacheGet { key: key.into() })
    }
    fn cache_set(&self, _: &str, key: &str, json: &str, ttl_secs: u64) {
        self.put(HostCall::CacheSet { key: key.into(), json: json.into(), ttl_secs })
    }
    fn pref_get(&self, _: &str, name: &str) -> Option<String> {
        self.get(HostCall::PrefGet { name: name.into() })
    }
    fn pref_set(&self, _: &str, name: &str, json: &str) {
        self.put(HostCall::PrefSet { name: name.into(), json: json.into() })
    }
}

pub struct RemoteSecrets(pub Arc<Link>);

impl SecretStore for RemoteSecrets {
    fn get<'a>(&'a self, _: &'a str, key: &'a str) -> BoxFuture<'a, Result<Option<String>, SecretError>> {
        Box::pin(async move { self.0.call(HostCall::SecretGet { key: key.into() }).await.map(json_string).map_err(SecretError) })
    }
    fn set<'a>(&'a self, _: &'a str, key: &'a str, value: &'a str) -> BoxFuture<'a, Result<(), SecretError>> {
        Box::pin(async move { self.0.call(HostCall::SecretSet { key: key.into(), value: value.into() }).await.map(|_| ()).map_err(SecretError) })
    }
    fn remove<'a>(&'a self, _: &'a str, key: &'a str) -> BoxFuture<'a, Result<(), SecretError>> {
        Box::pin(async move { self.0.call(HostCall::SecretRemove { key: key.into() }).await.map(|_| ()).map_err(SecretError) })
    }
}

// ---------------------------------------------------------------- privileged stubs

/// `flux.<namespace>.<function>` functions that only exist in the launcher, and the capability
/// each one needs. Anything not listed here cannot be requested through the broker.
pub const BROKERED: &[(&str, &str, Capability)] = &[
    ("process", "exec", Capability::ProcessExecute),
    ("process", "spawn", Capability::ProcessExecute),
    ("process", "kill", Capability::ProcessKill),
    ("clipboard", "getText", Capability::ClipboardRead),
    ("clipboard", "setText", Capability::ClipboardWrite),
    ("clipboard", "clear", Capability::ClipboardWrite),
    ("browser", "open", Capability::BrowserOpen),
    ("filesystem", "home", Capability::FilesystemRead),
    ("filesystem", "list", Capability::FilesystemRead),
    ("filesystem", "stat", Capability::FilesystemRead),
    ("filesystem", "exists", Capability::FilesystemRead),
    ("filesystem", "read_text", Capability::FilesystemRead),
    ("filesystem", "write_text", Capability::FilesystemWrite),
    ("filesystem", "open", Capability::FilesystemRead),
    // The launcher makes every request, so the host needs no network access at all.
    ("http", "_request", Capability::Network),
];

/// Lua value -> JSON. Strings that are not UTF-8 are converted lossily; functions, userdata and
/// other things that cannot cross the boundary become `null`.
pub fn lua_to_json(v: &Value, depth: u32) -> serde_json::Value {
    use serde_json::Value as J;
    if depth > 16 {
        return J::Null;
    }
    match v {
        Value::Nil => J::Null,
        Value::Boolean(b) => J::Bool(*b),
        Value::Integer(i) => J::from(*i),
        Value::Number(n) => serde_json::Number::from_f64(*n).map(J::Number).unwrap_or(J::Null),
        Value::String(s) => match s.to_str() {
            Ok(text) => J::String(text.to_string()),
            // Binary data (an image, a gzip body) travels as {"$b64": "…"} so nothing is lost.
            Err(_) => {
                use base64::Engine as _;
                let mut m = serde_json::Map::new();
                m.insert("$b64".into(), J::String(base64::engine::general_purpose::STANDARD.encode(&*s.as_bytes())));
                J::Object(m)
            }
        },
        Value::Table(t) => {
            let len = t.raw_len();
            let pairs = t.pairs::<Value, Value>().flatten().count();
            if len > 0 && pairs == len {
                J::Array((1..=len).map(|i| t.raw_get::<Value>(i).map(|v| lua_to_json(&v, depth + 1)).unwrap_or(J::Null)).collect())
            } else {
                let mut m = serde_json::Map::new();
                for (k, v) in t.pairs::<Value, Value>().flatten() {
                    if let Value::String(k) = k {
                        m.insert(k.to_string_lossy().to_string(), lua_to_json(&v, depth + 1));
                    }
                }
                J::Object(m)
            }
        }
        _ => J::Null,
    }
}

/// JSON -> Lua, the inverse of [`lua_to_json`] (`{"$b64": …}` becomes a binary string again).
pub fn json_to_lua(lua: &Lua, v: &serde_json::Value) -> mlua::Result<Value> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => Value::Nil,
        J::Bool(b) => Value::Boolean(*b),
        J::Number(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Number(n.as_f64().unwrap_or(0.0)),
        },
        J::String(s) => Value::String(lua.create_string(s)?),
        J::Array(items) => {
            let t = lua.create_table()?;
            for (i, item) in items.iter().enumerate() {
                t.raw_set(i + 1, json_to_lua(lua, item)?)?;
            }
            Value::Table(t)
        }
        J::Object(m) => {
            if m.len() == 1 {
                if let Some(J::String(b64)) = m.get("$b64") {
                    use base64::Engine as _;
                    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| mlua::Error::runtime(format!("bad binary value: {e}")))?;
                    return Ok(Value::String(lua.create_string(&bytes)?));
                }
            }
            let t = lua.create_table()?;
            for (k, item) in m {
                t.raw_set(k.as_str(), json_to_lua(lua, item)?)?;
            }
            Value::Table(t)
        }
    })
}

/// Replaces the brokered functions of `flux` with stubs that go through the link.
pub fn install_stubs(lua: &Lua, ctx: &Rc<CmdCtx>, flux: &Table) -> mlua::Result<()> {
    let Some(link) = ctx.link.clone() else { return Ok(()) };
    for (ns, func, cap) in BROKERED {
        let Ok(table) = flux.get::<Table>(*ns) else { continue };
        let (ctx, link) = (ctx.clone(), link.clone());
        let (namespace, function, cap) = (ns.to_string(), func.to_string(), *cap);
        let stub = lua.create_async_function(move |lua, args: MultiValue| {
            let (ctx, link, namespace, function) = (ctx.clone(), link.clone(), namespace.clone(), function.clone());
            async move {
                ctx.require(cap)?;
                let mut json = Vec::new();
                let mut token: Option<crate::http::Token> = None;
                for a in args.iter() {
                    if let Value::Table(t) = a {
                        if let Ok(Some(tok)) = t.get::<Option<mlua::UserDataRef<crate::http::Token>>>("signal") {
                            token = Some(tok.clone());
                        }
                    }
                    json.push(lua_to_json(a, 0));
                }
                if namespace == "process" && function != "kill" {
                    if let Some(serde_json::Value::String(cmd)) = json.first() {
                        if !ctx.permissions.command_allowed(cmd) {
                            return Err(mlua::Error::runtime(format!("flux.process: '{cmd}' is not one of the commands this extension may run (grants.exec)")));
                        }
                    }
                }
                let namespace_is_http = namespace == "http";
                let call = link.call(HostCall::Privileged { namespace, function, args: json });
                let result = match token {
                    Some(t) => tokio::select! {
                        r = call => r,
                        _ = t.cancelled() => {
                        if namespace_is_http {
                            // flux.http reports problems as values, not errors
                            let err = lua.create_table()?;
                            err.set("kind", "cancelled")?;
                            err.set("message", "the request was cancelled")?;
                            return Ok(MultiValue::from_vec(vec![Value::Nil, Value::Table(err)]));
                        }
                        return Err(mlua::Error::runtime("the operation was cancelled"));
                    }
                    },
                    None => call.await,
                };
                let values = result.map_err(mlua::Error::runtime)?;
                let serde_json::Value::Array(items) = values else { return Ok(MultiValue::new()) };
                let mut out = MultiValue::new();
                for item in items.iter().rev() {
                    out.push_front(json_to_lua(&lua, item)?);
                }
                Ok(out)
            }
        })?;
        table.set(*func, stub)?;
    }
    Ok(())
}
