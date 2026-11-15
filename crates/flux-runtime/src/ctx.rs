use std::{cell::RefCell, path::PathBuf, rc::{Rc, Weak}, sync::Arc};

use async_channel::Sender;
use flux_api::{Capability, PermissionSet, PreferenceSpec, RuntimeEvent, SecretStore, Storage};
use mlua::Function;

use crate::{http::HttpState, views::Views};

/// Context of a command run. It is captured by that run's `flux` functions, so
/// logs, storage and permissions always identify the responsible extension and
/// command. It holds no strong references to Lua values except `lifecycle`.
pub struct CmdCtx {
    pub extension: String,
    pub command: String,
    /// The effective permissions: the manifest narrowed by the user. Checked on every call.
    pub permissions: Arc<PermissionSet>,
    pub dir: PathBuf,
    pub data_dir: PathBuf,
    pub events: Sender<RuntimeEvent>,
    pub storage: Arc<dyn Storage>,
    pub secrets: Arc<dyn SecretStore>,
    pub app_version: String,
    pub views: Weak<Views>,
    pub prefs: Rc<Vec<PreferenceSpec>>,
    /// Functions registered with `flux.lifecycle.on_unload`.
    pub on_unload: Rc<RefCell<Vec<Function>>>,
    pub http: Rc<HttpState>,
    pub log_limiter: Rc<crate::api::LogLimiter>,
    /// Set when the extension runs in a host process: privileged functions go through it.
    pub link: Option<Arc<crate::remote::Link>>,
    /// Counters behind the limits that span commands (requests in flight, background processes).
    pub res: Rc<crate::resources::Resources>,
    /// Set in a host-process deployment: commands run confined through it.
    pub exec_wrapper: Option<crate::execwrap::ExecWrapper>,
}

impl CmdCtx {
    pub fn require(&self, permission: Capability) -> mlua::Result<()> {
        if self.permissions.has(permission) {
            Ok(())
        } else {
            Err(mlua::Error::runtime(format!(
                "extension '{}' does not have the '{}' permission (declare it in manifest.lua, and check it was not revoked with `flux permissions`)",
                self.extension,
                permission.as_str()
            )))
        }
    }

    pub fn emit(&self, event: RuntimeEvent) {
        let _ = self.events.try_send(event);
    }

    pub fn tag(&self) -> String {
        format!("{}:{}", self.extension, self.command)
    }

    pub fn views(&self) -> mlua::Result<Rc<Views>> {
        self.views.upgrade().ok_or_else(|| mlua::Error::runtime("the extension was unloaded"))
    }
}
