use std::path::{Path, PathBuf};

use flux_api::{Limits, RestartPolicy};
use serde::Deserialize;

pub const DEFAULT_HOTKEY: &str = "ALT+space";

#[derive(Debug, Clone)]
pub struct Config {
    pub extension_dirs: Vec<PathBuf>,
    pub data_dir: PathBuf,
    /// Where `flux install` puts extensions (also scanned for extensions).
    pub installed_dir: PathBuf,
    /// Per-extension log files.
    pub log_dir: PathBuf,
    /// Reload extensions when files change on disk.
    pub dev: bool,
    /// Preferred trigger for the global shortcut (a hint: the compositor/portal has the final say).
    pub hotkey: String,
    /// After being hidden this long, the launcher reopens at the root with an empty search.
    pub reset_after_secs: u64,
    pub limits: Limits,
    pub restart: RestartPolicy,
    /// How extensions are isolated (`[host]` in config.toml). `None` = threads (no isolation).
    pub host: Option<flux_runtime::hostproc::HostOptions>,
    /// Indeterminate loading indicators do not animate.
    pub reduce_motion: bool,
}

/// `~/.config/flux/config.toml`
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    pub hotkey: Option<String>,
    pub reset_after_secs: Option<u64>,
    pub reduce_motion: Option<bool>,
    pub extension_dirs: Option<Vec<PathBuf>>,
    pub limits: FileLimits,
    pub restart: FileRestart,
    pub host: FileHost,
}

/// `[host]`: process isolation of extensions (see docs/security.md and docs/resources.md).
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileHost {
    /// "process" (default) or "thread" (no isolation; for development).
    pub isolation: Option<String>,
    /// "strict" (default: refuse to run an extension that cannot be confined), "best-effort" or "off".
    pub sandbox: Option<String>,
    /// Enforce memory/CPU/task limits with a systemd user scope when available (default true).
    pub cgroup: Option<bool>,
    pub memory_mb: Option<u64>,
    pub cpu_percent: Option<u32>,
    pub max_tasks: Option<u32>,
    pub max_open_files: Option<u64>,
    pub ping_timeout_ms: Option<u64>,
}

/// `[restart]`: backoff for extensions that crash (see docs/lifecycle.md).
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileRestart {
    pub base_ms: Option<u64>,
    pub max_ms: Option<u64>,
    pub max_crashes: Option<u32>,
    pub window_s: Option<u64>,
    pub stable_s: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileLimits {
    pub memory_mb: Option<usize>,
    pub slice_ms: Option<u64>,
    pub command_timeout_s: Option<u64>,
    pub max_http_concurrent: Option<usize>,
    pub max_children: Option<usize>,
    pub exec_output_kb: Option<usize>,
    pub exec_timeout_max_s: Option<u64>,
    pub http_max_mb: Option<usize>,
    pub max_coroutines: Option<usize>,
    pub max_tasks: Option<usize>,
}

impl FileConfig {
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }
}

impl Config {
    pub fn new(extension_dirs: Vec<PathBuf>, data_dir: PathBuf, dev: bool) -> Self {
        let installed_dir = data_dir.join("installed");
        let log_dir = data_dir.join("logs");
        Self { extension_dirs, data_dir, installed_dir, log_dir, dev, hotkey: DEFAULT_HOTKEY.into(), reset_after_secs: 60, limits: Limits::default(), restart: RestartPolicy::default(), host: None, reduce_motion: false }
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("flux.db")
    }

    pub fn apply(&mut self, file: FileConfig) {
        if let Some(h) = file.hotkey { self.hotkey = h }
        if let Some(s) = file.reset_after_secs { self.reset_after_secs = s }
        if let Some(d) = file.extension_dirs { self.extension_dirs = d }
        if let Some(v) = file.reduce_motion { self.reduce_motion = v }
        if let Some(v) = file.limits.memory_mb { self.limits.memory_mb = v }
        if let Some(v) = file.limits.slice_ms { self.limits.slice_ms = v }
        if let Some(v) = file.limits.command_timeout_s { self.limits.command_timeout_s = v }
        if let Some(v) = file.limits.max_http_concurrent { self.limits.max_http_concurrent = v.max(1) }
        if let Some(v) = file.limits.max_children { self.limits.max_children = v }
        if let Some(v) = file.limits.exec_output_kb { self.limits.exec_output_kb = v.max(1) }
        if let Some(v) = file.limits.exec_timeout_max_s { self.limits.exec_timeout_max_s = v.max(1) }
        if let Some(v) = file.limits.http_max_mb { self.limits.http_max_mb = v.max(1) }
        if let Some(v) = file.limits.max_coroutines { self.limits.max_coroutines = v.max(16) }
        if let Some(v) = file.limits.max_tasks { self.limits.max_tasks = v.max(4) }
        if let Some(v) = file.restart.base_ms { self.restart.base_ms = v }
        if let Some(v) = file.restart.max_ms { self.restart.max_ms = v }
        if let Some(v) = file.restart.max_crashes { self.restart.max_crashes = v.max(1) }
        if let Some(v) = file.restart.window_s { self.restart.window_s = v }
        if let Some(v) = file.restart.stable_s { self.restart.stable_s = v }
        match file.host.isolation.as_deref() {
            None | Some("process") => {}
            Some("thread") => self.host = None,
            Some(other) => log::warn!("config.toml: host.isolation '{other}' is not 'process' or 'thread'; using process isolation"),
        }
        if let Some(h) = &mut self.host {
            match file.host.sandbox.as_deref() {
                None | Some("strict") => {}
                Some("best-effort") => h.sandbox = flux_api::wire::SandboxMode::BestEffort,
                Some("off") => h.sandbox = flux_api::wire::SandboxMode::Off,
                Some(other) => log::warn!("config.toml: host.sandbox '{other}' is not strict, best-effort or off; using strict"),
            }
            if let Some(v) = file.host.cgroup { h.cgroup = v }
            if let Some(v) = file.host.memory_mb { h.memory_mb = v.max(64) }
            if let Some(v) = file.host.cpu_percent { h.cpu_percent = v.clamp(1, 10_000) }
            if let Some(v) = file.host.max_tasks { h.max_tasks = v.max(8) }
            if let Some(v) = file.host.max_open_files { h.max_open_files = v.max(32) }
            if let Some(v) = file.host.ping_timeout_ms { h.ping_timeout_ms = v.max(1000) }
        }
    }

    pub fn config_dir() -> PathBuf {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(".config")).join("flux")
    }

    /// Defaults, then `config.toml`, then `FLUX_EXTENSIONS_DIR` (`:`-separated).
    /// A broken config file is reported and ignored instead of preventing startup.
    pub fn from_env(dev: bool) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let data = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(".local/share")).join("flux");

        let mut dirs = Vec::new();
        if Path::new("extensions").is_dir() {
            dirs.push(PathBuf::from("extensions"));
        }
        dirs.push(Self::config_dir().join("extensions"));
        // Extensions shipped by a package or an install prefix: `$XDG_DATA_DIRS/flux/extensions`.
        let data_dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        let user_share = home.join(".local/share");
        for d in data_dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from).chain([user_share]) {
            let p = d.join("flux/extensions");
            if p.is_dir() && !dirs.contains(&p) {
                dirs.push(p);
            }
        }
        let mut cfg = Config::new(dirs, data, dev);
        // The real launcher isolates extensions in processes; the program that hosts them is itself.
        cfg.host = std::env::current_exe().ok().map(flux_runtime::hostproc::HostOptions::new);
        cfg.installed_dir = cfg.data_dir.join("installed");
        let state = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(".local/state")).join("flux");
        cfg.log_dir = state.join("logs");

        let path = Self::config_dir().join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => match FileConfig::parse(&text) {
                Ok(f) => cfg.apply(f),
                Err(e) => log::warn!("ignoring {}: {e}", path.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("cannot read {}: {e}", path.display()),
        }
        if let Some(v) = std::env::var_os("FLUX_EXTENSIONS_DIR") {
            cfg.extension_dirs = std::env::split_paths(&v).collect();
        }
        // Installed extensions are always scanned, wherever the other directories point.
        if !cfg.extension_dirs.contains(&cfg.installed_dir) {
            cfg.extension_dirs.push(cfg.installed_dir.clone());
        }
        cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_file_overrides() {
        let mut cfg = Config::new(vec!["ext".into()], "data".into(), false);
        assert_eq!((cfg.hotkey.as_str(), cfg.reset_after_secs, cfg.limits), ("ALT+space", 60, Limits::default()));
        let f = FileConfig::parse(
            r#"
            hotkey = "SUPER+space"
            reset_after_secs = 5
            extension_dirs = ["/x", "/y"]
            [limits]
            memory_mb = 8
            slice_ms = 100
            "#,
        )
        .unwrap();
        cfg.apply(f);
        assert_eq!(cfg.hotkey, "SUPER+space");
        assert_eq!(cfg.reset_after_secs, 5);
        assert_eq!(cfg.extension_dirs, [PathBuf::from("/x"), PathBuf::from("/y")]);
        assert_eq!((cfg.limits.memory_mb, cfg.limits.slice_ms, cfg.limits.command_timeout_s), (8, 100, 60));
    }

    #[test]
    fn unknown_keys_and_bad_types_are_errors() {
        assert!(FileConfig::parse("hotkeyy = 'x'").is_err());
        assert!(FileConfig::parse("reset_after_secs = 'soon'").is_err());
        assert!(FileConfig::parse("").is_ok());
    }
}
