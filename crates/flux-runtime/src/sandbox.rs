use std::{cell::Cell, path::Path, rc::Rc, time::Duration};

use flux_api::Limits;
use mlua::{Function, Lua, Table};

use crate::{ctx::CmdCtx, limits::{Heartbeat, SliceGuard}};

/// One isolated Lua state: sandboxed globals, memory limit and time-slice hook.
pub struct Sandbox {
    pub lua: Lua,
    pub guard: SliceGuard,
    pub base_env: Table,
    make_env: Function,
    extend: Function,
    read_module: Function,
    pub new_instance: Function,
}

impl Sandbox {
    pub fn new(limits: Limits, heartbeat: Heartbeat) -> mlua::Result<Self> {
        let lua = Lua::new();
        let tripped = Rc::new(Cell::new(false));
        let prelude: Table = lua.load(include_str!("prelude.lua")).set_name("=prelude").call((lua.globals(), limits.max_coroutines))?;
        let t = tripped.clone();
        let is_tripped = lua.create_function(move |_, ()| Ok(t.get()))?;
        let base_env: Table = prelude.get::<Function>("safe_base")?.call(is_tripped)?;
        let read_module = lua.create_function(|_, (dir, name): (String, String)| Ok(read_module(Path::new(&dir), &name)))?;
        // The hook is installed after the prelude so that start-up is never limited.
        let guard = SliceGuard::install(&lua, limits.memory_mb, Duration::from_millis(limits.slice_ms), heartbeat, tripped)?;
        Ok(Self {
            make_env: prelude.get("make_env")?,
            new_instance: prelude.get("new_instance")?,
            extend: prelude.get("extend")?,
            read_module,
            base_env,
            guard,
            lua,
        })
    }

    /// Evaluates a declarative file (`manifest.lua`, `preferences.lua`) without access to `flux`.
    pub fn eval_declaration(&self, path: &Path) -> Result<Table, String> {
        let src = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.guard.arm();
        self.lua
            .load(&src)
            .set_name(format!("@{}", path.display()))
            .set_environment(self.base_env.clone())
            .eval()
            .map_err(|e| format!("{}: {e}", path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()))
    }

    /// Creates a run's environment: sandbox globals + the `flux` object for `ctx`.
    pub fn env_for(&self, ctx: &Rc<CmdCtx>) -> mlua::Result<Table> {
        let flux = crate::api::build_flux(&self.lua, ctx, &self.extend)?;
        self.make_env.call((self.base_env.clone(), flux, ctx.dir.to_string_lossy().into_owned(), self.read_module.clone()))
    }
}

/// Resolves `require("a.b")` to `<dir>/a/b.lua` or `<dir>/a/b/init.lua`, never leaving `dir`.
fn read_module(dir: &Path, name: &str) -> (Option<String>, String) {
    let ok = !name.is_empty()
        && name.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-'));
    if !ok {
        return (None, String::new());
    }
    let rel: std::path::PathBuf = name.split('.').collect();
    for candidate in [dir.join(&rel).with_extension("lua"), dir.join(&rel).join("init.lua")] {
        if let Ok(src) = std::fs::read_to_string(&candidate) {
            return (Some(src), format!("@{}", candidate.display()));
        }
    }
    (None, String::new())
}
