//! Action Panel logic (Ctrl+K): which actions are shown, sections, filtering and selection.
//! The UI only draws it; everything testable lives here.

use flux_api::{ActionSection, ActionSpec, ActionStyle, Confirm, Shortcut, resolve_actions};

use crate::search::fuzzy_score;

/// What happens when an entry is chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelTarget {
    /// A callback inside an extension view.
    Runtime { view_id: u64, action_id: u64 },
    /// A built-in launcher action.
    RunCommand(String),
    Reload,
    Quit,
    /// Submit the form on screen.
    SubmitForm,
    ManageExtensions,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PanelEntry {
    pub section: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub shortcut: Option<Shortcut>,
    pub style: ActionStyle,
    pub confirm: Option<Confirm>,
    pub target: PanelTarget,
}

/// A line of the panel: section headers are interleaved with the (filtered) actions.
#[derive(Debug, Clone, PartialEq)]
pub enum PanelRow<'a> {
    Header(&'a str),
    Action { index: usize, entry: &'a PanelEntry },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActionPanelModel {
    entries: Vec<PanelEntry>,
    filter: String,
    selected: usize,
}

impl ActionPanelModel {
    pub fn new(entries: Vec<PanelEntry>) -> Self {
        Self { entries, filter: String::new(), selected: 0 }
    }

    /// Builds entries from a view's sections (item actions first, then global ones).
    pub fn from_sections(view_id: u64, sections: &[ActionSection]) -> Self {
        Self::from_sections_with(view_id, sections, true)
    }

    /// `implicit_shortcuts = false` shows only shortcuts the actions declared (forms use Enter themselves).
    pub fn from_sections_with(view_id: u64, sections: &[ActionSection], implicit_shortcuts: bool) -> Self {
        let entries = resolve_actions(sections)
            .into_iter()
            .map(|r| {
                let ActionSpec { id, title, description, style, confirm, .. } = r.spec.clone();
                PanelEntry {
                    section: sections[r.section].title.clone(),
                    title,
                    description,
                    shortcut: if implicit_shortcuts { r.shortcut } else { r.spec.shortcut.clone() },
                    style,
                    confirm,
                    target: PanelTarget::Runtime { view_id, action_id: id },
                }
            })
            .collect();
        Self::new(entries)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn prepend(&mut self, entry: PanelEntry) {
        self.entries.insert(0, entry);
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn set_filter(&mut self, filter: &str) {
        self.filter = filter.to_string();
        self.selected = 0;
    }

    /// Entries matching the filter, in their original order.
    pub fn visible(&self) -> Vec<&PanelEntry> {
        self.entries.iter().filter(|e| fuzzy_score(&self.filter, &e.title).is_some()).collect()
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn current(&self) -> Option<&PanelEntry> {
        self.visible().get(self.selected).copied()
    }

    pub fn select_next(&mut self) {
        let n = self.visible().len();
        if n > 0 {
            self.selected = (self.selected + 1) % n;
        }
    }

    pub fn select_prev(&mut self) {
        let n = self.visible().len();
        if n > 0 {
            self.selected = (self.selected + n - 1) % n;
        }
    }

    pub fn rows(&self) -> Vec<PanelRow<'_>> {
        let mut rows = Vec::new();
        let mut last: Option<&Option<String>> = None;
        for (index, entry) in self.visible().into_iter().enumerate() {
            if last != Some(&entry.section) {
                if let Some(title) = &entry.section {
                    rows.push(PanelRow::Header(title));
                }
                last = Some(&entry.section);
            }
            rows.push(PanelRow::Action { index, entry });
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(id: u64, title: &str) -> ActionSpec {
        ActionSpec { id, title: title.into(), ..Default::default() }
    }

    fn model() -> ActionPanelModel {
        ActionPanelModel::from_sections(
            7,
            &[
                ActionSection { title: None, actions: vec![act(1, "Open"), act(2, "Copy link")] },
                ActionSection { title: Some("Danger".into()), actions: vec![act(3, "Delete")] },
                ActionSection { title: Some("Global".into()), actions: vec![act(4, "Refresh")] },
            ],
        )
    }

    #[test]
    fn builds_entries_with_effective_shortcuts_and_targets() {
        let m = model();
        let v = m.visible();
        assert_eq!(v.len(), 4);
        assert_eq!(v[0].shortcut.as_ref().unwrap().to_string(), "enter");
        assert_eq!(v[1].shortcut.as_ref().unwrap().to_string(), "ctrl+enter");
        assert_eq!(v[2].shortcut, None);
        assert_eq!(v[2].target, PanelTarget::Runtime { view_id: 7, action_id: 3 });
        assert_eq!(v[2].section.as_deref(), Some("Danger"));
    }

    #[test]
    fn rows_interleave_headers_and_selection_wraps() {
        let mut m = model();
        let rows = m.rows();
        let headers: Vec<_> = rows.iter().filter_map(|r| if let PanelRow::Header(h) = r { Some(*h) } else { None }).collect();
        assert_eq!(headers, ["Danger", "Global"]);
        assert!(matches!(rows[0], PanelRow::Action { index: 0, .. }), "untitled first section has no header");

        m.select_prev();
        assert_eq!(m.current().unwrap().title, "Refresh");
        m.select_next();
        assert_eq!(m.current().unwrap().title, "Open");
    }

    #[test]
    fn filtering_narrows_and_resets_selection() {
        let mut m = model();
        m.select_next();
        m.set_filter("del");
        assert_eq!(m.visible().len(), 1);
        assert_eq!(m.selected_index(), 0);
        assert_eq!(m.current().unwrap().title, "Delete");
        m.set_filter("zzz");
        assert!(m.current().is_none());
        assert!(m.rows().is_empty());
        m.select_next(); // no panic on empty
    }
}
