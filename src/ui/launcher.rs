//! The launcher window. It renders [`Core`] state and forwards user input to it; every rule
//! (navigation, actions, search, history, panel, forms, grids, confirmation) lives in `flux-core`.

use std::{collections::HashMap, rc::Rc, time::Duration};

use flux_api::{ActionStyle, Accessory, GridContent, GridFit, KeyInput};
use flux_core::{Core, DetailItem, DisplayRow, Effect, PanelRow, Screen, columns_for};
use gpui::{
    Animation, AnimationExt, AnyElement, App, ClickEvent, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, KeyBinding, KeyDownEvent, ListAlignment,
    ListState, ObjectFit, Render, ScrollHandle, Subscription, Window, actions, div, list, point, prelude::*, px, relative,
};

use super::{
    form_ui::{FormKey, FormUi},
    icons::{named_color, render_icon, render_image},
    markdown_view, shell, theme,
    text_input::{TextInput, TextInputEvent},
    window::WIDTH,
};

actions!(
    flux_launcher,
    [Confirm, Back, SelectNext, SelectPrev, SelectLeft, SelectRight, ToggleActions, HistoryPrev, HistoryNext, FocusNext, FocusPrev, PageUp, PageDown]
);

const KEY_CONTEXT: &str = "Launcher";
const ROW_H: f32 = 52.;
const HEADER_H: f32 = 30.;
const GRID_GAP: f32 = 8.;
/// Body width available to a grid (window minus its horizontal padding).
const GRID_W: f32 = WIDTH - 32.;

pub fn bind_keys(cx: &mut App) {
    let c = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("enter", Confirm, c),
        KeyBinding::new("escape", Back, c),
        KeyBinding::new("down", SelectNext, c),
        KeyBinding::new("up", SelectPrev, c),
        KeyBinding::new("left", SelectLeft, c),
        KeyBinding::new("right", SelectRight, c),
        KeyBinding::new("ctrl-n", SelectNext, c),
        KeyBinding::new("ctrl-p", SelectPrev, c),
        KeyBinding::new("ctrl-k", ToggleActions, c),
        KeyBinding::new("ctrl-up", HistoryPrev, c),
        KeyBinding::new("ctrl-down", HistoryNext, c),
        KeyBinding::new("tab", FocusNext, c),
        KeyBinding::new("shift-tab", FocusPrev, c),
        KeyBinding::new("pageup", PageUp, c),
        KeyBinding::new("pagedown", PageDown, c),
    ]);
}

pub struct Launcher {
    core: Entity<Core>,
    input: Entity<TextInput>,
    panel_input: Entity<TextInput>,
    list_state: ListState,
    /// (view id, display rows, columns) the list state was built for.
    list_sig: (Option<u64>, usize, usize),
    forms: HashMap<u64, Entity<FormUi>>,
    detail_scroll: ScrollHandle,
    /// Focus target for screens without a text field (detail pages), so keys still reach the launcher.
    root_focus: FocusHandle,
    seen_epoch: u64,
    seen_selected: usize,
    panel_open: bool,
    _subs: Vec<Subscription>,
}

impl Launcher {
    pub fn new(core: Entity<Core>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("Search commands…", cx));
        let panel_input = cx.new(|cx| TextInput::new("Search actions…", cx));

        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(&input, window, |this, input, _: &TextInputEvent, _, cx| {
            let text = input.read(cx).text().to_string();
            this.core.update(cx, |core, cx| {
                core.set_query(&text);
                cx.notify();
            });
        }));
        subs.push(cx.subscribe_in(&panel_input, window, |this, input, _: &TextInputEvent, _, cx| {
            let text = input.read(cx).text().to_string();
            this.core.update(cx, |core, cx| {
                if let Some(p) = core.panel_mut() {
                    p.set_filter(&text);
                }
                cx.notify();
            });
        }));
        subs.push(cx.observe_in(&core, window, |this, _, window, cx| this.sync(window, cx)));

        let mut this = Self {
            core,
            input,
            panel_input,
            list_state: ListState::new(0, ListAlignment::Top, px(300.)),
            list_sig: (None, 0, 0),
            forms: HashMap::new(),
            detail_scroll: ScrollHandle::new(),
            root_focus: cx.focus_handle(),
            seen_epoch: u64::MAX,
            seen_selected: usize::MAX,
            panel_open: false,
            _subs: subs,
        };
        // The search field has focus when the launcher opens; screens without one (a form being
        // shown again after the file chooser, a detail page) focus their own widget in `sync`.
        if this.core.read(cx).shows_search_field() {
            window.focus(&this.input.read(cx).focus_handle(cx), cx);
        }
        this.sync(window, cx);
        this
    }

    fn form_ui(&self, cx: &App) -> Option<Entity<FormUi>> {
        let id = self.core.read(cx).screen().view_id()?;
        self.forms.get(&id).cloned()
    }

    /// Pulls state that the *core* changed into the widgets (text, placeholder, focus, scroll).
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (epoch, query, placeholder, secure, panel_open, selected, is_input, is_grid, cols_cell, screen_id, is_form) = {
            let c = self.core.read(cx);
            (
                c.epoch(),
                c.query().to_string(),
                c.placeholder(),
                c.is_secure_input(),
                c.panel().is_some(),
                c.selected(),
                matches!(c.screen(), Screen::View { node: flux_api::ViewNode::Input(_), .. }),
                c.grid_geometry().is_some(),
                c.grid_geometry(),
                c.screen().view_id(),
                c.form().is_some(),
            )
        };

        if let Some((cell_w, _, forced, _)) = cols_cell {
            let cols = columns_for(GRID_W, cell_w, GRID_GAP, forced);
            self.core.update(cx, |c, _| c.set_grid_columns(cols));
        }

        // forms: one widget tree per open form screen; drop those no longer on the stack
        if is_form {
            if let Some(id) = screen_id {
                if !self.forms.contains_key(&id) {
                    let core = self.core.clone();
                    let f = cx.new(|cx| FormUi::new(core, id, window, cx));
                    self.forms.insert(id, f);
                }
            }
        }
        let alive: Vec<u64> = self.core.read(cx).navigator_view_ids();
        self.forms.retain(|id, _| alive.contains(id));
        if let Some(f) = self.form_ui(cx) {
            f.update(cx, |f, cx| f.sync(window, cx));
        }

        if epoch != self.seen_epoch {
            self.seen_epoch = epoch;
            let editing_value = is_input && !query.is_empty();
            self.input.update(cx, |i, cx| {
                i.set_text(&query, cx);
                if editing_value {
                    i.select_all_text(cx); // typing replaces the current value
                }
            });
        }
        self.input.update(cx, |i, cx| {
            i.set_placeholder(&placeholder, cx);
            i.set_secure(secure, cx);
            i.set_arrows_navigate(is_grid, cx);
        });
        if panel_open != self.panel_open {
            self.panel_open = panel_open;
            if panel_open {
                self.panel_input.update(cx, |i, cx| i.set_text("", cx));
                window.focus(&self.panel_input.read(cx).focus_handle(cx), cx);
            } else if is_form {
                if let Some(f) = self.form_ui(cx) {
                    f.update(cx, |f, cx| f.sync(window, cx));
                }
            } else {
                window.focus(&self.input.read(cx).focus_handle(cx), cx);
            }
        }
        if !is_form && !panel_open && !self.core.read(cx).shows_search_field() {
            if !self.root_focus.is_focused(window) {
                window.focus(&self.root_focus, cx);
            }
        } else if !is_form && !panel_open && self.core.read(cx).shows_search_field() {
            // leaving a form (or the panel) gives the search field its focus back
            let h = self.input.read(cx).focus_handle(cx);
            if !h.is_focused(window) && self.forms.is_empty() {
                window.focus(&h, cx);
            }
        }

        // keep the list state in step with the display rows, and the selection visible
        let model = self.core.read(cx).model();
        let sig = (screen_id, model.rows.len(), model.layout.as_ref().map(|l| l.cols).unwrap_or(0));
        if sig != self.list_sig {
            self.list_sig = sig;
            self.list_state.reset(model.rows.len());
        }
        if selected != self.seen_selected || sig != self.list_sig {
            self.seen_selected = selected;
            if let Some(ix) = model.rows.iter().position(|r| match r {
                DisplayRow::Item(i) => *i == selected,
                DisplayRow::GridRow { start, len } => selected >= *start && selected < start + len,
                DisplayRow::Header { .. } => false,
            }) {
                self.list_state.scroll_to_reveal_item(ix);
            }
        }
        cx.notify();
    }

    fn apply(&mut self, effects: Vec<Effect>, window: &mut Window, cx: &mut Context<Self>) {
        for effect in effects {
            match effect {
                Effect::Hide => shell::hide_from_window(&self.core, window, cx),
                Effect::Quit => shell::quit(cx),
                other => shell::apply_one(other, cx),
            }
        }
    }

    fn run(&mut self, window: &mut Window, cx: &mut Context<Self>, f: impl FnOnce(&mut Core) -> Vec<Effect>) {
        let effects = self.core.update(cx, |core, cx| {
            let mut e = f(core);
            e.extend(core.take_effects());
            cx.notify();
            e
        });
        self.apply(effects, window, cx);
    }

    // ------------------------------------------------------------ actions

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let (panel, confirming) = {
            let c = self.core.read(cx);
            (c.panel().is_some(), c.confirmation().is_some())
        };
        if !panel && !confirming {
            if let Some(f) = self.form_ui(cx) {
                if f.update(cx, |f, cx| f.key(FormKey::Enter, window, cx)) {
                    self.sync(window, cx);
                    return;
                }
            }
        }
        self.run(window, cx, |c| if panel { c.run_panel_selected() } else { c.activate() });
    }

    fn back(&mut self, _: &Back, window: &mut Window, cx: &mut Context<Self>) {
        let blocked = {
            let c = self.core.read(cx);
            c.panel().is_some() || c.confirmation().is_some()
        };
        if !blocked {
            if let Some(f) = self.form_ui(cx) {
                if f.update(cx, |f, cx| f.close_popup(cx)) {
                    return;
                }
            }
        }
        self.run(window, cx, |c| c.back());
    }

    fn vertical(&mut self, up: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (panel, confirming, is_form, is_detail) = {
            let c = self.core.read(cx);
            (c.panel().is_some(), c.confirmation().is_some(), c.form().is_some(), c.detail().is_some())
        };
        if confirming {
            return;
        }
        if panel {
            self.run(window, cx, |c| {
                if let Some(p) = c.panel_mut() {
                    if up { p.select_prev() } else { p.select_next() }
                }
                vec![]
            });
            return;
        }
        if is_form {
            let used = self.form_ui(cx).is_some_and(|f| f.update(cx, |f, cx| f.key(if up { FormKey::Up } else { FormKey::Down }, window, cx)));
            if !used {
                self.run(window, cx, |c| {
                    if up { c.form_focus_prev() } else { c.form_focus_next() }
                    vec![]
                });
                self.sync(window, cx);
            }
            return;
        }
        if is_detail {
            let h = self.detail_scroll.clone();
            let max = h.max_offset().y;
            let y = (h.offset().y + px(if up { 48. } else { -48. })).clamp(-max, px(0.));
            h.set_offset(point(h.offset().x, y));
            cx.notify();
            return;
        }
        self.run(window, cx, |c| {
            if up { c.select_up() } else { c.select_down() }
            vec![]
        });
    }

    fn select_next(&mut self, _: &SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        self.vertical(false, window, cx);
    }

    fn select_prev(&mut self, _: &SelectPrev, window: &mut Window, cx: &mut Context<Self>) {
        self.vertical(true, window, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.read(cx).panel().is_none() {
            if let Some(f) = self.form_ui(cx) {
                if f.update(cx, |f, cx| f.key(FormKey::Left, window, cx)) {
                    return;
                }
            }
        }
        self.run(window, cx, |c| {
            c.select_left();
            vec![]
        });
    }

    fn select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.read(cx).panel().is_none() {
            if let Some(f) = self.form_ui(cx) {
                if f.update(cx, |f, cx| f.key(FormKey::Right, window, cx)) {
                    return;
                }
            }
        }
        self.run(window, cx, |c| {
            c.select_right();
            vec![]
        });
    }

    fn page(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.read(cx).detail().is_some() {
            let h = self.detail_scroll.clone();
            let max = h.max_offset().y;
            let y = (h.offset().y + px(if down { -280. } else { 280. })).clamp(-max, px(0.));
            h.set_offset(point(h.offset().x, y));
            cx.notify();
            return;
        }
        for _ in 0..6 {
            self.vertical(!down, window, cx);
        }
    }

    fn page_up(&mut self, _: &PageUp, window: &mut Window, cx: &mut Context<Self>) {
        self.page(false, window, cx);
    }

    fn page_down(&mut self, _: &PageDown, window: &mut Window, cx: &mut Context<Self>) {
        self.page(true, window, cx);
    }

    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.read(cx).form().is_some() {
            self.run(window, cx, |c| {
                c.form_focus_next();
                vec![]
            });
            self.sync(window, cx);
        }
    }

    fn focus_prev(&mut self, _: &FocusPrev, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.read(cx).form().is_some() {
            self.run(window, cx, |c| {
                c.form_focus_prev();
                vec![]
            });
            self.sync(window, cx);
        }
    }

    fn toggle_actions(&mut self, _: &ToggleActions, window: &mut Window, cx: &mut Context<Self>) {
        self.run(window, cx, |c| {
            c.toggle_panel();
            vec![]
        });
    }

    fn history_prev(&mut self, _: &HistoryPrev, window: &mut Window, cx: &mut Context<Self>) {
        self.run(window, cx, |c| {
            c.history_prev();
            vec![]
        });
    }

    fn history_next(&mut self, _: &HistoryNext, window: &mut Window, cx: &mut Context<Self>) {
        self.run(window, cx, |c| {
            c.history_next();
            vec![]
        });
    }

    /// Item-level shortcuts (ctrl+enter, ctrl+shift+c, …): any chord that is not an editor binding.
    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        if !(m.control || m.alt || m.platform) {
            return;
        }
        let input = KeyInput { key: ev.keystroke.key.clone(), ctrl: m.control, alt: m.alt, shift: m.shift, meta: m.platform };
        let (hit, effects) = self.core.update(cx, |c, cx| {
            let hit = c.panel().is_none() && c.confirmation().is_none() && c.trigger_shortcut(&input);
            let e = c.take_effects();
            cx.notify();
            (hit, e)
        });
        if hit {
            cx.stop_propagation();
        }
        self.apply(effects, window, cx);
    }
}

impl Focusable for Launcher {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

// ------------------------------------------------------------------- rows

fn accessory(a: &Accessory, cx: &mut App) -> AnyElement {
    let mut row = div().flex().items_center().gap_1p5().flex_none();
    if let Some(i) = &a.icon {
        row = row.child(render_icon(i, 14., theme::text_dim().into(), cx));
    }
    if let Some(p) = a.progress {
        row = row.child(
            div()
                .w(px(56.))
                .h(px(5.))
                .rounded_full()
                .bg(theme::chip())
                .child(div().h_full().w(relative(p.clamp(0., 1.))).rounded_full().bg(theme::accent())),
        );
    }
    if let Some(t) = &a.text {
        row = row.child(div().text_size(px(12.)).text_color(theme::text_dim()).child(t.clone()));
    }
    if let Some(tag) = &a.tag {
        let color = tag.color.as_deref().map(named_color).unwrap_or_else(|| theme::text_dim().into());
        row = row.child(
            div()
                .px_1p5()
                .py_0p5()
                .rounded_full()
                .border_1()
                .border_color(color.opacity(0.5))
                .bg(color.opacity(0.14))
                .text_size(px(11.))
                .text_color(color)
                .child(tag.text.clone()),
        );
    }
    row.into_any_element()
}

fn header_row(title: &Option<String>, subtitle: &Option<String>) -> AnyElement {
    div()
        .h(px(HEADER_H))
        .px_5()
        .flex()
        .items_end()
        .gap_2()
        .pb_1()
        .text_size(px(11.))
        .text_color(theme::text_faint())
        .child(div().font_weight(FontWeight::BOLD).child(title.clone().unwrap_or_default().to_uppercase()))
        .when_some(subtitle.clone(), |d, s| d.child(div().child(s)))
        .into_any_element()
}

// ----------------------------------------------------------------- render

impl Render for Launcher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let core_entity = self.core.clone();
        let core = self.core.read(cx);
        let screen = core.screen().clone();
        let model = core.model();
        let error = core.error().map(str::to_string);
        let primary = core.primary_action_title();
        let depth = core.navigator().depth();
        let loading = core.is_loading();
        let shows_search = core.shows_search_field();
        let confirmation = core.confirmation().cloned();
        let is_form = core.form().is_some();
        let (empty_title, empty_desc, empty_icon) = core.empty_view();
        let item_count = model.vis.len();
        let detail = core.detail();
        let grid = core.grid_geometry();

        let body: AnyElement = match &screen {
            Screen::View { node: flux_api::ViewNode::Input(view), title, .. } => div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .child(div().text_color(theme::text()).child(title.clone()))
                .child(div().text_size(px(12.)).text_color(theme::text_faint()).child(if view.secure {
                    "The text is hidden. Press Enter to save, Esc to cancel."
                } else {
                    "Press Enter to save, Esc to cancel."
                }))
                .into_any_element(),
            Screen::View { node: flux_api::ViewNode::Form(f), .. } if f.fields.is_empty() => centered("This form has no fields", Some("Press Esc to go back."), None, cx),
            Screen::View { node: flux_api::ViewNode::Form(_), view_id, .. } => match self.forms.get(view_id) {
                Some(f) => f.clone().into_any_element(),
                None => div().into_any_element(),
            },
            Screen::View { node: flux_api::ViewNode::Detail(d), .. } if d.blocks.is_empty() && d.metadata.is_empty() && !d.is_loading => {
                centered("Nothing to show", Some("This screen has no content."), None, cx)
            }
            Screen::View { node: flux_api::ViewNode::Detail(_), .. } => {
                let model = detail.clone().unwrap_or_default();
                let on_link: markdown_view::LinkHandler = {
                    let core = core_entity.clone();
                    Rc::new(move |url, _w, cx| {
                        let effects = core.update(cx, |c, cx| {
                            let e = c.open_url(url);
                            cx.notify();
                            e
                        });
                        shell::apply(effects, cx);
                    })
                };
                let mut content = div().flex().flex_col().gap_3();
                for item in &model.items {
                    content = content.child(match item {
                        DetailItem::Markdown(blocks) => markdown_view::render(blocks, &on_link),
                        DetailItem::ProgressBar { value, label } => progress_block(*value, label.as_deref()),
                        DetailItem::Loading { label } => progress_block(None, label.as_deref().or(Some("Loading…"))),
                    });
                }
                let meta = (!model.metadata.is_empty()).then(|| metadata_panel(&model.metadata, &on_link, cx));
                div()
                    .size_full()
                    .flex()
                    .child(div().id("detail-scroll").flex_1().min_w_0().h_full().overflow_y_scroll().track_scroll(&self.detail_scroll).px_5().py_2().child(content))
                    .children(meta)
                    .into_any_element()
            }
            Screen::Loading { .. } => centered(&empty_title, None, None, cx),
            _ if item_count == 0 => centered(&empty_title, empty_desc.as_deref(), empty_icon.as_ref(), cx),
            _ => {
                let selected = core.selected();
                let this = cx.entity();
                let model2 = model.clone();
                let core_e = core_entity.clone();
                let grid_cell = grid.map(|(w, h, _, fit)| (w, h, fit));
                list(self.list_state.clone(), move |ix, _window, cx| match &model2.rows[ix] {
                    DisplayRow::Header { title, subtitle } => header_row(title, subtitle),
                    DisplayRow::Item(i) => {
                        let Some(item) = core_e.read(cx).item(*i) else { return div().into_any_element() };
                        let is_selected = *i == selected;
                        let idx = *i;
                        let this = this.clone();
                        let icon = item.icon.clone();
                        let fallback_letter = item.title.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
                        div()
                            .id(("row", ix))
                            .group("row")
                            .h(px(ROW_H))
                            .px_2()
                            .child(
                                div()
                                    .size_full()
                                    .px_3()
                                    .rounded_lg()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .when(is_selected, |d| d.bg(theme::selected()))
                                    .when(!is_selected, |d| d.group_hover("row", |h| h.bg(theme::hover())))
                                    .child(div().w(px(28.)).flex_none().flex().justify_center().child(match &icon {
                                        Some(ic) => render_icon(ic, 20., theme::text_dim().into(), cx),
                                        None => div().text_size(px(18.)).text_color(theme::text_dim()).child(fallback_letter).into_any_element(),
                                    }))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .child(div().text_color(theme::text()).truncate().child(item.title.clone()))
                                            .when_some(item.subtitle.clone(), |d, s| d.child(div().text_size(px(12.)).text_color(theme::text_dim()).truncate().child(s))),
                                    )
                                    .children(item.accessories.iter().map(|a| accessory(a, cx))),
                            )
                            .on_click(move |_: &ClickEvent, window, cx| {
                                this.update(cx, |this, cx| {
                                    this.run(window, cx, |c| {
                                        c.set_selected(idx);
                                        c.activate()
                                    });
                                })
                            })
                            .into_any_element()
                    }
                    DisplayRow::GridRow { start, len } => {
                        let (cw, ch, fit) = grid_cell.unwrap_or((112., 112., GridFit::Contain));
                        let mut row = div().px_4().py(px(GRID_GAP / 2.)).flex().gap(px(GRID_GAP));
                        for i in *start..start + len {
                            let Some(cell) = core_e.read(cx).cell(i) else { continue };
                            let is_selected = i == selected;
                            let this = this.clone();
                            let fit = if fit == GridFit::Fill { ObjectFit::Cover } else { ObjectFit::Contain };
                            let content: AnyElement = match &cell.content {
                                GridContent::Image(img) => render_image(img, cw, ch, fit, theme::text_dim().into(), cx),
                                GridContent::Icon(ic) => div().w(px(cw)).h(px(ch)).flex().items_center().justify_center().child(render_icon(ic, (cw.min(ch) * 0.5).clamp(16., 64.), theme::text_dim().into(), cx)).into_any_element(),
                                GridContent::Text(t) => div().w(px(cw)).h(px(ch)).flex().items_center().justify_center().text_size(px((cw.min(ch) * 0.4).clamp(14., 48.))).text_color(theme::text()).child(t.clone()).into_any_element(),
                            };
                            row = row.child(
                                div()
                                    .id(("cell", i))
                                    .w(px(cw))
                                    .flex_none()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .p_1()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(if is_selected { theme::accent() } else { gpui::Rgba { r: 0., g: 0., b: 0., a: 0. } })
                                    .when(is_selected, |d| d.bg(theme::selected()))
                                    .when(!is_selected, |d| d.hover(|h| h.bg(theme::hover())))
                                    .child(div().rounded_md().overflow_hidden().bg(theme::chip()).child(content))
                                    .when_some(cell.title.clone(), |d, t| d.child(div().text_size(px(12.)).text_color(theme::text()).truncate().child(t)))
                                    .when_some(cell.subtitle.clone(), |d, t| d.child(div().text_size(px(11.)).text_color(theme::text_dim()).truncate().child(t)))
                                    .on_click(move |_: &ClickEvent, window, cx| {
                                        this.update(cx, |this, cx| {
                                            this.run(window, cx, |c| {
                                                c.set_selected(i);
                                                c.activate()
                                            });
                                        })
                                    }),
                            );
                        }
                        row.into_any_element()
                    }
                })
                .size_full()
                .into_any_element()
            }
        };

        let hint = match (&screen, &primary) {
            (Screen::Loading { title, .. }, _) => format!("Running {title}…   esc Cancel"),
            (Screen::Root, _) => "↵ Run    ↑↓ Navigate    esc Close".to_string(),
            (_, _) if is_form => "tab Next field    ctrl+↵ Submit    esc Back".to_string(),
            (Screen::View { node: flux_api::ViewNode::Grid(_), .. }, Some(p)) => format!("↵ {p}    ←↑↓→ Navigate    esc Back"),
            (Screen::View { node: flux_api::ViewNode::Detail(_), .. }, p) => match p {
                Some(p) => format!("↵ {p}    ↑↓ Scroll    esc Back"),
                None => "↑↓ Scroll    esc Back".to_string(),
            },
            (_, Some(p)) => format!("↵ {p}    ↑↓ Navigate    esc Back"),
            _ => "↑↓ Navigate    esc Back".to_string(),
        };
        let (footer_text, footer_color) = match &error {
            Some(e) => (e.clone(), theme::error()),
            None => (hint, theme::text_faint()),
        };
        let has_actions = !matches!(screen, Screen::Loading { .. } | Screen::View { node: flux_api::ViewNode::Input(_), .. }) && (primary.is_some() || is_form);

        let panel = self.core.read(cx).panel().map(|p| {
            let rows = p.rows();
            let selected = p.selected_index();
            div()
                .absolute()
                .bottom(px(40.))
                .right(px(12.))
                .w(px(340.))
                .max_h(px(300.))
                .flex()
                .flex_col()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::panel_background())
                .shadow_lg()
                .overflow_hidden()
                .child(div().h(px(40.)).px_3().flex().items_center().text_size(px(14.)).child(self.panel_input.clone()))
                .child(div().h(px(1.)).bg(theme::divider()))
                .child(
                    div()
                        .id("panel-rows")
                        .flex_1()
                        .overflow_y_scroll()
                        .py_1()
                        .children(rows.into_iter().map(|row| match row {
                            PanelRow::Header(title) => div().px_3().pt_2().pb_1().text_size(px(11.)).text_color(theme::text_faint()).child(title.to_string()).into_any_element(),
                            PanelRow::Action { index, entry } => {
                                let color = if entry.style == ActionStyle::Destructive { theme::destructive() } else { theme::text() };
                                div()
                                    .id(("panel-action", index))
                                    .mx_1()
                                    .px_2()
                                    .h(px(34.))
                                    .rounded_md()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .when(index == selected, |d| d.bg(theme::selected()))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .child(div().text_size(px(13.)).text_color(color).truncate().child(entry.title.clone()))
                                            .when_some(entry.description.clone(), |d, s| d.child(div().text_size(px(11.)).text_color(theme::text_faint()).truncate().child(s))),
                                    )
                                    .when_some(entry.shortcut.as_ref().map(|s| s.pretty()), |d, s| {
                                        d.child(div().px_1p5().py_0p5().rounded_sm().bg(theme::chip()).text_size(px(11.)).text_color(theme::text_dim()).child(s))
                                    })
                                    .into_any_element()
                            }
                        })),
                )
        });

        let confirm_overlay = confirmation.map(|c| {
            div()
                .absolute()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::black().opacity(0.45))
                .child(
                    div()
                        .w(px(380.))
                        .p_4()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::panel_background())
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).text_color(if c.destructive { theme::destructive() } else { theme::text() }).child(c.title.clone()))
                        .when_some(c.message.clone(), |d, m| d.child(div().text_size(px(13.)).text_color(theme::text_dim()).child(m)))
                        .child(div().mt_2().flex().justify_end().gap_3().text_size(px(12.)).text_color(theme::text_dim()).child("esc Cancel").child(
                            div().text_color(if c.destructive { theme::destructive() } else { theme::accent() }).child("↵ Confirm"),
                        )),
                )
        });

        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.root_focus)
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::back))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::toggle_actions))
            .on_action(cx.listener(Self::history_prev))
            .on_action(cx.listener(Self::history_next))
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_prev))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_key_down(ev, window, cx)))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded_xl()
            .border_1()
            .border_color(theme::border())
            .bg(theme::background())
            .text_size(px(15.))
            .text_color(theme::text())
            .child(
                div()
                    .h(px(60.))
                    .px_5()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(18.))
                    .when(depth > 1, |d| {
                        d.child(
                            div()
                                .id("back-button")
                                .px_1p5()
                                .rounded_md()
                                .cursor_pointer()
                                .text_color(theme::text_dim())
                                .hover(|h| h.bg(theme::hover()).text_color(theme::text()))
                                .child("‹")
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.run(window, cx, |c| c.back()))),
                        )
                    })
                    .when(shows_search, |d| d.child(self.input.clone()))
                    .when(!shows_search, |d| d.child(div().text_color(theme::text()).child(screen_title(&screen)))),
            )
            .child(loading_bar(loading))
            .child(div().flex_1().min_h_0().py_2().child(body))
            .child(div().h(px(1.)).bg(theme::divider()))
            .child(
                div()
                    .h(px(34.))
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(12.))
                    .text_color(footer_color)
                    .child(div().flex_1().truncate().child(footer_text))
                    .when(has_actions, |d| {
                        d.child(div().flex().items_center().gap_1().text_color(theme::text_faint()).child("Actions").child(div().px_1p5().py_0p5().rounded_sm().bg(theme::chip()).child("Ctrl+K")))
                    }),
            )
            .children(panel)
            .children(confirm_overlay)
            .when(false, |d| d.child(window_marker(window)))
    }
}

fn window_marker(_w: &Window) -> gpui::Div {
    div()
}

fn screen_title(screen: &Screen) -> String {
    match screen {
        Screen::View { title, .. } => title.clone(),
        _ => String::new(),
    }
}

fn centered(title: &str, desc: Option<&str>, icon: Option<&flux_api::Icon>, cx: &mut App) -> AnyElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_1p5()
        .when_some(icon.cloned(), |d, i| d.child(render_icon(&i, 28., theme::text_faint().into(), cx)))
        .child(div().text_color(theme::text_dim()).child(title.to_string()))
        .when_some(desc.map(String::from), |d, s| d.child(div().text_size(px(12.)).text_color(theme::text_faint()).child(s)))
        .into_any_element()
}

/// A 2 px indeterminate bar under the search field while the screen is loading.
fn loading_bar(loading: bool) -> AnyElement {
    let track = div().h(px(2.)).w_full().relative().overflow_hidden();
    if !loading {
        return track.bg(theme::divider()).into_any_element();
    }
    let segment = div().absolute().top_0().h_full().w(relative(0.3)).bg(theme::accent());
    if super::reduce_motion() {
        return track.bg(theme::divider()).child(segment.left(relative(0.35))).into_any_element();
    }
    track
        .bg(theme::divider())
        .child(segment.with_animation("loading-bar", Animation::new(Duration::from_millis(1100)).repeat(), |el, t| el.left(relative(-0.3 + 1.3 * t))))
        .into_any_element()
}

fn progress_block(value: Option<f32>, label: Option<&str>) -> AnyElement {
    let bar = div().h(px(6.)).w_full().rounded_full().bg(theme::chip()).overflow_hidden().relative();
    let bar = match value {
        Some(v) => bar.child(div().h_full().w(relative(v.clamp(0., 1.))).bg(theme::accent())),
        None => {
            let segment = div().absolute().top_0().h_full().w(relative(0.3)).bg(theme::accent());
            if super::reduce_motion() {
                bar.child(segment.left(relative(0.35)))
            } else {
                bar.child(segment.with_animation("progress-indeterminate", Animation::new(Duration::from_millis(1100)).repeat(), |el, t| el.left(relative(-0.3 + 1.3 * t))))
            }
        }
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .when_some(label.map(String::from), |d, l| d.child(div().flex().justify_between().text_size(px(12.)).text_color(theme::text_dim()).child(l).when_some(value, |d, v| d.child(format!("{:.0}%", v * 100.)))))
        .child(bar)
        .into_any_element()
}

fn metadata_panel(items: &[flux_api::MetaItem], on_link: &markdown_view::LinkHandler, cx: &mut App) -> AnyElement {
    use flux_api::MetaItem;
    let mut col = div().w(px(220.)).flex_none().h_full().border_l_1().border_color(theme::divider()).px_4().py_3().flex().flex_col().gap_3().overflow_hidden();
    for (i, item) in items.iter().enumerate() {
        col = col.child(match item {
            MetaItem::Separator => div().h(px(1.)).w_full().bg(theme::divider()).into_any_element(),
            MetaItem::Label { title, text, icon } => div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_size(px(11.)).text_color(theme::text_faint()).child(title.clone()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_size(px(13.))
                        .text_color(theme::text())
                        .when_some(icon.clone(), |d, ic| d.child(render_icon(&ic, 14., theme::text_dim().into(), cx)))
                        // long values (folder paths) end in an ellipsis instead of running under the panel edge
                        .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(text.clone())),
                )
                .into_any_element(),
            MetaItem::TagList { title, tags } => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_size(px(11.)).text_color(theme::text_faint()).child(title.clone()))
                .child(div().flex().flex_wrap().gap_1().children(tags.iter().map(|t| {
                    let color = t.color.as_deref().map(named_color).unwrap_or_else(|| theme::text_dim().into());
                    div().px_1p5().py_0p5().rounded_full().border_1().border_color(color.opacity(0.5)).bg(color.opacity(0.14)).text_size(px(11.)).text_color(color).child(t.text.clone())
                })))
                .into_any_element(),
            MetaItem::Link { title, text, url } => {
                let (on_link, url) = (on_link.clone(), url.clone());
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(div().text_size(px(11.)).text_color(theme::text_faint()).child(title.clone()))
                    .child(
                        div()
                            .id(("meta-link", i))
                            .text_size(px(13.))
                            .text_color(theme::accent())
                            .cursor_pointer()
                            .truncate()
                            .child(text.clone())
                            .on_click(move |_, window, cx| on_link(&url, window, cx)),
                    )
                    .into_any_element()
            }
        });
    }
    col.into_any_element()
}
