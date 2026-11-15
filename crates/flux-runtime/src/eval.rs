//! Evaluating `manifest.lua` and `preferences.lua`.
//!
//! Declarations are Lua code written by the extension author, so the launcher does not run them in
//! its own process: it starts a throw-away, confined process (`<exe> host --eval <dir>`) that
//! evaluates them and prints the result as JSON. The process can read the extension folder and
//! nothing else, has no sockets, cannot start programs, and is killed after a few seconds. What
//! comes back is plain data, validated again here. (Tests and `isolation = "thread"` use the
//! in-process evaluator.)

use std::{
    collections::HashMap,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use flux_api::{PreferenceSpec, wire::SandboxMode};
use serde::{Deserialize, Serialize};

use crate::{
    confine::{self, Plan},
    hostproc::HostOptions,
    limits::Heartbeat,
    manifest::{KNOWN_KEYS, Manifest},
    preferences,
    sandbox::Sandbox,
};

/// Longest an evaluation may take, and the most output it may produce.
const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_OUTPUT: u64 = 1024 * 1024;

/// Everything an extension's declaration files say.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Declaration {
    pub manifest: Manifest,
    pub prefs: Vec<PreferenceSpec>,
    /// Top-level manifest keys this Flux does not know (a warning for `flux validate`).
    pub unknown_keys: Vec<String>,
}

pub enum Evaluator {
    /// In this process (tests, thread mode).
    Local(Sandbox),
    /// In a confined child process.
    Isolated(HostOptions),
}

impl Evaluator {
    /// Errors are prefixed with the file they come from (`manifest.lua: …`).
    pub fn declaration(&self, dir: &Path) -> Result<Declaration, Vec<String>> {
        match self {
            Self::Local(sb) => evaluate(sb, dir),
            Self::Isolated(opts) => evaluate_isolated(opts, dir),
        }
    }
}

/// The evaluation itself; used in-process and by the child.
pub fn evaluate(sb: &Sandbox, dir: &Path) -> Result<Declaration, Vec<String>> {
    let table = sb.eval_declaration(&dir.join("manifest.lua")).map_err(|e| vec![e])?;
    let mut unknown_keys = Vec::new();
    for (k, _) in table.pairs::<mlua::Value, mlua::Value>().flatten() {
        if let mlua::Value::String(k) = k {
            let k = k.to_string_lossy().to_string();
            if !KNOWN_KEYS.contains(&k.as_str()) {
                unknown_keys.push(k);
            }
        }
    }
    let manifest = Manifest::parse(&sb.lua, table).map_err(|errs| errs.into_iter().map(|e| format!("manifest.lua: {e}")).collect::<Vec<_>>())?;
    let prefs_path = dir.join("preferences.lua");
    let prefs = if prefs_path.is_file() {
        let t = sb.eval_declaration(&prefs_path).map_err(|e| vec![e])?;
        preferences::parse(&sb.lua, t).map_err(|e| vec![format!("preferences.lua: {e}")])?
    } else {
        vec![]
    };
    Ok(Declaration { manifest, prefs, unknown_keys })
}

type Cached = ((i64, i64), (i64, i64), Result<Declaration, Vec<String>>);

fn cache() -> &'static Mutex<HashMap<PathBuf, Cached>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, Cached>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// (mtime, size) of a file, or zeros when it does not exist: the cache key.
fn stamp(p: &Path) -> (i64, i64) {
    std::fs::metadata(p).map(|m| (m.mtime_nsec() + m.mtime() * 1_000_000_000, m.size() as i64)).unwrap_or((0, 0))
}

/// Runs the evaluation in a confined child process.
pub fn evaluate_isolated(opts: &HostOptions, dir: &Path) -> Result<Declaration, Vec<String>> {
    let key = (stamp(&dir.join("manifest.lua")), stamp(&dir.join("preferences.lua")));
    if let Some((a, b, r)) = cache().lock().unwrap().get(dir) {
        if (*a, *b) == key {
            return r.clone();
        }
    }
    let result = run_child(opts, dir);
    cache().lock().unwrap().insert(dir.to_path_buf(), (key.0, key.1, result.clone()));
    result
}

fn mode_arg(m: SandboxMode) -> &'static str {
    match m {
        SandboxMode::Strict => "strict",
        SandboxMode::BestEffort => "best-effort",
        SandboxMode::Off => "off",
    }
}

fn run_child(opts: &HostOptions, dir: &Path) -> Result<Declaration, Vec<String>> {
    let fail = |m: String| vec![format!("manifest.lua: {m}")];
    let mut child = Command::new(opts.eval_exe.as_ref().unwrap_or(&opts.exe))
        .args(["host", "--eval"])
        .arg(dir)
        .arg(mode_arg(opts.sandbox))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| fail(format!("could not start the declaration evaluator: {e}")))?;
    let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = (&mut out).take(MAX_OUTPUT).read_to_string(&mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = (&mut err).take(4096).read_to_string(&mut s);
        s
    });
    let end = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let stdout = reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    let Some(status) = status else {
        return Err(fail(format!("evaluating the declaration took longer than {} s and was stopped", TIMEOUT.as_secs())));
    };
    if !status.success() && stdout.trim().is_empty() {
        let why = stderr.lines().next().unwrap_or("it crashed").to_string();
        return Err(fail(format!("the declaration evaluator failed: {why}")));
    }
    // One line of JSON: Ok(declaration) or Err(messages).
    match serde_json::from_str::<Result<Declaration, Vec<String>>>(stdout.trim()) {
        Ok(r) => r,
        Err(e) => Err(fail(format!("unexpected answer from the declaration evaluator: {e}"))),
    }
}

/// `<exe> host --eval <dir> <mode>`: the child. Prints the result as one JSON line.
pub fn child_main(args: &[String]) -> i32 {
    let [dir, mode] = args else { return 2 };
    let mode = match mode.as_str() {
        "strict" => SandboxMode::Strict,
        "best-effort" => SandboxMode::BestEffort,
        _ => SandboxMode::Off,
    };
    let dir = PathBuf::from(dir);
    let plan = Plan { dir: dir.clone(), data_dir: None, network: false, max_open_files: 32, mode };
    let result: Result<Declaration, Vec<String>> = match confine::apply(&plan) {
        Err(e) => Err(vec![format!("manifest.lua: {e}")]),
        Ok(_) => match Sandbox::new(flux_api::Limits::default(), Heartbeat::default()) {
            Err(e) => Err(vec![format!("manifest.lua: could not create a Lua state: {e}")]),
            Ok(sb) => evaluate(&sb, &dir),
        },
    };
    println!("{}", serde_json::to_string(&result).unwrap_or_default());
    0
}
