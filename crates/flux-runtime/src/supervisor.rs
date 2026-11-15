//! Discovers extensions, owns one worker per enabled extension and routes UI requests to them.
//!
//! An invalid extension is reported and skipped; it never prevents the others from loading.
//! Extensions can be reloaded or enabled/disabled one at a time.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use async_channel::Sender;
use flux_api::{
    CommandInfo, CommandMode, ExtensionInfo, ExtensionSource, ExtensionStatus, Limits, RestartPolicy, RuntimeEvent, UserPermissions, RuntimeRequest, SecretStore, Storage,
};

use crate::{
    eval::{Declaration, Evaluator},
    hostproc::{self, HostOptions},
    limits::Heartbeat,
    manifest::Manifest,
    preferences,
    sandbox::Sandbox,
    worker::{self, LoadedExt, PREFERENCES_COMMAND, Shared, WorkerHandle, WorkerMsg},
};

/// Where extensions live and which ones start disabled.
pub struct Layout {
    pub dirs: Vec<PathBuf>,
    /// The directory `flux install` writes to; extensions found there are `Installed`, others `Dev`.
    pub installed_dir: Option<PathBuf>,
    pub disabled: HashSet<String>,
    /// Folders that are extensions themselves (attached by `flux dev`).
    pub single: Vec<PathBuf>,
}

struct Record {
    /// Folder name (the label for extensions whose manifest is invalid).
    label: String,
    dir: PathBuf,
    source: ExtensionSource,
    loaded: Result<LoadedExt, Vec<String>>,
    /// Set when the worker crashed or hung (cleared once its replacement has been stable for a while).
    crashed: Option<String>,
    restart: RestartState,
}

/// Crash history of one extension. A reload starts a new [`Record`], so it resets this.
#[derive(Default)]
struct RestartState {
    crashes: Vec<Instant>,
    /// When the worker should be started again (backoff).
    due: Option<Instant>,
    /// Too many crashes: stays stopped until reloaded.
    failed: bool,
    started: Option<Instant>,
}

impl Record {
    fn name(&self) -> &str {
        self.loaded.as_ref().map(|e| e.manifest.name.as_str()).unwrap_or(&self.label)
    }
}

pub struct Supervisor {
    layout: Layout,
    shared: Shared,
    records: Vec<Record>,
    workers: HashMap<String, WorkerHandle>,
    policy: RestartPolicy,
    next_generation: u64,
    permissions_file: Option<PathBuf>,
    host: Option<HostOptions>,
    /// What was last published as each extension's isolation (a host reports its confinement after it starts).
    isolation_seen: std::cell::RefCell<HashMap<String, String>>,
}

impl Supervisor {
    pub fn new(
        layout: Layout,
        data_root: PathBuf,
        storage: Arc<dyn Storage>,
        secrets: Arc<dyn SecretStore>,
        limits: Limits,
        app_version: String,
        events: Sender<RuntimeEvent>,
        policy: RestartPolicy,
        permissions_file: Option<PathBuf>,
        host: Option<HostOptions>,
    ) -> Self {
        let shared = Shared {
            events,
            storage,
            secrets,
            limits,
            data_root,
            app_version,
            view_ids: Arc::new(AtomicU64::new(0)),
            owners: Arc::new(Mutex::new(HashMap::new())),
            generation: 0,
            link: None,
            stats: Default::default(),
        };
        Self { layout, shared, records: vec![], workers: HashMap::new(), policy, next_generation: 0, permissions_file, host, isolation_seen: Default::default() }
    }

    /// The user's permission overrides, read fresh so `flux permissions` takes effect on reload.
    /// A broken file is reported and ignored: it must never stop extensions from loading.
    fn user_permissions(&self) -> UserPermissions {
        let Some(path) = &self.permissions_file else { return UserPermissions::default() };
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                self.error("flux", format!("{}: {}; ignoring the file", path.display(), e.to_string().lines().next().unwrap_or("")));
                UserPermissions::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => UserPermissions::default(),
            Err(e) => {
                self.error("flux", format!("{}: {e}", path.display()));
                UserPermissions::default()
            }
        }
    }

    fn emit(&self, e: RuntimeEvent) {
        let _ = self.shared.events.try_send(e);
    }

    /// Forgets (and returns) the screens a worker owned: they stop existing with it.
    fn take_views(&self, name: &str) -> Vec<u64> {
        let mut owners = self.shared.owners.lock().unwrap();
        let ids: Vec<u64> = owners.iter().filter(|(_, (ext, _))| ext == name).map(|(id, _)| *id).collect();
        for id in &ids {
            owners.remove(id);
        }
        ids
    }

    fn error(&self, extension: &str, message: String) {
        log::error!("[{extension}] {message}");
        let _ = self.shared.events.try_send(RuntimeEvent::ExtensionError { extension: extension.into(), message });
    }

    fn source_of(&self, root: &Path) -> ExtensionSource {
        if self.layout.installed_dir.as_deref() == Some(root) { ExtensionSource::Installed } else { ExtensionSource::Dev }
    }

    fn enabled(&self, name: &str) -> bool {
        !self.layout.disabled.contains(name)
    }

    // ---------------------------------------------------------------- discovery

    fn load_dir(sb: &Evaluator, dir: &Path, source: ExtensionSource, overrides: &UserPermissions) -> Record {
        let label = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let loaded = load_extension(sb, dir, overrides);
        Record { label, dir: dir.to_path_buf(), source, loaded, crashed: None, restart: RestartState::default() }
    }

    fn scan(&self, sb: &Evaluator) -> Vec<Record> {
        let overrides = self.user_permissions();
        let mut out: Vec<Record> = Vec::new();
        for root in &self.layout.dirs {
            let source = self.source_of(root);
            let Ok(entries) = std::fs::read_dir(root) else { continue };
            let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.join("manifest.lua").is_file()).collect();
            dirs.sort();
            for dir in dirs {
                let rec = Self::load_dir(sb, &dir, source, &overrides);
                out.push(rec);
            }
        }
        for dir in &self.layout.single {
            if dir.join("manifest.lua").is_file() {
                out.push(Self::load_dir(sb, dir, ExtensionSource::Dev, &overrides));
            }
        }
        self.dedupe(&mut out);
        out
    }

    /// First extension with a given name wins; later ones are marked invalid.
    fn dedupe(&self, records: &mut [Record]) {
        let mut first: HashMap<String, PathBuf> = HashMap::new();
        for r in records.iter_mut() {
            let Ok(ext) = &r.loaded else { continue };
            match first.get(&ext.manifest.name) {
                Some(other) => {
                    let msg = format!("duplicate extension '{}': {} is already loaded from {}", ext.manifest.name, r.dir.display(), other.display());
                    r.loaded = Err(vec![msg]);
                }
                None => {
                    first.insert(ext.manifest.name.clone(), r.dir.clone());
                }
            }
        }
    }

    /// How declarations are evaluated: in a confined child process when extensions run in host
    /// processes, in this process otherwise.
    fn scratch_sandbox(&self) -> Option<Evaluator> {
        if let Some(opts) = &self.host {
            return Some(Evaluator::Isolated(opts.clone()));
        }
        match Sandbox::new(self.shared.limits, Heartbeat::default()) {
            Ok(sb) => Some(Evaluator::Local(sb)),
            Err(e) => {
                self.error("flux", format!("could not create Lua state: {e}"));
                None
            }
        }
    }

    fn start_enabled(&mut self) {
        for i in 0..self.records.len() {
            self.start_record(i);
        }
    }

    fn start_record(&mut self, i: usize) {
        let r = &self.records[i];
        let Ok(ext) = &r.loaded else { return };
        if !self.enabled(&ext.manifest.name) || self.workers.contains_key(&ext.manifest.name) {
            return;
        }
        let ext = ext.clone();
        self.start_worker(i, &ext);
    }

    /// Starts a worker whose events pass through a gate: once the worker is replaced or
    /// abandoned, whatever it still emits is dropped instead of reaching the UI.
    fn start_worker(&mut self, record: usize, ext: &LoadedExt) {
        self.next_generation += 1;
        let generation = self.next_generation;
        let name = ext.manifest.name.clone();
        let (wtx, wrx) = async_channel::unbounded::<RuntimeEvent>();
        let root = self.shared.events.clone();
        let mut shared = self.shared.clone();
        shared.events = wtx;
        shared.generation = generation;
        let mut handle = match &self.host {
            None => worker::spawn(ext.clone(), shared),
            Some(opts) => {
                let base = (generation % 60_000) << 32;
                match hostproc::spawn_host(ext, shared, opts, base) {
                    Ok(h) => h,
                    Err(e) => {
                        let why = format!("could not start the extension host: {e}");
                        self.error(&name, why.clone());
                        self.register_crash(record, why, false);
                        self.publish();
                        return;
                    }
                }
            }
        };
        let gate = handle.alive.clone();
        let forwarder = std::thread::Builder::new().name(format!("flux-evt-{name}")).spawn(move || {
            while let Ok(e) = wrx.recv_blocking() {
                if gate.load(Ordering::Acquire) {
                    let _ = root.try_send(e);
                }
            }
        });
        if forwarder.is_err() {
            log::error!("[{name}] could not start the event forwarder");
        }
        handle.generation = generation;
        self.records[record].restart.started = Some(Instant::now());
        self.workers.insert(name.clone(), handle);
        self.emit(RuntimeEvent::ExtensionLoaded { extension: name, generation });
    }

    // ------------------------------------------------------------- publishing

    fn info(&self, r: &Record) -> ExtensionInfo {
        match &r.loaded {
            Ok(ext) => {
                let m = &ext.manifest;
                let enabled = self.enabled(&m.name);
                let perms: Vec<String> = ext.policy.names();
                ExtensionInfo {
                    name: m.name.clone(),
                    title: m.title.clone(),
                    version: m.version.clone(),
                    description: m.description.clone(),
                    author: m.author.clone(),
                    license: m.license.clone(),
                    dir: r.dir.to_string_lossy().into_owned(),
                    source: r.source,
                    enabled,
                    status: match (&r.crashed, enabled) {
                        (_, false) => ExtensionStatus::Disabled,
                        (Some(message), _) if r.restart.failed => ExtensionStatus::Failed { message: message.clone() },
                        (Some(message), _) => ExtensionStatus::Crashed { message: message.clone() },
                        _ => ExtensionStatus::Loaded,
                    },
                    permissions: perms.clone(),
                    isolation: self.isolation_of(&m.name),
                    revoked_permissions: m.permissions.names().into_iter().filter(|n| !perms.contains(n)).collect(),
                    allowed_hosts: m.allowed_hosts.clone(),
                    commands: m.commands.iter().map(|c| c.name.clone()).collect(),
                }
            }
            Err(errors) => ExtensionInfo {
                name: r.label.clone(),
                title: r.label.clone(),
                version: String::new(),
                description: None,
                author: None,
                license: None,
                dir: r.dir.to_string_lossy().into_owned(),
                source: r.source,
                enabled: self.enabled(&r.label),
                status: ExtensionStatus::Invalid { errors: errors.clone() },
                permissions: vec![],
                allowed_hosts: vec![],
                commands: vec![],
                isolation: String::new(),
                revoked_permissions: vec![],
            },
        }
    }

    fn isolation_of(&self, name: &str) -> String {
        if self.host.is_none() {
            return "thread, not isolated".into();
        }
        match self.workers.get(name).map(|h| h.sandbox.lock().unwrap().clone()) {
            None => "process (not running)".into(),
            Some(None) => "process (starting)".into(),
            Some(Some(r)) if r.fully_confined() => "process, confined".into(),
            Some(Some(r)) => format!("process, partly confined: {}", if r.missing.is_empty() { "see logs".to_string() } else { r.missing.join("; ") }),
        }
    }

    fn current_isolation(&self) -> HashMap<String, String> {
        self.workers.keys().map(|n| (n.clone(), self.isolation_of(n))).collect()
    }

    fn publish(&self) {
        *self.isolation_seen.borrow_mut() = self.current_isolation();
        let mut commands = Vec::new();
        for r in &self.records {
            let Ok(ext) = &r.loaded else { continue };
            let m = &ext.manifest;
            if !self.enabled(&m.name) {
                continue;
            }
            for c in &m.commands {
                commands.push(CommandInfo {
                    id: format!("{}:{}", m.name, c.name),
                    title: c.title.clone(),
                    subtitle: c.description.clone().or_else(|| Some(m.title.clone())),
                    icon: c.icon.clone().or_else(|| m.icon.clone()),
                    mode: c.mode,
                    extension: Some(m.name.clone()),
                });
            }
            if !ext.prefs.is_empty() {
                commands.push(CommandInfo {
                    id: format!("{}:{PREFERENCES_COMMAND}", m.name),
                    title: format!("Configure {}", m.title),
                    subtitle: Some("Extension preferences".into()),
                    icon: Some("gear".into()),
                    mode: CommandMode::View,
                    extension: Some(m.name.clone()),
                });
            }
        }
        let infos: Vec<ExtensionInfo> = self.records.iter().map(|r| self.info(r)).collect();
        let _ = self.shared.events.try_send(RuntimeEvent::ExtensionsChanged(infos));
        let _ = self.shared.events.try_send(RuntimeEvent::CommandsRegistered(commands));
    }

    fn report_invalid(&self) {
        for r in &self.records {
            if let Err(errors) = &r.loaded {
                for e in errors {
                    self.error(&r.label, e.clone());
                }
            }
        }
    }

    // ------------------------------------------------------------- lifecycle

    /// Stops every worker (running `on_unload` hooks), rescans the disk, starts new workers
    /// and announces the commands. Also used for hot reload.
    pub fn reload(&mut self) {
        let stopped = self.stop_all();
        let Some(sb) = self.scratch_sandbox() else { return };
        self.records = self.scan(&sb);
        self.report_invalid();
        self.start_enabled();
        for (name, generation, views) in stopped {
            if self.workers.contains_key(&name) {
                self.emit(RuntimeEvent::ExtensionReloaded { extension: name, generation, views });
            } else {
                self.emit(RuntimeEvent::ExtensionUnloaded { extension: name, generation, views });
            }
        }
        self.publish();
    }

    /// Reloads one extension (by name or folder name). Unknown keys fall back to a full reload,
    /// which also picks up extensions that appeared on disk.
    pub fn reload_one(&mut self, key: &str) {
        let Some(i) = self.records.iter().position(|r| r.name() == key || r.label == key) else { return self.reload() };
        let (old_name, dir, source) = (self.records[i].name().to_string(), self.records[i].dir.clone(), self.records[i].source);
        let stopped = self.stop_worker(&old_name);
        if !dir.join("manifest.lua").is_file() {
            self.records.remove(i); // the folder is gone
            self.announce_stopped(&old_name, stopped, false);
            return self.publish();
        }
        let Some(sb) = self.scratch_sandbox() else { return };
        let overrides = self.user_permissions();
        self.records[i] = Self::load_dir(&sb, &dir, source, &overrides);
        let mut records = std::mem::take(&mut self.records);
        self.dedupe(&mut records);
        self.records = records;
        if let Err(errors) = &self.records[i].loaded {
            for e in errors {
                self.error(&self.records[i].label.clone(), e.clone());
            }
        }
        self.start_record(i);
        self.announce_stopped(&old_name, stopped, self.workers.contains_key(&old_name));
        self.publish();
    }

    pub fn set_enabled(&mut self, key: &str, enabled: bool) {
        let name = self.records.iter().find(|r| r.name() == key || r.label == key).map(|r| r.name().to_string()).unwrap_or_else(|| key.to_string());
        if enabled {
            self.layout.disabled.remove(&name);
            if let Some(i) = self.records.iter().position(|r| r.name() == name) {
                self.start_record(i);
            }
        } else {
            self.layout.disabled.insert(name.clone());
            let stopped = self.stop_worker(&name);
            self.announce_stopped(&name, stopped, false);
        }
        self.publish();
    }

    fn announce_stopped(&self, name: &str, stopped: Option<(u64, Vec<u64>)>, restarted: bool) {
        let Some((generation, views)) = stopped else { return };
        let extension = name.to_string();
        self.emit(if restarted {
            RuntimeEvent::ExtensionReloaded { extension, generation, views }
        } else {
            RuntimeEvent::ExtensionUnloaded { extension, generation, views }
        });
    }

    /// Unloads one worker: `on_unload` hooks run, then the thread is joined. Returns its
    /// generation and the screens it owned. A worker that does not stop in time is abandoned
    /// and its events are muted (a thread cannot be killed; see docs/security.md).
    fn stop_worker(&mut self, name: &str) -> Option<(u64, Vec<u64>)> {
        let h = self.workers.remove(name)?;
        let generation = h.generation;
        // Taken before unloading: the worker forgets its screens while it shuts down.
        let views = self.take_views(name);
        let (tx, rx) = std::sync::mpsc::channel();
        if h.tx.send(WorkerMsg::Unload { done: tx }) && rx.recv_timeout(Duration::from_secs(3)).is_ok() {
            if let Some(j) = h.join {
                let _ = j.join();
            }
        } else {
            log::warn!("[{name}] did not stop in time; abandoning its worker");
            h.alive.store(false, Ordering::Release);
        }
        Some((generation, views))
    }

    fn stop_all(&mut self) -> Vec<(String, u64, Vec<u64>)> {
        let mut waits = Vec::new();
        let drained: Vec<(String, WorkerHandle)> = self.workers.drain().collect();
        for (name, h) in drained {
            let views = self.take_views(&name);
            let (tx, rx) = std::sync::mpsc::channel();
            if !h.tx.send(WorkerMsg::Unload { done: tx }) {
                h.alive.store(false, Ordering::Release);
            }
            waits.push((name, rx, h, views));
        }
        // Workers unload in parallel; one that is stuck is abandoned (its thread is detached).
        let mut stopped = Vec::new();
        for (name, rx, h, views) in waits {
            let generation = h.generation;
            match rx.recv_timeout(Duration::from_secs(3)) {
                Ok(()) => {
                    if let Some(j) = h.join {
                        let _ = j.join();
                    }
                }
                Err(_) => {
                    log::warn!("[{name}] did not stop in time; abandoning its worker");
                    h.alive.store(false, Ordering::Release);
                }
            }
            stopped.push((name, generation, views));
        }
        for r in &mut self.records {
            r.restart.due = None;
        }
        stopped
    }

    // ------------------------------------------------------------- crashes

    /// Replaces a worker that died or hangs, with exponential backoff; too many crashes in a
    /// short window leave it stopped (`Failed`) until it is reloaded by hand.
    fn crash(&mut self, name: &str, why: String) {
        let Some(h) = self.workers.remove(name) else { return };
        // Whatever the old worker still emits is dropped. A thread stuck in C code cannot be
        // killed: it is detached and keeps its CPU slice at nice 10 until it returns.
        h.alive.store(false, Ordering::Release);
        let generation = h.generation;
        let fatal = h.fatal.load(Ordering::Acquire);
        // Dropping the handle ends the pump thread of a host process, which kills the process.
        drop(h);
        let views = self.take_views(name);
        self.error(name, why.clone());
        self.emit(RuntimeEvent::ExtensionCrashed { extension: name.to_string(), generation, reason: why.clone(), views });
        if let Some(i) = self.records.iter().position(|r| r.name() == name) {
            self.register_crash(i, why, fatal);
        }
        self.publish();
    }

    /// Records a crash of record `i`: schedules the backed-off restart, or gives up.
    fn register_crash(&mut self, i: usize, why: String, fatal: bool) {
        let (window, max) = (Duration::from_secs(self.policy.window_s), self.policy.max_crashes);
        let now = Instant::now();
        let delay = |n: u32| self.policy.delay_ms(n);
        let r = &mut self.records[i];
        r.restart.crashes.retain(|t| now.duration_since(*t) < window);
        r.restart.crashes.push(now);
        let n = r.restart.crashes.len() as u32;
        if fatal {
            r.restart.failed = true;
            r.restart.due = None;
            r.crashed = Some(format!("{why} (not restarting: retrying would not help)"));
        } else if n >= max {
            r.restart.failed = true;
            r.restart.due = None;
            r.crashed = Some(format!("{why} ({n} crashes in {} s; not restarting)", self.policy.window_s));
        } else {
            r.restart.due = Some(now + Duration::from_millis(delay(n)));
            r.crashed = Some(why);
        }
    }

    /// Detects a worker that died or is stuck in a busy loop.
    fn check_worker(&mut self, name: &str) {
        let stuck_after = Duration::from_millis(self.shared.limits.slice_ms * 4 + 2000);
        let Some(h) = self.workers.get(name) else { return };
        let stuck = h.heartbeat.busy_for().is_some_and(|d| d > stuck_after);
        let dead = h.tx.is_closed() || h.join.as_ref().is_some_and(|j| j.is_finished());
        let reason = h.reason.lock().unwrap().clone();
        if dead {
            self.crash(name, reason.unwrap_or_else(|| "the extension crashed".into()));
        } else if stuck {
            self.crash(name, "the extension was unresponsive".into());
        }
    }

    /// Periodic housekeeping: finds dead workers without waiting for the next request, performs
    /// due restarts and forgives extensions that have been stable for a while.
    pub fn tick(&mut self) {
        let names: Vec<String> = self.workers.keys().cloned().collect();
        for n in names {
            self.check_worker(&n);
        }
        let now = Instant::now();
        let stable = Duration::from_secs(self.policy.stable_s);
        let mut changed = false;
        for i in 0..self.records.len() {
            let r = &self.records[i];
            if r.restart.failed {
                continue;
            }
            if r.restart.due.is_some_and(|d| d <= now) {
                self.records[i].restart.due = None;
                self.start_record(i);
                changed = true;
            } else if r.crashed.is_some()
                && r.restart.due.is_none()
                && r.restart.started.is_some_and(|s| now.duration_since(s) >= stable)
                && self.workers.get(r.name()).is_some_and(|h| h.heartbeat.busy_for().is_none())
            {
                let r = &mut self.records[i];
                r.crashed = None;
                r.restart.crashes.clear();
                changed = true;
            }
        }
        if self.host.is_some() && self.current_isolation() != *self.isolation_seen.borrow() {
            changed = true;
        }
        if changed {
            self.publish();
        }
    }

    /// Sends to the extension's worker (replacing it first if it crashed).
    fn send(&mut self, name: &str, msg: WorkerMsg) {
        self.check_worker(name);
        if let Some(h) = self.workers.get(name) {
            h.tx.send(msg);
        }
    }

    /// The extension owning a view, unless the entry was written by a worker that has been replaced.
    fn owner_of(&self, view_id: u64) -> Option<String> {
        let (ext, generation) = self.shared.owners.lock().unwrap().get(&view_id).cloned()?;
        (self.workers.get(&ext)?.generation == generation).then_some(ext)
    }

    /// A snapshot of every extension's resource use, for `flux status`.
    fn stats(&self) -> Vec<flux_api::ExtensionStats> {
        self.records
            .iter()
            .filter_map(|r| r.loaded.as_ref().ok().map(|e| (r, e)))
            .map(|(r, e)| {
                let name = e.manifest.name.clone();
                let worker = self.workers.get(&name);
                let mut s = worker.map(|h| h.live.snapshot()).unwrap_or_default();
                if let Some(b) = worker.and_then(|h| h.broker_live.as_ref()) {
                    s.children = b.snapshot().children;
                }
                let pid = worker.and_then(|h| h.pid);
                let usage = pid.and_then(crate::procinfo::usage);
                let state = if !self.enabled(&name) {
                    "disabled"
                } else if r.restart.failed {
                    "failed"
                } else if r.restart.due.is_some() {
                    "restarting"
                } else if worker.is_some() {
                    "running"
                } else {
                    "stopped"
                };
                flux_api::ExtensionStats {
                    isolation: self.isolation_of(&name),
                    state: state.into(),
                    generation: worker.map_or(0, |h| h.generation),
                    pid,
                    uptime_s: worker.map_or(0, |h| h.started.elapsed().as_secs()),
                    recent_crashes: r.restart.crashes.len() as u32,
                    rss_kb: usage.map(|u| u.rss_kb),
                    cpu_ms: usage.map(|u| u.cpu_ms),
                    tasks: s.tasks,
                    http_in_flight: s.http_in_flight,
                    children: s.children,
                    lua_memory_kb: s.lua_memory_kb,
                    name,
                }
            })
            .collect()
    }

    fn why_not_running(&self, ext: &str, command_id: &str) -> String {
        match self.records.iter().find(|r| r.name() == ext) {
            Some(r) if r.restart.failed => format!("extension '{ext}' stopped after repeated crashes; reload it to try again"),
            Some(r) if r.restart.due.is_some() => format!("extension '{ext}' is restarting after a crash; try again in a moment"),
            _ => format!("command '{command_id}' does not exist or its extension is disabled"),
        }
    }

    pub fn handle(&mut self, req: RuntimeRequest) {
        match req {
            RuntimeRequest::RunCommand { command_id } => {
                let parsed = command_id.split_once(':').map(|(e, n)| (e.to_string(), n.to_string()));
                if let Some((ext, _)) = &parsed {
                    self.check_worker(ext);
                }
                match parsed {
                    Some((ext, name)) if self.workers.contains_key(&ext) => self.send(&ext, WorkerMsg::Run { command_id, name }),
                    other => {
                        let why = match other {
                            Some((ext, _)) => self.why_not_running(&ext, &command_id),
                            None => format!("command '{command_id}' does not exist"),
                        };
                        self.error("flux", why);
                        self.emit(RuntimeEvent::CommandFinished { command_id });
                    }
                }
            }
            RuntimeRequest::InvokeAction { view_id, action_id } => {
                if let Some(ext) = self.owner_of(view_id) {
                    self.send(&ext, WorkerMsg::Invoke { view_id, action_id });
                }
            }
            RuntimeRequest::SubmitInput { view_id, text } => {
                if let Some(ext) = self.owner_of(view_id) {
                    self.send(&ext, WorkerMsg::Submit { view_id, text });
                }
            }
            RuntimeRequest::SearchChanged { view_id, text } => {
                if let Some(ext) = self.owner_of(view_id) {
                    self.send(&ext, WorkerMsg::Search { view_id, text });
                }
            }
            RuntimeRequest::SelectionChanged { view_id, key } => {
                if let Some(ext) = self.owner_of(view_id) {
                    self.send(&ext, WorkerMsg::Selection { view_id, key });
                }
            }
            RuntimeRequest::FormChanged { view_id, field, value } => {
                if let Some(ext) = self.owner_of(view_id) {
                    self.send(&ext, WorkerMsg::FormChanged { view_id, field, value });
                }
            }
            RuntimeRequest::FormSubmit { view_id, values } => {
                if let Some(ext) = self.owner_of(view_id) {
                    self.send(&ext, WorkerMsg::FormSubmit { view_id, values });
                }
            }
            RuntimeRequest::ViewClosed { view_id } => {
                if let Some(ext) = self.owner_of(view_id) {
                    self.send(&ext, WorkerMsg::ViewClosed(view_id));
                }
            }
            RuntimeRequest::CancelCommand { command_id } => {
                if let Some((ext, _)) = command_id.split_once(':') {
                    let ext = ext.to_string();
                    self.send(&ext, WorkerMsg::CancelCommand(command_id));
                }
            }
            RuntimeRequest::Reload => self.reload(),
            RuntimeRequest::AddExtensionDir { path } => {
                if !self.layout.single.contains(&path) {
                    self.layout.single.push(path);
                }
                self.reload();
            }
            RuntimeRequest::RemoveExtensionDir { path } => {
                self.layout.single.retain(|p| p != &path);
                self.reload();
            }
            RuntimeRequest::ReloadExtension { name } => self.reload_one(&name),
            RuntimeRequest::SetEnabled { name, enabled } => self.set_enabled(&name, enabled),
            RuntimeRequest::Stats { reply } => {
                let _ = reply.try_send(self.stats());
            }
            RuntimeRequest::Shutdown => {
                self.stop_all();
            }
        }
    }
}

pub(crate) fn load_extension(ev: &Evaluator, dir: &Path, overrides: &UserPermissions) -> Result<LoadedExt, Vec<String>> {
    let Declaration { manifest, prefs, .. } = ev.declaration(dir)?;
    let policy = match overrides.extensions.get(&manifest.name) {
        Some(o) => manifest.permissions.restricted(o),
        None => manifest.permissions.clone(),
    };
    Ok(LoadedExt { dir: dir.to_path_buf(), manifest, policy: Arc::new(policy), prefs })
}
