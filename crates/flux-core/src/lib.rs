//! Flux core: command registry, history, persistence, navigation and the bridge to the
//! extension runtime. It knows nothing about GPUI: every rule that can be tested without a
//! window (selection, filtering, grid movement, forms, panels, confirmations, management) lives here.

mod config;
pub mod dates;
mod db;
mod extensions;
mod form;
mod gridnav;
mod logs;
mod manage;
pub mod markdown;
mod navigator;
mod panel;
pub mod perms;
mod search;
mod visible;
mod watch;

use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use async_channel::Receiver;
pub use config::{Config, DEFAULT_HOTKEY, FileConfig};
pub use db::Db;
pub use extensions::{ExtError, InstallOutcome, Installed, Manager, PACKAGE_EXTENSION, Packaged, package};
use flux_api::*;
use flux_runtime::{RuntimeConfig, RuntimeHandle};
pub use form::{FormState, is_focusable, validate as validate_field};
pub use gridnav::{GridLayout, GridRow, columns_for};
pub use logs::{ExtLogs, LogLine, fmt_ts, log_path, tail_file};
pub use manage::NativeAction;
pub use navigator::{Closed, Entry, Navigator, Outcome, Screen};
pub use panel::{ActionPanelModel, PanelEntry, PanelRow, PanelTarget};
pub use search::{fuzzy_score, search};
pub use visible::{DisplayRow, VItem, VSection, Visible};

const BUILTIN_RELOAD: &str = "builtin:reload";
const BUILTIN_QUIT: &str = "builtin:quit";
const BUILTIN_EXTENSIONS: &str = "builtin:extensions";
/// View ids at or above this belong to screens built by the core itself.
const NATIVE_BASE: u64 = 1 << 48;
const HISTORY_LIMIT: usize = 50;

fn builtins() -> Vec<CommandInfo> {
    let mk = |id: &str, title: &str, subtitle: &str, icon: &str| CommandInfo {
        id: id.into(),
        title: title.into(),
        subtitle: Some(subtitle.into()),
        icon: Some(icon.into()),
        mode: CommandMode::NoView,
        extension: None,
    };
    let mut ext = mk(BUILTIN_EXTENSIONS, "Manage Extensions", "Enable, disable, reload, inspect and remove extensions", "package");
    ext.mode = CommandMode::View;
    vec![
        ext,
        mk(BUILTIN_RELOAD, "Reload Extensions", "Reloads the Lua extensions", "refresh"),
        mk(BUILTIN_QUIT, "Quit Flux", "Closes the launcher", "x"),
    ]
}

/// Something the application shell must do, produced by the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Hide the launcher window (the process stays alive).
    Hide,
    /// Exit the application.
    Quit,
    /// Open a file or folder with the desktop's default application.
    OpenPath(String),
    /// Open an http(s)/mailto link in the browser.
    OpenUrl(String),
    CopyText(String),
}

/// One visible list item, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemData {
    pub key: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub icon: Option<Icon>,
    pub accessories: Vec<Accessory>,
}

/// One visible grid cell, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct CellData {
    pub key: String,
    pub content: GridContent,
    pub title: Option<String>,
    pub subtitle: Option<String>,
}

/// What is shown for the current screen and search: the visible items and the display rows.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Model {
    pub vis: Visible,
    pub rows: Vec<DisplayRow>,
    pub layout: Option<GridLayout>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DetailItem {
    Markdown(Vec<markdown::Block>),
    ProgressBar { value: Option<f32>, label: Option<String> },
    Loading { label: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DetailModel {
    pub items: Vec<DetailItem>,
    pub metadata: Vec<MetaItem>,
}

/// A destructive action waiting for the user's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Confirmation {
    pub title: String,
    pub message: Option<String>,
    pub destructive: bool,
    view_id: u64,
    action_id: u64,
}

pub struct Core {
    db: Arc<Db>,
    runtime: RuntimeHandle,
    extension_commands: Vec<CommandInfo>,
    root_results: Vec<CommandInfo>,
    nav: Navigator,
    panel: Option<ActionPanelModel>,
    confirm: Option<Confirmation>,
    error: Option<String>,
    history: Vec<String>,
    history_idx: Option<usize>,
    hidden_since: Option<Instant>,
    reset_after: Duration,
    epoch: u64,
    /// Bumped whenever what is visible may have changed (invalidates the memoised model).
    rev: u64,
    grid_cols: usize,
    memo: RefCell<Option<(u64, usize, Rc<Model>)>>,
    detail_memo: RefCell<Option<(u64, Rc<DetailModel>)>>,
    pending: Vec<Effect>,
    // management
    infos: Vec<ExtensionInfo>,
    logs: Arc<ExtLogs>,
    manager: Option<Manager>,
    data_root: Option<PathBuf>,
    log_dir: Option<PathBuf>,
    /// Where `permissions.toml` lives.
    pub(crate) config_dir: PathBuf,
    native: HashMap<u64, HashMap<u64, NativeAction>>,
    next_native: u64,
    manage_view: Option<u64>,
    singles: Option<watch::Singles>,
}

impl Core {
    /// Opens the database, starts the Lua runtime and (in `dev`) hot reload.
    pub fn start(config: &Config, secrets: Arc<dyn SecretStore>) -> anyhow::Result<(Core, Receiver<RuntimeEvent>)> {
        let db = Arc::new(Db::open(&config.db_path())?);
        let (tx, rx) = async_channel::unbounded();
        let data_root = config.data_dir.join("extensions");
        let runtime = flux_runtime::spawn(
            RuntimeConfig {
                extension_dirs: config.extension_dirs.clone(),
                installed_dir: Some(config.installed_dir.clone()),
                disabled: db.disabled_extensions(),
                data_dir: data_root.clone(),
                storage: db.clone(),
                secrets,
                app_version: env!("CARGO_PKG_VERSION").into(),
                limits: config.limits,
                restart: config.restart,
                permissions_file: Some(Config::config_dir().join("permissions.toml")),
                host: config.host.clone(),
            },
            tx,
        );
        let singles = config.dev.then(|| watch::spawn(config.extension_dirs.clone(), runtime.clone()));
        let mut core = Core::new(db.clone(), runtime, Duration::from_secs(config.reset_after_secs));
        core.singles = singles;
        core.manager = Some(Manager::new(config.installed_dir.clone(), db));
        core.logs = Arc::new(ExtLogs::with_files(config.log_dir.clone()));
        core.data_root = Some(data_root);
        core.log_dir = Some(config.log_dir.clone());
        Ok((core, rx))
    }

    /// Assembles a core from parts (used by `start` and by tests with a detached runtime).
    pub fn new(db: Arc<Db>, runtime: RuntimeHandle, reset_after: Duration) -> Core {
        let mut core = Core {
            db,
            runtime,
            extension_commands: vec![],
            root_results: vec![],
            nav: Navigator::default(),
            panel: None,
            confirm: None,
            error: None,
            history: vec![],
            history_idx: None,
            hidden_since: None,
            reset_after,
            epoch: 0,
            rev: 0,
            grid_cols: 5,
            memo: RefCell::new(None),
            detail_memo: RefCell::new(None),
            pending: vec![],
            infos: vec![],
            logs: Arc::new(ExtLogs::in_memory()),
            manager: None,
            data_root: None,
            log_dir: None,
            config_dir: Config::config_dir(),
            native: HashMap::new(),
            next_native: 0,
            manage_view: None,
            singles: None,
        };
        core.refresh_root();
        core
    }

    pub fn logs(&self) -> &Arc<ExtLogs> {
        &self.logs
    }

    pub fn extensions(&self) -> &[ExtensionInfo] {
        &self.infos
    }

    // ------------------------------------------------------------ queries

    pub fn commands(&self) -> Vec<CommandInfo> {
        let mut all = self.extension_commands.clone();
        all.extend(builtins());
        all
    }

    /// Ranked commands for an arbitrary query (frecency applied).
    pub fn search(&self, query: &str) -> Vec<CommandInfo> {
        search::search(&self.commands(), query, &self.db.history())
    }

    fn dirty(&mut self) {
        self.rev += 1;
        *self.memo.borrow_mut() = None;
        *self.detail_memo.borrow_mut() = None;
    }

    fn nav_mut(&mut self) -> &mut Navigator {
        self.dirty();
        &mut self.nav
    }

    fn refresh_root(&mut self) {
        let q = if self.nav.is_root() { self.nav.top().query.clone() } else { String::new() };
        self.root_results = self.search(&q);
        self.dirty();
        self.resync_selection();
    }

    /// A handle to the extension runtime (the daemon uses it to answer `flux status`).
    pub fn runtime_handle(&self) -> RuntimeHandle {
        self.runtime.clone()
    }

    pub fn screen(&self) -> &Screen {
        &self.nav.top().screen
    }

    pub fn navigator(&self) -> &Navigator {
        &self.nav
    }

    /// View ids of every screen currently on the stack (UI widgets of closed screens can be dropped).
    pub fn navigator_view_ids(&self) -> Vec<u64> {
        self.nav.view_ids()
    }

    fn top_node(&self) -> Option<&ViewNode> {
        self.nav.top().screen.node()
    }

    /// Changes whenever the screen shown (or its text) was changed by the core, so the UI
    /// knows to reload its text field from [`Core::query`].
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn query(&self) -> &str {
        &self.nav.top().query
    }

    pub fn selected(&self) -> usize {
        self.nav.top().selected
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn set_error(&mut self, message: String) {
        self.error = Some(message);
    }

    pub fn clear_error(&mut self) {
        self.error = None;
    }

    /// Effects produced outside a call that returns them (native actions run from shortcuts).
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.pending)
    }

    fn finish(&mut self, mut effects: Vec<Effect>) -> Vec<Effect> {
        effects.extend(self.pending.drain(..));
        effects
    }

    pub fn placeholder(&self) -> String {
        match self.screen() {
            Screen::View { title, node: ViewNode::List(v), .. } => v.search_bar_placeholder.clone().unwrap_or_else(|| format!("Search {title}…")),
            Screen::View { title, node: ViewNode::Grid(v), .. } => v.search_bar_placeholder.clone().unwrap_or_else(|| format!("Search {title}…")),
            Screen::View { title, node: ViewNode::Input(v), .. } => v.placeholder.clone().unwrap_or_else(|| title.clone()),
            _ => "Search commands…".into(),
        }
    }

    pub fn is_secure_input(&self) -> bool {
        matches!(self.top_node(), Some(ViewNode::Input(v)) if v.secure)
    }

    /// Whether the search field is part of the current screen (not for detail pages and forms).
    pub fn shows_search_field(&self) -> bool {
        !matches!(self.top_node(), Some(ViewNode::Detail(_) | ViewNode::Form(_)))
    }

    /// The screen shows a thin indeterminate bar (`is_loading`, or a command still starting).
    pub fn is_loading(&self) -> bool {
        match self.screen() {
            Screen::Loading { .. } => true,
            Screen::View { node, .. } => node.is_loading(),
            Screen::Root => false,
        }
    }

    // ------------------------------------------------------- visible items

    fn build_model(&self) -> Model {
        let q = self.query();
        match self.screen() {
            Screen::Root => {
                let n = self.root_results.len();
                let vis = Visible {
                    items: (0..n).map(|i| VItem { section: 0, item: i }).collect(),
                    sections: if n > 0 { vec![VSection { section: 0, start: 0, len: n }] } else { vec![] },
                };
                Model { rows: (0..n).map(DisplayRow::Item).collect(), vis, layout: None }
            }
            Screen::View { node: ViewNode::List(v), .. } => {
                let vis = visible::visible_list(v, q);
                Model { rows: visible::display_list(v, &vis), vis, layout: None }
            }
            Screen::View { node: ViewNode::Grid(v), .. } => {
                let vis = visible::visible_grid(v, q);
                let (layout, rows) = visible::display_grid(v, &vis, self.grid_columns(v));
                Model { rows, vis, layout: Some(layout) }
            }
            _ => Model::default(),
        }
    }

    fn grid_columns(&self, v: &GridView) -> usize {
        v.columns.map(|c| c as usize).unwrap_or(self.grid_cols).max(1)
    }

    /// Visible items and display rows (memoised until something changes).
    pub fn model(&self) -> Rc<Model> {
        if let Some((rev, cols, m)) = &*self.memo.borrow() {
            if *rev == self.rev && *cols == self.grid_cols {
                return m.clone();
            }
        }
        let m = Rc::new(self.build_model());
        *self.memo.borrow_mut() = Some((self.rev, self.grid_cols, m.clone()));
        m
    }

    pub fn item_count(&self) -> usize {
        self.model().vis.len()
    }

    /// The i-th visible item of the root, a list or (as text) anything else listable.
    pub fn item(&self, index: usize) -> Option<ItemData> {
        let model = self.model();
        let vi = *model.vis.items.get(index)?;
        match self.screen() {
            Screen::Root => self.root_results.get(vi.item).map(|c| ItemData {
                key: c.id.clone(),
                title: c.title.clone(),
                subtitle: c.subtitle.clone(),
                icon: c.icon.as_deref().map(Icon::from_str),
                accessories: vec![],
            }),
            Screen::View { node: ViewNode::List(v), .. } => {
                let it = v.sections.get(vi.section)?.items.get(vi.item)?;
                Some(ItemData { key: it.key.clone(), title: it.title.clone(), subtitle: it.subtitle.clone(), icon: it.icon.clone(), accessories: it.accessories.clone() })
            }
            _ => None,
        }
    }

    pub fn cell(&self, index: usize) -> Option<CellData> {
        let model = self.model();
        let vi = *model.vis.items.get(index)?;
        let Screen::View { node: ViewNode::Grid(v), .. } = self.screen() else { return None };
        let it = v.sections.get(vi.section)?.items.get(vi.item)?;
        Some(CellData { key: it.key.clone(), content: it.content.clone(), title: it.title.clone(), subtitle: it.subtitle.clone() })
    }

    fn key_at(&self, index: usize) -> Option<String> {
        let model = self.model();
        let vi = *model.vis.items.get(index)?;
        match self.screen() {
            Screen::Root => self.root_results.get(vi.item).map(|c| c.id.clone()),
            Screen::View { node: ViewNode::List(v), .. } => Some(v.sections.get(vi.section)?.items.get(vi.item)?.key.clone()),
            Screen::View { node: ViewNode::Grid(v), .. } => Some(v.sections.get(vi.section)?.items.get(vi.item)?.key.clone()),
            _ => None,
        }
    }

    fn index_of_key(&self, key: &str) -> Option<usize> {
        (0..self.item_count()).find(|i| self.key_at(*i).as_deref() == Some(key))
    }

    /// Key of the selected item.
    pub fn selected_key(&self) -> Option<String> {
        self.key_at(self.selected())
    }

    pub fn empty_view(&self) -> (String, Option<String>, Option<Icon>) {
        let from = |e: &EmptyView| (e.title.clone().unwrap_or_else(|| "No results".into()), e.description.clone(), e.icon.clone());
        match self.screen() {
            Screen::Loading { .. } => ("Loading…".into(), None, None),
            Screen::View { node: ViewNode::List(v), .. } => from(&v.empty),
            Screen::View { node: ViewNode::Grid(v), .. } => from(&v.empty),
            _ => ("No commands found".into(), None, None),
        }
    }

    /// Cell box (w, h) for the current grid, and its forced column count.
    pub fn grid_geometry(&self) -> Option<(f32, f32, Option<u32>, GridFit)> {
        match self.top_node() {
            Some(ViewNode::Grid(v)) => {
                let (w, h) = v.size.cell();
                Some((w, h, v.columns, v.fit))
            }
            _ => None,
        }
    }

    /// The UI reports how many columns fit; the model re-lays out when it changes.
    pub fn set_grid_columns(&mut self, cols: usize) {
        let cols = cols.max(1);
        if cols != self.grid_cols {
            self.grid_cols = cols;
            self.dirty();
            self.resync_selection();
        }
    }

    pub fn detail(&self) -> Option<Rc<DetailModel>> {
        let Some(ViewNode::Detail(d)) = self.top_node() else { return None };
        if let Some((rev, m)) = &*self.detail_memo.borrow() {
            if *rev == self.rev {
                return Some(m.clone());
            }
        }
        let items = d
            .blocks
            .iter()
            .map(|b| match b {
                DetailBlock::Markdown(s) => DetailItem::Markdown(markdown::parse(s)),
                DetailBlock::ProgressBar { value, label } => DetailItem::ProgressBar { value: *value, label: label.clone() },
                DetailBlock::LoadingIndicator { label } => DetailItem::Loading { label: label.clone() },
            })
            .collect();
        let m = Rc::new(DetailModel { items, metadata: d.metadata.clone() });
        *self.detail_memo.borrow_mut() = Some((self.rev, m.clone()));
        Some(m)
    }

    /// Title and shortcut of the primary action of the selected item (for the footer).
    pub fn primary_action_title(&self) -> Option<String> {
        match self.screen() {
            Screen::Root => self.root_results.get(self.selected()).map(|_| "Run".to_string()),
            Screen::View { node: ViewNode::Input(_), .. } => Some("Save".into()),
            Screen::View { node: ViewNode::Form(_), .. } => Some("Submit".into()),
            Screen::View { .. } => {
                let (_, sections) = self.selected_sections()?;
                resolve_actions(&sections).first().map(|r| r.spec.title.clone())
            }
            Screen::Loading { .. } => None,
        }
    }

    /// Actions available for what is selected: the item's own, then the view's global ones.
    fn selected_sections(&self) -> Option<(u64, Vec<ActionSection>)> {
        let Screen::View { view_id, node, .. } = self.screen() else { return None };
        let model = self.model();
        let vi = model.vis.items.get(self.selected());
        let mut out = match (node, vi) {
            (ViewNode::List(v), Some(vi)) => v.sections.get(vi.section)?.items.get(vi.item)?.actions.clone(),
            (ViewNode::Grid(v), Some(vi)) => v.sections.get(vi.section)?.items.get(vi.item)?.actions.clone(),
            _ => vec![],
        };
        out.extend(node.global_actions().iter().cloned());
        Some((*view_id, out))
    }

    // ------------------------------------------------------------- input

    fn bump(&mut self) {
        self.epoch += 1;
    }

    /// The user typed: update the search text of the current screen.
    pub fn set_query(&mut self, query: &str) {
        self.error = None;
        self.history_idx = None;
        let top = self.nav_mut().top_mut();
        top.query = query.to_string();
        top.selected = 0;
        top.selected_key = None;
        if self.nav.is_root() {
            self.refresh_root();
        }
        self.resync_selection();
        if let Screen::View { view_id, node, .. } = self.screen() {
            if node.events().is_some_and(|e| e.search_change) {
                let (view_id, text) = (*view_id, self.query().to_string());
                self.runtime.send(RuntimeRequest::SearchChanged { view_id, text });
            }
        }
    }

    pub fn set_selected(&mut self, index: usize) {
        let n = self.item_count();
        let key = if n == 0 { None } else { self.key_at(index.min(n - 1)) };
        let top = self.nav_mut().top_mut();
        top.selected = if n == 0 { 0 } else { index.min(n - 1) };
        top.selected_key = key;
        self.report_selection();
    }

    /// After the visible set changed (typing, patch, relayout): keep the *same item* selected by
    /// key when it is still there, otherwise stay near the old position.
    fn resync_selection(&mut self) {
        let n = self.item_count();
        let key = self.nav.top().selected_key.clone();
        let (selected, new_key) = match key.as_deref().and_then(|k| self.index_of_key(k)) {
            Some(i) => (i, key),
            None => {
                let i = if n == 0 { 0 } else { self.nav.top().selected.min(n - 1) };
                (i, if n == 0 { None } else { self.key_at(i) })
            }
        };
        let top = self.nav.top_mut();
        top.selected = selected;
        top.selected_key = new_key;
        self.report_selection();
    }

    /// Tells the extension when the selected item changed (only if it listens).
    fn report_selection(&mut self) {
        let Screen::View { view_id, node, .. } = self.screen() else { return };
        if !node.events().is_some_and(|e| e.selection_change) {
            return;
        }
        let (view_id, key) = (*view_id, self.nav.top().selected_key.clone());
        if self.nav.top().reported_key.as_ref() == Some(&key) {
            return;
        }
        self.nav.top_mut().reported_key = Some(key.clone());
        self.runtime.send(RuntimeRequest::SelectionChanged { view_id, key });
    }

    pub fn select_next(&mut self) {
        self.step(1);
    }

    pub fn select_prev(&mut self) {
        self.step(-1);
    }

    fn step(&mut self, delta: isize) {
        let len = self.item_count();
        if len > 0 {
            self.set_selected((self.selected() as isize + delta).rem_euclid(len as isize) as usize);
        }
    }

    pub fn select_first(&mut self) {
        self.set_selected(0);
    }

    pub fn select_last(&mut self) {
        let n = self.item_count();
        self.set_selected(n.saturating_sub(1));
    }

    /// Grid arrows (also used by lists: up/down = prev/next, left/right ignored).
    pub fn select_left(&mut self) {
        self.grid_move(|l, i| l.left(i));
    }

    pub fn select_right(&mut self) {
        self.grid_move(|l, i| l.right(i));
    }

    pub fn select_up(&mut self) {
        match self.model().layout.is_some() {
            true => self.grid_move(|l, i| l.up(i)),
            false => self.select_prev(),
        }
    }

    pub fn select_down(&mut self) {
        match self.model().layout.is_some() {
            true => self.grid_move(|l, i| l.down(i)),
            false => self.select_next(),
        }
    }

    fn grid_move(&mut self, f: impl Fn(&GridLayout, usize) -> usize) {
        let model = self.model();
        if let Some(layout) = &model.layout {
            let to = f(layout, self.selected());
            self.set_selected(to);
        }
    }

    /// Ctrl+Up: previous (older) search; the text must be reloaded from [`Core::query`].
    pub fn history_prev(&mut self) -> bool {
        if !self.nav.is_root() {
            return false;
        }
        if self.history_idx.is_none() {
            self.history = self.db.recent_searches(HISTORY_LIMIT);
        }
        if self.history.is_empty() {
            return false;
        }
        let idx = self.history_idx.map(|i| (i + 1).min(self.history.len() - 1)).unwrap_or(0);
        self.apply_history(Some(idx));
        true
    }

    /// Ctrl+Down: newer search, ending at an empty box.
    pub fn history_next(&mut self) -> bool {
        let Some(i) = self.history_idx else { return false };
        self.apply_history(i.checked_sub(1));
        true
    }

    fn apply_history(&mut self, idx: Option<usize>) {
        let text = idx.map(|i| self.history[i].clone()).unwrap_or_default();
        let top = self.nav_mut().top_mut();
        top.query = text;
        top.selected = 0;
        top.selected_key = None;
        self.history_idx = idx;
        self.refresh_root();
        self.bump();
    }

    // ----------------------------------------------------------- running

    fn notify_closed(&mut self, closed: Closed) {
        match closed {
            Closed::View(view_id) => {
                self.native.remove(&view_id);
                if self.manage_view == Some(view_id) {
                    self.manage_view = None;
                }
                self.runtime.send(RuntimeRequest::ViewClosed { view_id });
            }
            Closed::Command(command_id) => self.runtime.send(RuntimeRequest::CancelCommand { command_id }),
        }
    }

    pub fn run_command(&mut self, id: &str) -> Vec<Effect> {
        let Some(cmd) = self.commands().into_iter().find(|c| c.id == id) else {
            self.error = Some(format!("unknown command: {id}"));
            return vec![];
        };
        self.db.record_use(id);
        self.panel = None;
        let effects = match id {
            BUILTIN_QUIT => vec![Effect::Quit],
            BUILTIN_RELOAD => {
                self.runtime.send(RuntimeRequest::Reload);
                vec![Effect::Hide]
            }
            BUILTIN_EXTENSIONS => {
                self.open_manage_extensions();
                vec![]
            }
            _ => {
                self.runtime.send(RuntimeRequest::RunCommand { command_id: id.into() });
                self.nav_mut().push_loading(id, &cmd.title);
                self.bump();
                vec![]
            }
        };
        self.finish(effects)
    }

    /// Enter: run the selected command, trigger the primary action, or submit the input.
    pub fn activate(&mut self) -> Vec<Effect> {
        self.error = None;
        if self.confirm.is_some() {
            return self.confirm_accept();
        }
        match self.screen().clone() {
            Screen::Root => {
                let Some(cmd) = self.root_results.get(self.selected()).cloned() else { return vec![] };
                let query = self.query().to_string();
                self.db.record_search(&query);
                self.run_command(&cmd.id)
            }
            Screen::View { node: ViewNode::Input(_), view_id, .. } => {
                let text = self.query().to_string();
                self.runtime.send(RuntimeRequest::SubmitInput { view_id, text });
                vec![]
            }
            Screen::View { node: ViewNode::Form(_), .. } => self.form_submit(),
            Screen::View { .. } => {
                self.trigger_shortcut(&KeyInput { key: "enter".into(), ..Default::default() });
                self.finish(vec![])
            }
            Screen::Loading { .. } => vec![],
        }
    }

    /// Runs the action bound to `input`: view key bindings first, then the selected item's actions
    /// (enter, ctrl+enter, custom shortcuts). Effects of native actions are in [`Core::take_effects`].
    pub fn trigger_shortcut(&mut self, input: &KeyInput) -> bool {
        let Screen::View { view_id, node, .. } = self.screen() else { return false };
        let view_id = *view_id;
        if let Some(kb) = node.events().and_then(|e| e.key_bindings.iter().find(|k| k.shortcut.matches(input))) {
            let action = kb.action;
            self.runtime.send(RuntimeRequest::InvokeAction { view_id, action_id: action });
            return true;
        }
        if matches!(node, ViewNode::Form(_)) && input.ctrl && input.key == "enter" {
            let effects = self.form_submit();
            self.pending.extend(effects);
            return true;
        }
        let is_form = matches!(node, ViewNode::Form(_));
        let Some((view_id, sections)) = self.selected_sections() else { return false };
        let hit = resolve_actions(&sections)
            .into_iter()
            .find(|r| {
                let sc = if is_form { r.spec.shortcut.as_ref() } else { r.shortcut.as_ref() };
                sc.is_some_and(|s: &Shortcut| s.matches(input))
            })
            .map(|r| r.spec.clone());
        match hit {
            Some(spec) => {
                let effects = self.invoke_spec(view_id, &spec);
                self.pending.extend(effects);
                true
            }
            None => false,
        }
    }

    fn invoke_spec(&mut self, view_id: u64, spec: &ActionSpec) -> Vec<Effect> {
        if let Some(c) = &spec.confirm {
            self.confirm = Some(Confirmation {
                title: c.title.clone(),
                message: c.message.clone(),
                destructive: spec.style == ActionStyle::Destructive,
                view_id,
                action_id: spec.id,
            });
            return vec![];
        }
        self.invoke(view_id, spec.id)
    }

    fn invoke(&mut self, view_id: u64, action_id: u64) -> Vec<Effect> {
        if view_id >= NATIVE_BASE {
            return self.run_native(view_id, action_id);
        }
        self.runtime.send(RuntimeRequest::InvokeAction { view_id, action_id });
        vec![]
    }

    // -------------------------------------------------------- confirmation

    pub fn confirmation(&self) -> Option<&Confirmation> {
        self.confirm.as_ref()
    }

    pub fn confirm_accept(&mut self) -> Vec<Effect> {
        let Some(c) = self.confirm.take() else { return vec![] };
        let e = self.invoke(c.view_id, c.action_id);
        self.finish(e)
    }

    pub fn confirm_cancel(&mut self) {
        self.confirm = None;
    }

    // ------------------------------------------------------- action panel

    pub fn panel(&self) -> Option<&ActionPanelModel> {
        self.panel.as_ref()
    }

    pub fn panel_mut(&mut self) -> Option<&mut ActionPanelModel> {
        self.panel.as_mut()
    }

    /// Ctrl+K. Returns whether a panel is now open.
    pub fn toggle_panel(&mut self) -> bool {
        if self.panel.take().is_some() {
            return false;
        }
        if self.confirm.is_some() {
            return false;
        }
        let model = match self.screen() {
            Screen::Root => self.root_results.get(self.selected()).map(|cmd| {
                let entry = |section: Option<&str>, title: &str, d: Option<&str>, sc: Option<Shortcut>, target| PanelEntry {
                    section: section.map(String::from),
                    title: title.into(),
                    description: d.map(String::from),
                    shortcut: sc,
                    style: ActionStyle::Default,
                    confirm: None,
                    target,
                };
                let mut entries = vec![entry(None, "Run", Some(&cmd.title), Some(Shortcut::new("enter")), PanelTarget::RunCommand(cmd.id.clone()))];
                if let Some(ext) = &cmd.extension {
                    let prefs = format!("{ext}:__preferences");
                    if cmd.id != prefs && self.extension_commands.iter().any(|c| c.id == prefs) {
                        entries.push(entry(None, "Configure Extension", Some(ext), None, PanelTarget::RunCommand(prefs)));
                    }
                }
                entries.push(entry(Some("Launcher"), "Manage Extensions", None, None, PanelTarget::ManageExtensions));
                entries.push(entry(Some("Launcher"), "Reload Extensions", None, None, PanelTarget::Reload));
                entries.push(entry(Some("Launcher"), "Quit Flux", None, None, PanelTarget::Quit));
                ActionPanelModel::new(entries)
            }),
            Screen::View { node: ViewNode::Input(_), .. } => None,
            Screen::View { node: ViewNode::Form(_), .. } => {
                let sections = self.selected_sections();
                let mut m = match &sections {
                    Some((id, s)) => ActionPanelModel::from_sections_with(*id, s, false),
                    None => ActionPanelModel::new(vec![]),
                };
                m.prepend(PanelEntry {
                    section: None,
                    title: "Submit Form".into(),
                    description: None,
                    shortcut: Some(Shortcut::new("enter").ctrl()),
                    style: ActionStyle::Default,
                    confirm: None,
                    target: PanelTarget::SubmitForm,
                });
                Some(m)
            }
            Screen::View { .. } => self.selected_sections().map(|(view_id, s)| ActionPanelModel::from_sections(view_id, &s)).filter(|m| !m.is_empty()),
            Screen::Loading { .. } => None,
        };
        self.panel = model;
        self.panel.is_some()
    }

    pub fn close_panel(&mut self) {
        self.panel = None;
    }

    /// Executes the panel's selected entry and closes the panel.
    pub fn run_panel_selected(&mut self) -> Vec<Effect> {
        let Some(entry) = self.panel.as_ref().and_then(|p| p.current()).cloned() else { return vec![] };
        self.panel = None;
        let effects = match entry.target {
            PanelTarget::Runtime { view_id, action_id } => match entry.confirm {
                Some(c) => {
                    self.confirm = Some(Confirmation { title: c.title, message: c.message, destructive: entry.style == ActionStyle::Destructive, view_id, action_id });
                    vec![]
                }
                None => self.invoke(view_id, action_id),
            },
            PanelTarget::RunCommand(id) => self.run_command(&id),
            PanelTarget::Reload => self.run_command(BUILTIN_RELOAD),
            PanelTarget::ManageExtensions => self.run_command(BUILTIN_EXTENSIONS),
            PanelTarget::Quit => vec![Effect::Quit],
            PanelTarget::SubmitForm => self.form_submit(),
        };
        self.finish(effects)
    }

    // --------------------------------------------------------------- forms

    pub fn form(&self) -> Option<(&FormView, &FormState)> {
        let e = self.nav.top();
        match (e.screen.node(), e.form.as_ref()) {
            (Some(ViewNode::Form(v)), Some(s)) => Some((v, s)),
            _ => None,
        }
    }

    /// The user edited a field.
    pub fn form_set(&mut self, id: &str, value: FieldValue) {
        let (view_id, notify) = {
            let entry = self.nav.top_mut();
            let (Screen::View { view_id, node: ViewNode::Form(v), .. }, Some(st)) = (&entry.screen, entry.form.as_mut()) else { return };
            st.set(v, id, value.clone());
            (*view_id, v.fields.iter().any(|f| f.id == id && f.change_events))
        };
        if notify {
            self.runtime.send(RuntimeRequest::FormChanged { view_id, field: id.into(), value });
        }
        self.dirty();
    }

    pub fn form_focus_next(&mut self) {
        let entry = self.nav.top_mut();
        if let (Some(ViewNode::Form(v)), Some(st)) = (entry.screen.node(), entry.form.as_mut()) {
            st.focus_next(v);
        }
    }

    pub fn form_focus_prev(&mut self) {
        let entry = self.nav.top_mut();
        if let (Some(ViewNode::Form(v)), Some(st)) = (entry.screen.node(), entry.form.as_mut()) {
            st.focus_prev(v);
        }
    }

    pub fn form_focus(&mut self, id: &str) {
        let entry = self.nav.top_mut();
        if let (Some(ViewNode::Form(v)), Some(st)) = (entry.screen.node(), entry.form.as_mut()) {
            st.set_focus(v, id);
        }
    }

    /// Validates declaratively; only a valid form reaches the extension's `on_submit`.
    pub fn form_submit(&mut self) -> Vec<Effect> {
        let entry = self.nav.top_mut();
        let (Screen::View { view_id, node: ViewNode::Form(v), .. }, Some(st)) = (&entry.screen, entry.form.as_mut()) else { return vec![] };
        let view_id = *view_id;
        if st.validate_all(v).is_empty() {
            let values = st.submit_values(v);
            self.runtime.send(RuntimeRequest::FormSubmit { view_id, values });
        } else {
            self.error = Some("Fix the highlighted fields to submit".into());
        }
        self.dirty();
        vec![]
    }

    // --------------------------------------------------------- navigation

    /// Escape: close the confirmation, then the panel, then go back one screen, else clear the
    /// search, else hide.
    pub fn back(&mut self) -> Vec<Effect> {
        if self.confirm.take().is_some() {
            return vec![];
        }
        if self.panel.take().is_some() {
            return vec![];
        }
        if let Some(closed) = self.nav_mut().back() {
            self.notify_closed(closed);
            self.error = None;
            self.refresh_root();
            self.bump();
            return vec![];
        }
        if !self.query().is_empty() {
            self.set_query("");
            self.bump();
            return vec![];
        }
        vec![Effect::Hide]
    }

    /// Opens a link from a detail page. Only web and mail links leave the launcher.
    pub fn open_url(&mut self, url: &str) -> Vec<Effect> {
        if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
            vec![Effect::OpenUrl(url.to_string())]
        } else {
            self.error = Some(format!("refusing to open '{url}' (only http, https and mailto links)"));
            vec![]
        }
    }

    /// Applies a runtime event to the state. Effects are for the application shell.
    pub fn on_event(&mut self, event: RuntimeEvent) -> Vec<Effect> {
        match event {
            RuntimeEvent::CommandsRegistered(cmds) => {
                self.extension_commands = cmds;
                self.refresh_root();
            }
            RuntimeEvent::Nav(nav) => {
                // Live refreshes (a process list updating every 2 s) must not close what the user
                // is doing: only real navigation dismisses the panel and confirmations.
                let live = matches!(&nav, NavEvent::Update { .. } | NavEvent::FormErrors { .. } | NavEvent::SetSearch { .. });
                if !live {
                    self.panel = None;
                    self.confirm = None;
                    self.error = None;
                }
                let was_input = matches!(self.top_node(), Some(ViewNode::Input(_)));
                let changed_stack = !matches!(&nav, NavEvent::Replace { .. } | NavEvent::Update { .. } | NavEvent::FormErrors { .. } | NavEvent::SetSearch { .. });
                let silent = matches!(&nav, NavEvent::Update { .. } | NavEvent::FormErrors { .. } | NavEvent::SetSearch { .. });
                let top_before = self.nav.top().screen.view_id();
                let out = self.nav_mut().apply(nav);
                if let Some(e) = out.error {
                    self.error = Some(e);
                }
                for c in out.closed {
                    self.notify_closed(c);
                }
                if out.search_changed.is_some() && out.search_changed == top_before {
                    self.bump(); // the UI reloads its text field
                }
                if silent && out.patched != top_before {
                    // a covered screen was patched: nothing visible changed
                } else {
                    self.resync_selection();
                }
                if !silent && (changed_stack || was_input || matches!(self.top_node(), Some(ViewNode::Input(_)))) {
                    self.bump();
                }
            }
            RuntimeEvent::ExtensionError { extension, message } => {
                let name = extension.split(':').next().unwrap_or(&extension).to_string();
                self.logs.push(&name, LogLevel::Error, &message);
                self.error = Some(format!("{extension}: {message}"));
                self.nav_mut().fail_loading();
                self.refresh_root();
                self.bump();
            }
            RuntimeEvent::Log { extension, level, message } => self.logs.push(&extension, level, &message),
            RuntimeEvent::ExtensionsChanged(infos) => {
                self.infos = infos;
                self.refresh_manage_extensions();
            }
            RuntimeEvent::ExtensionLoaded { .. } | RuntimeEvent::CommandStarted { .. } => {}
            RuntimeEvent::ExtensionUnloaded { extension, views, .. } => self.extension_gone(&extension, &views, "was disabled"),
            RuntimeEvent::ExtensionReloaded { extension, views, .. } => self.extension_gone(&extension, &views, "was reloaded"),
            RuntimeEvent::ExtensionCrashed { extension, views, reason, .. } => {
                self.logs.push(&extension, LogLevel::Error, &reason);
                self.extension_gone(&extension, &views, &format!("crashed ({reason})"));
            }
            RuntimeEvent::CommandCancelled { command_id } => {
                if self.nav_mut().finish_loading(&command_id) {
                    self.refresh_root();
                    self.bump();
                }
            }
            RuntimeEvent::CommandFinished { command_id } => {
                if self.nav_mut().finish_loading(&command_id) {
                    self.refresh_root();
                    self.bump();
                    return vec![Effect::Hide];
                }
            }
            RuntimeEvent::CloseLauncher => return vec![Effect::Hide],
        }
        vec![]
    }

    /// The extension's worker is gone: the screens it owned can no longer react, so they are
    /// closed (the runtime has already dropped them) instead of lingering until the user presses Esc.
    fn extension_gone(&mut self, extension: &str, views: &[u64], what: &str) {
        if self.nav_mut().close_extension(views, extension) {
            self.panel = None;
            self.confirm = None;
            self.error = Some(format!("{extension} {what}; its screen was closed"));
            self.resync_selection();
            self.bump();
        }
        self.refresh_root();
    }

    // ------------------------------------------------------- visibility

    pub fn on_hide(&mut self, now: Instant) {
        self.hidden_since = Some(now);
        self.panel = None;
        self.confirm = None;
        self.error = None;
    }

    /// Called when the launcher is shown again. Returns true if the state was reset to a
    /// clean root because it had been hidden for at least `reset_after`.
    pub fn on_show(&mut self, now: Instant) -> bool {
        let reset = self.hidden_since.take().is_some_and(|t| now.duration_since(t) >= self.reset_after);
        if reset {
            let closed = self.nav_mut().reset();
            for c in closed {
                self.notify_closed(c);
            }
            self.history_idx = None;
        }
        self.refresh_root(); // frecency may have changed
        self.bump();
        reset
    }

    /// Loads the extension in `path` as well (`flux dev`) and watches it for changes.
    pub fn attach_dir(&mut self, path: PathBuf) {
        if let Some(s) = &self.singles {
            if let Ok(mut v) = s.lock() {
                if !v.contains(&path) {
                    v.push(path.clone());
                }
            }
        }
        self.runtime.send(RuntimeRequest::AddExtensionDir { path });
    }

    pub fn detach_dir(&mut self, path: PathBuf) {
        if let Some(s) = &self.singles {
            if let Ok(mut v) = s.lock() {
                v.retain(|p| p != &path);
            }
        }
        self.runtime.send(RuntimeRequest::RemoveExtensionDir { path });
    }

    /// Enables or disables an extension and remembers the choice.
    pub fn set_extension_enabled(&mut self, name: &str, enabled: bool) {
        self.db.set_extension_enabled(name, enabled);
        self.runtime.send(RuntimeRequest::SetEnabled { name: name.into(), enabled });
    }

    pub fn reload_extension(&self, name: &str) {
        self.runtime.send(RuntimeRequest::ReloadExtension { name: name.into() });
    }

    pub fn shutdown(&self) {
        self.runtime.send(RuntimeRequest::Shutdown);
    }

    pub fn reload(&self) {
        self.runtime.send(RuntimeRequest::Reload);
    }

    // ------------------------------------------------ built-in screens

    fn push_native(&mut self, title: &str, native: manage::Native) -> u64 {
        self.next_native += 1;
        let id = NATIVE_BASE + self.next_native;
        self.native.insert(id, native.actions);
        self.nav_mut().push_view(id, title, native.node);
        self.panel = None;
        self.resync_selection();
        self.bump();
        id
    }

    fn can_remove(&self, i: &ExtensionInfo) -> bool {
        self.manager.is_some() && i.source == ExtensionSource::Installed
    }

    fn open_manage_extensions(&mut self) {
        let n = manage::extensions_list(&self.infos, |i| self.can_remove(i));
        let id = self.push_native("Manage Extensions", n);
        self.manage_view = Some(id);
    }

    /// The list of extensions changed while "Manage Extensions" is open: patch it in place.
    fn refresh_manage_extensions(&mut self) {
        let Some(id) = self.manage_view else { return };
        let fresh = manage::extensions_list(&self.infos, |i| self.can_remove(i));
        let old = self.nav.entry_for_view(id).and_then(|e| e.screen.node().cloned());
        let Some(old) = old else { return };
        if let Some(patch) = diff_views(&old, &fresh.node) {
            self.nav_mut().apply(NavEvent::Update { view_id: id, patch });
        }
        self.native.insert(id, fresh.actions);
        if self.nav.top().screen.view_id() == Some(id) {
            self.resync_selection();
        }
        self.dirty();
    }

    fn info_of(&self, name: &str) -> Option<ExtensionInfo> {
        self.infos.iter().find(|i| i.name == name).cloned()
    }

    fn run_native(&mut self, view_id: u64, action_id: u64) -> Vec<Effect> {
        let Some(action) = self.native.get(&view_id).and_then(|m| m.get(&action_id)).cloned() else { return vec![] };
        match action {
            NativeAction::ShowInfo(name) => {
                if let Some(i) = self.info_of(&name) {
                    self.push_native(&i.title.clone(), manage::info_detail(&i));
                }
            }
            NativeAction::Toggle { name, enable } => {
                self.db.set_extension_enabled(&name, enable);
                self.runtime.send(RuntimeRequest::SetEnabled { name, enabled: enable });
            }
            NativeAction::Reload(name) => self.runtime.send(RuntimeRequest::ReloadExtension { name }),
            NativeAction::ShowLogs(name) => {
                let lines = self.logs.lines(&name, 200);
                let hint = self.log_dir.as_ref().map(|d| log_path(d, &name).display().to_string()).unwrap_or_else(|| "(in memory)".into());
                self.push_native(&format!("{name}: logs"), manage::logs_detail(&name, &lines, &hint));
            }
            NativeAction::ShowErrors(name) => {
                if let Some(i) = self.info_of(&name) {
                    let recent = self.logs.errors(&name);
                    self.push_native(&format!("{name}: errors"), manage::errors_detail(&i, &recent));
                }
            }
            NativeAction::Remove(name) => match &self.manager {
                Some(m) => match m.remove(&name, false, self.data_root.as_deref()) {
                    Ok(()) => self.runtime.send(RuntimeRequest::Reload),
                    Err(e) => self.error = Some(e.to_string()),
                },
                None => self.error = Some("this build cannot remove extensions".into()),
            },
            NativeAction::OpenFolder(dir) => self.pending.push(Effect::OpenPath(dir)),
            NativeAction::CopyPath(dir) => self.pending.push(Effect::CopyText(dir)),
            NativeAction::SetPermission { name, cap, revoke } => {
                let Some(capability) = Capability::parse(&cap) else { return vec![] };
                let result = perms::update(&self.config_dir.clone(), &name, |o| {
                    if revoke {
                        o.revoke.insert(capability);
                    } else {
                        o.revoke.remove(&capability);
                    }
                });
                match result {
                    Ok(_) => self.runtime.send(RuntimeRequest::ReloadExtension { name }),
                    Err(e) => self.error = Some(e),
                }
            }
        }
        vec![]
    }
}

#[cfg(test)]
impl Core {
    pub(crate) fn push_native_for_test(&mut self, info: &ExtensionInfo) {
        self.push_native(&info.title.clone(), manage::info_detail(info));
    }

    /// (view id, action id) of the native action with this title on the top screen.
    pub(crate) fn native_action_for_test(&self, title: &str) -> Option<(u64, u64)> {
        let ViewNode::Detail(d) = self.top_node()? else { return None };
        let id = d.actions.iter().flat_map(|s| &s.actions).find(|a| a.title == title)?.id;
        Some((self.nav.top().screen.view_id()?, id))
    }

    pub(crate) fn run_native_for_test(&mut self, view_id: u64, action_id: u64) {
        self.run_native(view_id, action_id);
    }
}

#[cfg(test)]
mod tests;
