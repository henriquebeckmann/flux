//! Which items of a list/grid are shown for the current search, in which order, and how they
//! are laid out as display rows (section headers interleaved). Pure and cheap to recompute.

use flux_api::{GridItem, GridView, ListItem, ListView};

use crate::{gridnav::GridLayout, search::fuzzy_score};

/// A visible item: indices into the view's `sections[section].items[item]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VItem {
    pub section: usize,
    pub item: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VSection {
    pub section: usize,
    /// Range in [`Visible::items`].
    pub start: usize,
    pub len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Visible {
    pub items: Vec<VItem>,
    pub sections: Vec<VSection>,
}

impl Visible {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// What a row of the display list is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayRow {
    Header { title: Option<String>, subtitle: Option<String> },
    /// A list item: index into [`Visible::items`].
    Item(usize),
    /// A grid line: items `start..start+len` of [`Visible::items`].
    GridRow { start: usize, len: usize },
}

trait Searchable {
    fn texts(&self) -> (&str, Option<&str>, &[String]);
}

impl Searchable for ListItem {
    fn texts(&self) -> (&str, Option<&str>, &[String]) {
        (&self.title, self.subtitle.as_deref(), &self.keywords)
    }
}

impl Searchable for GridItem {
    fn texts(&self) -> (&str, Option<&str>, &[String]) {
        (self.title.as_deref().unwrap_or(""), self.subtitle.as_deref(), &self.keywords)
    }
}

fn score<T: Searchable>(q: &str, it: &T) -> Option<i32> {
    let (title, subtitle, keywords) = it.texts();
    let mut best = fuzzy_score(q, title);
    for other in subtitle.into_iter().chain(keywords.iter().map(String::as_str)) {
        best = best.max(fuzzy_score(q, other).map(|s| s - 5)); // title matches rank above the rest
    }
    best
}

fn build<T: Searchable>(sections: &[(usize, &[T])], q: &str, filtering: bool) -> Visible {
    let mut v = Visible::default();
    for (si, items) in sections {
        let mut scored: Vec<(i32, usize)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| if filtering && !q.is_empty() { score(q, it).map(|s| (s, i)) } else { Some((0, i)) })
            .collect();
        if filtering && !q.is_empty() {
            scored.sort_by(|a, b| b.0.cmp(&a.0)); // stable: ties keep the extension's order
        }
        if scored.is_empty() {
            continue;
        }
        v.sections.push(VSection { section: *si, start: v.items.len(), len: scored.len() });
        v.items.extend(scored.into_iter().map(|(_, i)| VItem { section: *si, item: i }));
    }
    v
}

pub fn visible_list(view: &ListView, query: &str) -> Visible {
    let secs: Vec<(usize, &[ListItem])> = view.sections.iter().enumerate().map(|(i, s)| (i, s.items.as_slice())).collect();
    build(&secs, query, view.filtering)
}

pub fn visible_grid(view: &GridView, query: &str) -> Visible {
    let secs: Vec<(usize, &[GridItem])> = view.sections.iter().enumerate().map(|(i, s)| (i, s.items.as_slice())).collect();
    build(&secs, query, view.filtering)
}

fn header_of(sec: &flux_api::Section<impl Sized>) -> Option<DisplayRow> {
    (sec.title.is_some() || sec.subtitle.is_some()).then(|| DisplayRow::Header { title: sec.title.clone(), subtitle: sec.subtitle.clone() })
}

pub fn display_list(view: &ListView, vis: &Visible) -> Vec<DisplayRow> {
    let mut rows = Vec::with_capacity(vis.len() + vis.sections.len());
    for s in &vis.sections {
        rows.extend(header_of(&view.sections[s.section]));
        rows.extend((s.start..s.start + s.len).map(DisplayRow::Item));
    }
    rows
}

/// Layout of a grid for `cols` columns, plus its display rows (headers and grid lines).
pub fn display_grid(view: &GridView, vis: &Visible, cols: usize) -> (GridLayout, Vec<DisplayRow>) {
    let sizes: Vec<usize> = vis.sections.iter().map(|s| s.len).collect();
    let layout = GridLayout::new(&sizes, cols);
    let mut rows = Vec::new();
    for r in &layout.rows {
        if r.first_in_section {
            rows.extend(header_of(&view.sections[vis.sections[r.section].section]));
        }
        rows.push(DisplayRow::GridRow { start: r.start, len: r.len });
    }
    (layout, rows)
}

#[cfg(test)]
mod tests {
    use flux_api::Section;

    use super::*;

    fn item(key: &str, title: &str, kw: &[&str]) -> ListItem {
        ListItem { key: key.into(), title: title.into(), keywords: kw.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    fn view(filtering: bool) -> ListView {
        ListView {
            filtering,
            sections: vec![
                Section { key: "a".into(), title: Some("Apps".into()), items: vec![item("1", "Firefox", &["browser"]), item("2", "Files", &[]), item("3", "Terminal", &[])], ..Default::default() },
                Section { key: "b".into(), title: None, items: vec![item("4", "Fish shell", &["terminal"])], ..Default::default() },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn no_query_shows_everything_in_order() {
        let v = visible_list(&view(true), "");
        assert_eq!(v.items.len(), 4);
        assert_eq!(v.sections, vec![VSection { section: 0, start: 0, len: 3 }, VSection { section: 1, start: 3, len: 1 }]);
    }

    #[test]
    fn fuzzy_filtering_drops_empty_sections_and_matches_keywords() {
        let vw = view(true);
        let v = visible_list(&vw, "brow");
        assert_eq!(v.items, vec![VItem { section: 0, item: 0 }], "matched through a keyword");
        assert_eq!(v.sections.len(), 1, "the other section is hidden");
        let none = visible_list(&vw, "zzzz");
        assert!(none.is_empty() && none.sections.is_empty());
        // title matches outrank keyword matches
        let t = visible_list(&vw, "term");
        assert_eq!(t.items[0], VItem { section: 0, item: 2 });
        assert_eq!(t.items[1], VItem { section: 1, item: 0 });
    }

    #[test]
    fn filtering_false_leaves_the_items_exactly_as_given() {
        let v = visible_list(&view(false), "zzzz");
        assert_eq!(v.items.len(), 4);
    }

    #[test]
    fn display_rows_interleave_headers_only_for_titled_sections() {
        let vw = view(true);
        let vis = visible_list(&vw, "");
        let rows = display_list(&vw, &vis);
        assert_eq!(rows[0], DisplayRow::Header { title: Some("Apps".into()), subtitle: None });
        assert_eq!(rows.len(), 1 + 3 + 1, "the untitled section has no header");
        assert_eq!(rows[4], DisplayRow::Item(3));
    }

    #[test]
    fn grid_rows_follow_columns() {
        let g = GridView {
            sections: vec![Section {
                key: "s".into(),
                title: Some("T".into()),
                items: (0..5).map(|i| GridItem { key: i.to_string(), title: Some(format!("n{i}")), ..Default::default() }).collect(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let vis = visible_grid(&g, "");
        let (layout, rows) = display_grid(&g, &vis, 2);
        assert_eq!(layout.rows.len(), 3);
        assert_eq!(rows[0], DisplayRow::Header { title: Some("T".into()), subtitle: None });
        assert_eq!(rows[3], DisplayRow::GridRow { start: 4, len: 1 });
        let q = visible_grid(&g, "n3");
        assert_eq!(q.items, vec![VItem { section: 0, item: 3 }]);
    }
}
