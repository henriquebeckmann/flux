//! The screen stack. Pure data: no UI toolkit, no Lua. The runtime decides *what* to
//! push/pop/replace/patch ([`NavEvent`]); the user decides when to go back. State (query,
//! selection, form values) lives with each entry so popping restores the previous screen exactly.

use flux_api::{NavEvent, ViewNode, apply_patch};

use crate::form::FormState;

#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    Root,
    /// A command was started and has not produced a view (or finished) yet.
    Loading { command_id: String, title: String },
    /// A screen described by an extension (or by the core itself, for built-in screens).
    View { view_id: u64, title: String, node: ViewNode },
}

impl Screen {
    pub fn view_id(&self) -> Option<u64> {
        match self {
            Screen::View { view_id, .. } => Some(*view_id),
            _ => None,
        }
    }

    pub fn node(&self) -> Option<&ViewNode> {
        match self {
            Screen::View { node, .. } => Some(node),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub screen: Screen,
    pub query: String,
    /// Index among the *visible* items (after filtering).
    pub selected: usize,
    /// Identity of the selected item: survives re-renders that move/insert items.
    pub selected_key: Option<String>,
    pub form: Option<FormState>,
    /// Last key reported with `on_selection_change`, to avoid repeating events.
    pub reported_key: Option<Option<String>>,
}

impl Entry {
    fn new(screen: Screen) -> Self {
        let query = match screen.node() {
            Some(ViewNode::Input(v)) => v.value.clone().unwrap_or_default(),
            _ => String::new(),
        };
        let form = match screen.node() {
            Some(ViewNode::Form(f)) => Some(FormState::new(f)),
            _ => None,
        };
        let selected_key = match screen.node() {
            Some(ViewNode::List(l)) => l.selected_key.clone(),
            Some(ViewNode::Grid(g)) => g.selected_key.clone(),
            _ => None,
        };
        Self { screen, query, selected: 0, selected_key, form, reported_key: None }
    }
}

/// What the owner must tell the runtime after a navigation change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Closed {
    View(u64),
    Command(String),
}

/// Result of applying a runtime event.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub closed: Vec<Closed>,
    /// The view that was patched in place (its selection must be re-synchronised by key).
    pub patched: Option<u64>,
    /// The search text of this screen was replaced by the extension.
    pub search_changed: Option<u64>,
    /// The event was invalid; the stack is unchanged.
    pub error: Option<String>,
}

pub struct Navigator {
    stack: Vec<Entry>,
}

impl Default for Navigator {
    fn default() -> Self {
        Self { stack: vec![Entry::new(Screen::Root)] }
    }
}

fn screen_for(view_id: u64, title: String, node: ViewNode) -> Screen {
    let title = node.title().map(String::from).unwrap_or(title);
    Screen::View { view_id, title, node }
}

impl Navigator {
    pub fn top(&self) -> &Entry {
        self.stack.last().expect("root is never popped")
    }

    pub fn top_mut(&mut self) -> &mut Entry {
        self.stack.last_mut().expect("root is never popped")
    }

    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    pub fn is_root(&self) -> bool {
        self.stack.len() == 1
    }

    /// View ids of every screen on the stack.
    pub fn view_ids(&self) -> Vec<u64> {
        self.stack.iter().filter_map(|e| e.screen.view_id()).collect()
    }

    pub fn entry_for_view(&mut self, view_id: u64) -> Option<&mut Entry> {
        self.stack.iter_mut().rev().find(|e| e.screen.view_id() == Some(view_id))
    }

    /// Titles from the root down: the navigation history for breadcrumbs/tests.
    pub fn titles(&self) -> Vec<String> {
        self.stack
            .iter()
            .map(|e| match &e.screen {
                Screen::Root => "Root".to_string(),
                Screen::Loading { title, .. } | Screen::View { title, .. } => title.clone(),
            })
            .collect()
    }

    pub fn push_loading(&mut self, command_id: &str, title: &str) {
        self.stack.push(Entry::new(Screen::Loading { command_id: command_id.into(), title: title.into() }));
    }

    /// Pushes a screen built by the core itself (built-in screens). The caller owns `view_id`.
    pub fn push_view(&mut self, view_id: u64, title: &str, node: ViewNode) {
        let title = title.to_string();
        self.stack.push(Entry::new(screen_for(view_id, title, node)));
    }

    /// Applies a runtime navigation event. A malformed event (e.g. popping an unknown view,
    /// patching a screen that is gone) leaves the stack unchanged and reports why.
    pub fn apply(&mut self, ev: NavEvent) -> Outcome {
        let mut out = Outcome::default();
        match ev {
            NavEvent::Push { view_id, view } => {
                if let Screen::Loading { title, .. } = &self.top().screen {
                    // The loading placeholder becomes the first screen of the command.
                    let screen = screen_for(view_id, title.clone(), view);
                    *self.top_mut() = Entry::new(screen);
                } else {
                    let title = self.top_title();
                    self.stack.push(Entry::new(screen_for(view_id, title, view)));
                }
            }
            NavEvent::Replace { old_view_id, view_id, view } => {
                let pos = self.stack.iter().rposition(|e| e.screen.view_id() == Some(old_view_id));
                let Some(pos) = pos else {
                    let title = self.top_title();
                    self.stack.push(Entry::new(screen_for(view_id, title, view)));
                    return out;
                };
                let old = &self.stack[pos];
                let same_kind = old.screen.node().map(ViewNode::kind) == Some(view.kind());
                let (query, selected, key) = (old.query.clone(), old.selected, old.selected_key.clone());
                let title = match &old.screen {
                    Screen::View { title, .. } => title.clone(),
                    _ => String::new(),
                };
                let mut entry = Entry::new(screen_for(view_id, title, view));
                if same_kind && !matches!(entry.screen.node(), Some(ViewNode::Form(_))) {
                    // Refreshing a list keeps what the user typed and where the selection was.
                    entry.query = query;
                    entry.selected = selected;
                    entry.selected_key = key.or(entry.selected_key);
                }
                self.stack[pos] = entry;
            }
            NavEvent::Pop { view_id } => {
                if self.top().screen.view_id() == Some(view_id) {
                    self.stack.pop();
                }
            }
            NavEvent::Update { view_id, patch } => match self.entry_for_view(view_id) {
                None => out.error = Some(format!("update for unknown screen {view_id}")),
                Some(entry) => {
                    let Screen::View { node, title, .. } = &mut entry.screen else { return out };
                    match apply_patch(node, &patch) {
                        Ok(()) => {
                            if let Some(t) = node.title() {
                                *title = t.to_string();
                            }
                            if let (Some(form), ViewNode::Form(view)) = (&mut entry.form, &*node) {
                                form.reconcile(view);
                            }
                            out.patched = Some(view_id);
                        }
                        Err(e) => out.error = Some(format!("could not apply an update: {e}")),
                    }
                }
            },
            NavEvent::SetSearch { view_id, text } => {
                if let Some(e) = self.entry_for_view(view_id) {
                    e.query = text;
                    e.selected = 0;
                    e.selected_key = None;
                    out.patched = Some(view_id);
                    out.search_changed = Some(view_id);
                }
            }
            NavEvent::FormErrors { view_id, errors } => {
                if let Some(form) = self.entry_for_view(view_id).and_then(|e| e.form.as_mut()) {
                    form.set_external_errors(&errors);
                    out.patched = Some(view_id);
                }
            }
        }
        out
    }

    fn top_title(&self) -> String {
        self.titles().pop().unwrap_or_default()
    }

    /// The command failed or finished without a view: drop its loading placeholder.
    pub fn finish_loading(&mut self, command_id: &str) -> bool {
        if matches!(&self.top().screen, Screen::Loading { command_id: c, .. } if c == command_id) {
            self.stack.pop();
            true
        } else {
            false
        }
    }

    pub fn fail_loading(&mut self) {
        if matches!(self.top().screen, Screen::Loading { .. }) {
            self.stack.pop();
        }
    }

    /// Removes the screens of an extension whose worker is gone: its views (`views`) and its
    /// loading placeholders (command ids are `<extension>:<command>`). Returns whether any was removed.
    pub fn close_extension(&mut self, views: &[u64], extension: &str) -> bool {
        let prefix = format!("{extension}:");
        let before = self.stack.len();
        self.stack.retain(|e| match &e.screen {
            Screen::Root => true,
            Screen::View { view_id, .. } => !views.contains(view_id),
            Screen::Loading { command_id, .. } => !command_id.starts_with(&prefix),
        });
        self.stack.len() != before
    }

    /// User-initiated back. Returns what was closed, or `None` when already at the root.
    pub fn back(&mut self) -> Option<Closed> {
        if self.is_root() {
            return None;
        }
        let entry = self.stack.pop()?;
        Some(match entry.screen {
            Screen::Loading { command_id, .. } => Closed::Command(command_id),
            Screen::View { view_id, .. } => Closed::View(view_id),
            Screen::Root => unreachable!(),
        })
    }

    /// Back to a clean root (used after being hidden for a while).
    pub fn reset(&mut self) -> Vec<Closed> {
        let mut closed = Vec::new();
        while let Some(c) = self.back() {
            closed.push(c);
        }
        *self.top_mut() = Entry::new(Screen::Root);
        closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_api::*;

    fn item(key: &str) -> ListItem {
        ListItem { key: key.into(), title: key.into(), ..Default::default() }
    }

    pub fn list(title: &str, n: usize) -> ViewNode {
        ViewNode::List(ListView {
            title: Some(title.into()),
            sections: vec![Section { key: "s".into(), items: (0..n).map(|i| item(&format!("i{i}"))).collect(), ..Default::default() }],
            ..Default::default()
        })
    }

    #[test]
    fn loading_becomes_first_screen_then_push_pop_keeps_state() {
        let mut nav = Navigator::default();
        nav.push_loading("x:main", "X");
        assert_eq!(nav.depth(), 2);
        nav.apply(NavEvent::Push { view_id: 1, view: list("One", 3) });
        assert_eq!(nav.depth(), 2, "loading is replaced, not stacked");
        nav.top_mut().query = "abc".into();
        nav.top_mut().selected = 2;

        nav.apply(NavEvent::Push { view_id: 2, view: list("Two", 1) });
        assert_eq!(nav.titles(), ["Root", "One", "Two"]);
        assert_eq!(nav.top().query, "", "new screens start clean");

        nav.apply(NavEvent::Pop { view_id: 2 });
        assert_eq!((nav.top().query.as_str(), nav.top().selected), ("abc", 2), "state is restored");
    }

    #[test]
    fn replace_refreshes_in_place_and_keeps_query_and_selection() {
        let mut nav = Navigator::default();
        nav.push_loading("x:main", "X");
        nav.apply(NavEvent::Push { view_id: 1, view: list("One", 3) });
        nav.top_mut().query = "i".into();
        nav.top_mut().selected = 1;
        nav.apply(NavEvent::Replace { old_view_id: 1, view_id: 5, view: list("One", 4) });
        assert_eq!(nav.depth(), 2);
        assert_eq!((nav.top().query.as_str(), nav.top().selected), ("i", 1));
        assert_eq!(nav.top().screen.view_id(), Some(5));
    }

    #[test]
    fn replacing_a_list_with_an_input_resets_state() {
        let mut nav = Navigator::default();
        nav.push_loading("x:main", "X");
        nav.apply(NavEvent::Push { view_id: 1, view: list("One", 3) });
        nav.top_mut().query = "zzz".into();
        nav.apply(NavEvent::Replace { old_view_id: 1, view_id: 2, view: ViewNode::Input(InputView { value: Some("init".into()), ..Default::default() }) });
        assert_eq!(nav.top().query, "init");
    }

    #[test]
    fn invalid_events_leave_the_stack_unchanged() {
        let mut nav = Navigator::default();
        nav.push_loading("x:main", "X");
        nav.apply(NavEvent::Push { view_id: 1, view: list("One", 1) });
        let before = nav.titles();
        nav.apply(NavEvent::Pop { view_id: 99 }); // not the top view
        assert_eq!(nav.titles(), before);
        nav.apply(NavEvent::Pop { view_id: 1 });
        nav.apply(NavEvent::Pop { view_id: 1 }); // root is never popped
        assert!(nav.is_root());
        let out = nav.apply(NavEvent::Update { view_id: 77, patch: ViewPatch::Replace(list("x", 1)) });
        assert!(out.error.is_some() && nav.is_root());
    }

    #[test]
    fn back_reports_what_to_close_and_reset_closes_everything() {
        let mut nav = Navigator::default();
        assert_eq!(nav.back(), None);
        nav.push_loading("x:main", "X");
        assert_eq!(nav.back(), Some(Closed::Command("x:main".into())));

        nav.push_loading("x:main", "X");
        nav.apply(NavEvent::Push { view_id: 1, view: list("One", 1) });
        nav.apply(NavEvent::Push { view_id: 2, view: list("Two", 1) });
        nav.top_mut().query = "q".into();
        let closed = nav.reset();
        assert_eq!(closed, [Closed::View(2), Closed::View(1)]);
        assert!(nav.is_root() && nav.top().query.is_empty());
    }

    #[test]
    fn finish_and_fail_loading() {
        let mut nav = Navigator::default();
        nav.push_loading("x:a", "A");
        assert!(!nav.finish_loading("x:b"));
        assert!(nav.finish_loading("x:a"));
        assert!(nav.is_root());
        nav.push_loading("x:a", "A");
        nav.fail_loading();
        assert!(nav.is_root());
    }

    #[test]
    fn updates_patch_any_screen_in_the_stack_not_only_the_top() {
        let mut nav = Navigator::default();
        nav.push_loading("x:main", "X");
        nav.apply(NavEvent::Push { view_id: 1, view: list("One", 2) });
        nav.apply(NavEvent::Push { view_id: 2, view: list("Two", 1) });
        let patch = diff_views(&list("One", 2), &list("One", 3)).unwrap();
        let out = nav.apply(NavEvent::Update { view_id: 1, patch });
        assert_eq!((out.patched, out.error), (Some(1), None));
        nav.apply(NavEvent::Pop { view_id: 2 });
        let ViewNode::List(l) = nav.top().screen.node().unwrap() else { panic!() };
        assert_eq!(l.item_count(), 3);
    }

    #[test]
    fn form_state_survives_patches_and_receives_extension_errors() {
        let form = |label: &str| {
            ViewNode::Form(FormView {
                fields: vec![Field::new("a", FieldKind::Text), { let mut f = Field::new("b", FieldKind::Description { text: label.into() }); f.title = Some(label.into()); f }],
                ..Default::default()
            })
        };
        let mut nav = Navigator::default();
        nav.push_loading("x:f", "F");
        nav.apply(NavEvent::Push { view_id: 1, view: form("one") });
        let view = match nav.top().screen.node() { Some(ViewNode::Form(f)) => f.clone(), _ => panic!() };
        nav.top_mut().form.as_mut().unwrap().set(&view, "a", FieldValue::Text("typed".into()));
        let patch = diff_views(&form("one"), &form("two")).unwrap();
        nav.apply(NavEvent::Update { view_id: 1, patch });
        assert_eq!(nav.top().form.as_ref().unwrap().text("a"), "typed");
        nav.apply(NavEvent::FormErrors { view_id: 1, errors: vec![("a".into(), "taken".into())] });
        assert_eq!(nav.top().form.as_ref().unwrap().error("a"), Some("taken"));
    }
}
