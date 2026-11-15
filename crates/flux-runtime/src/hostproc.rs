//! Launcher side of an Extension Host: starts the process, speaks the wire protocol with it,
//! watches it, and kills it when it must.
//!
//! A host is one OS process per extension. If it exits, crashes, hangs or violates the protocol the
//! launcher notices at once (the reader thread sees the connection end; the pump thread pings it)
//! and `SIGKILL`s whatever is left. Unlike a thread, a stuck host can really be stopped.

use std::{
    collections::VecDeque,
    io::{BufRead, BufReader},
    os::unix::{io::AsRawFd, net::UnixStream, process::CommandExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use flux_api::{
    NavEvent, RuntimeEvent,
    wire::{self, FromHost, Init, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION, SandboxMode, SandboxReport, ToHost},
};

use crate::{
    broker::{self, BrokerEnv},
    limits::Heartbeat,
    worker::{LoadedExt, Shared, WorkerHandle, WorkerMsg, WorkerTx},
};

/// How extensions are run. `None` in [`crate::RuntimeConfig::host`] means threads (no isolation).
#[derive(Debug, Clone)]
pub struct HostOptions {
    /// The program that implements `host` (the launcher itself, or `flux-host` in tests).
    pub exe: PathBuf,
    /// Program that evaluates declarations (`<exe> host --eval`); defaults to `exe`.
    pub eval_exe: Option<PathBuf>,
    pub sandbox: SandboxMode,
    /// Put each host in its own systemd scope to enforce the limits below (if systemd is available).
    pub cgroup: bool,
    /// Hard memory limit of a host process (Lua's own limit is `Limits::memory_mb`).
    pub memory_mb: u64,
    pub cpu_percent: u32,
    pub max_tasks: u32,
    pub max_open_files: u64,
    pub ping_ms: u64,
    /// No answer to pings for this long: the host is killed.
    pub ping_timeout_ms: u64,
}

impl HostOptions {
    pub fn new(exe: PathBuf) -> Self {
        Self { exe, eval_exe: None, sandbox: SandboxMode::Strict, cgroup: true, memory_mb: 512, cpu_percent: 100, max_tasks: 64, max_open_files: 256, ping_ms: 500, ping_timeout_ms: 5000 }
    }
}

/// Whether `systemd-run --user --scope` works here (checked once).
fn systemd_scopes_available() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "--collect", "true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

struct Out(Mutex<UnixStream>, std::sync::atomic::AtomicU64);

impl Out {
    fn send(&self, corr: Option<u64>, body: &ToHost) -> std::io::Result<()> {
        let id = self.1.fetch_add(1, Ordering::Relaxed);
        let mut w = self.0.lock().unwrap();
        wire::write_frame(&mut *w, id, corr, body)
    }
}

#[derive(Default)]
struct State {
    unloaded: (Mutex<bool>, Condvar),
    last_pong: Mutex<Option<Instant>>,
    stderr_tail: Mutex<VecDeque<String>>,
    pid: AtomicU32,
    reaped: AtomicBool,
}

fn kill_pid(state: &State) {
    let pid = state.pid.load(Ordering::Acquire);
    if pid != 0 && !state.reaped.load(Ordering::Acquire) {
        // SAFETY: SIGKILL to our own child; `reaped` guards against a recycled pid.
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
}

pub fn spawn_host(ext: &LoadedExt, shared: Shared, opts: &HostOptions, view_id_base: u64) -> std::io::Result<WorkerHandle> {
    let name = ext.manifest.name.clone();
    let (core_end, host_end) = UnixStream::pair()?;
    let host_fd = host_end.as_raw_fd();

    let use_scope = opts.cgroup && systemd_scopes_available();
    let mut cmd = if use_scope {
        let mut c = Command::new("systemd-run");
        c.args(["--user", "--scope", "--quiet", "--collect", "--description"]).arg(format!("Flux extension {name}"));
        c.arg("-p").arg(format!("MemoryMax={}M", opts.memory_mb));
        c.arg("-p").arg("MemorySwapMax=0");
        c.arg("-p").arg(format!("TasksMax={}", opts.max_tasks));
        c.arg("-p").arg(format!("CPUQuota={}%", opts.cpu_percent));
        c.arg("--").arg(&opts.exe).arg("host");
        c
    } else {
        let mut c = Command::new(&opts.exe);
        c.arg("host");
        c
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).env_clear();
    // A minimal environment: the host needs nothing the launcher has (tokens, session variables…).
    for key in ["PATH", "HOME", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "LANG", "RUST_LOG", "RUST_BACKTRACE"] {
        if let Some(v) = std::env::var_os(key) {
            if use_scope || !matches!(key, "DBUS_SESSION_BUS_ADDRESS" | "XDG_RUNTIME_DIR") {
                cmd.env(key, v);
            }
        }
    }
    // SAFETY: only async-signal-safe calls (dup2/fcntl) between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            if host_fd != 3 {
                if libc::dup2(host_fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            } else {
                let flags = libc::fcntl(3, libc::F_GETFD);
                libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
            Ok(())
        });
    }
    let mut child: Child = cmd.spawn()?;
    drop(host_end);
    let state = Arc::new(State::default());
    state.pid.store(child.id(), Ordering::Release);

    let out = Arc::new(Out(Mutex::new(core_end.try_clone()?), Default::default()));
    let mut reader_stream = core_end;

    let heartbeat = Heartbeat::default();
    let dead = Arc::new(AtomicBool::new(false));
    let fatal = Arc::new(AtomicBool::new(false));
    let reason: Arc<Mutex<Option<String>>> = Default::default();
    let sandbox: Arc<Mutex<Option<SandboxReport>>> = Default::default();

    let init = Init {
        proto_min: MIN_PROTOCOL_VERSION,
        proto_max: PROTOCOL_VERSION,
        extension: name.clone(),
        generation: shared.generation,
        dir: ext.dir.to_string_lossy().into_owned(),
        data_dir: shared.data_root.join(&name).to_string_lossy().into_owned(),
        app_version: shared.app_version.clone(),
        limits: shared.limits,
        policy: (*ext.policy).clone(),
        view_id_base,
        sandbox: opts.sandbox,
        max_open_files: opts.max_open_files,
    };
    out.send(None, &ToHost::Init(init))?;

    let broker_stats: Arc<crate::resources::LiveStats> = Default::default();
    let live: Arc<crate::resources::LiveStats> = Default::default();
    // What the host asks of us is done by the broker, with the policy *we* hold.
    let reply_out = out.clone();
    let broker = broker::spawn(BrokerEnv {
        extension: name.clone(),
        dir: ext.dir.clone(),
        data_dir: shared.data_root.join(&name),
        policy: ext.policy.clone(),
        storage: shared.storage.clone(),
        secrets: shared.secrets.clone(),
        limits: shared.limits,
        app_version: shared.app_version.clone(),
        stats: broker_stats.clone(),
        exec_exe: (opts.sandbox != SandboxMode::Off).then(|| opts.exe.clone()),
        reply: Arc::new(move |corr, result| {
            if let Err(e) = reply_out.send(Some(corr), &ToHost::Reply { result }) {
                // Most likely the answer did not fit in a frame: tell the extension instead of leaving it waiting.
                let _ = reply_out.send(Some(corr), &ToHost::Reply { result: Err(format!("the result could not be sent to the extension: {e}")) });
            }
        }),
    });

    // stderr: kept for crash reports and written to the log.
    let stderr = child.stderr.take();
    let (st, ext_name) = (state.clone(), name.clone());
    let stderr_thread = std::thread::Builder::new().name(format!("flux-err-{name}")).spawn(move || {
        let Some(pipe) = stderr else { return };
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            log::warn!("[{ext_name}] host: {line}");
            let mut tail = st.stderr_tail.lock().unwrap();
            if tail.len() >= 12 {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    })?;

    // reader: everything the host says.
    {
        let (state, dead, fatal, reason, sandbox, hb, broker, live) = (state.clone(), dead.clone(), fatal.clone(), reason.clone(), sandbox.clone(), heartbeat.clone(), broker.clone(), live.clone());
        let (events, owners, generation, ext_name) = (shared.events.clone(), shared.owners.clone(), shared.generation, name.clone());
        let strict = opts.sandbox == SandboxMode::Strict;
        std::thread::Builder::new().name(format!("flux-host-rx-{name}")).spawn(move || {
            let end: String = loop {
                let frame = match wire::read_frame::<_, FromHost>(&mut reader_stream) {
                    Ok(Some(f)) => f,
                    Ok(None) => break "the extension host closed the connection".to_string(),
                    Err(e) => {
                        kill_pid(&state);
                        break format!("protocol error from the extension host: {e}");
                    }
                };
                if frame.v < MIN_PROTOCOL_VERSION {
                    kill_pid(&state);
                    break format!("the extension host speaks protocol {} (this launcher needs {MIN_PROTOCOL_VERSION} or newer)", frame.v);
                }
                match frame.body {
                    FromHost::Ready { sandbox: report, .. } => {
                        if strict && !report.fully_confined() {
                            kill_pid(&state);
                            fatal.store(true, Ordering::Release);
                            let mut gaps = report.missing.clone();
                            if report.landlock_abi.is_none() {
                                gaps.push("no filesystem restriction (Landlock)".into());
                            }
                            if !report.seccomp {
                                gaps.push("no syscall filter (seccomp)".into());
                            }
                            break format!("the extension host is not fully confined: {}", gaps.join("; "));
                        }
                        *state.last_pong.lock().unwrap() = Some(Instant::now());
                        *sandbox.lock().unwrap() = Some(report);
                    }
                    FromHost::Failed { message } => {
                        fatal.store(true, Ordering::Release);
                        break message;
                    }
                    FromHost::Event { event } => {
                        match &event {
                            RuntimeEvent::Nav(NavEvent::Push { view_id, .. }) => {
                                owners.lock().unwrap().insert(*view_id, (ext_name.clone(), generation));
                            }
                            RuntimeEvent::Nav(NavEvent::Replace { old_view_id, view_id, .. }) => {
                                let mut o = owners.lock().unwrap();
                                o.remove(old_view_id);
                                o.insert(*view_id, (ext_name.clone(), generation));
                            }
                            RuntimeEvent::Nav(NavEvent::Pop { view_id }) => {
                                owners.lock().unwrap().remove(view_id);
                            }
                            _ => {}
                        }
                        let _ = events.try_send(event);
                    }
                    FromHost::Call { call } => broker.call(frame.id, call),
                    FromHost::CancelCall { call } => broker.cancel(call),
                    FromHost::Pong { busy_ms, stats } => {
                        live.store(&stats);
                        *state.last_pong.lock().unwrap() = Some(Instant::now());
                        hb.set_busy_for(busy_ms.map(Duration::from_millis));
                    }
                    FromHost::Unloaded => {
                        *state.unloaded.0.lock().unwrap() = true;
                        state.unloaded.1.notify_all();
                    }
                    FromHost::Unknown => log::debug!("[{ext_name}] ignoring a message kind this launcher does not know"),
                }
            };
            // The connection is over: collect the exit status for the report, then clean up.
            broker.stop();
            let status = wait_for_exit(&mut child, &state);
            // A grandchild may still hold the pipe open; the last lines are only for the report.
            let patience = Instant::now() + Duration::from_millis(300);
            while !stderr_thread.is_finished() && Instant::now() < patience {
                std::thread::sleep(Duration::from_millis(10));
            }
            let tail = state.stderr_tail.lock().unwrap().iter().rev().take(3).cloned().collect::<Vec<_>>();
            let mut why = format!("{end} ({status})");
            if !tail.is_empty() {
                why.push_str(&format!(": {}", tail.into_iter().rev().collect::<Vec<_>>().join(" | ")));
            }
            reason.lock().unwrap().get_or_insert(why);
            dead.store(true, Ordering::Release);
            state.unloaded.1.notify_all();
        })?;
    }

    // pump: requests from the supervisor, plus liveness probing.
    let (tx, rx) = std::sync::mpsc::channel::<WorkerMsg>();
    let alive = Arc::new(AtomicBool::new(true));
    let join = {
        let (state, dead, reason, out, broker, owners) = (state.clone(), dead.clone(), reason.clone(), out.clone(), broker.clone(), shared.owners.clone());
        let (ping_every, ping_timeout) = (Duration::from_millis(opts.ping_ms), Duration::from_millis(opts.ping_timeout_ms));
        std::thread::Builder::new().name(format!("flux-host-tx-{name}")).spawn(move || {
            let mut last_ping = Instant::now();
            loop {
                if dead.load(Ordering::Acquire) {
                    break;
                }
                match rx.recv_timeout(ping_every) {
                    Ok(WorkerMsg::Unload { done }) => {
                        let _ = out.send(None, &ToHost::Unload);
                        let guard = state.unloaded.0.lock().unwrap();
                        let _ = state.unloaded.1.wait_timeout_while(guard, Duration::from_millis(2500), |u| !*u && !dead.load(Ordering::Acquire));
                        let _ = done.send(());
                        break;
                    }
                    Ok(msg) => {
                        if let Err(e) = send_worker_msg(&out, &owners, msg) {
                            reason.lock().unwrap().get_or_insert(format!("could not reach the extension host: {e}"));
                            break;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
                if last_ping.elapsed() >= ping_every {
                    last_ping = Instant::now();
                    let _ = out.send(None, &ToHost::Ping);
                    let silent = state.last_pong.lock().unwrap().is_some_and(|t| t.elapsed() > ping_timeout);
                    if silent {
                        reason.lock().unwrap().get_or_insert("the extension host stopped answering and was killed".to_string());
                        break;
                    }
                }
            }
            // Whatever the reason we are leaving, the host does not outlive us.
            kill_pid(&state);
            broker.stop();
            dead.store(true, Ordering::Release);
        })?
    };

    Ok(WorkerHandle {
        tx: WorkerTx::Process { tx, dead },
        heartbeat,
        join: Some(join),
        reason,
        fatal,
        sandbox,
        live,
        broker_live: Some(broker_stats),
        pid: Some(state.pid.load(Ordering::Acquire)),
        generation: shared.generation,
        alive,
        started: Instant::now(),
    })
}

/// Waits briefly for the process to be gone (killing it if it lingers) and describes how it ended.
fn wait_for_exit(child: &mut Child, state: &State) -> String {
    let end = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                state.reaped.store(true, Ordering::Release);
                return describe(status);
            }
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let status = child.wait().map(describe).unwrap_or_else(|e| e.to_string());
                state.reaped.store(true, Ordering::Release);
                return status;
            }
            Err(e) => return e.to_string(),
        }
    }
}

fn describe(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(c), _) => format!("exit code {c}"),
        (_, Some(s)) => format!("killed by signal {s}"),
        _ => "unknown exit".into(),
    }
}

fn send_worker_msg(out: &Out, owners: &Arc<Mutex<std::collections::HashMap<u64, (String, u64)>>>, msg: WorkerMsg) -> std::io::Result<()> {
    let body = match msg {
        WorkerMsg::Run { command_id, name } => ToHost::Run { command_id, name },
        WorkerMsg::Invoke { view_id, action_id } => ToHost::Invoke { view_id, action_id },
        WorkerMsg::Submit { view_id, text } => ToHost::Submit { view_id, text },
        WorkerMsg::Search { view_id, text } => ToHost::Search { view_id, text },
        WorkerMsg::Selection { view_id, key } => ToHost::Selection { view_id, key },
        WorkerMsg::FormChanged { view_id, field, value } => ToHost::FormChanged { view_id, field, value },
        WorkerMsg::FormSubmit { view_id, values } => ToHost::FormSubmit { view_id, values },
        WorkerMsg::ViewClosed(view_id) => {
            owners.lock().unwrap().remove(&view_id);
            ToHost::ViewClosed { view_id }
        }
        WorkerMsg::CancelCommand(command_id) => ToHost::CancelCommand { command_id },
        WorkerMsg::Unload { .. } => unreachable!("handled by the pump"),
    };
    out.send(None, &body)
}
