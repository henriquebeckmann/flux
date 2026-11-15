//! Contracts shared by the UI, core and extension runtime.
//!
//! This crate depends on neither `gpui` nor `mlua`: it only describes data and
//! messages. The UI never talks to Lua directly — everything goes through
//! [`RuntimeRequest`] (UI → runtime) and [`RuntimeEvent`] (runtime → UI).

use serde::{Deserialize, Serialize};

mod mem;
mod patch;
mod permissions;
mod shortcut;
mod view;
pub mod wire;

pub use mem::{MemSecretStore, UnavailableSecretStore};
pub use permissions::{Capability, Grants, LEGACY, PermissionSet, RESERVED, UserOverride, UserPermissions};
pub use patch::{ItemOp, PatchError, SectionOp, ViewPatch, apply as apply_patch, diff as diff_views};
pub use shortcut::{KeyInput, Shortcut};
pub use view::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommandMode {
    /// Shows a UI in the launcher.
    View,
    /// Runs a task without a UI.
    NoView,
}

/// A command listed/runnable in the launcher (built-in or from an extension).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandInfo {
    /// Unique id: `"<extension>:<command>"` or `"builtin:<name>"`.
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub icon: Option<String>,
    pub mode: CommandMode,
    /// `None` for built-in commands.
    pub extension: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionStyle {
    #[default]
    Default,
    Secondary,
    Destructive,
}

impl ActionStyle {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "default" => Self::Default,
            "secondary" => Self::Secondary,
            "destructive" => Self::Destructive,
            _ => return None,
        })
    }
}

/// Asks the user to confirm before a (usually destructive) action runs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Confirm {
    pub title: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ActionSpec {
    /// Opaque reference to the Lua callback stored in the runtime.
    pub id: u64,
    pub title: String,
    pub description: Option<String>,
    pub shortcut: Option<Shortcut>,
    pub style: ActionStyle,
    pub confirm: Option<Confirm>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ActionSection {
    pub title: Option<String>,
    pub actions: Vec<ActionSpec>,
}

/// An action together with the section it belongs to and its *effective* shortcut:
/// the first action defaults to `enter`, the second to `ctrl+enter`, unless they declare one.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAction<'a> {
    pub section: usize,
    pub spec: &'a ActionSpec,
    pub shortcut: Option<Shortcut>,
}

pub fn resolve_actions(sections: &[ActionSection]) -> Vec<ResolvedAction<'_>> {
    let mut out = Vec::new();
    for (si, section) in sections.iter().enumerate() {
        for spec in &section.actions {
            let shortcut = spec.shortcut.clone().or_else(|| match out.len() {
                0 => Some(Shortcut::new("enter")),
                1 => Some(Shortcut::new("enter").ctrl()),
                _ => None,
            });
            out.push(ResolvedAction { section: si, spec, shortcut });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Preferences (declared in `preferences.lua`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PreferenceKind {
    Text,
    Password,
    Checkbox,
    Dropdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PrefValue {
    Bool(bool),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreferenceOption {
    pub title: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreferenceSpec {
    pub name: String,
    pub title: String,
    pub description: Option<String>,
    pub kind: PreferenceKind,
    pub required: bool,
    pub default: Option<PrefValue>,
    pub options: Vec<PreferenceOption>,
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// UI → runtime.
#[derive(Debug, Clone)]
pub enum RuntimeRequest {
    RunCommand { command_id: String },
    InvokeAction { view_id: u64, action_id: u64 },
    /// Text submitted from a [`ViewNode::Input`] screen.
    SubmitInput { view_id: u64, text: String },
    /// The screen no longer exists (popped, reset): drop its callbacks and cancel its tasks.
    ViewClosed { view_id: u64 },
    /// The search field of a list/grid changed (only sent if the view declared `on_search_change`).
    SearchChanged { view_id: u64, text: String },
    /// The selected item changed (only sent if the view declared `on_selection_change`).
    SelectionChanged { view_id: u64, key: Option<String> },
    /// A form field changed (only sent if the field declared `on_change`).
    FormChanged { view_id: u64, field: String, value: FieldValue },
    /// The user submitted a form whose declarative rules passed.
    FormSubmit { view_id: u64, values: Vec<(String, FieldValue)> },
    /// Cancel a command that is still running (e.g. user left the loading screen).
    CancelCommand { command_id: String },
    /// Reloads all extensions (hot reload).
    Reload,
    /// Reloads one extension, identified by its name or by its folder name.
    ReloadExtension { name: String },
    /// Also load the extension in this folder (`flux dev`); the folder itself has a `manifest.lua`.
    AddExtensionDir { path: std::path::PathBuf },
    RemoveExtensionDir { path: std::path::PathBuf },
    /// Enables or disables an extension without touching its files.
    SetEnabled { name: String, enabled: bool },
    /// Asks for a snapshot of every extension's resource use (`flux status`).
    Stats { reply: async_channel::Sender<Vec<ExtensionStats>> },
    Shutdown,
}

/// Resource use of one extension, for diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionStats {
    pub name: String,
    pub state: String,
    pub isolation: String,
    pub generation: u64,
    /// The host process (`None` for threads or when not running).
    pub pid: Option<u32>,
    pub uptime_s: u64,
    /// Crashes within the restart window.
    pub recent_crashes: u32,
    pub rss_kb: Option<u64>,
    pub cpu_ms: Option<u64>,
    pub tasks: u32,
    pub http_in_flight: u32,
    pub children: u32,
    pub lua_memory_kb: u64,
}

/// Navigation changes decided by the runtime. The core's `Navigator` applies them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NavEvent {
    Push { view_id: u64, view: ViewNode },
    /// Replaces the top screen (`old_view_id`) with a new one.
    Replace { old_view_id: u64, view_id: u64, view: ViewNode },
    Pop { view_id: u64 },
    /// The extension replaced the text of the screen's search field (`self:set_search`).
    SetSearch { view_id: u64, text: String },
    /// The extension re-rendered the screen: apply the keyed patch in place.
    Update { view_id: u64, patch: ViewPatch },
    /// The extension rejected a form submission: (field id, message) pairs.
    FormErrors { view_id: u64, errors: Vec<(String, String)> },
}

/// Runtime → UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuntimeEvent {
    CommandsRegistered(Vec<CommandInfo>),
    Nav(NavEvent),
    CloseLauncher,
    /// A command finished running (with or without error).
    CommandFinished { command_id: String },
    ExtensionError { extension: String, message: String },
    /// A line from `flux.logger` (rate-limited by the runtime).
    Log { extension: String, level: LogLevel, message: String },
    /// Every discovered extension with its status; sent after each (re)load.
    ExtensionsChanged(Vec<ExtensionInfo>),
    /// A worker for `extension` started (`generation` grows with every start).
    ExtensionLoaded { extension: String, generation: u64 },
    /// The extension was disabled or removed. `views` are the screens it owned: they no longer exist.
    ExtensionUnloaded { extension: String, generation: u64, views: Vec<u64> },
    /// The extension was reloaded. `views` belong to the previous generation and are gone.
    ExtensionReloaded { extension: String, generation: u64, views: Vec<u64> },
    /// The worker died or hung. `views` are gone; a restart may follow (see [`RestartPolicy`]).
    ExtensionCrashed { extension: String, generation: u64, reason: String, views: Vec<u64> },
    /// A command began running.
    CommandStarted { command_id: String },
    /// A command was aborted before it finished (the user left, or the extension went away).
    CommandCancelled { command_id: String },
}

/// Persistence exposed to the runtime. Implemented by the core (SQLite); the
/// runtime only knows this contract. All access is isolated per `extension`.
pub trait Storage: Send + Sync + 'static {
    fn local_get(&self, extension: &str, key: &str) -> Option<String>;
    fn local_set(&self, extension: &str, key: &str, json: &str);
    fn local_remove(&self, extension: &str, key: &str);
    fn local_clear(&self, extension: &str);

    fn cache_get(&self, extension: &str, key: &str) -> Option<String>;
    fn cache_set(&self, extension: &str, key: &str, json: &str, ttl_secs: u64);

    /// Non-secret preference values, stored as JSON. Password preferences never come here.
    fn pref_get(&self, extension: &str, name: &str) -> Option<String>;
    fn pref_set(&self, extension: &str, name: &str, json: &str);
}

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretError(pub String);

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for SecretError {}

/// Secure storage for credentials (Secret Service on Linux). Implementations must never
/// fall back to plaintext on disk; when unavailable they return an error.
pub trait SecretStore: Send + Sync + 'static {
    fn get<'a>(&'a self, extension: &'a str, key: &'a str) -> BoxFuture<'a, Result<Option<String>, SecretError>>;
    fn set<'a>(&'a self, extension: &'a str, key: &'a str, value: &'a str) -> BoxFuture<'a, Result<(), SecretError>>;
    fn remove<'a>(&'a self, extension: &'a str, key: &'a str) -> BoxFuture<'a, Result<(), SecretError>>;
}

// ---------------------------------------------------------------------------
// Extension management
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExtensionSource {
    /// Found in a development directory (`./extensions`, `flux dev <path>`).
    Dev,
    /// Installed with `flux install` (the user extensions directory).
    Installed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum ExtensionStatus {
    Loaded,
    Disabled,
    /// The manifest (or a file it needs) is invalid; the extension is not running.
    Invalid { errors: Vec<String> },
    /// The worker crashed or was restarted after hanging.
    Crashed { message: String },
    /// The worker kept crashing and will not be restarted until it is reloaded by hand.
    Failed { message: String },
}

/// Everything the launcher shows about an extension (Manage Extensions, `flux list`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionInfo {
    /// Unique id (the manifest `name`; the folder name for extensions whose manifest is invalid).
    pub name: String,
    pub title: String,
    pub version: String,
    pub description: Option<String>,
    pub author: Option<String>,
    pub license: Option<String>,
    pub dir: String,
    pub source: ExtensionSource,
    pub enabled: bool,
    pub status: ExtensionStatus,
    pub permissions: Vec<String>,
    pub allowed_hosts: Vec<String>,
    pub commands: Vec<String>,
    /// How the extension is isolated, in words: "process, confined", "process, partly confined: …" or "thread, not isolated".
    #[serde(default)]
    pub isolation: String,
    /// Permissions the manifest asked for that the user has taken away (`permissions.toml`).
    #[serde(default)]
    pub revoked_permissions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// When and how often a crashed extension is restarted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartPolicy {
    /// Delay before the first restart; doubles with every crash in the window.
    pub base_ms: u64,
    pub max_ms: u64,
    /// This many crashes within `window_s` put the extension in the `Failed` state.
    pub max_crashes: u32,
    pub window_s: u64,
    /// A restarted worker that stays up this long is considered healthy again.
    pub stable_s: u64,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self { base_ms: 1000, max_ms: 60_000, max_crashes: 5, window_s: 300, stable_s: 30 }
    }
}

impl RestartPolicy {
    /// Delay before restart number `crashes_in_window` (1-based).
    pub fn delay_ms(&self, crashes_in_window: u32) -> u64 {
        let shift = crashes_in_window.saturating_sub(1).min(20);
        self.base_ms.saturating_mul(1u64 << shift).min(self.max_ms)
    }
}

/// Execution limits applied to every extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub memory_mb: usize,
    /// Max CPU time a Lua coroutine may run between two suspension points.
    pub slice_ms: u64,
    /// Max wall-clock time of one command/callback.
    pub command_timeout_s: u64,
    /// `flux.http` requests in flight at once (more are refused, not queued).
    pub max_http_concurrent: usize,
    /// Background processes started with `flux.process.spawn` that may be alive at once.
    pub max_children: usize,
    /// Output kept from `flux.process.exec` (per stream); the rest is read and dropped.
    pub exec_output_kb: usize,
    /// Upper bound for the `timeout` of `flux.process.exec`.
    pub exec_timeout_max_s: u64,
    /// Upper bound for the `max_bytes` of `flux.http` (the launcher makes the request and returns the whole body in one message).
    pub http_max_mb: usize,
    /// Live coroutines per Lua state.
    pub max_coroutines: usize,
    /// Background tasks (commands, callbacks, `self:task`) running at once.
    pub max_tasks: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_mb: 64,
            slice_ms: 500,
            command_timeout_s: 60,
            max_http_concurrent: 8,
            max_children: 16,
            exec_output_kb: 10 * 1024,
            exec_timeout_max_s: 600,
            http_max_mb: 8,
            max_coroutines: 1000,
            max_tasks: 64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_delay_doubles_and_is_capped() {
        let p = RestartPolicy::default();
        assert_eq!((p.delay_ms(1), p.delay_ms(2), p.delay_ms(3)), (1000, 2000, 4000));
        assert_eq!(p.delay_ms(30), 60_000);
        assert_eq!(p.delay_ms(0), 1000);
    }

    fn act(id: u64, shortcut: Option<&str>) -> ActionSpec {
        ActionSpec { id, title: format!("a{id}"), description: None, shortcut: shortcut.map(|s| Shortcut::parse(s).unwrap()), style: ActionStyle::Default, confirm: None }
    }

    #[test]
    fn primary_and_secondary_shortcuts_are_resolved() {
        let sections = vec![
            ActionSection { title: None, actions: vec![act(1, None), act(2, None)] },
            ActionSection { title: Some("More".into()), actions: vec![act(3, None), act(4, Some("ctrl+d"))] },
        ];
        let r = resolve_actions(&sections);
        assert_eq!(r.len(), 4);
        assert_eq!(r[0].shortcut, Some(Shortcut::new("enter")));
        assert_eq!(r[1].shortcut, Some(Shortcut::new("enter").ctrl()));
        assert_eq!(r[2].shortcut, None);
        assert_eq!(r[3].shortcut.as_ref().unwrap().to_string(), "ctrl+d");
        assert_eq!((r[2].section, r[3].section), (1, 1));
    }

    #[test]
    fn global_actions_follow_item_actions() {
        let view = ListView {
            sections: vec![Section { key: "s".into(), items: vec![ListItem { key: "x".into(), title: "x".into(), actions: vec![ActionSection { title: None, actions: vec![act(1, None)] }], ..Default::default() }], ..Default::default() }],
            actions: vec![ActionSection { title: Some("Global".into()), actions: vec![act(9, None)] }],
            ..Default::default()
        };
        let s = view.sections_for(0);
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].actions[0].id, 9);
        assert_eq!(view.sections_for(5).len(), 1); // unknown item -> only global
    }
}
