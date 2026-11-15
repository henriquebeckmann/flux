#![allow(dead_code)]
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use flux_api::*;
use flux_runtime::{RuntimeConfig, RuntimeHandle, spawn};

// ------------------------------------------------------------------ harness

#[derive(Default)]
pub struct MemStorage {
    pub local: Mutex<HashMap<(String, String), String>>,
    pub prefs: Mutex<HashMap<(String, String), String>>,
}
impl Storage for MemStorage {
    fn local_get(&self, e: &str, k: &str) -> Option<String> { self.local.lock().unwrap().get(&(e.into(), k.into())).cloned() }
    fn local_set(&self, e: &str, k: &str, v: &str) { self.local.lock().unwrap().insert((e.into(), k.into()), v.into()); }
    fn local_remove(&self, e: &str, k: &str) { self.local.lock().unwrap().remove(&(e.into(), k.into())); }
    fn local_clear(&self, e: &str) { self.local.lock().unwrap().retain(|(x, _), _| x != e); }
    fn cache_get(&self, _: &str, _: &str) -> Option<String> { None }
    fn cache_set(&self, _: &str, _: &str, _: &str, _: u64) {}
    fn pref_get(&self, e: &str, n: &str) -> Option<String> { self.prefs.lock().unwrap().get(&(e.into(), n.into())).cloned() }
    fn pref_set(&self, e: &str, n: &str, v: &str) { self.prefs.lock().unwrap().insert((e.into(), n.into()), v.into()); }
}

pub fn write(dir: &Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

/// Creates `<root>/<name>/` with a manifest declaring `commands` (name, mode) and the given files.
pub fn ext(root: &Path, name: &str, perms: &[&str], commands: &[(&str, &str)], files: &[(&str, &str)]) {
    let cmds: String = commands.iter().map(|(n, m)| format!("{{name='{n}',title='{n}',mode='{m}'}},")).collect();
    let perms: String = perms.iter().map(|p| format!("'{p}',")).collect();
    write(&root.join(name), "manifest.lua", &format!("return {{ name='{name}', commands={{{cmds}}}, permissions={{{perms}}} }}"));
    for (f, c) in files {
        write(&root.join(name), f, c);
    }
}

pub struct Harness {
    pub handle: RuntimeHandle,
    pub rx: async_channel::Receiver<RuntimeEvent>,
    pub storage: Arc<MemStorage>,
    pub secrets: Arc<dyn SecretStore>,
}

pub fn start(root: &Path) -> Harness {
    start_with(root, Limits { slice_ms: 200, ..Limits::default() }, Arc::new(MemSecretStore::default()))
}

pub fn start_with(root: &Path, limits: Limits, secrets: Arc<dyn SecretStore>) -> Harness {
    start_full(root, limits, secrets, None, Default::default())
}

/// Like [`start_with`] but with an explicit restart policy.
pub fn start_restart(root: &Path, limits: Limits, restart: RestartPolicy) -> Harness {
    let (tx, rx) = async_channel::unbounded();
    let storage = Arc::new(MemStorage::default());
    let secrets: Arc<dyn SecretStore> = Arc::new(MemSecretStore::default());
    let handle = spawn(
        RuntimeConfig {
            extension_dirs: vec![root.to_path_buf()],
            installed_dir: None,
            disabled: Default::default(),
            data_dir: root.join(".data"),
            storage: storage.clone(),
            secrets: secrets.clone(),
            app_version: "test".into(),
            limits,
            restart,
            permissions_file: None,
            host: None,
        },
        tx,
    );
    Harness { handle, rx, storage, secrets }
}

/// With `FLUX_TEST_HOST=1` the shared harness runs every extension in a real, confined host
/// process, so whole suites can be re-run against the process-isolated runtime.
fn test_host() -> Option<flux_runtime::hostproc::HostOptions> {
    std::env::var_os("FLUX_TEST_HOST")?;
    let mut o = flux_runtime::hostproc::HostOptions::new(env!("CARGO_BIN_EXE_flux-host").into());
    o.cgroup = false;
    o.ping_timeout_ms = 30_000;
    o.sandbox = if flux_runtime::confine::landlock_abi() == 0 { flux_api::wire::SandboxMode::BestEffort } else { flux_api::wire::SandboxMode::Strict };
    Some(o)
}

/// Runs every extension in a real host process (`flux-host`), confined per `sandbox`.
pub fn start_host(root: &Path, sandbox: flux_api::wire::SandboxMode, tweak: impl FnOnce(&mut flux_runtime::hostproc::HostOptions)) -> Harness {
    start_host_in(root, &root.join(".data"), env!("CARGO_BIN_EXE_flux-host").into(), sandbox, tweak)
}

/// Like [`start_host`] with an explicit data folder and host program (a script, to fake a host).
pub fn start_host_in(
    root: &Path,
    data: &Path,
    exe: std::path::PathBuf,
    sandbox: flux_api::wire::SandboxMode,
    tweak: impl FnOnce(&mut flux_runtime::hostproc::HostOptions),
) -> Harness {
    let (tx, rx) = async_channel::unbounded();
    let storage = Arc::new(MemStorage::default());
    let secrets: Arc<dyn SecretStore> = Arc::new(MemSecretStore::default());
    let mut opts = flux_runtime::hostproc::HostOptions::new(exe);
    opts.eval_exe = Some(env!("CARGO_BIN_EXE_flux-host").into()); // a fake host still needs a real evaluator
    opts.sandbox = sandbox;
    opts.cgroup = false; // scopes are exercised by their own test
    tweak(&mut opts);
    let handle = spawn(
        RuntimeConfig {
            extension_dirs: vec![root.to_path_buf()],
            installed_dir: None,
            disabled: Default::default(),
            data_dir: data.to_path_buf(),
            storage: storage.clone(),
            secrets: secrets.clone(),
            app_version: "test".into(),
            limits: Limits { slice_ms: 200, ..Limits::default() },
            restart: Default::default(),
            permissions_file: None,
            host: Some(opts),
        },
        tx,
    );
    Harness { handle, rx, storage, secrets }
}

/// Starts with a `permissions.toml` the user would have written.
pub fn start_user_permissions(root: &Path, file: &Path) -> Harness {
    let (tx, rx) = async_channel::unbounded();
    let storage = Arc::new(MemStorage::default());
    let secrets: Arc<dyn SecretStore> = Arc::new(MemSecretStore::default());
    let handle = spawn(
        RuntimeConfig {
            extension_dirs: vec![root.to_path_buf()],
            installed_dir: None,
            disabled: Default::default(),
            data_dir: root.join(".data"),
            storage: storage.clone(),
            secrets: secrets.clone(),
            app_version: "test".into(),
            limits: Limits { slice_ms: 200, ..Limits::default() },
            restart: Default::default(),
            permissions_file: Some(file.to_path_buf()),
            host: None,
        },
        tx,
    );
    Harness { handle, rx, storage, secrets }
}

pub fn start_full(
    root: &Path,
    limits: Limits,
    secrets: Arc<dyn SecretStore>,
    installed_dir: Option<std::path::PathBuf>,
    disabled: std::collections::HashSet<String>,
) -> Harness {
    start_dirs(vec![root.to_path_buf()], root, limits, secrets, installed_dir, disabled)
}

/// Several extension roots; `data` is where per-extension data directories go.
pub fn start_dirs(
    dirs: Vec<std::path::PathBuf>,
    data: &Path,
    limits: Limits,
    secrets: Arc<dyn SecretStore>,
    installed_dir: Option<std::path::PathBuf>,
    disabled: std::collections::HashSet<String>,
) -> Harness {
    let (tx, rx) = async_channel::unbounded();
    let storage = Arc::new(MemStorage::default());
    let handle = spawn(
        RuntimeConfig {
            extension_dirs: dirs,
            installed_dir,
            disabled,
            data_dir: data.join(".data"),
            storage: storage.clone(),
            secrets: secrets.clone(),
            app_version: "test".into(),
            limits,
            restart: Default::default(),
            permissions_file: None,
            host: test_host(),
        },
        tx,
    );
    Harness { handle, rx, storage, secrets }
}

impl Harness {
    pub fn recv(&self, timeout: Duration) -> Option<RuntimeEvent> {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if let Ok(e) = self.rx.try_recv() {
                return Some(e);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    /// Next event, ignoring `CommandFinished`.
    pub fn next(&self) -> RuntimeEvent {
        loop {
            match self.recv(Duration::from_secs(10)).expect("timed out waiting for a runtime event") {
                RuntimeEvent::CommandFinished { .. }
                | RuntimeEvent::ExtensionsChanged(_)
                | RuntimeEvent::Log { .. }
                | RuntimeEvent::ExtensionLoaded { .. }
                | RuntimeEvent::CommandStarted { .. }
                | RuntimeEvent::CommandCancelled { .. }
                | RuntimeEvent::ExtensionUnloaded { .. }
                | RuntimeEvent::ExtensionReloaded { .. } => continue,
                e => return e,
            }
        }
    }

    pub fn next_push(&self) -> (u64, ViewNode) {
        match self.next() {
            RuntimeEvent::Nav(NavEvent::Push { view_id, view }) => (view_id, view),
            e => panic!("expected Push, got {e:?}"),
        }
    }

    pub fn next_error(&self) -> String {
        match self.next() {
            RuntimeEvent::ExtensionError { message, .. } => message,
            e => panic!("expected an error, got {e:?}"),
        }
    }

    /// Everything that arrives within `d`.
    pub fn drain_for(&self, d: Duration) -> Vec<RuntimeEvent> {
        let mut v = Vec::new();
        while let Some(e) = self.recv(d) {
            v.push(e);
        }
        v
    }

    /// Consumes events until `f` matches one (any kind, nothing is skipped).
    pub fn wait_event<T>(&self, f: impl Fn(&RuntimeEvent) -> Option<T>) -> T {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if let Some(e) = self.recv(Duration::from_millis(50)) {
                if let Some(t) = f(&e) {
                    return t;
                }
            }
        }
        panic!("timed out waiting for a matching event");
    }

    /// Applies the view's patches to `node` as they arrive until `done(node)`; any extension error panics.
    pub fn wait_view(&self, view_id: u64, node: &mut ViewNode, done: impl Fn(&ViewNode) -> bool) {
        let end = Instant::now() + Duration::from_secs(15);
        while !done(node) {
            assert!(Instant::now() < end, "timed out waiting for the view to reach the expected state: {node:#?}");
            match self.recv(Duration::from_millis(50)) {
                Some(RuntimeEvent::Nav(NavEvent::Update { view_id: id, patch })) if id == view_id => {
                    apply_patch(node, &patch).expect("patch applies");
                }
                Some(RuntimeEvent::ExtensionError { extension, message }) => panic!("{extension}: {message}"),
                _ => {}
            }
        }
    }

    /// The next `ExtensionsChanged` list.
    pub fn next_infos(&self) -> Vec<ExtensionInfo> {
        self.wait_event(|e| if let RuntimeEvent::ExtensionsChanged(v) = e { Some(v.clone()) } else { None })
    }

    pub fn next_commands(&self) -> Vec<CommandInfo> {
        self.wait_event(|e| if let RuntimeEvent::CommandsRegistered(v) = e { Some(v.clone()) } else { None })
    }

    pub fn run(&self, id: &str) {
        self.handle.send(RuntimeRequest::RunCommand { command_id: id.into() });
    }

    pub fn wait_local(&self, ext: &str, key: &str) -> String {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if let Some(v) = self.storage.local_get(ext, key) {
                return v;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for storage {ext}/{key}");
    }
}

/// Copies `extensions/<name>` of the repository into `root/<name>`.
pub fn install_example(root: &Path, name: &str) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions").join(name);
    for f in walk(&src) {
        let rel = f.strip_prefix(&src).unwrap();
        write(&root.join(name), rel.to_str().unwrap(), &std::fs::read_to_string(&f).unwrap());
    }
}

/// Titles of all list items, flat.
pub fn titles(v: &ViewNode) -> Vec<String> {
    match v {
        ViewNode::List(l) => l.items().map(|i| i.title.clone()).collect(),
        ViewNode::Grid(g) => g.items().map(|i| i.title.clone().unwrap_or_default()).collect(),
        _ => vec![],
    }
}

pub fn list(v: ViewNode) -> ListView {
    match v {
        ViewNode::List(l) => l,
        other => panic!("expected a list, got {other:?}"),
    }
}

pub fn demo_dir() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/demo");
    for entry in walk(&src) {
        let rel = entry.strip_prefix(&src).unwrap();
        write(&d.path().join("demo"), rel.to_str().unwrap(), &std::fs::read_to_string(&entry).unwrap());
    }
    d
}

pub fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() { out.extend(walk(&p)) } else if p.extension().is_some_and(|x| x == "lua") { out.push(p) }
    }
    out
}

/// pid of a `sleep <arg>` process (exact argv match, so wrapper shells that merely mention the
/// text are never picked up).
pub fn pid_of(pattern: &str) -> Option<u32> {
    let arg = pattern.strip_prefix("sleep ")?;
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
        let Ok(raw) = std::fs::read(entry.path().join("cmdline")) else { continue };
        let argv: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
        if argv.len() >= 2 && argv[0].ends_with(b"sleep") && argv[1] == arg.as_bytes() {
            return Some(pid);
        }
    }
    None
}

/// True while `/proc/<pid>` exists, i.e. also for zombies that were killed but never reaped.
pub fn pid_exists(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

pub fn wait_until_gone(pid: u32) {
    let end = Instant::now() + Duration::from_secs(5);
    while pid_exists(pid) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!pid_exists(pid), "process {pid} must be killed and reaped (no zombie)");
}

