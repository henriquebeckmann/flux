//! Owns the screens an extension has open: their component tree, callbacks and (for
//! `ui.view{state, render}`) the Lua `self` object. It mirrors the extension's screen stack so
//! `flux.navigation.*` works without talking to the UI, and turns state changes into keyed
//! patches ([`flux_api::diff_views`]) instead of whole-screen replacements.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::{Rc, Weak},
    sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}},
};

use async_channel::Sender;
use flux_api::{NavEvent, RuntimeEvent, ViewNode, diff_views};
use mlua::{Function, Lua, Table, Value};

use crate::build::{Build, Interner, Policy, build_view};

/// What the views need from the worker that runs them (kept as closures so `Views` stays testable
/// and free of the worker's types).
#[derive(Clone)]
pub struct Hooks {
    /// A view became dirty: schedule a (coalesced) render pass.
    pub schedule_render: Rc<dyn Fn()>,
    /// `self:task(name, fn)`.
    pub start_task: Rc<dyn Fn(u64, String, Function)>,
    /// `self:cancel(name)`.
    pub cancel_task: Rc<dyn Fn(u64, String)>,
    /// The runtime itself closed a view (pop/replace): cancel what it was running.
    pub view_closed: Rc<dyn Fn(u64)>,
    /// Re-arms the CPU-slice deadline before running synchronous Lua (render, init, lifecycle callbacks).
    pub arm: Rc<dyn Fn()>,
}

struct Stateful {
    def: Table,
    me: Table,
}

struct Instance {
    tag: String,
    node: ViewNode,
    interner: Interner,
    callbacks: HashMap<u64, Function>,
    stateful: Option<Stateful>,
}

pub struct Views {
    me: Weak<Views>,
    lua: Lua,
    ext: String,
    events: Sender<RuntimeEvent>,
    view_ids: Arc<AtomicU64>,
    /// view id -> (extension name, worker generation); the supervisor uses it to route UI requests
    /// and ignores entries written by an outdated worker.
    owners: Arc<Mutex<HashMap<u64, (String, u64)>>>,
    generation: u64,
    dir: PathBuf,
    network: bool,
    filesystem: bool,
    permissions: Arc<flux_api::PermissionSet>,
    data_dir: PathBuf,
    hosts: Arc<crate::http::HostPolicy>,
    factory: RefCell<Option<Function>>,
    hooks: RefCell<Option<Hooks>>,
    instances: RefCell<HashMap<u64, Instance>>,
    stack: RefCell<Vec<u64>>,
    dirty: RefCell<Vec<u64>>,
    scheduled: Cell<bool>,
    rendering: Cell<bool>,
}

pub struct ViewsConfig {
    pub lua: Lua,
    pub ext: String,
    pub events: Sender<RuntimeEvent>,
    pub view_ids: Arc<AtomicU64>,
    pub owners: Arc<Mutex<HashMap<u64, (String, u64)>>>,
    pub generation: u64,
    pub dir: PathBuf,
    pub network: bool,
    pub filesystem: bool,
    pub permissions: Arc<flux_api::PermissionSet>,
    pub data_dir: PathBuf,
    pub hosts: Arc<crate::http::HostPolicy>,
}

type R<T> = Result<T, String>;

impl Views {
    pub fn new(c: ViewsConfig) -> Rc<Self> {
        Rc::new_cyclic(|me| Self {
            me: me.clone(),
            lua: c.lua,
            ext: c.ext,
            events: c.events,
            view_ids: c.view_ids,
            owners: c.owners,
            generation: c.generation,
            dir: c.dir,
            network: c.network,
            filesystem: c.filesystem,
            permissions: c.permissions,
            data_dir: c.data_dir,
            hosts: c.hosts,
            factory: RefCell::new(None),
            hooks: RefCell::new(None),
            instances: Default::default(),
            stack: Default::default(),
            dirty: Default::default(),
            scheduled: Cell::new(false),
            rendering: Cell::new(false),
        })
    }

    pub fn set_factory(&self, f: Function) {
        *self.factory.borrow_mut() = Some(f);
    }

    pub fn set_hooks(&self, h: Hooks) {
        *self.hooks.borrow_mut() = Some(h);
    }

    fn hooks(&self) -> Option<Hooks> {
        self.hooks.borrow().clone()
    }

    fn emit(&self, e: RuntimeEvent) {
        let _ = self.events.try_send(e);
    }

    fn report(&self, tag: &str, message: String) {
        log::error!("[{tag}] {message}");
        self.emit(RuntimeEvent::ExtensionError { extension: tag.to_string(), message });
    }

    fn next_id(&self) -> u64 {
        self.view_ids.fetch_add(1, Ordering::Relaxed) + 1
    }

    // ------------------------------------------------------------ navigation

    /// Builds a screen (static tree or `ui.view`), registers it and returns its id.
    fn instantiate(&self, tag: &str, value: Value) -> R<(u64, ViewNode)> {
        let id = self.next_id();
        let mut interner = Interner::default();
        let (value, stateful) = match &value {
            Value::Table(t) if t.get::<Option<String>>("__kind").ok().flatten().as_deref() == Some("view") => {
                let (me, def) = self.new_self(id, t)?;
                if let Some(init) = def.get::<Option<Function>>("init").map_err(|e| e.to_string())? {
                    self.arm();
                    init.call::<()>(me.clone()).map_err(|e| format!("init: {e}"))?;
                }
                let v = self.call_render(&def, &me)?;
                (v, Some(Stateful { def, me }))
            }
            _ => (value, None),
        };
        let (node, callbacks) = self.build(&mut interner, value)?;
        self.instances.borrow_mut().insert(id, Instance { tag: tag.into(), node: node.clone(), interner, callbacks, stateful });
        self.owners.lock().unwrap().insert(id, (self.ext.clone(), self.generation));
        Ok((id, node))
    }

    fn build(&self, interner: &mut Interner, value: Value) -> R<(ViewNode, HashMap<u64, Function>)> {
        let policy = Policy { dir: &self.dir, hosts: &self.hosts, network: self.network, filesystem: self.filesystem, permissions: &self.permissions, data_dir: &self.data_dir };
        let mut b = Build::new(policy, interner);
        let node = build_view(&mut b, value)?;
        Ok((node, b.callbacks))
    }

    fn arm(&self) {
        if let Some(h) = self.hooks() {
            (h.arm)();
        }
    }

    fn call_render(&self, def: &Table, me: &Table) -> R<Value> {
        self.arm();
        let render: Function = def.get("render").map_err(|_| "ui.view needs a render(self) function".to_string())?;
        self.rendering.set(true);
        let r = render.call::<Value>(me.clone());
        self.rendering.set(false);
        r.map_err(|e| format!("render: {e}"))
    }

    /// Opens a new screen on top of the stack.
    pub fn push(&self, tag: &str, value: Value) -> R<()> {
        let (id, node) = self.instantiate(tag, value)?;
        self.stack.borrow_mut().push(id);
        self.emit(RuntimeEvent::Nav(NavEvent::Push { view_id: id, view: node }));
        self.fire_named(id, "on_appear");
        Ok(())
    }

    /// Replaces the top screen (or pushes, if the stack is empty). On error nothing changes.
    pub fn replace(&self, tag: &str, value: Value) -> R<()> {
        let (id, node) = self.instantiate(tag, value)?;
        let old = self.stack.borrow_mut().pop();
        self.stack.borrow_mut().push(id);
        match old {
            Some(old_view_id) => {
                self.emit(RuntimeEvent::Nav(NavEvent::Replace { old_view_id, view_id: id, view: node }));
                self.drop_view(old_view_id, true);
            }
            None => self.emit(RuntimeEvent::Nav(NavEvent::Push { view_id: id, view: node })),
        }
        self.fire_named(id, "on_appear");
        Ok(())
    }

    pub fn pop(&self) {
        let top = self.stack.borrow().last().copied();
        if let Some(id) = top {
            self.pop_view(id);
        }
    }

    /// Pops `view_id` if it is the top screen.
    pub fn pop_view(&self, view_id: u64) {
        if self.stack.borrow().last() == Some(&view_id) {
            self.stack.borrow_mut().pop();
            self.drop_view(view_id, true);
            self.emit(RuntimeEvent::Nav(NavEvent::Pop { view_id }));
        }
    }

    /// The UI/core closed this screen: forget it (no event).
    pub fn close(&self, view_id: u64) {
        self.fire_named(view_id, "on_disappear");
        self.stack.borrow_mut().retain(|v| *v != view_id);
        self.drop_view(view_id, false);
    }

    fn drop_view(&self, id: u64, runtime_initiated: bool) {
        if runtime_initiated {
            self.fire_named(id, "on_disappear");
        }
        self.instances.borrow_mut().remove(&id);
        self.owners.lock().unwrap().remove(&id);
        self.dirty.borrow_mut().retain(|v| *v != id);
        if runtime_initiated {
            if let Some(h) = self.hooks() {
                (h.view_closed)(id);
            }
        }
    }

    /// Forgets every screen (a new command session starts, or the extension unloads).
    pub fn clear(&self) {
        let ids: Vec<u64> = self.instances.borrow().keys().copied().collect();
        for id in ids {
            self.drop_view(id, false);
        }
        self.stack.borrow_mut().clear();
    }

    // ------------------------------------------------------------- callbacks

    pub fn callback(&self, view_id: u64, action_id: u64) -> Option<(String, Function)> {
        let inst = self.instances.borrow();
        let i = inst.get(&view_id)?;
        i.callbacks.get(&action_id).map(|f| (i.tag.clone(), f.clone()))
    }

    /// A callback registered under a well-known path (`on_submit`, `on_search_change`, `field:<id>:on_change`…).
    pub fn named(&self, view_id: u64, path: &str) -> Option<(String, Function)> {
        let inst = self.instances.borrow();
        let i = inst.get(&view_id)?;
        let id = i.interner.lookup(path)?;
        i.callbacks.get(&id).map(|f| (i.tag.clone(), f.clone()))
    }

    /// Calls a lifecycle callback (`on_appear`, `on_disappear`) synchronously; they must be quick
    /// and start async work with `self:task`.
    fn fire_named(&self, view_id: u64, path: &str) {
        if let Some((tag, f)) = self.named(view_id, path) {
            self.arm();
            if let Err(e) = f.call::<()>(()) {
                self.report(&tag, format!("{path}: {e}"));
            }
        }
    }

    /// Per-field validators of a form: (field id, function).
    pub fn field_validators(&self, view_id: u64) -> Vec<(String, Function)> {
        let inst = self.instances.borrow();
        let Some(ViewNode::Form(f)) = inst.get(&view_id).map(|i| &i.node) else { return vec![] };
        f.fields
            .iter()
            .filter_map(|fl| {
                let id = inst.get(&view_id)?.interner.lookup(&format!("field:{}:validate", fl.id))?;
                Some((fl.id.clone(), inst.get(&view_id)?.callbacks.get(&id)?.clone()))
            })
            .collect()
    }

    // --------------------------------------------------------- reactive views

    /// Creates the Lua `self` of a `ui.view` (see `components.lua`).
    fn new_self(&self, view_id: u64, def: &Table) -> R<(Table, Table)> {
        let factory = self.factory.borrow().clone().ok_or("views are not initialised")?;
        let host = self.lua.create_table().map_err(|e| e.to_string())?;
        let weak = self.me.clone();
        let mk = |r: mlua::Result<Function>| r.map_err(|e| e.to_string());

        let w = weak.clone();
        host.set("invalidate", mk(self.lua.create_function(move |_, ()| {
            if let Some(v) = w.upgrade() {
                v.invalidate(view_id)?;
            }
            Ok(())
        }))?).map_err(|e| e.to_string())?;
        let w = weak.clone();
        host.set("set_search", mk(self.lua.create_function(move |_, text: String| {
            if let Some(v) = w.upgrade() {
                v.emit(RuntimeEvent::Nav(NavEvent::SetSearch { view_id, text }));
            }
            Ok(())
        }))?).map_err(|e| e.to_string())?;
        let w = weak.clone();
        host.set("task", mk(self.lua.create_function(move |_, (name, f): (String, Function)| {
            if let Some(h) = w.upgrade().and_then(|v| v.hooks()) {
                (h.start_task)(view_id, name, f);
            }
            Ok(())
        }))?).map_err(|e| e.to_string())?;
        let w = weak.clone();
        host.set("cancel", mk(self.lua.create_function(move |_, name: String| {
            if let Some(h) = w.upgrade().and_then(|v| v.hooks()) {
                (h.cancel_task)(view_id, name);
            }
            Ok(())
        }))?).map_err(|e| e.to_string())?;
        let w = weak.clone();
        host.set("push", mk(self.lua.create_function(move |_, view: Value| {
            let v = w.upgrade().ok_or_else(|| mlua::Error::runtime("the extension was unloaded"))?;
            let tag = v.tag_of(view_id);
            v.push(&tag, view).map_err(mlua::Error::runtime)
        }))?).map_err(|e| e.to_string())?;
        let w = weak.clone();
        host.set("pop", mk(self.lua.create_function(move |_, ()| {
            if let Some(v) = w.upgrade() {
                v.pop_view(view_id);
            }
            Ok(())
        }))?).map_err(|e| e.to_string())?;
        let w = weak;
        host.set("close", mk(self.lua.create_function(move |_, ()| {
            if let Some(v) = w.upgrade() {
                v.emit(RuntimeEvent::CloseLauncher);
            }
            Ok(())
        }))?).map_err(|e| e.to_string())?;

        let me: Table = factory.call((def.clone(), host)).map_err(|e| e.to_string())?;
        Ok((me, def.clone()))
    }

    pub fn tag_of(&self, view_id: u64) -> String {
        self.instances.borrow().get(&view_id).map(|i| i.tag.clone()).unwrap_or_else(|| self.ext.clone())
    }

    /// `self:set(…)` marks the view dirty; a render pass is scheduled by the worker.
    fn invalidate(&self, view_id: u64) -> mlua::Result<()> {
        if self.rendering.get() {
            return Err(mlua::Error::runtime("self:set() cannot be called while rendering: render must be a pure function of the state"));
        }
        if !self.instances.borrow().contains_key(&view_id) {
            return Ok(()); // the view is gone: late updates from a finishing task are ignored
        }
        if !self.dirty.borrow().contains(&view_id) {
            self.dirty.borrow_mut().push(view_id);
        }
        if !self.scheduled.replace(true) {
            if let Some(h) = self.hooks() {
                (h.schedule_render)();
            }
        }
        Ok(())
    }

    /// Re-renders every dirty view. A failing `render` reports the error and keeps the last good screen.
    pub fn render_dirty(&self) {
        self.scheduled.set(false);
        let ids: Vec<u64> = std::mem::take(&mut *self.dirty.borrow_mut());
        for id in ids {
            if let Err(e) = self.rerender(id) {
                let tag = self.tag_of(id);
                self.report(&tag, e);
            }
        }
    }

    fn rerender(&self, id: u64) -> R<()> {
        let (def, me, mut interner) = {
            let mut inst = self.instances.borrow_mut();
            let Some(i) = inst.get_mut(&id) else { return Ok(()) };
            let Some(s) = &i.stateful else { return Ok(()) };
            (s.def.clone(), s.me.clone(), std::mem::take(&mut i.interner))
        };
        // Neither render nor the build may hold a RefCell borrow: Lua metamethods can re-enter.
        let result = self.call_render(&def, &me).and_then(|v| self.build(&mut interner, v));
        let mut inst = self.instances.borrow_mut();
        let Some(i) = inst.get_mut(&id) else { return Ok(()) }; // closed meanwhile
        i.interner = interner;
        let (node, callbacks) = result?;
        i.callbacks = callbacks; // ids are stable, so late clicks on the old tree still resolve
        if let Some(patch) = diff_views(&i.node, &node) {
            log::debug!("[{}] view {id}: {} op(s)", i.tag, patch.op_count());
            i.node = node;
            self.emit(RuntimeEvent::Nav(NavEvent::Update { view_id: id, patch }));
        }
        Ok(())
    }

    /// Last tree sent to the core for `view_id` (tests, debugging).
    pub fn node(&self, view_id: u64) -> Option<ViewNode> {
        self.instances.borrow().get(&view_id).map(|i| i.node.clone())
    }
}
