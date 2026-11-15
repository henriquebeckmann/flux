//! Keyed tree diffing. After a re-render the runtime compares the new [`ViewNode`] with the
//! previous one and sends only a [`ViewPatch`]; the core applies it to its copy. Items,
//! sections and form fields are matched by `key`, so reordering/inserting keeps the identity
//! (and with it the selection, scroll position and typed text) of everything that stays.
//!
//! Invariant (property-tested): `apply(old, diff(old, new)) == new`.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{Field, FormView, GridItem, GridView, Keyed, ListItem, ListView, Section, ViewNode};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ItemOp<T> {
    Insert { index: usize, item: T },
    Remove { key: String },
    Move { key: String, to: usize },
    Update { key: String, item: T },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SectionOp<T> {
    Insert { index: usize, section: Section<T> },
    Remove { key: String },
    Move { key: String, to: usize },
    Header { key: String, title: Option<String>, subtitle: Option<String> },
    Items { key: String, ops: Vec<ItemOp<T>> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ViewPatch {
    /// The whole tree is replaced (different kind, or no cheaper description).
    Replace(ViewNode),
    /// `props` is the full view with its collection emptied: every non-collection field is overwritten.
    List { props: Option<Box<ListView>>, sections: Vec<SectionOp<ListItem>> },
    Grid { props: Option<Box<GridView>>, sections: Vec<SectionOp<GridItem>> },
    Form { props: Option<Box<FormView>>, fields: Vec<ItemOp<Field>> },
}

impl ViewPatch {
    /// Rough size of the patch, for logging and tests.
    pub fn op_count(&self) -> usize {
        match self {
            ViewPatch::Replace(_) => 1,
            ViewPatch::List { props, sections } => props.is_some() as usize + section_ops(sections),
            ViewPatch::Grid { props, sections } => props.is_some() as usize + section_ops(sections),
            ViewPatch::Form { props, fields } => props.is_some() as usize + fields.len(),
        }
    }
}

fn section_ops<T>(ops: &[SectionOp<T>]) -> usize {
    ops.iter().map(|o| if let SectionOp::Items { ops, .. } = o { ops.len() } else { 1 }).sum()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchError(pub String);

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for PatchError {}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

enum Step {
    Remove(String),
    Move(String, usize),
    /// Insert `new[index]` at `index`.
    Insert(usize),
}

/// Minimal-ish edit script turning the `old` key order into the `new` one. `None` if a key repeats.
fn plan(old: &[&str], new: &[&str]) -> Option<Vec<Step>> {
    let unique = |v: &[&str]| v.iter().collect::<HashSet<_>>().len() == v.len();
    if !unique(old) || !unique(new) {
        return None;
    }
    let keep: HashSet<&str> = new.iter().copied().collect();
    let mut steps = Vec::new();
    let mut cur: Vec<&str> = Vec::with_capacity(old.len());
    for k in old {
        if keep.contains(k) {
            cur.push(k);
        } else {
            steps.push(Step::Remove((*k).to_string()));
        }
    }
    for (i, k) in new.iter().enumerate() {
        if cur.get(i) == Some(k) {
            continue;
        }
        match cur.iter().position(|c| c == k) {
            Some(pos) => {
                cur.remove(pos);
                cur.insert(i, k);
                steps.push(Step::Move((*k).to_string(), i));
            }
            None => {
                cur.insert(i, k);
                steps.push(Step::Insert(i));
            }
        }
    }
    Some(steps)
}

fn diff_items<T: Keyed + Clone + PartialEq>(old: &[T], new: &[T]) -> Option<Vec<ItemOp<T>>> {
    let ok: Vec<&str> = old.iter().map(Keyed::key).collect();
    let nk: Vec<&str> = new.iter().map(Keyed::key).collect();
    let steps = plan(&ok, &nk)?;
    let mut ops: Vec<ItemOp<T>> = steps
        .into_iter()
        .map(|s| match s {
            Step::Remove(key) => ItemOp::Remove { key },
            Step::Move(key, to) => ItemOp::Move { key, to },
            Step::Insert(i) => ItemOp::Insert { index: i, item: new[i].clone() },
        })
        .collect();
    let old_by_key: HashMap<&str, &T> = old.iter().map(|t| (t.key(), t)).collect();
    for item in new {
        if let Some(prev) = old_by_key.get(item.key()) {
            if *prev != item {
                ops.push(ItemOp::Update { key: item.key().to_string(), item: item.clone() });
            }
        }
    }
    Some(ops)
}

fn diff_sections<T: Keyed + Clone + PartialEq>(old: &[Section<T>], new: &[Section<T>]) -> Option<Vec<SectionOp<T>>> {
    let ok: Vec<&str> = old.iter().map(|s| s.key.as_str()).collect();
    let nk: Vec<&str> = new.iter().map(|s| s.key.as_str()).collect();
    let steps = plan(&ok, &nk)?;
    let mut ops: Vec<SectionOp<T>> = steps
        .into_iter()
        .map(|s| match s {
            Step::Remove(key) => SectionOp::Remove { key },
            Step::Move(key, to) => SectionOp::Move { key, to },
            Step::Insert(i) => SectionOp::Insert { index: i, section: new[i].clone() },
        })
        .collect();
    let old_by_key: HashMap<&str, &Section<T>> = old.iter().map(|s| (s.key.as_str(), s)).collect();
    for s in new {
        let Some(prev) = old_by_key.get(s.key.as_str()) else { continue };
        if prev.title != s.title || prev.subtitle != s.subtitle {
            ops.push(SectionOp::Header { key: s.key.clone(), title: s.title.clone(), subtitle: s.subtitle.clone() });
        }
        if prev.items != s.items {
            ops.push(SectionOp::Items { key: s.key.clone(), ops: diff_items(&prev.items, &s.items)? });
        }
    }
    Some(ops)
}

fn list_props(v: &ListView) -> ListView {
    ListView {
        title: v.title.clone(),
        search_bar_placeholder: v.search_bar_placeholder.clone(),
        filtering: v.filtering,
        is_loading: v.is_loading,
        empty: v.empty.clone(),
        sections: vec![],
        actions: v.actions.clone(),
        selected_key: v.selected_key.clone(),
        events: v.events.clone(),
    }
}

fn grid_props(v: &GridView) -> GridView {
    GridView {
        title: v.title.clone(),
        search_bar_placeholder: v.search_bar_placeholder.clone(),
        filtering: v.filtering,
        is_loading: v.is_loading,
        size: v.size,
        columns: v.columns,
        fit: v.fit,
        empty: v.empty.clone(),
        sections: vec![],
        actions: v.actions.clone(),
        selected_key: v.selected_key.clone(),
        events: v.events.clone(),
    }
}

fn form_props(v: &FormView) -> FormView {
    FormView {
        title: v.title.clone(),
        submit_title: v.submit_title.clone(),
        is_loading: v.is_loading,
        fields: vec![],
        actions: v.actions.clone(),
        events: v.events.clone(),
    }
}

/// `None` = the trees are identical.
pub fn diff(old: &ViewNode, new: &ViewNode) -> Option<ViewPatch> {
    if old == new {
        return None;
    }
    let replace = || Some(ViewPatch::Replace(new.clone()));
    match (old, new) {
        (ViewNode::List(a), ViewNode::List(b)) => {
            let Some(sections) = diff_sections(&a.sections, &b.sections) else { return replace() };
            let (pa, pb) = (list_props(a), list_props(b));
            Some(ViewPatch::List { props: (pa != pb).then(|| Box::new(pb)), sections })
        }
        (ViewNode::Grid(a), ViewNode::Grid(b)) => {
            let Some(sections) = diff_sections(&a.sections, &b.sections) else { return replace() };
            let (pa, pb) = (grid_props(a), grid_props(b));
            Some(ViewPatch::Grid { props: (pa != pb).then(|| Box::new(pb)), sections })
        }
        (ViewNode::Form(a), ViewNode::Form(b)) => {
            let Some(fields) = diff_items(&a.fields, &b.fields) else { return replace() };
            let (pa, pb) = (form_props(a), form_props(b));
            Some(ViewPatch::Form { props: (pa != pb).then(|| Box::new(pb)), fields })
        }
        _ => replace(),
    }
}

// ---------------------------------------------------------------------------
// Apply
// ---------------------------------------------------------------------------

fn pos_of<T: Keyed>(v: &[T], key: &str) -> Result<usize, PatchError> {
    v.iter().position(|t| t.key() == key).ok_or_else(|| PatchError(format!("unknown key '{key}'")))
}

fn apply_items<T: Keyed + Clone>(v: &mut Vec<T>, ops: &[ItemOp<T>]) -> Result<(), PatchError> {
    for op in ops {
        match op {
            ItemOp::Insert { index, item } => v.insert((*index).min(v.len()), item.clone()),
            ItemOp::Remove { key } => {
                let p = pos_of(v, key)?;
                v.remove(p);
            }
            ItemOp::Move { key, to } => {
                let p = pos_of(v, key)?;
                let item = v.remove(p);
                v.insert((*to).min(v.len()), item);
            }
            ItemOp::Update { key, item } => {
                let p = pos_of(v, key)?;
                v[p] = item.clone();
            }
        }
    }
    Ok(())
}

fn apply_sections<T: Keyed + Clone>(v: &mut Vec<Section<T>>, ops: &[SectionOp<T>]) -> Result<(), PatchError> {
    for op in ops {
        match op {
            SectionOp::Insert { index, section } => v.insert((*index).min(v.len()), section.clone()),
            SectionOp::Remove { key } => {
                let p = pos_of(v, key)?;
                v.remove(p);
            }
            SectionOp::Move { key, to } => {
                let p = pos_of(v, key)?;
                let s = v.remove(p);
                v.insert((*to).min(v.len()), s);
            }
            SectionOp::Header { key, title, subtitle } => {
                let p = pos_of(v, key)?;
                v[p].title = title.clone();
                v[p].subtitle = subtitle.clone();
            }
            SectionOp::Items { key, ops } => {
                let p = pos_of(v, key)?;
                apply_items(&mut v[p].items, ops)?;
            }
        }
    }
    Ok(())
}

/// Applies `patch` atomically: on error `view` is left untouched.
pub fn apply(view: &mut ViewNode, patch: &ViewPatch) -> Result<(), PatchError> {
    match (&mut *view, patch) {
        (_, ViewPatch::Replace(n)) => *view = n.clone(),
        (ViewNode::List(v), ViewPatch::List { props, sections }) => {
            let mut sections_new = v.sections.clone();
            apply_sections(&mut sections_new, sections)?;
            if let Some(p) = props {
                *v = ListView { sections: vec![], ..(**p).clone() };
            }
            v.sections = sections_new;
        }
        (ViewNode::Grid(v), ViewPatch::Grid { props, sections }) => {
            let mut sections_new = v.sections.clone();
            apply_sections(&mut sections_new, sections)?;
            if let Some(p) = props {
                *v = GridView { sections: vec![], ..(**p).clone() };
            }
            v.sections = sections_new;
        }
        (ViewNode::Form(v), ViewPatch::Form { props, fields }) => {
            let mut fields_new = v.fields.clone();
            apply_items(&mut fields_new, fields)?;
            if let Some(p) = props {
                *v = FormView { fields: vec![], ..(**p).clone() };
            }
            v.fields = fields_new;
        }
        _ => return Err(PatchError(format!("patch does not match a {} view", view.kind()))),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionSection, FieldKind};

    fn item(key: &str, title: &str) -> ListItem {
        ListItem { key: key.into(), title: title.into(), ..Default::default() }
    }

    fn list(sections: Vec<(&str, Vec<ListItem>)>) -> ViewNode {
        ViewNode::List(ListView {
            sections: sections.into_iter().map(|(k, items)| Section { key: k.into(), title: Some(k.into()), subtitle: None, items }).collect(),
            ..Default::default()
        })
    }

    fn roundtrip(a: &ViewNode, b: &ViewNode) -> Option<ViewPatch> {
        let patch = diff(a, b);
        let mut x = a.clone();
        if let Some(p) = &patch {
            apply(&mut x, p).unwrap();
        }
        assert_eq!(&x, b);
        patch
    }

    #[test]
    fn identical_trees_produce_no_patch() {
        let a = list(vec![("s", vec![item("1", "a"), item("2", "b")])]);
        assert!(diff(&a, &a.clone()).is_none());
    }

    #[test]
    fn changing_one_title_is_one_update() {
        let a = list(vec![("s", vec![item("1", "a"), item("2", "b"), item("3", "c")])]);
        let b = list(vec![("s", vec![item("1", "a"), item("2", "B"), item("3", "c")])]);
        let p = roundtrip(&a, &b).unwrap();
        assert_eq!(p.op_count(), 1);
    }

    #[test]
    fn insert_remove_and_move_keep_identity() {
        let a = list(vec![("s", vec![item("1", "a"), item("2", "b"), item("3", "c")])]);
        let b = list(vec![("s", vec![item("3", "c"), item("0", "z"), item("1", "a")])]);
        roundtrip(&a, &b).unwrap();
    }

    #[test]
    fn sections_can_be_added_removed_and_reordered() {
        let a = list(vec![("x", vec![item("1", "a")]), ("y", vec![item("2", "b")])]);
        let b = list(vec![("y", vec![item("2", "b"), item("4", "d")]), ("z", vec![item("5", "e")])]);
        roundtrip(&a, &b).unwrap();
    }

    #[test]
    fn prop_changes_are_sent_without_touching_items() {
        let a = list(vec![("s", vec![item("1", "a")])]);
        let mut b = a.clone();
        if let ViewNode::List(l) = &mut b {
            l.is_loading = true;
        }
        let p = roundtrip(&a, &b).unwrap();
        assert!(matches!(&p, ViewPatch::List { props: Some(_), sections } if sections.is_empty()));
    }

    #[test]
    fn duplicate_keys_fall_back_to_replace() {
        let a = list(vec![("s", vec![item("1", "a")])]);
        let b = list(vec![("s", vec![item("1", "a"), item("1", "dup")])]);
        assert!(matches!(roundtrip(&a, &b), Some(ViewPatch::Replace(_))));
    }

    #[test]
    fn kind_change_replaces() {
        let a = list(vec![]);
        let b = ViewNode::Detail(Default::default());
        assert!(matches!(roundtrip(&a, &b), Some(ViewPatch::Replace(_))));
    }

    #[test]
    fn form_fields_are_matched_by_id() {
        let f = |ids: &[&str]| {
            ViewNode::Form(FormView {
                fields: ids.iter().map(|i| Field::new(i, FieldKind::Text)).collect(),
                ..Default::default()
            })
        };
        roundtrip(&f(&["a", "b", "c"]), &f(&["c", "a"])).unwrap();
        let mut b = f(&["a", "b"]);
        if let ViewNode::Form(v) = &mut b {
            v.fields[1].error = Some("bad".into());
            v.actions = vec![ActionSection::default()];
        }
        roundtrip(&f(&["a", "b"]), &b).unwrap();
    }

    #[test]
    fn failed_apply_leaves_the_view_untouched() {
        let mut v = list(vec![("s", vec![item("1", "a")])]);
        let before = v.clone();
        let bad = ViewPatch::List {
            props: None,
            sections: vec![SectionOp::Items { key: "s".into(), ops: vec![ItemOp::Insert { index: 0, item: item("9", "x") }, ItemOp::Remove { key: "nope".into() }] }],
        };
        assert!(apply(&mut v, &bad).is_err());
        assert_eq!(v, before);
    }

    /// Random keyed lists (tiny deterministic PRNG): `apply(diff(a, b)) == b` always holds.
    #[test]
    fn property_apply_diff_equals_new() {
        let mut seed = 0x2545F4914F6CDD1Du64;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut gen_view = |rnd: &mut dyn FnMut(u64) -> u64| {
            let mut sections = Vec::new();
            let mut skeys: Vec<u64> = (0..4).collect();
            for i in (1..skeys.len()).rev() {
                skeys.swap(i, rnd(i as u64 + 1) as usize);
            }
            for sk in skeys.into_iter().take(rnd(4) as usize + 1) {
                let mut ikeys: Vec<u64> = (0..8).collect();
                for i in (1..ikeys.len()).rev() {
                    ikeys.swap(i, rnd(i as u64 + 1) as usize);
                }
                let items: Vec<ListItem> = ikeys.into_iter().take(rnd(9) as usize).map(|k| item(&format!("i{}-{}", sk, k), &format!("t{}", rnd(3)))).collect();
                sections.push((format!("s{sk}"), items));
            }
            list(sections.iter().map(|(k, v)| (k.as_str(), v.clone())).collect())
        };
        for _ in 0..500 {
            let (a, b) = (gen_view(&mut rnd), gen_view(&mut rnd));
            roundtrip(&a, &b);
        }
    }

    #[test]
    fn five_thousand_items_with_one_change_is_a_single_op() {
        let items = |t: &str| (0..5000).map(|i| item(&i.to_string(), if i == 2500 { t } else { "x" })).collect::<Vec<_>>();
        let (a, b) = (list(vec![("s", items("a"))]), list(vec![("s", items("b"))]));
        let start = std::time::Instant::now();
        let p = roundtrip(&a, &b).unwrap();
        assert_eq!(p.op_count(), 1);
        assert!(start.elapsed().as_millis() < 500, "diff too slow: {:?}", start.elapsed());
    }
}
