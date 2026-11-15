use super::*;
use tokio::sync::mpsc::UnboundedReceiver;

struct T {
    core: Core,
    rx: UnboundedReceiver<RuntimeRequest>,
}

impl T {
    fn new(reset_secs: u64) -> T {
        let (rt, rx) = RuntimeHandle::detached();
        let mut core = Core::new(Arc::new(Db::open_in_memory().unwrap()), rt, Duration::from_secs(reset_secs));
        core.on_event(RuntimeEvent::CommandsRegistered(vec![
            cmd("demo:hello", "Hello", CommandMode::View),
            cmd("demo:sync", "Sync", CommandMode::NoView),
            cmd("demo:__preferences", "Configure Demo", CommandMode::View),
        ]));
        T { core, rx }
    }

    fn sent(&mut self) -> Vec<RuntimeRequest> {
        let mut v = vec![];
        while let Ok(r) = self.rx.try_recv() {
            v.push(r);
        }
        v
    }

    fn select(&mut self, title: &str) {
        let i = (0..self.core.item_count()).position(|i| self.core.item(i).unwrap().title == title).unwrap_or_else(|| panic!("no row {title}"));
        self.core.set_selected(i);
    }

    fn titles(&self) -> Vec<String> {
        (0..self.core.item_count()).map(|i| self.core.item(i).unwrap().title).collect()
    }

    fn push(&mut self, view_id: u64, view: ViewNode) {
        self.core.on_event(RuntimeEvent::Nav(NavEvent::Push { view_id, view }));
    }

    fn update(&mut self, view_id: u64, from: &ViewNode, to: &ViewNode) {
        let patch = diff_views(from, to).expect("views differ");
        self.core.on_event(RuntimeEvent::Nav(NavEvent::Update { view_id, patch }));
    }
}

fn cmd(id: &str, title: &str, mode: CommandMode) -> CommandInfo {
    CommandInfo { id: id.into(), title: title.into(), subtitle: None, icon: None, mode, extension: Some("demo".into()) }
}

fn act(id: u64, title: &str, shortcut: Option<&str>) -> ActionSpec {
    ActionSpec { id, title: title.into(), shortcut: shortcut.map(|s| Shortcut::parse(s).unwrap()), ..Default::default() }
}

fn item(key: &str, title: &str, actions: Vec<ActionSection>) -> ListItem {
    ListItem { key: key.into(), title: title.into(), actions, ..Default::default() }
}

fn section(key: &str, title: Option<&str>, items: Vec<ListItem>) -> Section<ListItem> {
    Section { key: key.into(), title: title.map(String::from), items, ..Default::default() }
}

fn list_view() -> ViewNode {
    ViewNode::List(ListView {
        title: Some("Things".into()),
        sections: vec![section(
            "s",
            None,
            vec![
                item("a", "Alpha", vec![ActionSection { title: None, actions: vec![act(1, "Open", None), act(2, "Copy", None), act(3, "Custom", Some("ctrl+d"))] }]),
                item("b", "Beta", vec![]),
            ],
        )],
        actions: vec![ActionSection { title: Some("Global".into()), actions: vec![act(9, "Refresh", None)] }],
        ..Default::default()
    })
}

fn open_list(t: &mut T) {
    t.select("Hello");
    t.core.activate();
    t.push(1, list_view());
    t.sent();
}

#[test]
fn running_a_command_shows_loading_then_the_view_and_records_history() {
    let mut t = T::new(60);
    t.core.set_query("hel");
    assert_eq!(t.core.item(0).unwrap().title, "Hello");
    t.core.activate();
    assert!(matches!(t.core.screen(), Screen::Loading { .. }));
    assert!(t.core.is_loading());
    assert_eq!(t.sent().len(), 1);
    t.push(1, list_view());
    assert!(matches!(t.core.screen(), Screen::View { title, .. } if title == "Things"));
    assert!(!t.core.is_loading());
    assert_eq!(t.core.navigator().depth(), 2);
    assert_eq!(t.core.db.recent_searches(5), ["hel"]);
}

#[test]
fn primary_secondary_and_custom_shortcuts_invoke_the_right_actions() {
    let mut t = T::new(60);
    open_list(&mut t);
    assert_eq!(t.core.primary_action_title().as_deref(), Some("Open"));
    t.core.activate();
    let enter = |k: &str, ctrl: bool| KeyInput { key: k.into(), ctrl, ..Default::default() };
    assert!(t.core.trigger_shortcut(&enter("enter", true)));
    assert!(t.core.trigger_shortcut(&enter("d", true)));
    assert!(!t.core.trigger_shortcut(&enter("x", true)));
    let ids: Vec<_> = t.sent().into_iter().map(|r| match r { RuntimeRequest::InvokeAction { view_id, action_id } => (view_id, action_id), o => panic!("{o:?}") }).collect();
    assert_eq!(ids, [(1, 1), (1, 2), (1, 3)]);
}

#[test]
fn items_without_own_actions_fall_back_to_global_actions() {
    let mut t = T::new(60);
    open_list(&mut t);
    t.core.select_next(); // Beta has no actions of its own
    t.core.activate(); // enter -> first (global) action
    assert!(matches!(t.sent()[0], RuntimeRequest::InvokeAction { view_id: 1, action_id: 9 }));
}

#[test]
fn action_panel_for_list_items_lists_item_then_global_actions_and_runs_the_selection() {
    let mut t = T::new(60);
    open_list(&mut t);
    assert!(t.core.toggle_panel());
    let p = t.core.panel().unwrap();
    let titles: Vec<_> = p.visible().iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, ["Open", "Copy", "Custom", "Refresh"]);
    t.core.panel_mut().unwrap().select_prev(); // wraps to the global action
    t.core.run_panel_selected();
    assert!(t.core.panel().is_none());
    assert!(matches!(t.sent()[0], RuntimeRequest::InvokeAction { view_id: 1, action_id: 9 }));
    assert!(t.core.toggle_panel());
    assert!(!t.core.toggle_panel(), "Ctrl+K again closes the panel");
}

#[test]
fn action_panel_on_root_offers_run_configure_manage_reload_quit() {
    let mut t = T::new(60);
    t.select("Hello");
    assert!(t.core.toggle_panel());
    let titles: Vec<_> = t.core.panel().unwrap().visible().iter().map(|e| e.title.clone()).collect();
    assert_eq!(titles, ["Run", "Configure Extension", "Manage Extensions", "Reload Extensions", "Quit Flux"]);
    t.core.panel_mut().unwrap().select_next(); // Configure Extension
    t.core.run_panel_selected();
    assert!(matches!(&t.sent()[0], RuntimeRequest::RunCommand { command_id } if command_id == "demo:__preferences"));
    // builtin commands have no panel entries for configuration
    t.core.back();
    t.select("Quit Flux");
    t.core.toggle_panel();
    assert!(!t.core.panel().unwrap().visible().iter().any(|e| e.title == "Configure Extension"));
}

#[test]
fn escape_order_is_panel_then_screen_then_search_then_hide() {
    let mut t = T::new(60);
    open_list(&mut t);
    t.core.toggle_panel();
    assert!(t.core.back().is_empty() && t.core.panel().is_none() && !t.core.navigator().is_root());
    assert!(t.core.back().is_empty());
    assert!(t.core.navigator().is_root());
    assert!(matches!(t.sent()[0], RuntimeRequest::ViewClosed { view_id: 1 }));
    t.core.set_query("zz");
    assert!(t.core.back().is_empty());
    assert_eq!(t.core.query(), "");
    assert_eq!(t.core.back(), [Effect::Hide]);
}

#[test]
fn leaving_a_loading_screen_cancels_the_command() {
    let mut t = T::new(60);
    t.select("Hello");
    t.core.activate();
    t.sent();
    t.core.back();
    assert!(matches!(&t.sent()[0], RuntimeRequest::CancelCommand { command_id } if command_id == "demo:hello"));
    assert!(t.core.navigator().is_root());
}

#[test]
fn errors_during_a_transition_return_to_root_with_a_message() {
    let mut t = T::new(60);
    t.select("Hello");
    t.core.activate();
    t.core.on_event(RuntimeEvent::ExtensionError { extension: "demo:hello".into(), message: "boom".into() });
    assert!(t.core.navigator().is_root());
    assert_eq!(t.core.error(), Some("demo:hello: boom"));
    assert_eq!(t.core.logs().last_error("demo").unwrap().message, "boom", "errors are kept per extension");
    t.core.on_event(RuntimeEvent::CommandFinished { command_id: "demo:hello".into() }); // no hide after a failure
    open_list(&mut t);
    t.core.on_event(RuntimeEvent::ExtensionError { extension: "demo".into(), message: "x".into() });
    assert_eq!(t.core.navigator().depth(), 2);
}

#[test]
fn no_view_commands_hide_the_launcher_when_they_finish() {
    let mut t = T::new(60);
    t.select("Sync");
    t.core.activate();
    assert!(matches!(t.core.screen(), Screen::Loading { .. }));
    assert_eq!(t.core.on_event(RuntimeEvent::CommandFinished { command_id: "demo:sync".into() }), [Effect::Hide]);
    assert!(t.core.navigator().is_root());
    assert_eq!(t.core.on_event(RuntimeEvent::CloseLauncher), [Effect::Hide]);
}

#[test]
fn builtin_commands() {
    let mut t = T::new(60);
    t.select("Quit Flux");
    assert_eq!(t.core.activate(), [Effect::Quit]);
    t.select("Reload Extensions");
    assert_eq!(t.core.activate(), [Effect::Hide]);
    assert!(matches!(t.sent()[0], RuntimeRequest::Reload));
}

#[test]
fn input_screens_submit_their_text() {
    let mut t = T::new(60);
    t.select("Hello");
    t.core.activate();
    t.sent();
    t.push(4, ViewNode::Input(InputView { value: Some("init".into()), secure: true, submit_action: 1, ..Default::default() }));
    assert_eq!(t.core.query(), "init");
    assert!(t.core.is_secure_input());
    t.core.set_query("new value");
    t.core.activate();
    assert!(matches!(&t.sent()[0], RuntimeRequest::SubmitInput { view_id: 4, text } if text == "new value"));
}

#[test]
fn search_history_navigation() {
    let mut t = T::new(60);
    for q in ["one", "two", "three"] {
        t.core.db.record_search(q);
    }
    assert!(t.core.history_prev());
    assert_eq!(t.core.query(), "three");
    assert!(t.core.history_prev() && t.core.history_prev() && t.core.history_prev());
    assert_eq!(t.core.query(), "one", "stops at the oldest");
    assert!(t.core.history_next());
    assert_eq!(t.core.query(), "two");
    t.core.history_next();
    t.core.history_next();
    assert_eq!(t.core.query(), "");
    assert!(!t.core.history_next());
    t.core.set_query("typed");
    assert!(t.core.history_prev() && t.core.query() == "three", "typing restarts navigation");
}

#[test]
fn hidden_state_is_preserved_briefly_and_reset_after_the_timeout() {
    let mut t = T::new(60);
    open_list(&mut t);
    t.core.set_query("alp");
    let t0 = Instant::now();
    t.core.on_hide(t0);
    assert!(!t.core.on_show(t0 + Duration::from_secs(5)));
    assert_eq!((t.core.navigator().depth(), t.core.query()), (2, "alp"), "state preserved");

    t.core.on_hide(t0);
    assert!(t.core.on_show(t0 + Duration::from_secs(61)));
    assert!(t.core.navigator().is_root() && t.core.query().is_empty());
    assert!(matches!(t.sent()[0], RuntimeRequest::ViewClosed { view_id: 1 }), "runtime told to drop the views");
}

#[test]
fn epoch_changes_when_the_core_replaces_the_text() {
    let mut t = T::new(60);
    let e0 = t.core.epoch();
    t.core.set_query("hel"); // user typing must not make the UI reload its text
    assert_eq!(t.core.epoch(), e0);
    t.select("Hello");
    t.core.activate();
    assert_ne!(t.core.epoch(), e0);
}

// ----------------------------------------------------- sections and keys

fn sectioned() -> ViewNode {
    ViewNode::List(ListView {
        sections: vec![
            section("fruit", Some("Fruit"), vec![item("apple", "Apple", vec![]), item("banana", "Banana", vec![])]),
            section("veg", Some("Vegetables"), vec![item("carrot", "Carrot", vec![]), item("beet", "Beet", vec![])]),
        ],
        ..Default::default()
    })
}

#[test]
fn sections_render_as_headers_and_filter_across_them() {
    let mut t = T::new(60);
    open_list(&mut t);
    t.core.on_event(RuntimeEvent::Nav(NavEvent::Replace { old_view_id: 1, view_id: 2, view: sectioned() }));
    let m = t.core.model();
    assert_eq!(m.rows.len(), 6, "2 headers + 4 items");
    assert!(matches!(&m.rows[0], DisplayRow::Header { title: Some(t), .. } if t == "Fruit"));
    assert!(matches!(&m.rows[3], DisplayRow::Header { title: Some(t), .. } if t == "Vegetables"));
    t.core.set_query("be");
    assert_eq!(t.titles(), ["Beet"], "only items that match; the empty section disappears");
}

#[test]
fn a_patch_keeps_the_same_item_selected_even_when_it_moves() {
    let mut t = T::new(60);
    open_list(&mut t);
    let v1 = sectioned();
    t.core.on_event(RuntimeEvent::Nav(NavEvent::Replace { old_view_id: 1, view_id: 2, view: v1.clone() }));
    t.select("Carrot");
    assert_eq!(t.core.selected_key().as_deref(), Some("carrot"));
    // a new item is inserted before it and another removed
    let ViewNode::List(mut l) = v1.clone() else { panic!() };
    l.sections[0].items.insert(0, item("avocado", "Avocado", vec![]));
    l.sections[0].items.retain(|i| i.key != "banana");
    let v2 = ViewNode::List(l);
    t.update(2, &v1, &v2);
    assert_eq!(t.core.selected_key().as_deref(), Some("carrot"), "selection follows the item, not the index");
    assert_eq!(t.core.item(t.core.selected()).unwrap().title, "Carrot");
    // the selected item disappears: stay near the old position
    let ViewNode::List(mut l) = v2.clone() else { panic!() };
    l.sections[1].items.retain(|i| i.key != "carrot");
    let v3 = ViewNode::List(l);
    t.update(2, &v2, &v3);
    assert!(t.core.selected_key().is_some());
    assert!(t.core.selected() < t.core.item_count());
}

#[test]
fn filtering_false_shows_what_the_extension_sent() {
    let mut t = T::new(60);
    open_list(&mut t);
    let v = ViewNode::List(ListView { filtering: false, sections: vec![section("s", None, vec![item("1", "Remote result", vec![])])], ..Default::default() });
    t.core.on_event(RuntimeEvent::Nav(NavEvent::Replace { old_view_id: 1, view_id: 2, view: v }));
    t.core.set_query("something unrelated");
    assert_eq!(t.titles(), ["Remote result"]);
}

#[test]
fn search_and_selection_events_are_sent_only_to_views_that_listen() {
    let mut t = T::new(60);
    open_list(&mut t); // plain view: no events
    t.core.set_query("a");
    t.core.select_next();
    assert!(t.sent().is_empty());
    t.core.set_query("");

    let v = ViewNode::List(ListView {
        events: Events { search_change: true, selection_change: true, ..Default::default() },
        sections: vec![section("s", None, vec![item("x", "X", vec![]), item("y", "Y", vec![])])],
        ..Default::default()
    });
    t.core.on_event(RuntimeEvent::Nav(NavEvent::Replace { old_view_id: 1, view_id: 2, view: v }));
    let first = t.sent();
    assert!(matches!(&first[..], [RuntimeRequest::SelectionChanged { view_id: 2, key: Some(k) }] if k == "x"), "{first:?}");
    t.core.select_next();
    assert!(matches!(&t.sent()[..], [RuntimeRequest::SelectionChanged { key: Some(k), .. }] if k == "y"));
    t.core.set_query("z");
    let sent = t.sent();
    assert!(sent.iter().any(|r| matches!(r, RuntimeRequest::SearchChanged { view_id: 2, text } if text == "z")), "{sent:?}");
    assert!(sent.iter().any(|r| matches!(r, RuntimeRequest::SelectionChanged { key: None, .. })), "nothing matches: selection cleared");
    t.core.select_next();
    assert!(t.sent().is_empty(), "no spurious events on an empty list");
}

#[test]
fn view_key_bindings_trigger_before_item_actions() {
    let mut t = T::new(60);
    open_list(&mut t);
    let v = ViewNode::List(ListView {
        events: Events { key_bindings: vec![KeyBinding { shortcut: Shortcut::parse("ctrl+d").unwrap(), action: 77 }], ..Default::default() },
        sections: vec![section("s", None, vec![item("x", "X", vec![ActionSection { title: None, actions: vec![act(1, "Open", Some("ctrl+d"))] }])])],
        ..Default::default()
    });
    t.core.on_event(RuntimeEvent::Nav(NavEvent::Replace { old_view_id: 1, view_id: 2, view: v }));
    t.sent();
    assert!(t.core.trigger_shortcut(&KeyInput { key: "d".into(), ctrl: true, ..Default::default() }));
    assert!(matches!(t.sent()[0], RuntimeRequest::InvokeAction { view_id: 2, action_id: 77 }));
}

// ------------------------------------------------------------------ grid

fn grid(n: usize) -> ViewNode {
    ViewNode::Grid(GridView {
        columns: Some(3),
        sections: vec![Section {
            key: "g".into(),
            title: Some("Pics".into()),
            items: (0..n).map(|i| GridItem { key: i.to_string(), title: Some(format!("p{i}")), actions: vec![ActionSection { title: None, actions: vec![act(i as u64 + 1, "Open", None)] }], ..Default::default() }).collect(),
            ..Default::default()
        }],
        ..Default::default()
    })
}

#[test]
fn grid_arrows_move_in_two_dimensions_and_actions_follow_the_cell() {
    let mut t = T::new(60);
    open_list(&mut t);
    t.core.on_event(RuntimeEvent::Nav(NavEvent::Replace { old_view_id: 1, view_id: 2, view: grid(8) }));
    let m = t.core.model();
    assert_eq!(m.layout.as_ref().unwrap().cols, 3, "forced columns win over the measured width");
    assert!(matches!(m.rows[0], DisplayRow::Header { .. }) && matches!(m.rows[1], DisplayRow::GridRow { start: 0, len: 3 }));
    t.core.set_grid_columns(5);
    assert_eq!(t.core.model().layout.as_ref().unwrap().cols, 3);
    t.core.select_right();
    t.core.select_down();
    assert_eq!(t.core.selected(), 4);
    t.core.select_down();
    assert_eq!(t.core.selected(), 7, "short last row clamps to its last cell");
    t.core.select_up();
    t.core.select_left();
    assert_eq!(t.core.selected(), 3);
    assert_eq!(t.core.cell(3).unwrap().title.as_deref(), Some("p3"));
    t.sent();
    t.core.activate();
    assert!(matches!(t.sent()[0], RuntimeRequest::InvokeAction { view_id: 2, action_id: 4 }), "enter runs the primary action of the selected cell");
    t.core.select_first();
    t.core.select_last();
    assert_eq!(t.core.selected(), 7);
}

#[test]
fn grid_columns_follow_the_measured_width_when_not_forced() {
    let mut t = T::new(60);
    open_list(&mut t);
    let ViewNode::Grid(mut g) = grid(10) else { panic!() };
    g.columns = None;
    t.core.on_event(RuntimeEvent::Nav(NavEvent::Replace { old_view_id: 1, view_id: 2, view: ViewNode::Grid(g) }));
    t.core.set_grid_columns(4);
    assert_eq!(t.core.model().layout.as_ref().unwrap().cols, 4);
    t.core.set_selected(5);
    t.core.set_grid_columns(2);
    assert_eq!(t.core.selected_key().as_deref(), Some("5"), "the same cell stays selected after a relayout");
}

// ---------------------------------------------------------------- confirm

#[test]
fn destructive_actions_ask_before_running_from_shortcuts_and_the_panel() {
    let mut t = T::new(60);
    open_list(&mut t);
    let mut kill = act(5, "Kill", Some("ctrl+x"));
    kill.style = ActionStyle::Destructive;
    kill.confirm = Some(Confirm { title: "Kill it?".into(), message: Some("This cannot be undone".into()) });
    let v = ViewNode::List(ListView { sections: vec![section("s", None, vec![item("p", "Proc", vec![ActionSection { title: None, actions: vec![act(4, "Open", None), kill] }])])], ..Default::default() });
    t.core.on_event(RuntimeEvent::Nav(NavEvent::Replace { old_view_id: 1, view_id: 2, view: v }));
    t.sent();

    assert!(t.core.trigger_shortcut(&KeyInput { key: "x".into(), ctrl: true, ..Default::default() }));
    assert!(t.sent().is_empty(), "nothing runs before the answer");
    let c = t.core.confirmation().unwrap();
    assert_eq!((c.title.as_str(), c.destructive), ("Kill it?", true));
    t.core.back(); // Esc cancels, and does not navigate
    assert!(t.core.confirmation().is_none() && t.core.navigator().depth() == 2);
    assert!(t.sent().is_empty());

    t.core.toggle_panel();
    t.core.panel_mut().unwrap().select_next(); // Kill
    t.core.run_panel_selected();
    assert!(t.core.confirmation().is_some() && t.sent().is_empty());
    t.core.activate(); // Enter confirms
    assert!(matches!(t.sent()[0], RuntimeRequest::InvokeAction { view_id: 2, action_id: 5 }));
    assert!(t.core.confirmation().is_none());
}

// ------------------------------------------------------------------ forms

fn form_view() -> ViewNode {
    let mut name = Field::new("name", FieldKind::Text);
    name.required = true;
    name.change_events = true;
    ViewNode::Form(FormView {
        fields: vec![name, Field::new("sep", FieldKind::Separator), Field::new("bio", FieldKind::TextArea { rows: 3 })],
        actions: vec![ActionSection { title: None, actions: vec![act(8, "Reset", Some("ctrl+r"))] }],
        ..Default::default()
    })
}

#[test]
fn forms_validate_before_submitting_and_report_changes() {
    let mut t = T::new(60);
    open_list(&mut t);
    t.core.on_event(RuntimeEvent::Nav(NavEvent::Replace { old_view_id: 1, view_id: 2, view: form_view() }));
    assert!(!t.core.shows_search_field());
    assert_eq!(t.core.form().unwrap().1.focused_id(t.core.form().unwrap().0), Some("name"));

    t.core.activate(); // Enter on a form submits; "name" is required
    assert!(t.sent().is_empty());
    assert_eq!(t.core.form().unwrap().1.error("name"), Some("This field is required"));
    assert!(t.core.error().is_some());

    t.core.form_set("name", FieldValue::Text("Ann".into()));
    assert!(matches!(&t.sent()[..], [RuntimeRequest::FormChanged { view_id: 2, field, value: FieldValue::Text(v) }] if field == "name" && v == "Ann"));
    t.core.form_set("bio", FieldValue::Text("hi".into()));
    assert!(t.sent().is_empty(), "no event for fields without on_change");

    assert!(t.core.trigger_shortcut(&KeyInput { key: "enter".into(), ctrl: true, ..Default::default() }), "Ctrl+Enter submits");
    let sent = t.sent();
    let [RuntimeRequest::FormSubmit { view_id: 2, values }] = &sent[..] else { panic!("{sent:?}") };
    assert_eq!(values, &[("name".to_string(), FieldValue::Text("Ann".into())), ("bio".to_string(), FieldValue::Text("hi".into()))]);

    t.core.form_focus_next();
    assert_eq!(t.core.form().unwrap().1.focused_id(t.core.form().unwrap().0), Some("bio"), "separators are skipped");
}

#[test]
fn form_panel_offers_submit_and_view_actions_without_implicit_shortcuts() {
    let mut t = T::new(60);
    open_list(&mut t);
    t.core.on_event(RuntimeEvent::Nav(NavEvent::Replace { old_view_id: 1, view_id: 2, view: form_view() }));
    assert!(t.core.toggle_panel());
    let entries: Vec<_> = t.core.panel().unwrap().visible().iter().map(|e| (e.title.clone(), e.shortcut.as_ref().map(|s| s.to_string()))).collect();
    assert_eq!(entries, [("Submit Form".to_string(), Some("ctrl+enter".to_string())), ("Reset".to_string(), Some("ctrl+r".to_string()))]);
    t.core.run_panel_selected(); // Submit Form -> invalid
    assert!(t.sent().is_empty());
}

#[test]
fn extension_form_errors_and_patches_do_not_clobber_user_input() {
    let mut t = T::new(60);
    open_list(&mut t);
    let v1 = form_view();
    t.core.on_event(RuntimeEvent::Nav(NavEvent::Replace { old_view_id: 1, view_id: 2, view: v1.clone() }));
    t.core.form_set("name", FieldValue::Text("typed".into()));
    let ViewNode::Form(mut f) = v1.clone() else { panic!() };
    f.is_loading = true;
    f.fields[2].placeholder = Some("new placeholder".into());
    t.update(2, &v1, &ViewNode::Form(f));
    assert_eq!(t.core.form().unwrap().1.text("name"), "typed");
    assert!(t.core.is_loading());
    t.core.on_event(RuntimeEvent::Nav(NavEvent::FormErrors { view_id: 2, errors: vec![("name".into(), "taken".into())] }));
    assert_eq!(t.core.form().unwrap().1.error("name"), Some("taken"));
}

// ----------------------------------------------------------------- detail

#[test]
fn detail_pages_expose_parsed_markdown_and_actions() {
    let mut t = T::new(60);
    open_list(&mut t);
    let v = ViewNode::Detail(DetailView {
        blocks: vec![DetailBlock::Markdown("# Title\n\ntext".into()), DetailBlock::ProgressBar { value: Some(0.5), label: None }],
        metadata: vec![MetaItem::Separator],
        actions: vec![ActionSection { title: None, actions: vec![act(3, "Open in browser", None)] }],
        ..Default::default()
    });
    t.core.on_event(RuntimeEvent::Nav(NavEvent::Replace { old_view_id: 1, view_id: 2, view: v }));
    let d = t.core.detail().unwrap();
    assert!(matches!(&d.items[0], DetailItem::Markdown(b) if matches!(b[0], markdown::Block::Heading { level: 1, .. })));
    assert!(matches!(d.items[1], DetailItem::ProgressBar { value: Some(_), .. }));
    assert!(Rc::ptr_eq(&d, &t.core.detail().unwrap()), "parsed once, not on every frame");
    assert_eq!(t.core.primary_action_title().as_deref(), Some("Open in browser"));
    t.sent();
    t.core.activate();
    assert!(matches!(t.sent()[0], RuntimeRequest::InvokeAction { view_id: 2, action_id: 3 }));
    assert!(!t.core.shows_search_field());
}

#[test]
fn links_open_only_web_and_mail_urls() {
    let mut t = T::new(60);
    assert_eq!(t.core.open_url("https://example.com/a"), [Effect::OpenUrl("https://example.com/a".into())]);
    assert_eq!(t.core.open_url("mailto:a@b.c").len(), 1);
    for bad in ["file:///etc/passwd", "javascript:alert(1)", "/usr/bin/x", "ftp://x"] {
        assert!(t.core.open_url(bad).is_empty(), "{bad}");
        assert!(t.core.error().is_some());
    }
}

// ------------------------------------------------------------- management

fn info(name: &str, status: ExtensionStatus, enabled: bool, source: ExtensionSource) -> ExtensionInfo {
    ExtensionInfo {
        name: name.into(),
        title: name.to_uppercase(),
        version: "1.0.0".into(),
        description: None,
        author: None,
        license: None,
        dir: format!("/ext/{name}"),
        source,
        enabled,
        status,
        permissions: vec![],
        allowed_hosts: vec![],
        commands: vec![],
        isolation: String::new(),
        revoked_permissions: vec![],
    }
}

fn open_manage(t: &mut T, infos: Vec<ExtensionInfo>) {
    t.core.on_event(RuntimeEvent::ExtensionsChanged(infos));
    t.select("Manage Extensions");
    t.core.activate();
    t.sent();
}

fn run_action(t: &mut T, title: &str) -> Vec<Effect> {
    t.core.toggle_panel();
    let p = t.core.panel_mut().unwrap();
    let idx = p.visible().iter().position(|e| e.title == title).unwrap_or_else(|| panic!("no action {title}"));
    for _ in 0..idx {
        p.select_next();
    }
    let e = t.core.run_panel_selected();
    t.core.confirm_accept().into_iter().chain(e).collect()
}

#[test]
fn manage_extensions_lists_toggles_reloads_and_updates_in_place() {
    let mut t = T::new(60);
    open_manage(&mut t, vec![info("a", ExtensionStatus::Loaded, true, ExtensionSource::Dev), info("b", ExtensionStatus::Disabled, false, ExtensionSource::Dev)]);
    assert!(matches!(t.core.screen(), Screen::View { title, .. } if title == "Manage Extensions"));
    assert_eq!(t.titles(), ["A", "B"]);
    t.select("B");
    run_action(&mut t, "Enable");
    assert!(t.core.db.extension_enabled("b"), "persisted");
    assert!(matches!(&t.sent()[..], [RuntimeRequest::SetEnabled { name, enabled: true }] if name == "b"));

    run_action(&mut t, "Reload");
    assert!(matches!(&t.sent()[..], [RuntimeRequest::ReloadExtension { name }] if name == "b"));

    // the runtime reports the new state: the open list is patched, the selection stays on "B"
    t.core.on_event(RuntimeEvent::ExtensionsChanged(vec![
        info("a", ExtensionStatus::Loaded, true, ExtensionSource::Dev),
        info("b", ExtensionStatus::Loaded, true, ExtensionSource::Dev),
        info("c", ExtensionStatus::Invalid { errors: vec!["bad".into()] }, true, ExtensionSource::Dev),
    ]));
    assert_eq!(t.titles(), ["A", "B", "C"]);
    assert_eq!(t.core.selected_key().as_deref(), Some("b"));
    t.select("C");
    assert!(t.core.toggle_panel());
    assert!(t.core.panel().unwrap().visible().iter().any(|e| e.title == "Show Errors"));
    t.core.close_panel();
    t.sent();
    run_action(&mut t, "Disable"); // actions of the *new* tree are the ones wired
    assert!(matches!(&t.sent()[..], [RuntimeRequest::SetEnabled { name, enabled: false }] if name == "c"));
}

#[test]
fn manage_extensions_details_logs_errors_and_file_effects() {
    let mut t = T::new(60);
    t.core.on_event(RuntimeEvent::Log { extension: "a".into(), level: LogLevel::Info, message: "hello from a".into() });
    t.core.on_event(RuntimeEvent::ExtensionError { extension: "a:cmd".into(), message: "boom".into() });
    open_manage(&mut t, vec![info("a", ExtensionStatus::Crashed { message: "restarted".into() }, true, ExtensionSource::Dev)]);

    run_action(&mut t, "Show Logs");
    let Screen::View { node: ViewNode::Detail(d), title, .. } = t.core.screen() else { panic!() };
    assert_eq!(title, "a: logs");
    let DetailBlock::Markdown(md) = &d.blocks[0] else { panic!() };
    assert!(md.contains("hello from a") && md.contains("ERROR boom"), "{md}");
    t.core.back();

    run_action(&mut t, "Show Errors");
    let Screen::View { node: ViewNode::Detail(d), .. } = t.core.screen() else { panic!() };
    let DetailBlock::Markdown(md) = &d.blocks[0] else { panic!() };
    assert!(md.contains("restarted") && md.contains("boom"));
    t.core.back();

    run_action(&mut t, "Show Details");
    assert!(matches!(t.core.screen(), Screen::View { node: ViewNode::Detail(_), .. }));
    t.core.back();

    assert_eq!(run_action(&mut t, "Open Folder"), [Effect::OpenPath("/ext/a".into())]);
    assert_eq!(run_action(&mut t, "Copy Path"), [Effect::CopyText("/ext/a".into())]);
    assert!(t.sent().iter().all(|r| matches!(r, RuntimeRequest::ViewClosed { .. })), "native screens never reach the extension runtime");
}

#[test]
fn removing_an_installed_extension_asks_first_and_reloads() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = T::new(60);
    let installed = dir.path().join("installed");
    std::fs::create_dir_all(installed.join("gone/commands")).unwrap();
    std::fs::write(installed.join("gone/manifest.lua"), "return { name='gone', commands={{name='hi'}} }").unwrap();
    std::fs::write(installed.join("gone/commands/hi.lua"), "function main() end").unwrap();
    t.core.manager = Some(Manager::new(installed.clone(), t.core.db.clone()));
    open_manage(&mut t, vec![info("gone", ExtensionStatus::Loaded, true, ExtensionSource::Installed), info("dev", ExtensionStatus::Loaded, true, ExtensionSource::Dev)]);

    t.select("DEV");
    t.core.toggle_panel();
    assert!(!t.core.panel().unwrap().visible().iter().any(|e| e.title == "Remove Extension"), "development folders are never removed by the launcher");
    t.core.close_panel();

    t.select("GONE");
    t.core.toggle_panel();
    let p = t.core.panel_mut().unwrap();
    while p.current().unwrap().title != "Remove Extension" {
        p.select_next();
    }
    t.core.run_panel_selected();
    assert!(installed.join("gone").exists(), "nothing is deleted before the confirmation");
    assert_eq!(t.core.confirmation().unwrap().title, "Remove GONE?");
    t.core.confirm_accept();
    assert!(!installed.join("gone").exists());
    assert!(t.sent().iter().any(|r| matches!(r, RuntimeRequest::Reload)));
}

#[test]
fn the_extension_can_clear_the_search_text() {
    let mut t = T::new(60);
    open_list(&mut t);
    t.core.set_query("alp");
    assert_eq!(t.titles(), ["Alpha"]);
    let e0 = t.core.epoch();
    t.core.on_event(RuntimeEvent::Nav(NavEvent::SetSearch { view_id: 1, text: String::new() }));
    assert_eq!(t.core.query(), "");
    assert_eq!(t.titles(), ["Alpha", "Beta"], "the new folder shows everything");
    assert_ne!(t.core.epoch(), e0, "the UI reloads its text field");
}

#[test]
fn live_updates_do_not_close_the_action_panel_or_a_confirmation() {
    let mut t = T::new(60);
    open_list(&mut t);
    let v1 = list_view();
    let ViewNode::List(mut l) = v1.clone() else { panic!() };
    l.sections[0].items[1].title = "Beta 2".into();
    let v2 = ViewNode::List(l);
    assert!(t.core.toggle_panel());
    t.update(1, &v1, &v2);
    assert!(t.core.panel().is_some(), "a refresh must not dismiss the panel");
    t.core.run_panel_selected();
    assert!(matches!(t.sent().last(), Some(RuntimeRequest::InvokeAction { view_id: 1, action_id: 1 })), "the action still targets the item it was opened for");
    // real navigation does close it
    t.core.toggle_panel();
    t.core.on_event(RuntimeEvent::Nav(NavEvent::Push { view_id: 2, view: list_view() }));
    assert!(t.core.panel().is_none());
}

// ------------------------------------------------------------ extension lifecycle

#[test]
fn reloading_an_extension_closes_its_open_screens() {
    let mut t = T::new(60);
    t.push(7, sectioned());
    assert!(!t.core.navigator().is_root());
    t.core.on_event(RuntimeEvent::ExtensionReloaded { extension: "demo".into(), generation: 1, views: vec![7] });
    assert!(t.core.navigator().is_root(), "the stale screen is gone");
    assert!(t.core.error().is_some_and(|e| e.contains("demo") && e.contains("reloaded")));
}

#[test]
fn a_crash_closes_screens_and_loading_placeholders_of_that_extension_only() {
    let mut t = T::new(60);
    t.push(7, sectioned()); // another extension's screen (not in the crash list)
    t.core.on_event(RuntimeEvent::ExtensionCrashed { extension: "other".into(), generation: 1, reason: "boom".into(), views: vec![99] });
    assert!(!t.core.navigator().is_root(), "unrelated screens stay");
    t.core.on_event(RuntimeEvent::ExtensionCrashed { extension: "demo".into(), generation: 1, reason: "boom".into(), views: vec![7] });
    assert!(t.core.navigator().is_root());
}

#[test]
fn a_cancelled_command_drops_its_loading_screen() {
    let mut t = T::new(60);
    t.core.run_command("demo:hello");
    assert!(matches!(t.core.screen(), Screen::Loading { .. }));
    t.core.on_event(RuntimeEvent::CommandCancelled { command_id: "demo:hello".into() });
    assert!(t.core.navigator().is_root());
}

// ------------------------------------------------------------ permissions in Manage Extensions

#[test]
fn the_details_screen_offers_to_revoke_and_restore_permissions() {
    let mut i = info("a", ExtensionStatus::Loaded, true, ExtensionSource::Dev);
    i.permissions = vec!["network".into(), "clipboard.write".into(), "credentials.read".into()];
    i.revoked_permissions = vec!["process.execute".into()];
    let n = manage::info_detail(&i);
    let ViewNode::Detail(d) = &n.node else { panic!() };
    let titles: Vec<&str> = d.actions.iter().flat_map(|s| s.actions.iter().map(|a| a.title.as_str())).collect();
    assert!(titles.contains(&"Revoke network") && titles.contains(&"Revoke clipboard.write") && titles.contains(&"Restore process.execute"), "{titles:?}");
    assert!(!titles.iter().any(|t| t.contains("credentials")), "implicit credentials are not offered");
    assert_eq!(d.actions[0].actions[0].title, "Reload", "the primary action (Enter) is harmless");
    let revoke = d.actions.iter().flat_map(|s| &s.actions).find(|a| a.title == "Revoke network").unwrap();
    assert!(revoke.confirm.is_some(), "taking a permission away is confirmed");
}

#[test]
fn revoking_from_the_ui_writes_permissions_toml_and_reloads_the_extension() {
    let mut t = T::new(60);
    let dir = tempfile::tempdir().unwrap();
    t.core.config_dir = dir.path().to_path_buf();
    let mut i = info("demo", ExtensionStatus::Loaded, true, ExtensionSource::Dev);
    i.permissions = vec!["network".into()];
    t.core.on_event(RuntimeEvent::ExtensionsChanged(vec![i.clone()]));
    t.core.push_native_for_test(&i);
    let (view_id, action_id) = t.core.native_action_for_test("Revoke network").expect("the action exists");
    t.core.run_native_for_test(view_id, action_id);
    let text = std::fs::read_to_string(dir.path().join("permissions.toml")).unwrap();
    assert!(text.contains("[extensions.demo]") && text.contains("network"), "{text}");
    assert!(t.sent().iter().any(|r| matches!(r, RuntimeRequest::ReloadExtension { name } if name == "demo")));
}
