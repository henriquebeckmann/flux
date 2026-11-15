//! The component tree extensions describe. Pure data: the runtime builds it from Lua, the
//! core stores/patches it, the UI draws it. Adding a component = a new struct here, a
//! variant in [`ViewNode`], keyed diffing in `patch.rs`, a Lua constructor, a core model and a
//! renderer (see `docs/adding-apis.md`).

use serde::{Deserialize, Serialize};

use crate::{ActionSection, Shortcut};

/// Names of the icons shipped with the launcher (drawn as SVG). Any other icon string is text/emoji.
pub const BUILTIN_ICONS: &[&str] = &[
    "app", "archive", "bell", "book", "bookmark", "calculator", "calendar", "check", "chevron-right", "clipboard",
    "clock", "code", "copy", "cpu", "download", "external-link", "file", "file-text", "film", "folder", "folder-open",
    "gear", "git-branch", "globe", "hard-drive", "history", "image", "info", "key", "link", "list", "lock", "music",
    "package", "plus", "refresh", "search", "star", "tag", "terminal", "trash", "user", "warning", "x",
];

// ---------------------------------------------------------------------------
// Images and icons
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ImageSource {
    /// Relative to the extension directory (never outside it) or absolute for `filesystem` extensions.
    Path(String),
    /// http(s) image; requires the `network` permission.
    Url(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImageMask {
    #[default]
    None,
    Circle,
    RoundedRectangle,
}

impl ImageMask {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "none" => Self::None,
            "circle" => Self::Circle,
            "rounded" | "rounded-rectangle" => Self::RoundedRectangle,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Image {
    pub source: ImageSource,
    pub mask: ImageMask,
    /// Shown while loading or when the image cannot be loaded (icon name or text).
    pub fallback: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum IconKind {
    /// One of [`BUILTIN_ICONS`].
    Named(String),
    /// A short text or emoji.
    Text(String),
    Image(Image),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Icon {
    pub kind: IconKind,
    /// A [`Color`] name or `#rrggbb`.
    pub tint: Option<String>,
}

impl Icon {
    /// A bare string: builtin icon name if it is one, otherwise text.
    pub fn from_str(s: &str) -> Icon {
        let kind = if BUILTIN_ICONS.contains(&s) { IconKind::Named(s.into()) } else { IconKind::Text(s.into()) };
        Icon { kind, tint: None }
    }
}

/// Color names accepted by tags and tints.
pub const COLOR_NAMES: &[&str] = &["red", "orange", "yellow", "green", "blue", "purple", "pink", "gray"];

/// `true` for a known color name or a `#rgb`/`#rrggbb` literal.
pub fn valid_color(s: &str) -> bool {
    COLOR_NAMES.contains(&s)
        || (s.starts_with('#') && matches!(s.len(), 4 | 7) && s[1..].chars().all(|c| c.is_ascii_hexdigit()))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tag {
    pub text: String,
    pub color: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared pieces
// ---------------------------------------------------------------------------

/// Which events the view's author listens to, so the core only forwards what is used.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Events {
    pub search_change: bool,
    pub selection_change: bool,
    /// View-scoped shortcuts (`on_key`), matched by the core before the text field.
    pub key_bindings: Vec<KeyBinding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyBinding {
    pub shortcut: Shortcut,
    /// Callback id inside the view.
    pub action: u64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EmptyView {
    pub title: Option<String>,
    pub description: Option<String>,
    pub icon: Option<Icon>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section<T> {
    pub key: String,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub items: Vec<T>,
}

impl<T> Default for Section<T> {
    fn default() -> Self {
        Self { key: String::new(), title: None, subtitle: None, items: vec![] }
    }
}

/// Anything with a stable identity inside its collection.
pub trait Keyed {
    fn key(&self) -> &str;
}

impl<T> Keyed for Section<T> {
    fn key(&self) -> &str {
        &self.key
    }
}

// ---------------------------------------------------------------------------
// View nodes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ViewNode {
    List(ListView),
    Grid(GridView),
    Detail(DetailView),
    Form(FormView),
    /// A single-value prompt: the launcher's search field becomes the value field.
    Input(InputView),
}

impl ViewNode {
    pub fn title(&self) -> Option<&str> {
        match self {
            ViewNode::List(v) => v.title.as_deref(),
            ViewNode::Grid(v) => v.title.as_deref(),
            ViewNode::Detail(v) => v.title.as_deref(),
            ViewNode::Form(v) => v.title.as_deref(),
            ViewNode::Input(v) => v.title.as_deref(),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            ViewNode::List(_) => "list",
            ViewNode::Grid(_) => "grid",
            ViewNode::Detail(_) => "detail",
            ViewNode::Form(_) => "form",
            ViewNode::Input(_) => "input",
        }
    }

    pub fn is_loading(&self) -> bool {
        match self {
            ViewNode::List(v) => v.is_loading,
            ViewNode::Grid(v) => v.is_loading,
            ViewNode::Detail(v) => v.is_loading,
            ViewNode::Form(v) => v.is_loading,
            ViewNode::Input(_) => false,
        }
    }

    pub fn events(&self) -> Option<&Events> {
        match self {
            ViewNode::List(v) => Some(&v.events),
            ViewNode::Grid(v) => Some(&v.events),
            ViewNode::Detail(v) => Some(&v.events),
            ViewNode::Form(v) => Some(&v.events),
            ViewNode::Input(_) => None,
        }
    }

    /// Global actions of the view (Action Panel, after the focused item's own).
    pub fn global_actions(&self) -> &[ActionSection] {
        match self {
            ViewNode::List(v) => &v.actions,
            ViewNode::Grid(v) => &v.actions,
            ViewNode::Detail(v) => &v.actions,
            ViewNode::Form(v) => &v.actions,
            ViewNode::Input(_) => &[],
        }
    }
}

// ----------------------------------------------------------------- List

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListView {
    pub title: Option<String>,
    pub search_bar_placeholder: Option<String>,
    /// `false` = the extension filters (remote search): the launcher shows items as given.
    pub filtering: bool,
    pub is_loading: bool,
    pub empty: EmptyView,
    pub sections: Vec<Section<ListItem>>,
    /// Global actions: shown in the Action Panel for every item, after the item's own.
    pub actions: Vec<ActionSection>,
    /// Select this item when the view is shown/refreshed.
    pub selected_key: Option<String>,
    pub events: Events,
}

impl Default for ListView {
    fn default() -> Self {
        Self {
            title: None,
            search_bar_placeholder: None,
            filtering: true,
            is_loading: false,
            empty: EmptyView::default(),
            sections: vec![],
            actions: vec![],
            selected_key: None,
            events: Events::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Accessory {
    pub text: Option<String>,
    pub icon: Option<Icon>,
    pub tag: Option<Tag>,
    /// 0.0..=1.0
    pub progress: Option<f32>,
    pub tooltip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ListItem {
    pub key: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub icon: Option<Icon>,
    pub accessories: Vec<Accessory>,
    /// Extra search terms.
    pub keywords: Vec<String>,
    /// The first action of the first section is the primary one (Enter).
    pub actions: Vec<ActionSection>,
}

impl Keyed for ListItem {
    fn key(&self) -> &str {
        &self.key
    }
}

impl ListView {
    pub fn items(&self) -> impl Iterator<Item = &ListItem> {
        self.sections.iter().flat_map(|s| s.items.iter())
    }

    pub fn item_count(&self) -> usize {
        self.sections.iter().map(|s| s.items.len()).sum()
    }

    /// Item by flat index (sections concatenated).
    pub fn item(&self, flat: usize) -> Option<&ListItem> {
        self.items().nth(flat)
    }

    /// Sections shown in the Action Panel for the item at `flat`: its own first, then the global ones.
    pub fn sections_for(&self, flat: usize) -> Vec<ActionSection> {
        let mut v = self.item(flat).map(|i| i.actions.clone()).unwrap_or_default();
        v.extend(self.actions.iter().cloned());
        v
    }
}

// ----------------------------------------------------------------- Grid

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum GridSize {
    Small,
    Medium,
    Large,
    /// Cell box in logical pixels.
    Custom { width: f32, height: f32 },
}

impl GridSize {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "small" => Self::Small,
            "medium" => Self::Medium,
            "large" => Self::Large,
            _ => return None,
        })
    }

    /// (width, height) of the image area, in logical pixels.
    pub fn cell(self) -> (f32, f32) {
        match self {
            Self::Small => (72., 72.),
            Self::Medium => (112., 112.),
            Self::Large => (168., 168.),
            Self::Custom { width, height } => (width, height),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GridFit {
    #[default]
    Contain,
    Fill,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridView {
    pub title: Option<String>,
    pub search_bar_placeholder: Option<String>,
    pub filtering: bool,
    pub is_loading: bool,
    pub size: GridSize,
    /// Forces the number of columns (otherwise derived from the width and `size`).
    pub columns: Option<u32>,
    pub fit: GridFit,
    pub empty: EmptyView,
    pub sections: Vec<Section<GridItem>>,
    pub actions: Vec<ActionSection>,
    pub selected_key: Option<String>,
    pub events: Events,
}

impl Default for GridView {
    fn default() -> Self {
        Self {
            title: None,
            search_bar_placeholder: None,
            filtering: true,
            is_loading: false,
            size: GridSize::Medium,
            columns: None,
            fit: GridFit::Contain,
            empty: EmptyView::default(),
            sections: vec![],
            actions: vec![],
            selected_key: None,
            events: Events::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GridContent {
    Image(Image),
    Icon(Icon),
    Text(String),
}

impl Default for GridContent {
    fn default() -> Self {
        GridContent::Text(String::new())
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GridItem {
    pub key: String,
    pub content: GridContent,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub keywords: Vec<String>,
    pub actions: Vec<ActionSection>,
}

impl Keyed for GridItem {
    fn key(&self) -> &str {
        &self.key
    }
}

impl GridView {
    pub fn items(&self) -> impl Iterator<Item = &GridItem> {
        self.sections.iter().flat_map(|s| s.items.iter())
    }

    pub fn item(&self, flat: usize) -> Option<&GridItem> {
        self.items().nth(flat)
    }

    pub fn sections_for(&self, flat: usize) -> Vec<ActionSection> {
        let mut v = self.item(flat).map(|i| i.actions.clone()).unwrap_or_default();
        v.extend(self.actions.iter().cloned());
        v
    }
}

// --------------------------------------------------------------- Detail

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DetailBlock {
    Markdown(String),
    /// `value` in 0.0..=1.0; `None` = indeterminate.
    ProgressBar { value: Option<f32>, label: Option<String> },
    LoadingIndicator { label: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MetaItem {
    Label { title: String, text: String, icon: Option<Icon> },
    TagList { title: String, tags: Vec<Tag> },
    Link { title: String, text: String, url: String },
    Separator,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DetailView {
    pub title: Option<String>,
    pub is_loading: bool,
    pub blocks: Vec<DetailBlock>,
    pub metadata: Vec<MetaItem>,
    pub actions: Vec<ActionSection>,
    pub events: Events,
}

// ----------------------------------------------------------------- Form

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldValue {
    Bool(bool),
    Text(String),
    List(Vec<String>),
}

impl FieldValue {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            FieldValue::Text(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextFormat {
    Email,
    Url,
    Integer,
    Number,
}

impl TextFormat {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "email" => Self::Email,
            "url" => Self::Url,
            "integer" => Self::Integer,
            "number" => Self::Number,
            _ => return None,
        })
    }
}

/// Declarative validation, evaluated by the core on every change/submit (no round trip to Lua).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Rules {
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub format: Option<TextFormat>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldOption {
    pub value: String,
    pub title: String,
    pub icon: Option<Icon>,
    /// Group heading (`Dropdown.Section`).
    pub section: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldKind {
    Text,
    Password,
    TextArea { rows: u32 },
    Checkbox { label: String },
    Dropdown { options: Vec<FieldOption> },
    TagPicker { options: Vec<FieldOption> },
    /// ISO-8601: `YYYY-MM-DD`, or `YYYY-MM-DDTHH:MM` with `include_time`.
    DatePicker { include_time: bool, min: Option<String>, max: Option<String> },
    FilePicker { multiple: bool, directories: bool, files: bool },
    Separator,
    Description { text: String },
}

impl FieldKind {
    /// Separators and descriptions carry no value.
    pub fn has_value(&self) -> bool {
        !matches!(self, FieldKind::Separator | FieldKind::Description { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// Unique inside the form; also the key of the submitted values table.
    pub id: String,
    pub kind: FieldKind,
    pub title: Option<String>,
    pub info: Option<String>,
    pub placeholder: Option<String>,
    /// Initial value (uncontrolled): applied once; later renders do not override what the user typed.
    pub default: Option<FieldValue>,
    /// Controlled value: applied whenever it changes between renders.
    pub value: Option<FieldValue>,
    pub required: bool,
    pub rules: Rules,
    /// Error set by the extension (shown until the field changes).
    pub error: Option<String>,
    pub autofocus: bool,
    /// The extension wants `on_change` events for this field.
    pub change_events: bool,
}

impl Field {
    pub fn new(id: &str, kind: FieldKind) -> Self {
        Self {
            id: id.into(),
            kind,
            title: None,
            info: None,
            placeholder: None,
            default: None,
            value: None,
            required: false,
            rules: Rules::default(),
            error: None,
            autofocus: false,
            change_events: false,
        }
    }
}

impl Keyed for Field {
    fn key(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FormView {
    pub title: Option<String>,
    pub submit_title: Option<String>,
    pub is_loading: bool,
    pub fields: Vec<Field>,
    pub actions: Vec<ActionSection>,
    pub events: Events,
}

// ---------------------------------------------------------------- Input

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct InputView {
    pub title: Option<String>,
    pub placeholder: Option<String>,
    pub value: Option<String>,
    /// Mask the typed text (passwords, tokens).
    pub secure: bool,
    /// Id of the callback invoked with the submitted text.
    pub submit_action: u64,
}

/// `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM`, with real calendar bounds.
pub fn valid_iso(s: &str, with_time: bool) -> bool {
    let (date, time) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    if with_time != time.is_some() {
        return false;
    }
    let parts: Vec<&str> = date.split('-').collect();
    let [y, m, d] = parts[..] else { return false };
    let (Ok(y), Ok(m), Ok(d)) = (y.parse::<i32>(), m.parse::<u32>(), d.parse::<u32>()) else { return false };
    if y.to_string().len() != 4 || !(1..=12).contains(&m) || d == 0 {
        return false;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = match m {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if d > dim {
        return false;
    }
    match time {
        None => true,
        Some(t) => match t.split_once(':').map(|(h, mi)| (h.parse::<u32>(), mi.parse::<u32>())) {
            Some((Ok(h), Ok(mi))) => h < 24 && mi < 60 && t.len() == 5,
            _ => false,
        },
    }
}


// ---------------------------------------------------------------------------
// Network safety shared by the runtime (flux.http) and the UI's image loader
// ---------------------------------------------------------------------------

fn is_private_v4(ip: std::net::Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || (o[0] == 100 && (64..128).contains(&o[1])) // carrier-grade NAT
        || o[0] == 0
}

pub fn is_private_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => is_private_v4(v4),
        std::net::IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_v4(v4);
            }
            let s0 = v6.segments()[0];
            v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || (s0 & 0xfe00) == 0xfc00 || (s0 & 0xffc0) == 0xfe80
        }
    }
}

