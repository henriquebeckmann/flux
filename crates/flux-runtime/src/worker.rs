//! One extension = one OS thread = one Lua state. Nothing here is shared with other
//! extensions except the outgoing event channel and the (thread-safe) storage backends.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
    sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU64}},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use async_channel::Sender;
use flux_api::{Capability, FieldValue, Limits, NavEvent, PermissionSet, PreferenceSpec, RuntimeEvent, SecretStore, Storage};
use mlua::{Function, IntoLuaMulti, Value};
use tokio::{sync::mpsc, task::AbortHandle};

use crate::{
    api, ctx::CmdCtx, limits::Heartbeat, manifest::Manifest, sandbox::Sandbox, views::{Hooks, Views, ViewsConfig},
};

/// Command name of the built-in preferences screen.
pub const PREFERENCES_COMMAND: &str = "__preferences";
const PREFS_SOURCE: &str = include_str!("prefs.lua");

#[derive(Clone)]
pub struct Shared {
    pub events: Sender<RuntimeEvent>,
    pub storage: Arc<dyn Storage>,
    pub secrets: Arc<dyn SecretStore>,
    pub limits: Limits,
    pub data_root: PathBuf,
    pub app_version: String,
    pub view_ids: Arc<AtomicU64>,
    pub owners: Arc<Mutex<HashMap<u64, (String, u64)>>>,
    /// Identifies this worker among the ones that ever ran for the extension.
    pub generation: u64,
    /// Set inside an Extension Host: the connection to the launcher.
    pub link: Option<Arc<crate::remote::Link>>,
    pub stats: Arc<crate::resources::LiveStats>,
}

#[derive(Clone)]
pub struct LoadedExt {
    pub dir: PathBuf,
    pub manifest: Manifest,
    /// What the extension may actually do: the manifest narrowed by the user's overrides.
    pub policy: Arc<PermissionSet>,
    pub prefs: Vec<PreferenceSpec>,
}

pub enum WorkerMsg {
    Run { command_id: String, name: String },
    Invoke { view_id: u64, action_id: u64 },
    Submit { view_id: u64, text: String },
    Search { view_id: u64, text: String },
    Selection { view_id: u64, key: Option<String> },
    FormChanged { view_id: u64, field: String, value: FieldValue },
    FormSubmit { view_id: u64, values: Vec<(String, FieldValue)> },
    ViewClosed(u64),
    CancelCommand(String),
    /// Run `on_unload` hooks, cancel everything and stop. `done` fires when finished.
    Unload { done: std::sync::mpsc::Sender<()> },
}

/// Where requests for a worker go: its thread's queue, or the connection to its host process.
#[derive(Clone)]
pub enum WorkerTx {
    Thread(mpsc::UnboundedSender<WorkerMsg>),
    Process { tx: std::sync::mpsc::Sender<WorkerMsg>, dead: Arc<AtomicBool> },
}

impl WorkerTx {
    pub fn send(&self, msg: WorkerMsg) -> bool {
        match self {
            Self::Thread(tx) => tx.send(msg).is_ok(),
            Self::Process { tx, .. } => tx.send(msg).is_ok(),
        }
    }

    pub fn is_closed(&self) -> bool {
        match self {
            Self::Thread(tx) => tx.is_closed(),
            Self::Process { dead, .. } => dead.load(std::sync::atomic::Ordering::Acquire),
        }
    }
}

pub struct WorkerHandle {
    pub tx: WorkerTx,
    pub heartbeat: Heartbeat,
    pub join: Option<JoinHandle<()>>,
    /// Why the worker is gone, when known (the host process's exit status and last error lines).
    pub reason: Arc<Mutex<Option<String>>>,
    /// The failure will not go away by retrying (for example the sandbox cannot be applied).
    pub fatal: Arc<AtomicBool>,
    /// What an Extension Host reported about its confinement (`None`: thread mode).
    pub sandbox: Arc<Mutex<Option<flux_api::wire::SandboxReport>>>,
    /// Live counters of the worker (threads) or the last ones its host reported (processes).
    pub live: Arc<crate::resources::LiveStats>,
    /// Counters of the launcher-side broker (background processes it runs for the host).
    pub broker_live: Option<Arc<crate::resources::LiveStats>>,
    pub pid: Option<u32>,
    pub generation: u64,
    /// While false, events from this worker are dropped (it was replaced or abandoned).
    pub alive: Arc<AtomicBool>,
    pub started: Instant,
}

pub fn spawn(ext: LoadedExt, shared: Shared) -> WorkerHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let heartbeat = Heartbeat::default();
    let hb = heartbeat.clone();
    let name = ext.manifest.name.clone();
    let events = shared.events.clone();
    let generation = shared.generation;
    let stats = shared.stats.clone();
    let join = std::thread::Builder::new()
        .name(format!("flux-ext-{name}"))
        .spawn(move || {
            lower_priority();
            // A panic must never take anything but this extension down.
            if let Err(payload) = catch_unwind(AssertUnwindSafe(|| main(ext, shared, rx, hb))) {
                let detail = payload.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_default();
                log::error!("[{name}] worker panicked: {detail}");
                let _ = events.try_send(RuntimeEvent::ExtensionError { extension: name, message: format!("the extension crashed: {detail}") });
            }
        })
        .expect("spawn extension worker");
    WorkerHandle {
        tx: WorkerTx::Thread(tx),
        heartbeat,
        join: Some(join),
        reason: Default::default(),
        fatal: Default::default(),
        sandbox: Default::default(),
        live: stats,
        broker_live: None,
        pid: None,
        generation,
        alive: Arc::new(AtomicBool::new(true)),
        started: Instant::now(),
    }
}

/// Extension threads run at a lower scheduling priority, so a worker stuck in uninterruptible
/// C code (see `limits.rs`) cannot starve the UI or the other extensions of CPU.
fn lower_priority() {
    // SAFETY: plain syscall on the calling thread (`who = 0` is the caller's tid on Linux).
    #[cfg(target_os = "linux")]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum Scope {
    Command(String),
    View(u64),
}

struct Worker {
    ext: LoadedExt,
    shared: Shared,
    sb: Sandbox,
    views: Rc<Views>,
    http: Rc<crate::http::HttpState>,
    res: Rc<crate::resources::Resources>,
    log_limiter: Rc<crate::api::LogLimiter>,
    prefs: Rc<Vec<PreferenceSpec>>,
    on_unload: Rc<RefCell<Vec<Function>>>,
    tasks: RefCell<HashMap<u64, TaskEntry>>,
    next_task: Cell<u64>,
    /// Id of the task being polled right now (0 outside tasks): cancellation never aborts its caller.
    current: Rc<Cell<u64>>,
}

struct TaskEntry {
    scope: Scope,
    name: Option<String>,
    handle: AbortHandle,
}

/// Records which task is running while it is polled.
struct Tracked<F> {
    fut: Pin<Box<F>>,
    id: u64,
    current: Rc<Cell<u64>>,
}

impl<F: Future> Future for Tracked<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let (id, cur) = (self.id, self.current.clone());
        cur.set(id);
        let r = self.fut.as_mut().poll(cx);
        cur.set(0);
        r
    }
}

const MAX_TASKS_PER_VIEW: usize = 32;

struct CommandGuard {
    command_id: String,
    events: Sender<RuntimeEvent>,
    finished: bool,
}

impl CommandGuard {
    fn finish(&mut self) {
        self.finished = true;
        let _ = self.events.try_send(RuntimeEvent::CommandFinished { command_id: self.command_id.clone() });
    }
}

impl Drop for CommandGuard {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.events.try_send(RuntimeEvent::CommandCancelled { command_id: std::mem::take(&mut self.command_id) });
        }
    }
}

pub(crate) fn main(ext: LoadedExt, shared: Shared, mut rx: mpsc::UnboundedReceiver<WorkerMsg>, hb: Heartbeat) {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let sb = match Sandbox::new(shared.limits, hb) {
            Ok(sb) => sb,
            Err(e) => {
                let _ = shared.events.try_send(RuntimeEvent::ExtensionError { extension: ext.manifest.name.clone(), message: format!("could not create Lua state: {e}") });
                return;
            }
        };
        let http = Rc::new(crate::http::HttpState::new(ext.policy.grants.hosts.clone(), &shared.app_version));
        let views = Views::new(ViewsConfig {
            lua: sb.lua.clone(),
            ext: ext.manifest.name.clone(),
            events: shared.events.clone(),
            view_ids: shared.view_ids.clone(),
            owners: shared.owners.clone(),
            generation: shared.generation,
            dir: ext.dir.clone(),
            network: ext.policy.has(Capability::Network),
            filesystem: ext.policy.has(Capability::FilesystemRead),
            permissions: ext.policy.clone(),
            data_dir: shared.data_root.join(&ext.manifest.name),
            hosts: http.policy.clone(),
        });
        views.set_factory(sb.new_instance.clone());
        let res = crate::resources::Resources::new(&shared.limits, shared.stats.clone());
        let w = Rc::new(Worker {
            prefs: Rc::new(ext.prefs.clone()),
            ext,
            shared,
            sb,
            views,
            http,
            res,
            log_limiter: Default::default(),
            on_unload: Default::default(),
            tasks: Default::default(),
            next_task: Cell::new(0),
            current: Rc::new(Cell::new(0)),
        });
        w.install_hooks();
        w.run_init().await;
        while let Some(msg) = rx.recv().await {
            match msg {
                WorkerMsg::Run { command_id, name } => w.run_command(command_id, name),
                WorkerMsg::Invoke { view_id, action_id } => w.invoke(view_id, action_id, None),
                WorkerMsg::Submit { view_id, text } => w.fire(view_id, "on_submit", text),
                WorkerMsg::Search { view_id, text } => w.fire(view_id, "on_search_change", text),
                WorkerMsg::Selection { view_id, key } => w.fire(view_id, "on_selection_change", key),
                WorkerMsg::FormChanged { view_id, field, value } => w.form_changed(view_id, field, value),
                WorkerMsg::FormSubmit { view_id, values } => w.form_submit(view_id, values),
                WorkerMsg::ViewClosed(id) => {
                    w.views.close(id);
                    w.cancel("the user left the screen", |s| *s == Scope::View(id));
                }
                WorkerMsg::CancelCommand(id) => w.cancel("the user cancelled the command", |s| *s == Scope::Command(id.clone())),
                WorkerMsg::Unload { done } => {
                    w.unload().await;
                    let _ = done.send(());
                    break;
                }
            }
        }
    });
}

impl Worker {
    fn name(&self) -> &str {
        &self.ext.manifest.name
    }

    fn error(&self, tag: &str, message: String) {
        log::error!("[{tag}] {message}");
        let _ = self.shared.events.try_send(RuntimeEvent::ExtensionError { extension: tag.to_string(), message });
    }

    fn emit(&self, e: RuntimeEvent) {
        let _ = self.shared.events.try_send(e);
    }

    fn make_ctx(&self, command: &str) -> Rc<CmdCtx> {
        let data_dir = self.shared.data_root.join(self.name());
        let _ = std::fs::create_dir_all(&data_dir);
        Rc::new(CmdCtx {
            extension: self.name().to_string(),
            command: command.to_string(),
            permissions: self.ext.policy.clone(),
            dir: self.ext.dir.clone(),
            data_dir,
            events: self.shared.events.clone(),
            storage: self.shared.storage.clone(),
            secrets: self.shared.secrets.clone(),
            app_version: self.shared.app_version.clone(),
            views: Rc::downgrade(&self.views),
            prefs: self.prefs.clone(),
            on_unload: self.on_unload.clone(),
            http: self.http.clone(),
            log_limiter: self.log_limiter.clone(),
            link: self.shared.link.clone(),
            res: self.res.clone(),
            exec_wrapper: None,
        })
    }

    // ----------------------------------------------------- task tracking

    /// Spawns a task in `scope`. A named task replaces the running one with the same name ("latest wins").
    fn spawn_task(self: &Rc<Self>, scope: Scope, name: Option<String>, fut: impl Future<Output = ()> + 'static) {
        if let Some(n) = &name {
            self.cancel_where("superseded by a newer one", |e| e.scope == scope && e.name.as_deref() == Some(n));
        }
        let max = self.shared.limits.max_tasks;
        if self.tasks.borrow().len() >= max {
            let tag = match &scope {
                Scope::Command(id) => id.clone(),
                Scope::View(id) => self.views.tag_of(*id),
            };
            self.error(&tag, format!("too many things running at once (max {max}); wait for some to finish"));
            if let Scope::Command(command_id) = scope {
                self.emit(RuntimeEvent::CommandFinished { command_id }); // never started: do not leave the UI waiting
            }
            return;
        }
        let id = self.next_task.get() + 1;
        self.next_task.set(id);
        let this = self.clone();
        let tracked = Tracked { fut: Box::pin(fut), id, current: self.current.clone() };
        let h = tokio::task::spawn_local(async move {
            tracked.await;
            this.tasks.borrow_mut().remove(&id);
            this.sample_stats();
        });
        self.tasks.borrow_mut().insert(id, TaskEntry { scope, name, handle: h.abort_handle() });
        self.sample_stats();
    }

    /// Publishes what `flux status` shows (cheap: two atomic stores).
    fn sample_stats(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        self.shared.stats.tasks.store(self.tasks.borrow().len() as u32, Relaxed);
        self.shared.stats.lua_kb.store((self.sb.lua.used_memory() / 1024) as u64, Relaxed);
    }

    fn cancel(&self, reason: &str, pred: impl Fn(&Scope) -> bool) {
        self.cancel_where(reason, |e| pred(&e.scope));
    }

    /// Aborts matching tasks (never the one that is calling). Dropping the future drops the Lua
    /// coroutine and any child process / HTTP request it owns.
    fn cancel_where(&self, reason: &str, pred: impl Fn(&TaskEntry) -> bool) {
        let current = self.current.get();
        let mut cancelled = 0;
        self.tasks.borrow_mut().retain(|id, e| {
            if *id != current && pred(e) {
                e.handle.abort();
                cancelled += 1;
                false
            } else {
                true
            }
        });
        if cancelled > 0 {
            log::info!("[{}] cancelled {cancelled} running task(s): {reason}", self.name());
        }
    }

    /// Wires the closures `Views` needs to start tasks, schedule renders and cancel work.
    fn install_hooks(self: &Rc<Self>) {
        let (w1, w2, w3, w4, w5) = (Rc::downgrade(self), Rc::downgrade(self), Rc::downgrade(self), Rc::downgrade(self), Rc::downgrade(self));
        self.views.set_hooks(Hooks {
            schedule_render: Rc::new(move || {
                if let Some(w) = w1.upgrade() {
                    w.schedule_render();
                }
            }),
            start_task: Rc::new(move |view_id, name, f| {
                if let Some(w) = w2.upgrade() {
                    w.start_task(view_id, name, f);
                }
            }),
            cancel_task: Rc::new(move |view_id, name| {
                if let Some(w) = w3.upgrade() {
                    w.cancel_where("cancelled by the extension", |e| e.scope == Scope::View(view_id) && e.name.as_deref() == Some(name.as_str()));
                }
            }),
            view_closed: Rc::new(move |view_id| {
                if let Some(w) = w4.upgrade() {
                    w.cancel("the screen was closed", |s| *s == Scope::View(view_id));
                }
            }),
            arm: Rc::new(move || {
                if let Some(w) = w5.upgrade() {
                    w.sb.guard.arm();
                }
            }),
        });
    }

    /// Coalesces bursts of `self:set` into one render pass.
    fn schedule_render(self: &Rc<Self>) {
        let this = self.clone();
        tokio::task::spawn_local(async move {
            tokio::time::sleep(Duration::from_millis(8)).await;
            this.views.render_dirty();
        });
    }

    /// `self:task(name, fn)`: background work tied to a screen. CPU slices are limited like any
    /// Lua code; there is no wall-clock timeout because a task lives as long as its screen.
    fn start_task(self: &Rc<Self>, view_id: u64, name: String, func: Function) {
        let running = self.tasks.borrow().values().filter(|e| e.scope == Scope::View(view_id) && e.name.as_deref() != Some(name.as_str())).count();
        let tag = self.views.tag_of(view_id);
        if running >= MAX_TASKS_PER_VIEW {
            return self.error(&tag, format!("too many background tasks on one screen (max {MAX_TASKS_PER_VIEW})"));
        }
        let this = self.clone();
        let label = name.clone();
        self.spawn_task(Scope::View(view_id), Some(name), async move {
            let res = this.sb.guard.sliced(func.call_async::<()>(())).await;
            if let Err(e) = res {
                this.error(&tag, format!("task '{label}': {e}"));
            }
        });
    }

    /// Runs `fut` under the CPU-slice guard and the wall-clock timeout.
    async fn limited<T>(&self, fut: impl Future<Output = mlua::Result<T>>) -> mlua::Result<T> {
        let secs = self.shared.limits.command_timeout_s;
        match tokio::time::timeout(Duration::from_secs(secs), self.sb.guard.sliced(fut)).await {
            Ok(r) => r,
            Err(_) => Err(mlua::Error::runtime(format!("timed out after {secs} s"))),
        }
    }

    // ------------------------------------------------------------ running

    async fn run_source(&self, ctx: &Rc<CmdCtx>, src: &str, chunk: &str, call_main: bool) -> mlua::Result<Value> {
        let env = self.sb.env_for(ctx)?;
        let load = self.sb.lua.load(src).set_name(chunk).set_environment(env.clone());
        self.limited(async {
            load.call_async::<()>(()).await?;
            if !call_main {
                return Ok(Value::Nil);
            }
            let main: Function = env.get("main").map_err(|_| mlua::Error::runtime("the command must define a main() function"))?;
            main.call_async(()).await
        })
        .await
    }

    async fn run_init(self: &Rc<Self>) {
        let path = self.ext.dir.join("init.lua");
        if !path.is_file() {
            return;
        }
        let ctx = self.make_ctx("init");
        let res = match std::fs::read_to_string(&path) {
            Ok(src) => self.run_source(&ctx, &src, &format!("@{}", path.display()), false).await,
            Err(e) => Err(mlua::Error::external(e)),
        };
        if let Err(e) = res {
            self.error(self.name(), format!("init.lua: {e}"));
        }
    }

    fn command_source(&self, name: &str) -> Result<(String, String), String> {
        if name == PREFERENCES_COMMAND {
            return Ok((PREFS_SOURCE.to_string(), "=preferences".to_string()));
        }
        if !self.ext.manifest.commands.iter().any(|c| c.name == name) {
            return Err(format!("command '{}:{name}' does not exist", self.name()));
        }
        let path = self.ext.dir.join("commands").join(format!("{name}.lua"));
        std::fs::read_to_string(&path).map(|s| (s, format!("@{}", path.display()))).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn run_command(self: &Rc<Self>, command_id: String, name: String) {
        let this = self.clone();
        log::info!("[{command_id}] command started");
        self.emit(RuntimeEvent::CommandStarted { command_id: command_id.clone() });
        self.spawn_task(Scope::Command(command_id.clone()), None, async move {
            // Reports `CommandCancelled` if the task is dropped before it finishes (abort, unload).
            let mut guard = CommandGuard { command_id: command_id.clone(), events: this.shared.events.clone(), finished: false };
            let mut name = name;
            let mut ctx = this.make_ctx(&name);
            if name != PREFERENCES_COMMAND && !this.prefs.is_empty() {
                match api::missing_required(&ctx).await {
                    Ok(missing) if !missing.is_empty() => {
                        // Like Raycast: a command with unmet required preferences opens the settings instead.
                        this.error(&ctx.tag(), format!("required preference(s) not set: {}", missing.join(", ")));
                        name = PREFERENCES_COMMAND.to_string();
                        ctx = this.make_ctx(&name);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        this.error(&ctx.tag(), e.to_string());
                        guard.finish();
                        return;
                    }
                }
            }
            this.views.clear();
            let res = match this.command_source(&name) {
                Ok((src, chunk)) => this.run_source(&ctx, &src, &chunk, true).await,
                Err(msg) => Err(mlua::Error::runtime(msg)),
            };
            this.handle_result(&ctx.tag(), res, true);
            guard.finish();
        });
    }

    fn invoke(self: &Rc<Self>, view_id: u64, action_id: u64, arg: Option<String>) {
        let Some((tag, func)) = self.views.callback(view_id, action_id) else { return };
        log::info!("[{tag}] action started (view {view_id}, action {action_id})");
        let this = self.clone();
        self.spawn_task(Scope::View(view_id), None, async move {
            let res = this.limited(async { func.call_async::<Value>(arg).await }).await;
            this.handle_result(&tag, res, false);
        });
    }

    /// Runs a view callback registered under `path` (`on_submit`, `on_search_change`, …). A newer
    /// event of the same kind cancels the previous handler that is still running.
    fn fire<A: IntoLuaMulti + 'static>(self: &Rc<Self>, view_id: u64, path: &str, arg: A) {
        let Some((tag, func)) = self.views.named(view_id, path) else { return };
        log::debug!("[{tag}] {path} (view {view_id})");
        let this = self.clone();
        self.spawn_task(Scope::View(view_id), Some(path.to_string()), async move {
            let res = this.limited(async { func.call_async::<Value>(arg).await }).await;
            this.handle_result(&tag, res, false);
        });
    }

    fn form_changed(self: &Rc<Self>, view_id: u64, field: String, value: FieldValue) {
        match api::to_lua(&self.sb.lua, &value) {
            Ok(v) => self.fire(view_id, &format!("field:{field}:on_change"), v),
            Err(e) => self.error(&self.views.tag_of(view_id), e.to_string()),
        }
    }

    /// Runs per-field and form-level `validate`, then `on_submit`. Validation errors go back to the form.
    fn form_submit(self: &Rc<Self>, view_id: u64, values: Vec<(String, FieldValue)>) {
        let this = self.clone();
        let tag = self.views.tag_of(view_id);
        self.spawn_task(Scope::View(view_id), Some("on_submit".into()), async move {
            let Some((_, on_submit)) = this.views.named(view_id, "on_submit") else {
                return this.error(&tag, "this form has no on_submit".into());
            };
            let run = async {
                let table = this.sb.lua.create_table()?;
                for (k, v) in &values {
                    table.set(k.as_str(), api::to_lua(&this.sb.lua, v)?)?;
                }
                let mut errors: Vec<(String, String)> = Vec::new();
                for (id, f) in this.views.field_validators(view_id) {
                    let v: Value = table.get(id.as_str())?;
                    if let Some(msg) = this.limited(f.call_async::<Option<String>>(v)).await? {
                        errors.push((id, msg));
                    }
                }
                if let Some((_, vf)) = this.views.named(view_id, "validate") {
                    if let Some(t) = this.limited(vf.call_async::<Option<mlua::Table>>(table.clone())).await? {
                        for pair in t.pairs::<String, String>() {
                            errors.push(pair?);
                        }
                    }
                }
                if !errors.is_empty() {
                    this.emit(RuntimeEvent::Nav(NavEvent::FormErrors { view_id, errors }));
                    return Ok(Value::Nil);
                }
                this.limited(on_submit.call_async::<Value>(table)).await
            };
            let res: mlua::Result<Value> = run.await;
            this.handle_result(&tag, res, false);
        });
    }

    /// A returned view opens a screen (`push`, for command entry points) or replaces the
    /// current one (actions). Errors never change the navigation stack.
    fn handle_result(&self, tag: &str, res: mlua::Result<Value>, push: bool) {
        let value = match res {
            Ok(v) => v,
            Err(e) => return self.error(tag, e.to_string()),
        };
        if matches!(value, Value::Nil) {
            return;
        }
        let r = if push { self.views.push(tag, value) } else { self.views.replace(tag, value) };
        if let Err(msg) = r {
            self.error(tag, msg);
        }
    }

    async fn unload(self: &Rc<Self>) {
        self.cancel("the extension is unloading", |_| true);
        let hooks: Vec<Function> = self.on_unload.borrow_mut().drain(..).collect();
        for f in hooks {
            let r = tokio::time::timeout(Duration::from_secs(1), self.sb.guard.sliced(async { f.call_async::<()>(()).await })).await;
            match r {
                Ok(Ok(())) => {}
                Ok(Err(e)) => self.error(self.name(), format!("on_unload: {e}")),
                Err(_) => self.error(self.name(), "on_unload timed out".into()),
            }
        }
        self.views.clear();
    }
}

