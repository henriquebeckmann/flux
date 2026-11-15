//! Lua extension runtime. It does not depend on GPUI: it talks to the rest of
//! the app only through [`RuntimeRequest`]/[`RuntimeEvent`].
//!
//! Every extension runs in its **own thread with its own Lua state** (memory limit,
//! CPU-slice hook, wall-clock timeout, cancellation of in-flight work). A slow, crashing
//! or looping extension can therefore never block the UI nor other extensions.

mod api;
pub mod broker;
mod build;
pub mod confine;
mod ctx;
pub mod execwrap;
pub mod deprecations;
pub mod eval;
pub mod host;
pub mod hostproc;
mod http;
mod limits;
mod manifest;
mod preferences;
mod procinfo;
mod remote;
mod resources;
mod sandbox;
mod supervisor;
pub mod validate;
mod views;
mod worker;

use std::{path::PathBuf, sync::Arc};

pub use http::HostPolicy;
pub use manifest::{API_VERSION, FLUX_VERSION, SUPPORTED_API_VERSIONS, compare_versions, valid_name, valid_semver};

use async_channel::Sender;
use flux_api::{Limits, RestartPolicy, RuntimeEvent, RuntimeRequest, SecretStore, Storage};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct RuntimeConfig {
    /// Directories that contain one folder per extension (each with a `manifest.lua`).
    pub extension_dirs: Vec<PathBuf>,
    /// The directory `flux install` writes to (extensions found there are reported as installed).
    pub installed_dir: Option<PathBuf>,
    /// Extensions that start disabled.
    pub disabled: std::collections::HashSet<String>,
    /// Root of the per-extension data directories.
    pub data_dir: PathBuf,
    pub storage: Arc<dyn Storage>,
    pub secrets: Arc<dyn SecretStore>,
    pub app_version: String,
    pub limits: Limits,
    pub restart: RestartPolicy,
    /// `permissions.toml`: what the user has revoked or narrowed per extension. Read on every (re)load.
    pub permissions_file: Option<PathBuf>,
    /// `Some`: every extension runs in its own host process. `None`: in a thread of the launcher
    /// (no isolation; for tests and development).
    pub host: Option<hostproc::HostOptions>,
}

#[derive(Clone)]
pub struct RuntimeHandle {
    tx: mpsc::UnboundedSender<RuntimeRequest>,
}

impl RuntimeHandle {
    /// A handle not connected to any supervisor: requests land in the returned receiver.
    /// Lets the core (and its tests) be exercised without starting Lua.
    pub fn detached() -> (RuntimeHandle, mpsc::UnboundedReceiver<RuntimeRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (RuntimeHandle { tx }, rx)
    }

    /// Snapshot of every extension's resource use (blocks for up to two seconds).
    pub fn stats(&self) -> Vec<flux_api::ExtensionStats> {
        let (tx, rx) = async_channel::bounded(1);
        self.send(RuntimeRequest::Stats { reply: tx });
        let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < end {
            if let Ok(v) = rx.try_recv() {
                return v;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Vec::new()
    }

    pub fn send(&self, request: RuntimeRequest) {
        let _ = self.tx.send(request);
    }
}

/// Starts the supervisor thread. Commands are announced via `events` right away.
pub fn spawn(config: RuntimeConfig, events: Sender<RuntimeEvent>) -> RuntimeHandle {
    let (tx, mut rx) = mpsc::unbounded_channel::<RuntimeRequest>();
    std::thread::Builder::new()
        .name("flux-supervisor".into())
        .spawn(move || {
            let mut sup = supervisor::Supervisor::new(
                supervisor::Layout { dirs: config.extension_dirs, installed_dir: config.installed_dir, disabled: config.disabled, single: vec![] },
                config.data_dir,
                config.storage,
                config.secrets,
                config.limits,
                config.app_version,
                events,
                config.restart,
                config.permissions_file,
                config.host,
            );
            sup.reload();
            // Besides requests, the supervisor wakes up regularly to notice dead workers and to
            // perform backed-off restarts.
            let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().expect("supervisor runtime");
            rt.block_on(async move {
                let period = std::time::Duration::from_millis(250);
                let mut last_tick = std::time::Instant::now();
                loop {
                    match tokio::time::timeout(period, rx.recv()).await {
                        Ok(Some(req)) => {
                            let stop = matches!(req, RuntimeRequest::Shutdown);
                            sup.handle(req);
                            if stop {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(_) => {}
                    }
                    if last_tick.elapsed() >= period {
                        sup.tick();
                        last_tick = std::time::Instant::now();
                    }
                }
            });
        })
        .expect("spawn supervisor thread");
    RuntimeHandle { tx }
}
