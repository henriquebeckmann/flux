//! Component construction (List/Grid/Detail/Form), reactive `ui.view` screens, events.

mod common;

use std::time::Duration;

use common::*;
use flux_api::*;

/// One extension `c` with the given command sources (`name` -> lua) and permissions.
fn setup(perms: &[&str], cmds: &[(&str, &str)]) -> (tempfile::TempDir, Harness) {
    let d = tempfile::tempdir().unwrap();
    let commands: Vec<(&str, &str)> = cmds.iter().map(|(n, _)| (*n, "view")).collect();
    let files: Vec<(String, &str)> = cmds.iter().map(|(n, src)| (format!("commands/{n}.lua"), *src)).collect();
    let files: Vec<(&str, &str)> = files.iter().map(|(p, s)| (p.as_str(), *s)).collect();
    ext(d.path(), "c", perms, &commands, &files);
    let h = start(d.path());
    let _ = h.next(); // CommandsRegistered
    (d, h)
}

fn push_of(h: &Harness, cmd: &str) -> (u64, ViewNode) {
    h.run(&format!("c:{cmd}"));
    h.next_push()
}

fn error_of(h: &Harness, cmd: &str) -> String {
    h.run(&format!("c:{cmd}"));
    h.next_error()
}

fn next_update(h: &Harness) -> (u64, ViewPatch) {
    match h.next() {
        RuntimeEvent::Nav(NavEvent::Update { view_id, patch }) => (view_id, patch),
        e => panic!("expected Update, got {e:?}"),
    }
}

fn updates(events: Vec<RuntimeEvent>) -> Vec<ViewPatch> {
    events
        .into_iter()
        .filter_map(|e| match e {
            RuntimeEvent::Nav(NavEvent::Update { patch, .. }) => Some(patch),
            _ => None,
        })
        .collect()
}

// ------------------------------------------------------------------- List

#[test]
fn list_builds_sections_items_accessories_icons_and_empty_view() {
    let (_d, h) = setup(&[], &[(
        "list",
        r#"
local ui = flux.ui
function main()
  return ui.List {
    title = "T", filtering = false, is_loading = true, search_bar_placeholder = "find",
    ui.List.Item { key = "loose", title = "Loose" },
    ui.List.Section { key = "a", title = "A", subtitle = "sub",
      ui.List.Item { key = "1", title = "One", subtitle = "s", icon = "folder", keywords = { "uno" },
        accessories = { { text = "x" }, { tag = { text = "new", color = "green" } }, { progress = 0.5, tooltip = "half" }, "plain" },
        actions = ui.ActionPanel { ui.Action { title = "Open", on_action = function() end } } },
      ui.List.Item { title = "Auto key" },
    },
    ui.List.EmptyView { title = "Nothing", description = "d", icon = ui.Icon { name = "search", tint = "red" } },
  }
end"#,
    )]);
    let (_, v) = push_of(&h, "list");
    let l = list(v);
    assert_eq!((l.title.as_deref(), l.filtering, l.is_loading), (Some("T"), false, true));
    assert_eq!(l.search_bar_placeholder.as_deref(), Some("find"));
    assert_eq!(l.sections.len(), 2);
    assert_eq!(l.sections[0].items[0].key, "loose");
    let a = &l.sections[1];
    assert_eq!((a.key.as_str(), a.title.as_deref(), a.subtitle.as_deref()), ("a", Some("A"), Some("sub")));
    let one = &a.items[0];
    assert_eq!(one.icon, Some(Icon { kind: IconKind::Named("folder".into()), tint: None }));
    assert_eq!(one.keywords, vec!["uno"]);
    assert_eq!(one.accessories.len(), 4);
    assert_eq!(one.accessories[1].tag, Some(Tag { text: "new".into(), color: Some("green".into()) }));
    assert_eq!(one.accessories[2].progress, Some(0.5));
    assert_eq!(one.accessories[3].text.as_deref(), Some("plain"));
    assert_eq!(one.actions[0].actions[0].title, "Open");
    assert!(a.items[1].key.starts_with('#'), "positional key for items without one");
    assert_eq!(l.empty.title.as_deref(), Some("Nothing"));
    assert_eq!(l.empty.icon.as_ref().unwrap().tint.as_deref(), Some("red"));
    assert_eq!(l.item_count(), 3);
}

#[test]
fn legacy_flux_ui_list_still_works() {
    let (_d, h) = setup(&[], &[("old", r#"function main() return flux.ui.list { title = "Old", empty_title = "none", items = { { title = "a", subtitle = "b", icon = "?" } } } end"#)]);
    let (_, v) = push_of(&h, "old");
    let l = list(v);
    assert_eq!(l.item_count(), 1);
    assert_eq!(l.empty.title.as_deref(), Some("none"));
    assert_eq!(l.item(0).unwrap().icon, Some(Icon { kind: IconKind::Text("?".into()), tint: None }));
}

#[test]
fn component_errors_name_the_offending_path() {
    let (_d, h) = setup(&["clipboard"], &[
        ("typo", "function main() return flux.ui.List { titel = 'x' } end"),
        ("progress", "function main() return flux.ui.List { flux.ui.List.Item { title = 'a', accessories = { { progress = 2 } } } } end"),
        ("dup", "function main() return flux.ui.List { flux.ui.List.Item { key = 'k', title = 'a' }, flux.ui.List.Item { key = 'k', title = 'b' } } end"),
        ("notitle", "function main() return flux.ui.List { flux.ui.List.Item { key = 'k' } } end"),
        ("badicon", "function main() return flux.ui.List { flux.ui.List.Item { title = 'a', icon = flux.ui.Icon { name = 'nope' } } } end"),
        ("nonet", "function main() return flux.ui.List { flux.ui.List.Item { title = 'a', icon = flux.ui.Icon { image = flux.ui.Image { url = 'https://x.test/a.png' } } } } end"),
        ("escape", "function main() return flux.ui.List { flux.ui.List.Item { title = 'a', icon = flux.ui.Icon { image = flux.ui.Image { path = '../secret.png' } } } } end"),
        ("notaview", "function main() return flux.ui.List.Item { title = 'a' } end"),
        ("badcolor", "function main() return flux.ui.List { flux.ui.List.Item { title = 'a', accessories = { { tag = { text = 't', color = 'chartreuse' } } } } } end"),
    ]);
    let e = error_of(&h, "typo");
    assert!(e.contains("List: unknown prop 'titel'") && e.contains("did you mean 'title'"), "{e}");
    let e = error_of(&h, "progress");
    assert!(e.contains("accessories[1].progress") && e.contains("between 0 and 1"), "{e}");
    assert!(error_of(&h, "dup").contains("duplicate key 'k'"));
    assert!(error_of(&h, "notitle").contains("title is required"));
    assert!(error_of(&h, "badicon").contains("unknown icon 'nope'"));
    assert!(error_of(&h, "nonet").contains("'network' permission"));
    assert!(error_of(&h, "escape").contains("inside the extension directory"));
    assert!(error_of(&h, "notaview").contains("cannot be used as a screen"));
    assert!(error_of(&h, "badcolor").contains("not a color"));
}

// ------------------------------------------------------------------- Grid

#[test]
fn grid_builds_sizes_images_icons_and_text_cells() {
    let (_d, h) = setup(&[], &[
        (
            "grid",
            r#"
local ui = flux.ui
function main()
  return ui.Grid { title = "G", size = "large", columns = 4, fit = "fill",
    ui.Grid.Section { key = "s", title = "Pics",
      ui.Grid.Item { key = "a", title = "A", content = ui.Image { path = "assets/a.png", mask = "circle" } },
      ui.Grid.Item { key = "b", content = ui.Icon { name = "folder" } },
      ui.Grid.Item { key = "c", content = "folder" },
      ui.Grid.Item { key = "d", content = "Hello" },
      ui.Grid.Item { key = "e", content = "icons/x.png" } },
    ui.Grid.EmptyView { title = "none" } }
end"#,
        ),
        ("custom", "function main() return flux.ui.Grid { size = { width = 120, height = 80 }, flux.ui.Grid.Item { content = 'T' } } end"),
        ("badsize", "function main() return flux.ui.Grid { size = 'huge' } end"),
        ("nocontent", "function main() return flux.ui.Grid { flux.ui.Grid.Item { title = 'x' } } end"),
    ]);
    let (_, v) = push_of(&h, "grid");
    let ViewNode::Grid(g) = v else { panic!() };
    assert_eq!((g.size, g.columns, g.fit), (GridSize::Large, Some(4), GridFit::Fill));
    let items: Vec<_> = g.items().collect();
    assert!(matches!(&items[0].content, GridContent::Image(Image { source: ImageSource::Path(p), mask: ImageMask::Circle, .. }) if p.ends_with("c/assets/a.png")));
    assert!(matches!(&items[1].content, GridContent::Icon(Icon { kind: IconKind::Named(n), .. }) if n == "folder"));
    assert!(matches!(&items[2].content, GridContent::Icon(_)));
    assert_eq!(items[3].content, GridContent::Text("Hello".into()));
    assert!(matches!(&items[4].content, GridContent::Image(_)));
    assert_eq!(g.empty.title.as_deref(), Some("none"));
    let (_, v) = push_of(&h, "custom");
    let ViewNode::Grid(g) = v else { panic!() };
    assert_eq!(g.size, GridSize::Custom { width: 120.0, height: 80.0 });
    assert!(error_of(&h, "badsize").contains("unknown size 'huge'"));
    assert!(error_of(&h, "nocontent").contains("content"));
}

// ----------------------------------------------------------------- Detail

#[test]
fn detail_builds_markdown_metadata_and_blocks() {
    let (_d, h) = setup(&[], &[
        (
            "detail",
            r##"
local ui = flux.ui
function main()
  return ui.Detail { title = "D", markdown = "# Hi",
    ui.Detail.Markdown { "more" },
    ui.ProgressBar { value = 0.25, label = "Copying" },
    ui.LoadingIndicator { label = "wait" },
    metadata = ui.Detail.Metadata {
      ui.Detail.Metadata.Label { title = "Stars", text = "12", icon = "star" },
      ui.Detail.Metadata.Separator {},
      ui.Detail.Metadata.TagList { title = "Langs", ui.Detail.Metadata.TagList.Item { text = "Lua", color = "blue" }, ui.Detail.Metadata.TagList.Item { text = "Rust" } },
      ui.Detail.Metadata.Link { title = "Site", text = "example", url = "https://example.com" },
    } }
end"##,
        ),
        ("badlink", "function main() return flux.ui.Detail { metadata = flux.ui.Detail.Metadata { flux.ui.Detail.Metadata.Link { title = 'x', url = 'javascript:alert(1)' } } } end"),
        ("badprogress", "function main() return flux.ui.Detail { flux.ui.ProgressBar { value = -1 } } end"),
    ]);
    let (_, v) = push_of(&h, "detail");
    let ViewNode::Detail(d) = v else { panic!() };
    assert_eq!(d.blocks.len(), 4);
    assert_eq!(d.blocks[0], DetailBlock::Markdown("# Hi".into()));
    assert_eq!(d.blocks[1], DetailBlock::Markdown("more".into()));
    assert_eq!(d.blocks[2], DetailBlock::ProgressBar { value: Some(0.25), label: Some("Copying".into()) });
    assert_eq!(d.metadata.len(), 4);
    assert!(matches!(&d.metadata[2], MetaItem::TagList { tags, .. } if tags.len() == 2 && tags[0].color.as_deref() == Some("blue")));
    assert!(matches!(&d.metadata[3], MetaItem::Link { url, .. } if url == "https://example.com"));
    assert!(error_of(&h, "badlink").contains("only http://"));
    assert!(error_of(&h, "badprogress").contains("between 0 and 1"));
}

// ------------------------------------------------------------------- Form

const FORM: &str = r#"
local ui = flux.ui
function main()
  return ui.Form { title = "F", submit_title = "Save",
    on_submit = function(values) flux.storage.set("submitted", values) end,
    validate = function(values) if values.name == "root" then return { name = "reserved" } end end,
    ui.Form.Description { text = "Fill in" },
    ui.Form.TextField { id = "name", title = "Name", required = true, min_length = 2, default_value = "ann",
      on_change = function(v) flux.storage.set("changed", v) end,
      validate = function(v) if v == "bad" then return "nope" end end },
    ui.Form.PasswordField { id = "pw" },
    ui.Form.TextArea { id = "bio", rows = 6 },
    ui.Form.Separator {},
    ui.Form.Checkbox { id = "tos", label = "I agree", default_value = true },
    ui.Form.Dropdown { id = "color", default_value = "r",
      ui.Form.Dropdown.Item { value = "r", title = "Red" },
      ui.Form.Dropdown.Section { title = "Cool", ui.Form.Dropdown.Item { value = "b", title = "Blue" } } },
    ui.Form.TagPicker { id = "tags", default_value = { "x" }, ui.Form.TagPicker.Item { value = "x", title = "X" }, ui.Form.TagPicker.Item { value = "y", title = "Y" } },
    ui.Form.DatePicker { id = "when", type = "datetime", min = "2026-01-01T00:00" },
    ui.Form.FilePicker { id = "file", multiple = true, directories = true },
  }
end"#;

#[test]
fn form_builds_every_field_kind_with_rules_and_defaults() {
    let (_d, h) = setup(&[], &[
        ("form", FORM),
        ("dupid", "function main() return flux.ui.Form { flux.ui.Form.TextField { id = 'a' }, flux.ui.Form.TextField { id = 'a' } } end"),
        ("noid", "function main() return flux.ui.Form { flux.ui.Form.TextField { title = 'a' } } end"),
        ("baddate", "function main() return flux.ui.Form { flux.ui.Form.DatePicker { id = 'd', min = '2026-02-30' } } end"),
        ("nooptions", "function main() return flux.ui.Form { flux.ui.Form.Dropdown { id = 'd' } } end"),
    ]);
    let (_, v) = push_of(&h, "form");
    let ViewNode::Form(f) = v else { panic!() };
    assert_eq!(f.submit_title.as_deref(), Some("Save"));
    let kinds: Vec<&str> = f
        .fields
        .iter()
        .map(|x| match x.kind {
            FieldKind::Description { .. } => "desc",
            FieldKind::Text => "text",
            FieldKind::Password => "pw",
            FieldKind::TextArea { .. } => "area",
            FieldKind::Separator => "sep",
            FieldKind::Checkbox { .. } => "check",
            FieldKind::Dropdown { .. } => "drop",
            FieldKind::TagPicker { .. } => "tags",
            FieldKind::DatePicker { .. } => "date",
            FieldKind::FilePicker { .. } => "file",
        })
        .collect();
    assert_eq!(kinds, ["desc", "text", "pw", "area", "sep", "check", "drop", "tags", "date", "file"]);
    let name = &f.fields[1];
    assert!(name.required && name.change_events);
    assert_eq!(name.rules.min_length, Some(2));
    assert_eq!(name.default, Some(FieldValue::Text("ann".into())));
    assert!(matches!(&f.fields[3].kind, FieldKind::TextArea { rows: 6 }));
    assert_eq!(f.fields[5].default, Some(FieldValue::Bool(true)));
    let FieldKind::Dropdown { options } = &f.fields[6].kind else { panic!() };
    assert_eq!((options.len(), options[1].section.as_deref()), (2, Some("Cool")));
    assert_eq!(f.fields[7].default, Some(FieldValue::List(vec!["x".into()])));
    assert!(matches!(&f.fields[8].kind, FieldKind::DatePicker { include_time: true, min: Some(_), .. }));
    assert!(matches!(&f.fields[9].kind, FieldKind::FilePicker { multiple: true, directories: true, files: true }));
    assert!(error_of(&h, "dupid").contains("duplicate field id 'a'"));
    assert!(error_of(&h, "noid").contains("'id' is required"));
    assert!(error_of(&h, "baddate").contains("not a valid"));
    assert!(error_of(&h, "nooptions").contains("at least one option"));
}

#[test]
fn form_events_validation_and_submit() {
    let (_d, h) = setup(&[], &[("form", FORM)]);
    let (view_id, _) = push_of(&h, "form");

    h.handle.send(RuntimeRequest::FormChanged { view_id, field: "name".into(), value: FieldValue::Text("bob".into()) });
    assert_eq!(h.wait_local("c", "changed"), "\"bob\"");

    let values = |name: &str| {
        vec![
            ("name".to_string(), FieldValue::Text(name.into())),
            ("tos".to_string(), FieldValue::Bool(true)),
            ("tags".to_string(), FieldValue::List(vec!["x".into(), "y".into()])),
        ]
    };
    // per-field validator
    h.handle.send(RuntimeRequest::FormSubmit { view_id, values: values("bad") });
    let RuntimeEvent::Nav(NavEvent::FormErrors { errors, .. }) = h.next() else { panic!("expected FormErrors") };
    assert_eq!(errors, vec![("name".to_string(), "nope".to_string())]);
    // form-level validator
    h.handle.send(RuntimeRequest::FormSubmit { view_id, values: values("root") });
    let RuntimeEvent::Nav(NavEvent::FormErrors { errors, .. }) = h.next() else { panic!() };
    assert_eq!(errors, vec![("name".to_string(), "reserved".to_string())]);
    assert!(h.storage.local_get("c", "submitted").is_none(), "invalid submissions never reach on_submit");
    // valid
    h.handle.send(RuntimeRequest::FormSubmit { view_id, values: values("ann") });
    let json = h.wait_local("c", "submitted");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["name"], "ann");
    assert_eq!(v["tos"], true);
    assert_eq!(v["tags"], serde_json::json!(["x", "y"]));
}

// ---------------------------------------------------------------- ui.view

const COUNTER: &str = r#"
local ui = flux.ui
function main()
  return ui.view {
    state = { n = 0, items = { "a", "b" } },
    render = function(self)
      local items = {}
      for _, k in ipairs(self.state.items) do
        items[#items + 1] = ui.List.Item { key = k, title = k == "a" and ("a" .. self.state.n) or k,
          actions = { ui.Action { title = "Inc", on_action = function()
            self:set { n = self.state.n + 1 }
            self:set { n = self.state.n + 1 }
            self:set { n = self.state.n + 1 }
          end } } }
      end
      return ui.List { items = items }
    end,
  }
end"#;

fn first_action(v: &ViewNode) -> u64 {
    list(v.clone()).item(0).unwrap().actions[0].actions[0].id
}

#[test]
fn state_changes_are_coalesced_into_one_minimal_patch_with_stable_callback_ids() {
    let (_d, h) = setup(&[], &[("counter", COUNTER)]);
    let (view_id, view) = push_of(&h, "counter");
    assert_eq!(list(view.clone()).item(0).unwrap().title, "a0");
    let action = first_action(&view);

    h.handle.send(RuntimeRequest::InvokeAction { view_id, action_id: action });
    let (id, patch) = next_update(&h);
    assert_eq!(id, view_id);
    // three set() calls -> one render; only item "a" changed -> one Update op
    assert_eq!(patch.op_count(), 1, "{patch:?}");
    let ViewPatch::List { sections, .. } = &patch else { panic!("{patch:?}") };
    let SectionOp::Items { ops, .. } = &sections[0] else { panic!() };
    let ItemOp::Update { key, item } = &ops[0] else { panic!() };
    assert_eq!((key.as_str(), item.title.as_str()), ("a", "a3"));
    assert_eq!(item.actions[0].actions[0].id, action, "callback ids are stable across renders");
    assert!(updates(h.drain_for(Duration::from_millis(150))).is_empty(), "exactly one update");

    // the same (old) id keeps working after the re-render
    h.handle.send(RuntimeRequest::InvokeAction { view_id, action_id: action });
    let (_, patch) = next_update(&h);
    assert!(format!("{patch:?}").contains("a6"));
}

#[test]
fn unchanged_state_produces_no_update() {
    let (_d, h) = setup(&[], &[(
        "noop",
        r#"function main() return flux.ui.view { state = { x = 1 }, render = function(self)
             return flux.ui.List { flux.ui.List.Item { key = "k", title = "t", actions = { flux.ui.Action { title = "same", on_action = function() self:set { x = 1 } end } } } } end } end"#,
    )]);
    let (view_id, view) = push_of(&h, "noop");
    h.handle.send(RuntimeRequest::InvokeAction { view_id, action_id: first_action(&view) });
    assert!(updates(h.drain_for(Duration::from_millis(250))).is_empty());
}

#[test]
fn render_errors_keep_the_last_screen_and_recover() {
    let (_d, h) = setup(&[], &[(
        "bad",
        r#"function main() return flux.ui.view { state = { boom = false },
             render = function(self)
               if self.state.boom then error("exploded") end
               return flux.ui.List { flux.ui.List.Item { key = "k", title = "ok",
                 actions = { flux.ui.Action { title = "boom", on_action = function() self:set { boom = true } end },
                             flux.ui.Action { title = "fix", on_action = function() self:set { boom = false } end } } } }
             end } end"#,
    )]);
    let (view_id, view) = push_of(&h, "bad");
    let l = list(view);
    let (boom, fix) = (l.item(0).unwrap().actions[0].actions[0].id, l.item(0).unwrap().actions[0].actions[1].id);
    h.handle.send(RuntimeRequest::InvokeAction { view_id, action_id: boom });
    let msg = h.next_error();
    assert!(msg.contains("render:") && msg.contains("exploded"), "{msg}");
    h.handle.send(RuntimeRequest::InvokeAction { view_id, action_id: fix });
    assert!(updates(h.drain_for(Duration::from_millis(300))).is_empty(), "back to the last good tree: nothing to patch");
}

#[test]
fn state_is_read_only_and_set_is_forbidden_inside_render() {
    let (_d, h) = setup(&[], &[
        ("ro", "function main() return flux.ui.view { state = { a = 1 }, render = function(self) self.state.a = 2 return flux.ui.List {} end } end"),
        ("inrender", "function main() return flux.ui.view { render = function(self) self:set { a = 1 } return flux.ui.List {} end } end"),
        ("norender", "function main() return flux.ui.view { state = {} } end"),
    ]);
    assert!(error_of(&h, "ro").contains("read-only"));
    assert!(error_of(&h, "inrender").contains("while rendering"));
    assert!(error_of(&h, "norender").contains("render"));
}

#[test]
fn search_and_selection_events_reach_the_view_and_remote_search_is_latest_wins() {
    let (_d, h) = setup(&["process"], &[(
        "search",
        r#"
local ui = flux.ui
function main()
  return ui.view {
    state = { result = "none" },
    render = function(self)
      return ui.List { filtering = false,
        on_search_change = function(text)
          self:task("search", function()
            flux.process.exec("sleep", { "0.2" })
            flux.storage.set("last", text)
            self:set { result = text }
          end)
        end,
        on_selection_change = function(key) flux.storage.set("sel", key or "nil") end,
        ui.List.Item { key = "r", title = self.state.result } }
    end,
  }
end"#,
    )]);
    let (view_id, view) = push_of(&h, "search");
    let ViewNode::List(l) = &view else { panic!() };
    assert!(l.events.search_change && l.events.selection_change && !l.filtering);

    h.handle.send(RuntimeRequest::SelectionChanged { view_id, key: Some("r".into()) });
    assert_eq!(h.wait_local("c", "sel"), "\"r\"");

    h.handle.send(RuntimeRequest::SearchChanged { view_id, text: "first".into() });
    h.handle.send(RuntimeRequest::SearchChanged { view_id, text: "second".into() });
    let (_, patch) = next_update(&h);
    assert!(format!("{patch:?}").contains("second"), "{patch:?}");
    assert!(updates(h.drain_for(Duration::from_millis(400))).is_empty(), "the superseded search never reports");
    assert_eq!(h.storage.local_get("c", "last").unwrap(), "\"second\"");
}

#[test]
fn closing_a_view_cancels_its_tasks_and_blocks_late_updates() {
    let (_d, h) = setup(&["process"], &[(
        "ticker",
        r#"
function main()
  return flux.ui.view {
    state = { n = 0 },
    init = function(self)
      self:task("tick", function()
        while true do
          flux.process.exec("sleep", { "0.05" })
          self:set { n = self.state.n + 1 }
        end
      end)
    end,
    render = function(self) return flux.ui.List { flux.ui.List.Item { key = "n", title = tostring(self.state.n) } } end,
  }
end"#,
    )]);
    let (view_id, _) = push_of(&h, "ticker");
    let _ = next_update(&h);
    let _ = next_update(&h);
    h.handle.send(RuntimeRequest::ViewClosed { view_id });
    std::thread::sleep(Duration::from_millis(100));
    let _ = h.drain_for(Duration::from_millis(50));
    assert!(updates(h.drain_for(Duration::from_millis(400))).is_empty(), "no ticks after the view closed");
}

#[test]
fn key_bindings_become_callbacks_and_lifecycle_hooks_run() {
    let (_d, h) = setup(&[], &[(
        "keys",
        r#"function main() return flux.ui.List { on_key = { ["ctrl+d"] = function() flux.storage.set("key", "d") end },
             flux.ui.List.Item { key = "a", title = "a" } } end"#,
    )]);
    let (view_id, view) = push_of(&h, "keys");
    let ViewNode::List(l) = view else { panic!() };
    assert_eq!(l.events.key_bindings.len(), 1);
    assert_eq!(l.events.key_bindings[0].shortcut, Shortcut::parse("ctrl+d").unwrap());
    h.handle.send(RuntimeRequest::InvokeAction { view_id, action_id: l.events.key_bindings[0].action });
    assert_eq!(h.wait_local("c", "key"), "\"d\"");
}

#[test]
fn on_appear_and_on_disappear_follow_the_screen_lifetime() {
    let (_d, h) = setup(&[], &[(
        "life",
        r#"function main() return flux.ui.view { render = function(self) return flux.ui.List { on_appear = function() flux.storage.set("appeared", true) end, on_disappear = function() flux.storage.set("gone", true) end } end } end"#,
    )]);
    let (view_id, _) = push_of(&h, "life");
    h.wait_local("c", "appeared");
    h.handle.send(RuntimeRequest::ViewClosed { view_id });
    h.wait_local("c", "gone");
}

#[test]
fn image_urls_obey_network_permission_and_allowed_hosts() {
    let d = tempfile::tempdir().unwrap();
    write(
        &d.path().join("img"),
        "manifest.lua",
        "return { name='img', permissions={'network'}, allowed_hosts={'cdn.example.com'}, commands={{name='ok'},{name='other'},{name='private'}} }",
    );
    let item = |url: &str| format!("function main() return flux.ui.Grid {{ flux.ui.Grid.Item {{ key='a', content = flux.ui.Image {{ url = '{url}' }} }} }} end");
    write(&d.path().join("img"), "commands/ok.lua", &item("https://cdn.example.com/a.png"));
    write(&d.path().join("img"), "commands/other.lua", &item("https://evil.test/a.png"));
    write(&d.path().join("img"), "commands/private.lua", &item("http://127.0.0.1/a.png"));
    let h = start(d.path());
    let _ = h.next();
    h.run("img:ok");
    let (_, v) = h.next_push();
    let ViewNode::Grid(g) = v else { panic!() };
    assert!(matches!(&g.item(0).unwrap().content, GridContent::Image(Image { source: ImageSource::Url(u), .. }) if u.contains("cdn.example.com")));
    h.run("img:other");
    assert!(h.next_error().contains("allowed_hosts"));
    h.run("img:private");
    assert!(h.next_error().contains("allowed_hosts") || h.next_error().contains("private"));
}

#[test]
fn set_search_replaces_the_search_text_of_the_screen() {
    let (_d, h) = setup(&[], &[(
        "s",
        r#"function main() return flux.ui.view { render = function(self) return flux.ui.List { flux.ui.List.Item { key = "a", title = "a",
             actions = { flux.ui.Action { title = "clear", on_action = function() self:set_search("") end } } } } end } end"#,
    )]);
    let (id, v) = push_of(&h, "s");
    h.handle.send(RuntimeRequest::InvokeAction { view_id: id, action_id: first_action(&v) });
    let e = h.wait_event(|e| matches!(e, RuntimeEvent::Nav(NavEvent::SetSearch { .. })).then(|| e.clone()));
    assert!(matches!(e, RuntimeEvent::Nav(NavEvent::SetSearch { view_id, text }) if view_id == id && text.is_empty()));
}
