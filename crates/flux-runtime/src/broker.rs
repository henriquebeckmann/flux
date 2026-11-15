//! The launcher's side of an Extension Host connection: performs, on behalf of one extension,
//! the operations the host is not allowed to do itself (start processes, use the clipboard,
//! read files outside the extension's folders, store data).
//!
//! Every call is checked against the policy **the launcher holds for that extension**, in the
//! real implementation of the function (`api.rs`), exactly as for an extension running in-process.
//! The host cannot widen that policy: it never sends it, and the extension's identity is the
//! connection, not a field of the message.

use std::{
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
};

use flux_api::{
    Limits, PermissionSet, SecretStore, Storage,
    wire::HostCall,
};
use mlua::{Function, MultiValue, Table};
use tokio::{sync::mpsc, task::AbortHandle};

use crate::{ctx::CmdCtx, limits::Heartbeat, remote, sandbox::Sandbox};

/// Delivers an answer to the host (`corr` is the id of the call).
pub type Reply = Arc<dyn Fn(u64, Result<serde_json::Value, String>) + Send + Sync>;

pub struct BrokerEnv {
    pub extension: String,
    pub dir: PathBuf,
    pub data_dir: PathBuf,
    pub policy: Arc<PermissionSet>,
    pub storage: Arc<dyn Storage>,
    pub secrets: Arc<dyn SecretStore>,
    pub limits: Limits,
    pub app_version: String,
    pub reply: Reply,
    /// Updated as the broker starts and ends background processes (`flux status`).
    pub stats: Arc<crate::resources::LiveStats>,
    /// Run commands confined through this program (`<exe> host --exec`); `None` runs them directly.
    pub exec_exe: Option<PathBuf>,
}

enum Msg {
    Call { corr: u64, call: HostCall },
    Stop,
}

#[derive(Clone)]
pub struct Broker {
    tx: mpsc::UnboundedSender<Msg>,
    aborts: Arc<Mutex<HashMap<u64, AbortHandle>>>,
}

impl Broker {
    pub fn call(&self, corr: u64, call: HostCall) {
        let _ = self.tx.send(Msg::Call { corr, call });
    }

    /// The host gave up on a call: stop it (this kills a command it started).
    pub fn cancel(&self, corr: u64) {
        if let Some(h) = self.aborts.lock().unwrap().remove(&corr) {
            h.abort();
        }
    }

    /// Ends the broker; whatever it still runs is dropped (children are killed by their guards).
    pub fn stop(&self) {
        let _ = self.tx.send(Msg::Stop);
    }
}

pub fn spawn(mut env: BrokerEnv) -> Broker {
    // Answers travel in one frame (16 MiB, base64 for binary data): cap what can be returned.
    env.limits.http_max_mb = env.limits.http_max_mb.min(8);
    env.limits.exec_output_kb = env.limits.exec_output_kb.min(4096);
    let (tx, rx) = mpsc::unbounded_channel();
    let aborts: Arc<Mutex<HashMap<u64, AbortHandle>>> = Default::default();
    let broker = Broker { tx, aborts: aborts.clone() };
    let name = format!("flux-broker-{}", env.extension);
    let spawned = std::thread::Builder::new().name(name).spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("broker runtime");
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, run(env, rx, aborts));
    });
    if let Err(e) = spawned {
        log::error!("could not start a broker thread: {e}");
    }
    broker
}

async fn run(env: BrokerEnv, mut rx: mpsc::UnboundedReceiver<Msg>, aborts: Arc<Mutex<HashMap<u64, AbortHandle>>>) {
    let sb = match Sandbox::new(env.limits, Heartbeat::default()) {
        Ok(sb) => Rc::new(sb),
        Err(e) => {
            log::error!("[{}] broker: could not create a Lua state: {e}", env.extension);
            return;
        }
    };
    let (events, _unused) = async_channel::unbounded();
    let http = Rc::new(crate::http::HttpState::new(env.policy.grants.hosts.clone(), &env.app_version));
    let ctx = Rc::new(CmdCtx {
        extension: env.extension.clone(),
        command: "broker".into(),
        permissions: env.policy.clone(),
        dir: env.dir.clone(),
        data_dir: env.data_dir.clone(),
        events,
        storage: env.storage.clone(),
        secrets: env.secrets.clone(),
        app_version: env.app_version.clone(),
        views: std::rc::Weak::new(),
        prefs: Rc::new(vec![]),
        on_unload: Default::default(),
        http,
        log_limiter: Default::default(),
        link: None,
        res: crate::resources::Resources::new(&env.limits, env.stats.clone()),
        exec_wrapper: env.exec_exe.clone().map(|exe| crate::execwrap::ExecWrapper { exe }),
    });
    let flux: Option<Table> = sb.env_for(&ctx).and_then(|e| e.get::<Table>("flux")).map_err(|e| log::error!("[{}] broker: {e}", env.extension)).ok();
    let Some(flux) = flux else { return };

    while let Some(msg) = rx.recv().await {
        let (corr, call) = match msg {
            Msg::Call { corr, call } => (corr, call),
            Msg::Stop => break,
        };
        let reply = env.reply.clone();
        let ext = env.extension.clone();
        match call {
            HostCall::LocalGet { key } => reply(corr, Ok(opt(env.storage.local_get(&ext, &key)))),
            HostCall::LocalSet { key, json } => {
                env.storage.local_set(&ext, &key, &json);
                reply(corr, Ok(serde_json::Value::Null))
            }
            HostCall::LocalRemove { key } => {
                env.storage.local_remove(&ext, &key);
                reply(corr, Ok(serde_json::Value::Null))
            }
            HostCall::LocalClear => {
                env.storage.local_clear(&ext);
                reply(corr, Ok(serde_json::Value::Null))
            }
            HostCall::CacheGet { key } => reply(corr, Ok(opt(env.storage.cache_get(&ext, &key)))),
            HostCall::CacheSet { key, json, ttl_secs } => {
                env.storage.cache_set(&ext, &key, &json, ttl_secs);
                reply(corr, Ok(serde_json::Value::Null))
            }
            HostCall::PrefGet { name } => reply(corr, Ok(opt(env.storage.pref_get(&ext, &name)))),
            HostCall::PrefSet { name, json } => {
                env.storage.pref_set(&ext, &name, &json);
                reply(corr, Ok(serde_json::Value::Null))
            }
            // `pref:*` keys are the extension's password preferences, read by the platform itself;
            // everything else is `flux.storage.secure` and needs the credentials permissions.
            HostCall::SecretGet { key } if !key.starts_with("pref:") && !ctx.permissions.has(flux_api::Capability::CredentialsRead) => reply(corr, Err(denied("credentials.read"))),
            HostCall::SecretSet { key, .. } | HostCall::SecretRemove { key } if !key.starts_with("pref:") && !ctx.permissions.has(flux_api::Capability::CredentialsWrite) => reply(corr, Err(denied("credentials.write"))),
            HostCall::SecretGet { key } => {
                let secrets = env.secrets.clone();
                tokio::task::spawn_local(async move {
                    reply(corr, secrets.get(&ext, &key).await.map(opt).map_err(|e| e.0));
                });
            }
            HostCall::SecretSet { key, value } => {
                let secrets = env.secrets.clone();
                tokio::task::spawn_local(async move {
                    reply(corr, secrets.set(&ext, &key, &value).await.map(|_| serde_json::Value::Null).map_err(|e| e.0));
                });
            }
            HostCall::SecretRemove { key } => {
                let secrets = env.secrets.clone();
                tokio::task::spawn_local(async move {
                    reply(corr, secrets.remove(&ext, &key).await.map(|_| serde_json::Value::Null).map_err(|e| e.0));
                });
            }
            HostCall::Privileged { namespace, function, args } => {
                if !remote::BROKERED.iter().any(|(n, f, _)| *n == namespace && *f == function) {
                    reply(corr, Err(format!("flux.{namespace}.{function} cannot be requested through the broker")));
                    continue;
                }
                let func: Option<Function> = if namespace == "http" {
                    sb.lua.named_registry_value::<Function>("flux.http._request").ok()
                } else {
                    flux.get::<Table>(namespace.as_str()).and_then(|t| t.get::<Function>(function.as_str())).ok()
                };
                let Some(func) = func else {
                    reply(corr, Err(format!("flux.{namespace}.{function} does not exist")));
                    continue;
                };
                let sb = sb.clone();
                let aborts2 = aborts.clone();
                let handle = tokio::task::spawn_local(async move {
                    let result = run_call(&sb, &func, args).await;
                    aborts2.lock().unwrap().remove(&corr);
                    reply(corr, result);
                });
                aborts.lock().unwrap().insert(corr, handle.abort_handle());
            }
            HostCall::Unknown => reply(corr, Err("unknown request".into())),
        }
    }
    // Dropping the LocalSet drops every running call; their guards kill the children.
}

fn opt(s: Option<String>) -> serde_json::Value {
    s.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null)
}

fn denied(cap: &str) -> String {
    format!("this extension does not have the '{cap}' permission")
}

async fn run_call(sb: &Sandbox, func: &Function, args: Vec<serde_json::Value>) -> Result<serde_json::Value, String> {
    let lua = &sb.lua;
    let mut values = MultiValue::new();
    for a in args.iter().rev() {
        values.push_front(remote::json_to_lua(lua, a).map_err(|e| e.to_string())?);
    }
    let out = sb.guard.sliced(func.call_async::<MultiValue>(values)).await.map_err(clean_error)?;
    Ok(serde_json::Value::Array(out.iter().map(|v| remote::lua_to_json(v, 0)).collect()))
}

/// The first line of a Lua error without the traceback and the "runtime error:" prefix.
fn clean_error(e: mlua::Error) -> String {
    let s = e.to_string();
    let line = s.lines().next().unwrap_or("").to_string();
    line.strip_prefix("runtime error: ").map(str::to_string).unwrap_or(line)
}
