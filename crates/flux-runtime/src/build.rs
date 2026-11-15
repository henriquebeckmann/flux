//! Lua component tables → [`ViewNode`]. Every error names the offending path
//! (`List.Item[3].accessories[1].progress: expected a number …`) so extension authors can fix
//! their code without reading Rust.
//!
//! Callback ids come from an [`Interner`] keyed by the *path* of the prop in the tree
//! (`item:<key>/0.1`, `on_search_change`, `field:<id>:on_change`…). A re-render therefore yields
//! the same ids for unchanged parts and the keyed diff stays small.

use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path},
};

use flux_api::*;
use mlua::{Function, Table, Value};

pub type R<T> = Result<T, String>;

#[derive(Default)]
pub struct Interner {
    ids: HashMap<String, u64>,
    next: u64,
}

impl Interner {
    pub fn id(&mut self, path: &str) -> u64 {
        if let Some(id) = self.ids.get(path) {
            return *id;
        }
        self.next += 1;
        self.ids.insert(path.to_string(), self.next);
        self.next
    }

    pub fn lookup(&self, path: &str) -> Option<u64> {
        self.ids.get(path).copied()
    }
}

/// What a view may reference, taken from the extension's manifest.
pub struct Policy<'a> {
    pub dir: &'a Path,
    /// `allowed_hosts` of the manifest: image URLs obey the same host rules as `flux.http`.
    pub hosts: &'a crate::http::HostPolicy,
    pub network: bool,
    pub filesystem: bool,
    pub permissions: &'a flux_api::PermissionSet,
    pub data_dir: &'a Path,
}

pub struct Build<'a> {
    pub policy: Policy<'a>,
    pub interner: &'a mut Interner,
    pub callbacks: HashMap<u64, Function>,
    keys: HashSet<String>,
}

impl<'a> Build<'a> {
    pub fn new(policy: Policy<'a>, interner: &'a mut Interner) -> Self {
        Self { policy, interner, callbacks: HashMap::new(), keys: HashSet::new() }
    }

    fn callback(&mut self, path: &str, f: Function) -> u64 {
        let id = self.interner.id(path);
        self.callbacks.insert(id, f);
        id
    }
}

// ------------------------------------------------------------------ props

fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i];
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur.push((prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost));
        }
        prev = cur;
    }
    prev[b.len()]
}

struct Props<'a> {
    t: &'a Table,
    path: String,
}

fn type_name(v: &Value) -> &'static str {
    v.type_name()
}

impl<'a> Props<'a> {
    fn new(t: &'a Table, path: impl Into<String>, allowed: &[&str]) -> R<Self> {
        let p = Self { t, path: path.into() };
        for pair in t.pairs::<Value, Value>() {
            let (k, _) = pair.map_err(|e| e.to_string())?;
            if let Value::String(k) = k {
                let k = k.to_string_lossy();
                if k.starts_with("__") || allowed.contains(&k.as_str()) {
                    continue;
                }
                let hint = allowed.iter().filter(|a| levenshtein(a, &k) <= 2).min_by_key(|a| levenshtein(a, &k));
                return Err(match hint {
                    Some(h) => format!("{}: unknown prop '{k}' (did you mean '{h}'?)", p.path),
                    None => format!("{}: unknown prop '{k}' (allowed: {})", p.path, allowed.join(", ")),
                });
            }
        }
        Ok(p)
    }

    fn at(&self, key: &str) -> String {
        format!("{}.{key}", self.path)
    }

    fn raw(&self, key: &str) -> R<Value> {
        self.t.get::<Value>(key).map_err(|e| format!("{}: {e}", self.at(key)))
    }

    fn bad(&self, key: &str, expected: &str, got: &Value) -> String {
        format!("{}: expected {expected}, got {}", self.at(key), type_name(got))
    }

    fn str(&self, key: &str) -> R<Option<String>> {
        match self.raw(key)? {
            Value::Nil => Ok(None),
            Value::String(s) => Ok(Some(s.to_string_lossy().to_string())),
            other => Err(self.bad(key, "a string", &other)),
        }
    }

    fn require_str(&self, key: &str) -> R<String> {
        self.str(key)?.ok_or_else(|| format!("{}: '{key}' is required", self.path))
    }

    fn bool(&self, key: &str) -> R<Option<bool>> {
        match self.raw(key)? {
            Value::Nil => Ok(None),
            Value::Boolean(b) => Ok(Some(b)),
            other => Err(self.bad(key, "a boolean", &other)),
        }
    }

    fn num(&self, key: &str) -> R<Option<f64>> {
        match self.raw(key)? {
            Value::Nil => Ok(None),
            Value::Integer(i) => Ok(Some(i as f64)),
            Value::Number(n) => Ok(Some(n)),
            other => Err(self.bad(key, "a number", &other)),
        }
    }

    fn table(&self, key: &str) -> R<Option<Table>> {
        match self.raw(key)? {
            Value::Nil => Ok(None),
            Value::Table(t) => Ok(Some(t)),
            other => Err(self.bad(key, "a table", &other)),
        }
    }

    fn func(&self, key: &str) -> R<Option<Function>> {
        match self.raw(key)? {
            Value::Nil => Ok(None),
            Value::Function(f) => Ok(Some(f)),
            other => Err(self.bad(key, "a function", &other)),
        }
    }

    fn strings(&self, key: &str) -> R<Vec<String>> {
        let Some(t) = self.table(key)? else { return Ok(vec![]) };
        t.sequence_values::<Value>()
            .map(|v| match v.map_err(|e| e.to_string())? {
                Value::String(s) => Ok(s.to_string_lossy().to_string()),
                other => Err(self.bad(key, "a list of strings", &other)),
            })
            .collect()
    }
}

fn kind_of(t: &Table) -> Option<String> {
    t.get::<Option<String>>("__kind").ok().flatten()
}

/// Positional entries of a component (its children), as tables.
fn children(t: &Table, path: &str) -> R<Vec<Table>> {
    let mut out = Vec::new();
    for (i, v) in t.sequence_values::<Value>().enumerate() {
        match v.map_err(|e| e.to_string())? {
            Value::Table(t) => out.push(t),
            other => return Err(format!("{path}[{}]: expected a component, got {}", i + 1, type_name(&other))),
        }
    }
    Ok(out)
}

/// Components in `prop` (array) followed by positional children.
fn entries(t: &Table, prop: &str, path: &str) -> R<Vec<Table>> {
    let mut out = Vec::new();
    if let Some(items) = t.get::<Option<Table>>(prop).map_err(|e| format!("{path}.{prop}: {e}"))? {
        out.extend(children(&items, &format!("{path}.{prop}"))?);
    }
    out.extend(children(t, path)?);
    Ok(out)
}

// ------------------------------------------------------------ entry point

pub fn build_view(b: &mut Build, value: Value) -> R<ViewNode> {
    let mut table = match value {
        Value::Table(t) => t,
        Value::Nil => return Err("expected a view (e.g. ui.List{…}), got nil".into()),
        other => return Err(format!("expected a view (e.g. ui.List{{…}}), got {}", type_name(&other))),
    };
    if kind_of(&table).is_none() {
        // `{ component = view }` wrapper kept for navigation tables.
        if let Some(inner) = table.get::<Option<Table>>("component").map_err(|e| e.to_string())? {
            table = inner;
        }
    }
    match kind_of(&table).as_deref() {
        Some("List") => list(b, &table).map(ViewNode::List),
        Some("Grid") => grid(b, &table).map(ViewNode::Grid),
        Some("Detail") => detail(b, &table).map(ViewNode::Detail),
        Some("Form") => form(b, &table).map(ViewNode::Form),
        Some("Input") => input(b, &table).map(ViewNode::Input),
        Some(other) => Err(format!("'{other}' cannot be used as a screen (use ui.List, ui.Grid, ui.Detail, ui.Form or ui.view)")),
        None => Err("the value is not a view (use ui.List, ui.Grid, ui.Detail, ui.Form or ui.view)".into()),
    }
}

// ------------------------------------------------------- shared builders

fn events(b: &mut Build, p: &Props) -> R<Events> {
    let mut e = Events::default();
    if let Some(f) = p.func("on_search_change")? {
        b.callback("on_search_change", f);
        e.search_change = true;
    }
    if let Some(f) = p.func("on_selection_change")? {
        b.callback("on_selection_change", f);
        e.selection_change = true;
    }
    for name in ["on_appear", "on_disappear"] {
        if let Some(f) = p.func(name)? {
            b.callback(name, f);
        }
    }
    if let Some(keys) = p.table("on_key")? {
        let mut pairs = Vec::new();
        for kv in keys.pairs::<String, Value>() {
            let (k, v) = kv.map_err(|_| format!("{}: keys must be shortcut strings (\"ctrl+d\")", p.at("on_key")))?;
            let Value::Function(f) = v else { return Err(format!("{}[\"{k}\"]: expected a function", p.at("on_key"))) };
            let shortcut = Shortcut::parse(&k).map_err(|m| format!("{}: {m}", p.at("on_key")))?;
            pairs.push((shortcut, f));
        }
        pairs.sort_by_key(|(s, _)| s.to_string());
        for (shortcut, f) in pairs {
            let action = b.callback(&format!("key:{shortcut}"), f);
            e.key_bindings.push(KeyBinding { shortcut, action });
        }
    }
    Ok(e)
}

fn color(p: &Props, key: &str) -> R<Option<String>> {
    match p.str(key)? {
        Some(c) if !valid_color(&c) => Err(format!("{}: '{c}' is not a color (use {} or #rrggbb)", p.at(key), COLOR_NAMES.join(", "))),
        c => Ok(c),
    }
}

fn image_from_props(b: &Build, p: &Props) -> R<Image> {
    let source = match (p.str("path")?, p.str("url")?, p.str("source")?) {
        (Some(path), None, None) => path_source(b, &p.path, &path)?,
        (None, Some(url), None) => url_source(b, &p.path, url)?,
        (None, None, Some(s)) => string_source(b, &p.path, &s)?,
        _ => return Err(format!("{}: give exactly one of path, url or source", p.path)),
    };
    let mask = match p.str("mask")? {
        Some(m) => ImageMask::parse(&m).ok_or_else(|| format!("{}: unknown mask '{m}' (none, circle, rounded)", p.at("mask")))?,
        None => ImageMask::None,
    };
    Ok(Image { source, mask, fallback: p.str("fallback")? })
}

fn string_source(b: &Build, label: &str, s: &str) -> R<ImageSource> {
    if s.starts_with("http://") || s.starts_with("https://") { url_source(b, label, s.to_string()) } else { path_source(b, label, s) }
}

fn url_source(b: &Build, label: &str, url: String) -> R<ImageSource> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("{label}: image urls must be http:// or https://"));
    }
    if !b.policy.network {
        return Err(format!("{label}: loading images from the network needs the 'network' permission"));
    }
    let parsed = reqwest::Url::parse(&url).map_err(|e| format!("{label}: invalid image URL: {e}"))?;
    b.policy.hosts.check(&parsed).map_err(|e| format!("{label}: {}", e.message))?;
    Ok(ImageSource::Url(url))
}

fn path_source(b: &Build, label: &str, path: &str) -> R<ImageSource> {
    let pp = Path::new(path);
    if pp.is_absolute() {
        if !b.policy.filesystem {
            return Err(format!("{label}: absolute image paths need the 'filesystem' permission"));
        }
        crate::api::fs::check_image(b.policy.permissions, b.policy.dir, b.policy.data_dir, path).map_err(|e| format!("{label}: {e}"))?;
        return Ok(ImageSource::Path(path.to_string()));
    }
    if pp.components().any(|c| matches!(c, Component::ParentDir | Component::Prefix(_) | Component::RootDir)) {
        return Err(format!("{label}: image paths must stay inside the extension directory"));
    }
    Ok(ImageSource::Path(b.policy.dir.join(pp).to_string_lossy().into_owned()))
}

fn image_value(b: &Build, v: Value, path: &str) -> R<Image> {
    match v {
        Value::String(s) => string_source(b, path, &s.to_string_lossy()).map(|source| Image { source, mask: ImageMask::None, fallback: None }),
        Value::Table(t) => {
            let p = Props::new(&t, path, &["path", "url", "source", "mask", "fallback"])?;
            image_from_props(b, &p)
        }
        other => Err(format!("{path}: expected an image (ui.Image or a path/url string), got {}", type_name(&other))),
    }
}

fn icon_value(b: &Build, v: Value, path: &str) -> R<Option<Icon>> {
    match v {
        Value::Nil => Ok(None),
        Value::String(s) => Ok(Some(Icon::from_str(&s.to_string_lossy()))),
        Value::Table(t) => match kind_of(&t).as_deref() {
            Some("Image") => {
                let p = Props::new(&t, path, &["path", "url", "source", "mask", "fallback", "tint"])?;
                Ok(Some(Icon { kind: IconKind::Image(image_from_props(b, &p)?), tint: color(&p, "tint")? }))
            }
            _ => {
                let p = Props::new(&t, path, &["name", "text", "image", "tint"])?;
                let tint = color(&p, "tint")?;
                let kind = match (p.str("name")?, p.str("text")?, p.raw("image")?) {
                    (Some(n), None, Value::Nil) => {
                        if !BUILTIN_ICONS.contains(&n.as_str()) {
                            return Err(format!("{}: unknown icon '{n}' (see docs/ui-components.md for the list)", p.at("name")));
                        }
                        IconKind::Named(n)
                    }
                    (None, Some(t), Value::Nil) => IconKind::Text(t),
                    (None, None, img) if !matches!(img, Value::Nil) => IconKind::Image(image_value(b, img, &p.at("image"))?),
                    _ => return Err(format!("{path}: give exactly one of name, text or image")),
                };
                Ok(Some(Icon { kind, tint }))
            }
        },
        other => Err(format!("{path}: expected an icon (string or ui.Icon{{…}}), got {}", type_name(&other))),
    }
}

fn icon_prop(b: &Build, p: &Props, key: &str) -> R<Option<Icon>> {
    icon_value(b, p.raw(key)?, &p.at(key))
}

fn empty_view(b: &Build, t: &Table, path: &str) -> R<EmptyView> {
    let p = Props::new(t, path, &["title", "description", "icon"])?;
    Ok(EmptyView { title: p.str("title")?, description: p.str("description")?, icon: icon_prop(b, &p, "icon")? })
}

fn accessories(b: &Build, p: &Props) -> R<Vec<Accessory>> {
    let Some(list) = p.table("accessories")? else { return Ok(vec![]) };
    let mut out = Vec::new();
    for (i, v) in list.sequence_values::<Value>().enumerate() {
        let path = format!("{}[{}]", p.at("accessories"), i + 1);
        let t = match v.map_err(|e| e.to_string())? {
            Value::Table(t) => t,
            Value::String(s) => {
                out.push(Accessory { text: Some(s.to_string_lossy().to_string()), ..Default::default() });
                continue;
            }
            other => return Err(format!("{path}: expected a table or a string, got {}", type_name(&other))),
        };
        let a = Props::new(&t, &path, &["text", "icon", "tag", "progress", "tooltip"])?;
        let tag = match a.raw("tag")? {
            Value::Nil => None,
            Value::String(s) => Some(Tag { text: s.to_string_lossy().to_string(), color: None }),
            Value::Table(tt) => {
                let tp = Props::new(&tt, a.at("tag"), &["text", "color"])?;
                Some(Tag { text: tp.require_str("text")?, color: color(&tp, "color")? })
            }
            other => return Err(a.bad("tag", "a string or table", &other)),
        };
        let progress = match a.num("progress")? {
            Some(n) if !(0.0..=1.0).contains(&n) => return Err(format!("{}: expected a number between 0 and 1, got {n}", a.at("progress"))),
            n => n.map(|n| n as f32),
        };
        out.push(Accessory { text: a.str("text")?, icon: icon_prop(b, &a, "icon")?, tag, progress, tooltip: a.str("tooltip")? });
    }
    Ok(out)
}

// ---------------------------------------------------------------- actions

/// `actions` accepts actions and sections mixed (also inside `ui.ActionPanel{…}`); consecutive
/// bare actions share one section.
fn action_sections(b: &mut Build, value: Value, prefix: &str, path: &str) -> R<Vec<ActionSection>> {
    let table = match value {
        Value::Nil => return Ok(vec![]),
        Value::Table(t) => t,
        other => return Err(format!("{path}: expected actions (ui.Action) or ui.ActionPanel, got {}", type_name(&other))),
    };
    let mut out: Vec<ActionSection> = Vec::new();
    let mut loose: Option<ActionSection> = None;
    for (i, v) in table.sequence_values::<Value>().enumerate() {
        let p = format!("{path}[{}]", i + 1);
        let Value::Table(v) = v.map_err(|e| e.to_string())? else {
            return Err(format!("{p}: expected an action (ui.Action) or section (ui.ActionPanel.Section)"));
        };
        match kind_of(&v).as_deref() {
            Some("section") => {
                if let Some(l) = loose.take() {
                    out.push(l);
                }
                let sp = Props::new(&v, &p, &["title", "actions"])?;
                let mut s = ActionSection { title: sp.str("title")?, actions: vec![] };
                let mut inner = entries(&v, "actions", &p)?;
                for (j, a) in inner.drain(..).enumerate() {
                    s.actions.push(action(b, &a, &format!("{prefix}/{}.{j}", out.len()), &format!("{p}.actions[{}]", j + 1))?);
                }
                out.push(s);
            }
            Some("action") | None => {
                let n = loose.as_ref().map_or(0, |l| l.actions.len());
                let id = format!("{prefix}/{}.{n}", out.len());
                loose.get_or_insert_with(ActionSection::default).actions.push(action(b, &v, &id, &p)?);
            }
            Some(other) => return Err(format!("{p}: unexpected '{other}' where an action was expected")),
        }
    }
    if let Some(l) = loose {
        out.push(l);
    }
    Ok(out)
}

fn action(b: &mut Build, a: &Table, id: &str, path: &str) -> R<ActionSpec> {
    let p = Props::new(a, path, &["title", "description", "shortcut", "style", "confirm", "on_action"])?;
    let func = p.func("on_action")?.ok_or_else(|| format!("{path}: every action needs on_action (function)"))?;
    let title = p.str("title")?.unwrap_or_else(|| "Action".into());
    let shortcut = match p.str("shortcut")? {
        Some(s) => Some(Shortcut::parse(&s).map_err(|m| format!("action '{title}': {m}"))?),
        None => None,
    };
    let style = match p.str("style")? {
        Some(s) => ActionStyle::parse(&s).ok_or_else(|| format!("action '{title}': unknown style '{s}' (default, secondary, destructive)"))?,
        None => ActionStyle::Default,
    };
    let confirm = match p.raw("confirm")? {
        Value::Nil | Value::Boolean(false) => None,
        Value::Boolean(true) => Some(Confirm { title: format!("{title}?"), message: None }),
        Value::String(s) => Some(Confirm { title: s.to_string_lossy().to_string(), message: None }),
        Value::Table(t) => {
            let c = Props::new(&t, p.at("confirm"), &["title", "message"])?;
            Some(Confirm { title: c.str("title")?.unwrap_or_else(|| format!("{title}?")), message: c.str("message")? })
        }
        other => return Err(p.bad("confirm", "true, a string or a table", &other)),
    };
    let cb = b.callback(&format!("action:{id}"), func);
    Ok(ActionSpec { id: cb, title, description: p.str("description")?, shortcut, style, confirm })
}

fn claim_key(b: &mut Build, key: &str, path: &str) -> R<()> {
    if !b.keys.insert(key.to_string()) {
        return Err(format!("{path}: duplicate key '{key}' (keys must be unique inside a view)"));
    }
    Ok(())
}

// ------------------------------------------------------------------- List

const LIST_PROPS: &[&str] = &[
    "title", "search_bar_placeholder", "filtering", "is_loading", "empty", "empty_title", "items", "actions", "selected_key",
    "on_search_change", "on_selection_change", "on_key", "on_appear", "on_disappear",
];

fn list(b: &mut Build, t: &Table) -> R<ListView> {
    let p = Props::new(t, "List", LIST_PROPS)?;
    let mut view = ListView {
        title: p.str("title")?,
        search_bar_placeholder: p.str("search_bar_placeholder")?,
        filtering: p.bool("filtering")?.unwrap_or(true),
        is_loading: p.bool("is_loading")?.unwrap_or(false),
        selected_key: p.str("selected_key")?,
        ..Default::default()
    };
    view.events = events(b, &p)?;
    view.actions = action_sections(b, p.raw("actions")?, "global", "List.actions")?;
    if let Some(e) = p.table("empty")? {
        view.empty = empty_view(b, &e, "List.empty")?;
    }
    if let Some(title) = p.str("empty_title")? {
        view.empty.title = Some(title); // legacy prop
    }
    let mut loose: Option<Section<ListItem>> = None;
    let mut flat = 0usize;
    for (i, c) in entries(t, "items", "List")?.into_iter().enumerate() {
        let path = format!("List[{}]", i + 1);
        match kind_of(&c).as_deref() {
            Some("ListSection") => {
                if let Some(l) = loose.take() {
                    view.sections.push(l);
                }
                let sp = Props::new(&c, &path, &["key", "title", "subtitle", "items"])?;
                let key = sp.str("key")?.unwrap_or_else(|| format!("#s{}", view.sections.len()));
                let mut s = Section { key, title: sp.str("title")?, subtitle: sp.str("subtitle")?, items: vec![] };
                for (j, it) in entries(&c, "items", &path)?.into_iter().enumerate() {
                    s.items.push(list_item(b, &it, &format!("{path}.Item[{}]", j + 1), &mut flat)?);
                }
                view.sections.push(s);
            }
            Some("ListEmpty") => view.empty = empty_view(b, &c, &format!("{path} (EmptyView)"))?,
            Some("ListItem") | None => {
                let sec = loose.get_or_insert_with(|| Section { key: format!("#loose{}", view.sections.len()), ..Default::default() });
                sec.items.push(list_item(b, &c, &format!("List.Item[{}]", i + 1), &mut flat)?);
            }
            Some(other) => return Err(format!("{path}: '{other}' cannot be used inside ui.List")),
        }
    }
    if let Some(l) = loose {
        view.sections.push(l);
    }
    check_section_keys(&view.sections.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(), "List")?;
    Ok(view)
}

fn check_section_keys(keys: &[&str], what: &str) -> R<()> {
    let mut seen = HashSet::new();
    for k in keys {
        if !seen.insert(*k) {
            return Err(format!("{what}: duplicate section key '{k}'"));
        }
    }
    Ok(())
}

fn list_item(b: &mut Build, t: &Table, path: &str, flat: &mut usize) -> R<ListItem> {
    let p = Props::new(t, path, &["key", "title", "subtitle", "icon", "accessories", "keywords", "actions"])?;
    let title = p.str("title")?.ok_or_else(|| format!("{path}.title is required (string)"))?;
    let key = p.str("key")?.unwrap_or_else(|| format!("#{flat}"));
    *flat += 1;
    claim_key(b, &key, path)?;
    Ok(ListItem {
        title,
        subtitle: p.str("subtitle")?,
        icon: icon_prop(b, &p, "icon")?,
        accessories: accessories(b, &p)?,
        keywords: p.strings("keywords")?,
        actions: action_sections(b, p.raw("actions")?, &format!("item:{key}"), &p.at("actions"))?,
        key,
    })
}

// ------------------------------------------------------------------- Grid

fn grid(b: &mut Build, t: &Table) -> R<GridView> {
    let p = Props::new(
        t,
        "Grid",
        &[
            "title", "search_bar_placeholder", "filtering", "is_loading", "size", "columns", "fit", "empty", "items", "actions",
            "selected_key", "on_search_change", "on_selection_change", "on_key", "on_appear", "on_disappear",
        ],
    )?;
    let size = match p.raw("size")? {
        Value::Nil => GridSize::Medium,
        Value::String(s) => GridSize::parse(&s.to_string_lossy()).ok_or_else(|| format!("Grid.size: unknown size '{}' (small, medium, large or {{ width, height }})", s.to_string_lossy()))?,
        Value::Table(tt) => {
            let sp = Props::new(&tt, "Grid.size", &["width", "height"])?;
            let (w, h) = (sp.num("width")?.unwrap_or(0.0), sp.num("height")?.unwrap_or(0.0));
            if !(16.0..=640.0).contains(&w) || !(16.0..=640.0).contains(&h) {
                return Err("Grid.size: width and height must be between 16 and 640".into());
            }
            GridSize::Custom { width: w as f32, height: h as f32 }
        }
        other => return Err(p.bad("size", "a string or table", &other)),
    };
    let fit = match p.str("fit")?.as_deref() {
        None | Some("contain") => GridFit::Contain,
        Some("fill") => GridFit::Fill,
        Some(o) => return Err(format!("Grid.fit: unknown value '{o}' (contain, fill)")),
    };
    let columns = match p.num("columns")? {
        Some(n) if !(1.0..=24.0).contains(&n) || n.fract() != 0.0 => return Err("Grid.columns: expected an integer between 1 and 24".into()),
        n => n.map(|n| n as u32),
    };
    let mut view = GridView {
        title: p.str("title")?,
        search_bar_placeholder: p.str("search_bar_placeholder")?,
        filtering: p.bool("filtering")?.unwrap_or(true),
        is_loading: p.bool("is_loading")?.unwrap_or(false),
        selected_key: p.str("selected_key")?,
        size,
        columns,
        fit,
        ..Default::default()
    };
    view.events = events(b, &p)?;
    view.actions = action_sections(b, p.raw("actions")?, "global", "Grid.actions")?;
    if let Some(e) = p.table("empty")? {
        view.empty = empty_view(b, &e, "Grid.empty")?;
    }
    let mut loose: Option<Section<GridItem>> = None;
    let mut flat = 0usize;
    for (i, c) in entries(t, "items", "Grid")?.into_iter().enumerate() {
        let path = format!("Grid[{}]", i + 1);
        match kind_of(&c).as_deref() {
            Some("GridSection") => {
                if let Some(l) = loose.take() {
                    view.sections.push(l);
                }
                let sp = Props::new(&c, &path, &["key", "title", "subtitle", "items"])?;
                let key = sp.str("key")?.unwrap_or_else(|| format!("#s{}", view.sections.len()));
                let mut s = Section { key, title: sp.str("title")?, subtitle: sp.str("subtitle")?, items: vec![] };
                for (j, it) in entries(&c, "items", &path)?.into_iter().enumerate() {
                    s.items.push(grid_item(b, &it, &format!("{path}.Item[{}]", j + 1), &mut flat)?);
                }
                view.sections.push(s);
            }
            Some("GridEmpty") => view.empty = empty_view(b, &c, &format!("{path} (EmptyView)"))?,
            Some("GridItem") | None => {
                let sec = loose.get_or_insert_with(|| Section { key: format!("#loose{}", view.sections.len()), ..Default::default() });
                sec.items.push(grid_item(b, &c, &format!("Grid.Item[{}]", i + 1), &mut flat)?);
            }
            Some(other) => return Err(format!("{path}: '{other}' cannot be used inside ui.Grid")),
        }
    }
    if let Some(l) = loose {
        view.sections.push(l);
    }
    check_section_keys(&view.sections.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(), "Grid")?;
    Ok(view)
}

fn grid_item(b: &mut Build, t: &Table, path: &str, flat: &mut usize) -> R<GridItem> {
    let p = Props::new(t, path, &["key", "content", "title", "subtitle", "keywords", "actions"])?;
    let key = p.str("key")?.unwrap_or_else(|| format!("#{flat}"));
    *flat += 1;
    claim_key(b, &key, path)?;
    let cpath = p.at("content");
    let content = match p.raw("content")? {
        Value::Nil => return Err(format!("{cpath}: required (ui.Image, ui.Icon or a string)")),
        Value::String(s) => {
            let s = s.to_string_lossy().to_string();
            let file_like = s.starts_with("http://") || s.starts_with("https://") || s.contains('/') || (s.contains('.') && !s.contains(' '));
            if file_like {
                GridContent::Image(string_source(b, &cpath, &s).map(|source| Image { source, mask: ImageMask::None, fallback: None })?)
            } else if BUILTIN_ICONS.contains(&s.as_str()) {
                GridContent::Icon(Icon::from_str(&s))
            } else {
                GridContent::Text(s)
            }
        }
        Value::Table(ct) => match kind_of(&ct).as_deref() {
            Some("Icon") => GridContent::Icon(icon_value(b, Value::Table(ct), &cpath)?.expect("icon table")),
            _ => GridContent::Image(image_value(b, Value::Table(ct), &cpath)?),
        },
        other => return Err(p.bad("content", "ui.Image, ui.Icon or a string", &other)),
    };
    Ok(GridItem {
        content,
        title: p.str("title")?,
        subtitle: p.str("subtitle")?,
        keywords: p.strings("keywords")?,
        actions: action_sections(b, p.raw("actions")?, &format!("item:{key}"), &p.at("actions"))?,
        key,
    })
}

// ----------------------------------------------------------------- Detail

fn detail(b: &mut Build, t: &Table) -> R<DetailView> {
    let p = Props::new(t, "Detail", &["title", "markdown", "is_loading", "metadata", "actions", "on_key", "on_appear", "on_disappear"])?;
    let mut view = DetailView { title: p.str("title")?, is_loading: p.bool("is_loading")?.unwrap_or(false), ..Default::default() };
    view.events = events(b, &p)?;
    view.actions = action_sections(b, p.raw("actions")?, "global", "Detail.actions")?;
    if let Some(md) = p.str("markdown")? {
        view.blocks.push(DetailBlock::Markdown(md));
    }
    for (i, c) in children(t, "Detail")?.into_iter().enumerate() {
        let path = format!("Detail[{}]", i + 1);
        match kind_of(&c).as_deref() {
            Some("DetailMarkdown") => {
                let text = match c.get::<Value>(1).map_err(|e| e.to_string())? {
                    Value::String(s) => s.to_string_lossy().to_string(),
                    _ => Props::new(&c, &path, &["text"])?.require_str("text")?,
                };
                view.blocks.push(DetailBlock::Markdown(text));
            }
            Some("ProgressBar") => view.blocks.push(progress_block(&c, &path)?),
            Some("LoadingIndicator") => {
                let lp = Props::new(&c, &path, &["label"])?;
                view.blocks.push(DetailBlock::LoadingIndicator { label: lp.str("label")? });
            }
            Some(other) => return Err(format!("{path}: '{other}' cannot be used inside ui.Detail")),
            None => return Err(format!("{path}: expected a component")),
        }
    }
    if let Some(m) = p.table("metadata")? {
        view.metadata = metadata(b, &m)?;
    }
    Ok(view)
}

fn progress_block(c: &Table, path: &str) -> R<DetailBlock> {
    let pp = Props::new(c, path, &["value", "label"])?;
    let value = match pp.num("value")? {
        Some(v) if !(0.0..=1.0).contains(&v) => return Err(format!("{}: expected a number between 0 and 1, got {v}", pp.at("value"))),
        v => v.map(|v| v as f32),
    };
    Ok(DetailBlock::ProgressBar { value, label: pp.str("label")? })
}

fn metadata(b: &Build, m: &Table) -> R<Vec<MetaItem>> {
    let mut out = Vec::new();
    for (i, c) in children(m, "Detail.Metadata")?.into_iter().enumerate() {
        let path = format!("Detail.Metadata[{}]", i + 1);
        out.push(match kind_of(&c).as_deref() {
            Some("MetaLabel") => {
                let p = Props::new(&c, &path, &["title", "text", "icon"])?;
                MetaItem::Label { title: p.require_str("title")?, text: p.str("text")?.unwrap_or_default(), icon: icon_prop(b, &p, "icon")? }
            }
            Some("MetaTagList") => {
                let p = Props::new(&c, &path, &["title", "tags"])?;
                let mut tags = Vec::new();
                for (j, tc) in entries(&c, "tags", &path)?.into_iter().enumerate() {
                    let tp = Props::new(&tc, format!("{path}.tags[{}]", j + 1), &["text", "color"])?;
                    tags.push(Tag { text: tp.require_str("text")?, color: color(&tp, "color")? });
                }
                MetaItem::TagList { title: p.require_str("title")?, tags }
            }
            Some("MetaLink") => {
                let p = Props::new(&c, &path, &["title", "text", "url"])?;
                let url = p.require_str("url")?;
                if !(url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:")) {
                    return Err(format!("{}: only http://, https:// and mailto: links are allowed", p.at("url")));
                }
                let text = p.str("text")?.unwrap_or_else(|| url.clone());
                MetaItem::Link { title: p.require_str("title")?, text, url }
            }
            Some("MetaSeparator") => MetaItem::Separator,
            Some(other) => return Err(format!("{path}: '{other}' cannot be used inside ui.Detail.Metadata")),
            None => return Err(format!("{path}: expected ui.Detail.Metadata.Label, TagList, Link or Separator")),
        });
    }
    Ok(out)
}

// ------------------------------------------------------------------- Form

fn field_value(p: &Props, key: &str) -> R<Option<FieldValue>> {
    Ok(match p.raw(key)? {
        Value::Nil => None,
        Value::Boolean(v) => Some(FieldValue::Bool(v)),
        Value::String(s) => Some(FieldValue::Text(s.to_string_lossy().to_string())),
        Value::Integer(i) => Some(FieldValue::Text(i.to_string())),
        Value::Number(n) => Some(FieldValue::Text(n.to_string())),
        Value::Table(_) => Some(FieldValue::List(p.strings(key)?)),
        other => return Err(p.bad(key, "a string, boolean or list of strings", &other)),
    })
}

fn options(c: &Table, path: &str, b: &Build) -> R<Vec<FieldOption>> {
    let mut out = Vec::new();
    let mut push = |t: &Table, section: Option<String>, path: &str| -> R<()> {
        let p = Props::new(t, path, &["value", "title", "icon"])?;
        let value = p.require_str("value")?;
        out.push(FieldOption { title: p.str("title")?.unwrap_or_else(|| value.clone()), value, icon: icon_prop(b, &p, "icon")?, section });
        Ok(())
    };
    for (i, o) in entries(c, "options", path)?.into_iter().enumerate() {
        let opath = format!("{path}[{}]", i + 1);
        match kind_of(&o).as_deref() {
            Some("FormOptionSection") => {
                let sp = Props::new(&o, &opath, &["title", "options"])?;
                let title = sp.require_str("title")?;
                for (j, it) in entries(&o, "options", &opath)?.into_iter().enumerate() {
                    push(&it, Some(title.clone()), &format!("{opath}[{}]", j + 1))?;
                }
            }
            Some("FormOption") | None => push(&o, None, &opath)?,
            Some(other) => return Err(format!("{opath}: '{other}' cannot be used as an option")),
        }
    }
    Ok(out)
}

const FIELD_COMMON: &[&str] = &[
    "id", "title", "info", "placeholder", "default_value", "value", "required", "error", "autofocus", "on_change", "validate",
    "min_length", "max_length", "format", "min", "max",
];

fn form(b: &mut Build, t: &Table) -> R<FormView> {
    let p = Props::new(t, "Form", &["title", "submit_title", "is_loading", "actions", "on_submit", "validate", "on_key", "on_appear", "on_disappear"])?;
    let mut view = FormView {
        title: p.str("title")?,
        submit_title: p.str("submit_title")?,
        is_loading: p.bool("is_loading")?.unwrap_or(false),
        ..Default::default()
    };
    view.events = events(b, &p)?;
    view.actions = action_sections(b, p.raw("actions")?, "global", "Form.actions")?;
    if let Some(f) = p.func("on_submit")? {
        b.callback("on_submit", f);
    }
    if let Some(f) = p.func("validate")? {
        b.callback("validate", f);
    }
    let mut ids = HashSet::new();
    for (i, c) in children(t, "Form")?.into_iter().enumerate() {
        let path = format!("Form[{}]", i + 1);
        let field = form_field(b, &c, &path)?;
        if !field.id.is_empty() && !ids.insert(field.id.clone()) {
            return Err(format!("{path}: duplicate field id '{}'", field.id));
        }
        view.fields.push(field);
    }
    Ok(view)
}

fn form_field(b: &mut Build, c: &Table, path: &str) -> R<Field> {
    let kind_name = kind_of(c).ok_or_else(|| format!("{path}: expected a form field (ui.Form.TextField, …)"))?;
    // Separator/Description get a synthetic id: they carry no value but still need identity for diffing.
    let synthetic = |what: &str| format!("#{what}{}", path.trim_start_matches("Form"));
    let (kind, extra): (FieldKind, &[&str]) = match kind_name.as_str() {
        "FormText" => (FieldKind::Text, &[]),
        "FormPassword" => (FieldKind::Password, &[]),
        "FormTextArea" => {
            let rows = Props::new(c, path, &[FIELD_COMMON, &["rows"]].concat())?.num("rows")?.unwrap_or(4.0);
            if !(1.0..=40.0).contains(&rows) {
                return Err(format!("{path}.rows: expected a number between 1 and 40"));
            }
            (FieldKind::TextArea { rows: rows as u32 }, &["rows"])
        }
        "FormCheckbox" => {
            let label = Props::new(c, path, &[FIELD_COMMON, &["label"]].concat())?.str("label")?.unwrap_or_default();
            (FieldKind::Checkbox { label }, &["label"])
        }
        "FormDropdown" => {
            let opts = options(c, path, b)?;
            if opts.is_empty() {
                return Err(format!("{path}: a dropdown needs at least one option (ui.Form.Dropdown.Item)"));
            }
            (FieldKind::Dropdown { options: opts }, &["options"])
        }
        "FormTagPicker" => (FieldKind::TagPicker { options: options(c, path, b)? }, &["options"]),
        "FormDatePicker" => {
            let p = Props::new(c, path, &[FIELD_COMMON, &["type"]].concat())?;
            let include_time = match p.str("type")?.as_deref() {
                None | Some("date") => false,
                Some("datetime") => true,
                Some(o) => return Err(format!("{}: unknown type '{o}' (date, datetime)", p.at("type"))),
            };
            for k in ["min", "max"] {
                if let Some(d) = p.str(k)? {
                    if !valid_iso(&d, include_time) {
                        return Err(format!("{}: '{d}' is not a valid {} (ISO-8601)", p.at(k), if include_time { "YYYY-MM-DDTHH:MM" } else { "YYYY-MM-DD" }));
                    }
                }
            }
            (FieldKind::DatePicker { include_time, min: p.str("min")?, max: p.str("max")? }, &["type"])
        }
        "FormFilePicker" => {
            let p = Props::new(c, path, &[FIELD_COMMON, &["multiple", "directories", "files"]].concat())?;
            let (directories, files) = (p.bool("directories")?.unwrap_or(false), p.bool("files")?.unwrap_or(true));
            if !directories && !files {
                return Err(format!("{path}: a file picker must allow files, directories or both"));
            }
            (FieldKind::FilePicker { multiple: p.bool("multiple")?.unwrap_or(false), directories, files }, &["multiple", "directories", "files"])
        }
        "FormSeparator" => {
            Props::new(c, path, &[])?;
            let mut f = Field::new(&synthetic("sep"), FieldKind::Separator);
            f.title = None;
            return Ok(f);
        }
        "FormDescription" => {
            let p = Props::new(c, path, &["title", "text"])?;
            let mut f = Field::new(&synthetic("desc"), FieldKind::Description { text: p.require_str("text")? });
            f.title = p.str("title")?;
            return Ok(f);
        }
        other => return Err(format!("{path}: '{other}' cannot be used inside ui.Form")),
    };
    let allowed: Vec<&str> = FIELD_COMMON.iter().chain(extra.iter()).copied().collect();
    let p = Props::new(c, path, &allowed)?;
    let id = p.require_str("id")?;
    if id.is_empty() || !id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-') {
        return Err(format!("{}: field ids use letters, digits, '_' and '-'", p.at("id")));
    }
    let mut f = Field::new(&id, kind);
    f.title = p.str("title")?;
    f.info = p.str("info")?;
    f.placeholder = p.str("placeholder")?;
    f.default = field_value(&p, "default_value")?;
    f.value = field_value(&p, "value")?;
    f.required = p.bool("required")?.unwrap_or(false);
    f.error = p.str("error")?;
    f.autofocus = p.bool("autofocus")?.unwrap_or(false);
    let numeric_bounds = !matches!(f.kind, FieldKind::DatePicker { .. });
    f.rules = Rules {
        min_length: p.num("min_length")?.map(|n| n as usize),
        max_length: p.num("max_length")?.map(|n| n as usize),
        format: match p.str("format")? {
            Some(s) => Some(TextFormat::parse(&s).ok_or_else(|| format!("{}: unknown format '{s}' (email, url, integer, number)", p.at("format")))?),
            None => None,
        },
        min: if numeric_bounds { p.num("min")? } else { None },
        max: if numeric_bounds { p.num("max")? } else { None },
    };
    if let Some(cb) = p.func("on_change")? {
        b.callback(&format!("field:{id}:on_change"), cb);
        f.change_events = true;
    }
    if let Some(cb) = p.func("validate")? {
        b.callback(&format!("field:{id}:validate"), cb);
    }
    Ok(f)
}

// ------------------------------------------------------------------ Input

fn input(b: &mut Build, t: &Table) -> R<InputView> {
    let p = Props::new(t, "Input", &["title", "placeholder", "value", "secure", "on_submit"])?;
    let f = p.func("on_submit")?.ok_or("flux.ui.input needs on_submit (function)")?;
    let id = b.callback("on_submit", f);
    Ok(InputView {
        title: p.str("title")?,
        placeholder: p.str("placeholder")?,
        value: p.str("value")?,
        secure: p.bool("secure")?.unwrap_or(false),
        submit_action: id,
    })
}
