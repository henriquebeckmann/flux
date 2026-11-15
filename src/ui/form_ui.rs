//! The widgets of `ui.Form`. All values, validation and focus order live in `flux_core::FormState`;
//! this file owns only what is purely visual or interactive: text widgets, which option is
//! highlighted, which popup is open, the calendar cursor.

use std::collections::HashMap;

use flux_api::{Field, FieldKind, FieldOption, FieldValue, FormView};
use flux_core::{Core, FormState, dates::{Date, DateTime, days_in_month, today_utc}};
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render, ScrollHandle, Styled,
    Subscription, Window, div, prelude::*, px,
};

use super::{
    icons,
    text_area::TextArea,
    text_input::{TextInput, TextInputEvent},
    theme,
};

/// Keys the launcher routes to the focused form field (they are bound as actions there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormKey {
    Enter,
    Up,
    Down,
    Left,
    Right,
    Escape,
}

/// Calendar popup state of one date field.
#[derive(Debug, Clone, PartialEq)]
struct Calendar {
    cursor: Date,
    hour: u32,
    minute: u32,
    /// `Some(0)` = editing the hour, `Some(1)` = the minute (datetime fields, after picking a day).
    time_stage: Option<usize>,
}

pub struct FormUi {
    core: Entity<Core>,
    view_id: u64,
    texts: HashMap<String, Entity<TextInput>>,
    areas: HashMap<String, Entity<TextArea>>,
    path_inputs: HashMap<String, Entity<TextInput>>,
    handles: HashMap<String, FocusHandle>,
    /// Highlighted option of dropdowns / tag pickers / file chips.
    cursor: HashMap<String, usize>,
    open: Option<String>,
    calendars: HashMap<String, Calendar>,
    notes: HashMap<String, String>,
    scroll: ScrollHandle,
    scrolled_for: Option<String>,
    last_focus: Option<usize>,
    /// (field, frames left): a brand-new window has not laid its fields out yet, so scrolling to the
    /// focused one is repeated for a few frames until it takes effect.
    pending_scroll: Option<(usize, u8)>,
    subs: Vec<Subscription>,
}

impl FormUi {
    pub fn new(core: Entity<Core>, view_id: u64, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            core,
            view_id,
            texts: HashMap::new(),
            areas: HashMap::new(),
            path_inputs: HashMap::new(),
            handles: HashMap::new(),
            cursor: HashMap::new(),
            open: None,
            calendars: HashMap::new(),
            notes: HashMap::new(),
            scroll: ScrollHandle::new(),
            scrolled_for: None,
            last_focus: None,
            pending_scroll: None,
            subs: vec![],
        };
        this.sync(window, cx);
        this
    }

    pub fn view_id(&self) -> u64 {
        self.view_id
    }

    fn form(&self, cx: &App) -> Option<(FormView, FormState)> {
        self.core.read(cx).form().map(|(v, s)| (v.clone(), s.clone()))
    }

    // ------------------------------------------------------------ syncing

    /// Creates widgets for new fields, copies values the *core* changed (controlled values, defaults)
    /// into the text widgets and moves keyboard focus to the field the core says is focused.
    pub fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((view, state)) = self.form(cx) else { return };
        let ids: Vec<&str> = view.fields.iter().map(|f| f.id.as_str()).collect();
        self.texts.retain(|k, _| ids.contains(&k.as_str()));
        self.areas.retain(|k, _| ids.contains(&k.as_str()));
        self.path_inputs.retain(|k, _| ids.contains(&k.as_str()));
        self.handles.retain(|k, _| ids.contains(&k.as_str()));

        for f in &view.fields {
            let id = f.id.clone();
            match &f.kind {
                FieldKind::Text | FieldKind::Password => {
                    let input = match self.texts.get(&id) {
                        Some(i) => i.clone(),
                        None => {
                            let i = cx.new(|cx| TextInput::new(f.placeholder.as_deref().unwrap_or(""), cx));
                            let fid = id.clone();
                            self.subs.push(cx.subscribe_in(&i, window, move |this, input, _: &TextInputEvent, _, cx| {
                                let text = input.read(cx).text().to_string();
                                this.core.update(cx, |c, cx| {
                                    c.form_set(&fid, FieldValue::Text(text));
                                    cx.notify();
                                });
                            }));
                            self.texts.insert(id.clone(), i.clone());
                            i
                        }
                    };
                    let want = state.text(&id).to_string();
                    input.update(cx, |i, cx| {
                        if i.text() != want {
                            i.set_text(&want, cx);
                        }
                        i.set_secure(matches!(f.kind, FieldKind::Password), cx);
                        i.set_placeholder(f.placeholder.as_deref().unwrap_or(""), cx);
                    });
                }
                FieldKind::TextArea { rows } => {
                    let area = match self.areas.get(&id) {
                        Some(a) => a.clone(),
                        None => {
                            let a = cx.new(|cx| TextArea::new(f.placeholder.as_deref().unwrap_or(""), *rows, cx));
                            let fid = id.clone();
                            self.subs.push(cx.subscribe_in(&a, window, move |this, area, _: &TextInputEvent, _, cx| {
                                let text = area.read(cx).text().to_string();
                                this.core.update(cx, |c, cx| {
                                    c.form_set(&fid, FieldValue::Text(text));
                                    cx.notify();
                                });
                            }));
                            self.areas.insert(id.clone(), a.clone());
                            a
                        }
                    };
                    let want = state.text(&id).to_string();
                    area.update(cx, |a, cx| {
                        a.set_text(&want, cx);
                        a.set_rows(*rows, cx);
                        a.set_placeholder(f.placeholder.as_deref().unwrap_or(""), cx);
                    });
                }
                FieldKind::FilePicker { .. } => {
                    if !self.path_inputs.contains_key(&id) {
                        let i = cx.new(|cx| TextInput::new("Type a path and press Enter", cx));
                        self.path_inputs.insert(id.clone(), i);
                    }
                    self.handles.entry(id.clone()).or_insert_with(|| cx.focus_handle());
                }
                FieldKind::Checkbox { .. } | FieldKind::Dropdown { .. } | FieldKind::TagPicker { .. } | FieldKind::DatePicker { .. } => {
                    self.handles.entry(id.clone()).or_insert_with(|| cx.focus_handle());
                }
                FieldKind::Separator | FieldKind::Description { .. } => {}
            }
        }

        // keyboard focus follows the core's focused field
        let focus = state.focus();
        if self.last_focus != Some(focus) {
            self.last_focus = Some(focus);
            self.open = None;
            if let Some(f) = view.fields.get(focus) {
                if let Some(h) = self.focus_handle_of(f, cx) {
                    window.focus(&h, cx);
                }
                self.scroll.scroll_to_item(focus);
                self.pending_scroll = Some((focus, 4));
            }
        }
        cx.notify();
    }

    fn focus_handle_of(&self, f: &Field, cx: &App) -> Option<FocusHandle> {
        match &f.kind {
            FieldKind::Text | FieldKind::Password => self.texts.get(&f.id).map(|i| i.read(cx).focus_handle(cx)),
            FieldKind::TextArea { .. } => self.areas.get(&f.id).map(|a| a.read(cx).focus_handle(cx)),
            FieldKind::FilePicker { .. } => self.path_inputs.get(&f.id).map(|i| i.read(cx).focus_handle(cx)),
            _ => self.handles.get(&f.id).cloned(),
        }
    }

    // --------------------------------------------------------------- values

    fn set(&mut self, id: &str, value: FieldValue, cx: &mut Context<Self>) {
        self.core.update(cx, |c, cx| {
            c.form_set(id, value);
            cx.notify();
        });
    }

    fn focused(&self, cx: &App) -> Option<Field> {
        let (view, state) = self.form(cx)?;
        view.fields.get(state.focus()).cloned()
    }

    pub fn has_open_popup(&self) -> bool {
        self.open.is_some()
    }

    pub fn close_popup(&mut self, cx: &mut Context<Self>) -> bool {
        let was = self.open.take().is_some();
        if was {
            cx.notify();
        }
        was
    }

    // ------------------------------------------------------------ keyboard

    /// Called by the launcher for Enter/Up/Down/Escape. Returns `true` when the focused field used the key.
    pub fn key(&mut self, key: FormKey, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(field) = self.focused(cx) else { return false };
        let Some((_, state)) = self.form(cx) else { return false };
        let id = field.id.clone();
        let is_open = self.open.as_deref() == Some(id.as_str());
        if matches!(key, FormKey::Left | FormKey::Right) {
            let named = if key == FormKey::Left { "left" } else { "right" };
            let handled = matches!(field.kind, FieldKind::TagPicker { .. } | FieldKind::FilePicker { .. }) || (matches!(field.kind, FieldKind::DatePicker { .. }) && is_open);
            if handled {
                self.on_field_key(&id, named, None, cx);
            }
            return handled;
        }
        let used = match (&field.kind, key) {
            (FieldKind::Checkbox { .. }, FormKey::Enter) => {
                let v = !state.bool(&id);
                self.set(&id, FieldValue::Bool(v), cx);
                true
            }
            (FieldKind::Dropdown { options }, k) => self.dropdown_key(&id, options, &state, k, is_open, cx),
            (FieldKind::TagPicker { options }, k) => self.tags_key(&id, options, &state, k, cx),
            (FieldKind::DatePicker { include_time, min, max }, k) => self.date_key(&id, *include_time, min.clone(), max.clone(), &state, k, is_open, cx),
            (FieldKind::FilePicker { .. }, FormKey::Enter) => {
                // Enter in the path box adds the typed path; on the chips it opens the portal
                let typed = self.path_inputs.get(&id).map(|i| i.read(cx).text().trim().to_string()).unwrap_or_default();
                if typed.is_empty() { self.browse(&field, cx) } else { self.add_typed_path(&field, &typed, &state, cx) }
                true
            }
            (FieldKind::Text | FieldKind::Password, FormKey::Enter) => {
                self.advance_or_submit(window, cx);
                true
            }
            _ => false,
        };
        cx.notify();
        used
    }

    fn advance_or_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let last = self.form(cx).map(|(v, s)| {
            let focusable: Vec<usize> = v.fields.iter().enumerate().filter(|(_, f)| flux_core::is_focusable(f)).map(|(i, _)| i).collect();
            focusable.last() == Some(&s.focus())
        });
        if last == Some(true) {
            self.core.update(cx, |c, cx| {
                c.form_submit();
                cx.notify();
            });
        } else {
            self.core.update(cx, |c, cx| {
                c.form_focus_next();
                cx.notify();
            });
        }
        self.sync(window, cx);
    }

    fn dropdown_key(&mut self, id: &str, options: &[FieldOption], state: &FormState, key: FormKey, is_open: bool, cx: &mut Context<Self>) -> bool {
        let sel = options.iter().position(|o| o.value == state.text(id)).unwrap_or(0);
        match (key, is_open) {
            (FormKey::Enter | FormKey::Down, false) => {
                self.open = Some(id.to_string());
                self.cursor.insert(id.to_string(), sel);
                true
            }
            (FormKey::Up, false) => false,
            (FormKey::Up, true) | (FormKey::Down, true) => {
                let c = self.cursor.get(id).copied().unwrap_or(sel) as isize + if key == FormKey::Down { 1 } else { -1 };
                self.cursor.insert(id.to_string(), c.rem_euclid(options.len().max(1) as isize) as usize);
                true
            }
            (FormKey::Enter, true) => {
                let c = self.cursor.get(id).copied().unwrap_or(sel);
                if let Some(o) = options.get(c) {
                    self.set(id, FieldValue::Text(o.value.clone()), cx);
                }
                self.open = None;
                true
            }
            (FormKey::Escape, true) => {
                self.open = None;
                true
            }
            _ => false,
        }
    }

    fn tags_key(&mut self, id: &str, options: &[FieldOption], state: &FormState, key: FormKey, cx: &mut Context<Self>) -> bool {
        if options.is_empty() {
            return false;
        }
        match key {
            FormKey::Enter => {
                let c = self.cursor.get(id).copied().unwrap_or(0).min(options.len() - 1);
                self.toggle_tag(id, &options[c].value, state, cx);
                true
            }
            _ => false,
        }
    }

    fn toggle_tag(&mut self, id: &str, value: &str, state: &FormState, cx: &mut Context<Self>) {
        let mut list: Vec<String> = state.list(id).to_vec();
        match list.iter().position(|x| x == value) {
            Some(p) => {
                list.remove(p);
            }
            None => list.push(value.to_string()),
        }
        self.set(id, FieldValue::List(list), cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn date_key(&mut self, id: &str, include_time: bool, min: Option<String>, max: Option<String>, state: &FormState, key: FormKey, is_open: bool, cx: &mut Context<Self>) -> bool {
        let current = state.text(id).to_string();
        let bounds_ok = |d: Date| {
            let iso = d.iso();
            let (lo, hi) = (min.as_deref().map(|m| &m[..10.min(m.len())]), max.as_deref().map(|m| &m[..10.min(m.len())]));
            lo.is_none_or(|m| iso.as_str() >= m) && hi.is_none_or(|m| iso.as_str() <= m)
        };
        match (key, is_open) {
            (FormKey::Enter | FormKey::Down, false) => {
                let (date, hour, minute) = if include_time {
                    DateTime::parse(&current).map(|d| (d.date, d.hour, d.minute)).unwrap_or((today_utc(), 9, 0))
                } else {
                    (Date::parse(&current).unwrap_or_else(today_utc), 0, 0)
                };
                self.calendars.insert(id.to_string(), Calendar { cursor: date, hour, minute, time_stage: None });
                self.open = Some(id.to_string());
                true
            }
            (FormKey::Up, false) => false,
            (FormKey::Escape, true) => {
                self.open = None;
                true
            }
            (k, true) => {
                let Some(cal) = self.calendars.get_mut(id) else { return false };
                match (k, cal.time_stage) {
                    (FormKey::Up | FormKey::Down, None) => cal.cursor = cal.cursor.add_days(if k == FormKey::Down { 7 } else { -7 }),
                    (FormKey::Up | FormKey::Down, Some(stage)) => {
                        let d = if k == FormKey::Up { 1 } else { -1 };
                        if stage == 0 {
                            cal.hour = (cal.hour as i32 + d).rem_euclid(24) as u32;
                        } else {
                            cal.minute = (cal.minute as i32 + d * 5).rem_euclid(60) as u32;
                        }
                    }
                    (FormKey::Enter, None) => {
                        if !bounds_ok(cal.cursor) {
                            return true; // outside min/max: not selectable
                        }
                        if include_time {
                            cal.time_stage = Some(0);
                        } else {
                            let iso = cal.cursor.iso();
                            self.open = None;
                            self.set(id, FieldValue::Text(iso), cx);
                        }
                    }
                    (FormKey::Enter, Some(0)) => cal.time_stage = Some(1),
                    (FormKey::Enter, Some(_)) => {
                        let v = DateTime { date: cal.cursor, hour: cal.hour, minute: cal.minute }.iso();
                        self.open = None;
                        self.set(id, FieldValue::Text(v), cx);
                    }
                    _ => {}
                }
                true
            }
            _ => false,
        }
    }

    /// Keys that are not actions in the launcher: characters, arrows, space, page keys.
    fn on_field_key(&mut self, id: &str, k: &str, key_char: Option<&str>, cx: &mut Context<Self>) {
        let Some((view, state)) = self.form(cx) else { return };
        let Some(field) = view.fields.iter().find(|f| f.id == id).cloned() else { return };
        let is_open = self.open.as_deref() == Some(id);
        match &field.kind {
            FieldKind::Checkbox { .. } if k == "space" => {
                let v = !state.bool(id);
                self.set(id, FieldValue::Bool(v), cx);
            }
            FieldKind::Dropdown { options } => {
                if k == "space" && !is_open {
                    self.open = Some(id.to_string());
                    let sel = options.iter().position(|o| o.value == state.text(id)).unwrap_or(0);
                    self.cursor.insert(id.to_string(), sel);
                } else if is_open {
                    // type-ahead: jump to the first option starting with the typed letter
                    if let Some(ch) = key_char.filter(|c| c.chars().count() == 1 && c.trim() != "") {
                        let ch = ch.to_lowercase();
                        if let Some(i) = options.iter().position(|o| o.title.to_lowercase().starts_with(&ch)) {
                            self.cursor.insert(id.to_string(), i);
                        }
                    }
                }
            }
            FieldKind::TagPicker { options } if !options.is_empty() => {
                let c = self.cursor.get(id).copied().unwrap_or(0);
                match k {
                    "left" => {
                        self.cursor.insert(id.to_string(), c.saturating_sub(1));
                    }
                    "right" => {
                        self.cursor.insert(id.to_string(), (c + 1).min(options.len() - 1));
                    }
                    "space" => self.toggle_tag(id, &options[c.min(options.len() - 1)].value, &state, cx),
                    _ => {}
                }
            }
            FieldKind::DatePicker { .. } => {
                if is_open {
                    if let Some(cal) = self.calendars.get_mut(id) {
                        match (k, cal.time_stage) {
                            ("left", None) => cal.cursor = cal.cursor.add_days(-1),
                            ("right", None) => cal.cursor = cal.cursor.add_days(1),
                            ("pageup", None) => cal.cursor = cal.cursor.add_months(-1),
                            ("pagedown", None) => cal.cursor = cal.cursor.add_months(1),
                            ("home", None) => cal.cursor = today_utc(),
                            ("left", Some(_)) => cal.time_stage = Some(0),
                            ("right", Some(_)) => cal.time_stage = Some(1),
                            _ => {}
                        }
                    }
                } else if k == "space" {
                    // open like Enter does
                    let mut dummy_window = None::<()>;
                    let _ = &mut dummy_window;
                    self.open = Some(id.to_string());
                    let (date, hour, minute) = match &field.kind {
                        FieldKind::DatePicker { include_time: true, .. } => DateTime::parse(state.text(id)).map(|d| (d.date, d.hour, d.minute)).unwrap_or((today_utc(), 9, 0)),
                        _ => (Date::parse(state.text(id)).unwrap_or_else(today_utc), 0, 0),
                    };
                    self.calendars.insert(id.to_string(), Calendar { cursor: date, hour, minute, time_stage: None });
                } else if k == "backspace" || k == "delete" {
                    self.set(id, FieldValue::Text(String::new()), cx);
                }
            }
            FieldKind::FilePicker { .. } => {
                let files = state.list(id).to_vec();
                let c = self.cursor.get(id).copied().unwrap_or(0);
                match k {
                    "left" => {
                        self.cursor.insert(id.to_string(), c.saturating_sub(1));
                    }
                    "right" if !files.is_empty() => {
                        self.cursor.insert(id.to_string(), (c + 1).min(files.len() - 1));
                    }
                    "delete" | "backspace" if !files.is_empty() => {
                        let mut f = files;
                        f.remove(c.min(f.len() - 1));
                        self.set(id, FieldValue::List(f), cx);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        cx.notify();
    }

    // ---------------------------------------------------------------- files

    fn add_typed_path(&mut self, field: &Field, typed: &str, state: &FormState, cx: &mut Context<Self>) {
        let FieldKind::FilePicker { multiple, directories, files } = field.kind else { return };
        let expanded = match typed.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(rest)).unwrap_or_else(|| typed.into()),
            None => typed.into(),
        };
        let note = if !expanded.is_absolute() {
            Some("Use an absolute path".to_string())
        } else if expanded.is_dir() && !directories {
            Some("That is a folder; this field takes files".to_string())
        } else if expanded.is_file() && !files {
            Some("That is a file; this field takes folders".to_string())
        } else if !expanded.exists() {
            Some("No such file or folder".to_string())
        } else {
            None
        };
        match note {
            Some(n) => {
                self.notes.insert(field.id.clone(), n);
            }
            None => {
                self.notes.remove(&field.id);
                let p = expanded.to_string_lossy().into_owned();
                let mut list = if multiple { state.list(&field.id).to_vec() } else { vec![] };
                if !list.contains(&p) {
                    list.push(p);
                }
                self.set(&field.id, FieldValue::List(list), cx);
                if let Some(i) = self.path_inputs.get(&field.id) {
                    i.update(cx, |i, cx| i.set_text("", cx));
                }
            }
        }
    }

    /// Opens the desktop's file chooser. The launcher is an overlay that would cover the dialog, so
    /// it hides while the dialog is open and comes back (with the form intact) when it closes.
    fn browse(&mut self, field: &Field, cx: &mut Context<Self>) {
        let FieldKind::FilePicker { multiple, directories, .. } = field.kind else { return };
        let title = field.title.clone().unwrap_or_else(|| "Choose".into());
        super::shell::pick_files(field.id.clone(), title, multiple, directories, cx);
    }

    // ------------------------------------------------------------ rendering

    fn box_style(focused: bool) -> gpui::Div {
        div()
            .w_full()
            .min_h(px(34.))
            .px_2p5()
            .py_1p5()
            .rounded_md()
            .border_1()
            .border_color(if focused { theme::accent() } else { theme::border() })
            .bg(theme::chip())
            .text_size(px(14.))
            .text_color(theme::text())
    }

    fn render_field(&mut self, index: usize, f: &Field, state: &FormState, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let id = f.id.clone();
        let error = state.error(&id).map(String::from);
        let is_open = self.open.as_deref() == Some(id.as_str());
        let core = self.core.clone();
        let fid = id.clone();

        let label = f.title.as_ref().map(|t| {
            div()
                .flex()
                .gap_1()
                .text_size(px(12.))
                .text_color(if focused { theme::text() } else { theme::text_dim() })
                .child(t.clone())
                .when(f.required, |d| d.child(div().text_color(theme::error()).child("*")))
        });

        let body: AnyElement = match &f.kind {
            FieldKind::Text | FieldKind::Password => Self::box_style(focused).child(self.texts[&id].clone()).into_any_element(),
            FieldKind::TextArea { .. } => Self::box_style(focused).child(self.areas[&id].clone()).into_any_element(),
            FieldKind::Checkbox { label } => {
                let on = state.bool(&id);
                let this = cx.entity();
                let fid2 = id.clone();
                div()
                    .id(("cb", index))
                    .track_focus(&self.handles[&id])
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(
                        div()
                            .size(px(18.))
                            .rounded_sm()
                            .border_1()
                            .border_color(if focused { theme::accent() } else { theme::border() })
                            .bg(if on { theme::accent() } else { theme::chip() })
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(on, |d| d.child(gpui::svg().path("icons/check.svg").size(px(14.)).text_color(gpui::white()))),
                    )
                    .child(div().text_size(px(14.)).text_color(theme::text()).child(label.clone()))
                    .on_click(move |_, _, cx| {
                        this.update(cx, |this, cx| {
                            let v = !this.form(cx).map(|(_, s)| s.bool(&fid2)).unwrap_or(false);
                            this.set(&fid2, FieldValue::Bool(v), cx);
                        })
                    })
                    .into_any_element()
            }
            FieldKind::Dropdown { options } => {
                let cur = options.iter().find(|o| o.value == state.text(&id));
                let this = cx.entity();
                let fid2 = id.clone();
                let header = Self::box_style(focused)
                    .id(("dd", index))
                    .track_focus(&self.handles[&id])
                    .flex()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .child(div().child(cur.map(|o| o.title.clone()).unwrap_or_else(|| f.placeholder.clone().unwrap_or_else(|| "Select…".into()))))
                    .child(div().text_color(theme::text_dim()).child(if is_open { "▴" } else { "▾" }))
                    .on_click(move |_, _, cx| {
                        this.update(cx, |this, cx| {
                            this.open = if this.open.as_deref() == Some(fid2.as_str()) { None } else { Some(fid2.clone()) };
                            cx.notify();
                        })
                    });
                let mut col = div().w_full().flex().flex_col().gap_1().child(header);
                if is_open {
                    let cursor = self.cursor.get(&id).copied().unwrap_or(0);
                    let mut list = div().w_full().max_h(px(180.)).overflow_hidden().rounded_md().border_1().border_color(theme::border()).bg(theme::panel_background()).flex().flex_col().py_1();
                    let mut last_section: Option<&Option<String>> = None;
                    for (i, o) in options.iter().enumerate() {
                        if last_section != Some(&o.section) {
                            if let Some(s) = &o.section {
                                list = list.child(div().px_2().pt_1().text_size(px(11.)).text_color(theme::text_faint()).child(s.clone()));
                            }
                            last_section = Some(&o.section);
                        }
                        let (this, fid3, val) = (cx.entity(), id.clone(), o.value.clone());
                        list = list.child(
                            div()
                                .id(("opt", index * 1000 + i))
                                .mx_1()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .text_size(px(13.))
                                .text_color(theme::text())
                                .cursor_pointer()
                                .when(i == cursor, |d| d.bg(theme::selected()))
                                .when(o.value == state.text(&id), |d| d.font_weight(FontWeight::BOLD))
                                .child(o.title.clone())
                                .on_click(move |_, _, cx| {
                                    this.update(cx, |this, cx| {
                                        this.set(&fid3, FieldValue::Text(val.clone()), cx);
                                        this.open = None;
                                        cx.notify();
                                    })
                                }),
                        );
                    }
                    col = col.child(list);
                }
                col.into_any_element()
            }
            FieldKind::TagPicker { options } => {
                let cursor = self.cursor.get(&id).copied().unwrap_or(0);
                let selected = state.list(&id).to_vec();
                let mut row = div().id(("tags", index)).track_focus(&self.handles[&id]).w_full().flex().flex_wrap().gap_1p5();
                for (i, o) in options.iter().enumerate() {
                    let on = selected.contains(&o.value);
                    let (this, fid3, val) = (cx.entity(), id.clone(), o.value.clone());
                    row = row.child(
                        div()
                            .id(("tag", index * 1000 + i))
                            .px_2p5()
                            .py_1()
                            .rounded_full()
                            .border_1()
                            .border_color(if focused && i == cursor { theme::accent() } else { theme::border() })
                            .bg(if on { theme::selection_text() } else { theme::chip() })
                            .text_size(px(13.))
                            .text_color(if on { theme::text() } else { theme::text_dim() })
                            .cursor_pointer()
                            .child(format!("{}{}", if on { "✓ " } else { "" }, o.title))
                            .on_click(move |_, _, cx| {
                                this.update(cx, |this, cx| {
                                    this.cursor.insert(fid3.clone(), i);
                                    if let Some((_, st)) = this.form(cx) {
                                        this.toggle_tag(&fid3, &val, &st, cx);
                                    }
                                })
                            }),
                    );
                }
                row.into_any_element()
            }
            FieldKind::DatePicker { include_time, min, max } => {
                let value = state.text(&id).to_string();
                let shown = if value.is_empty() { f.placeholder.clone().unwrap_or_else(|| if *include_time { "Pick a date and time".into() } else { "Pick a date".into() }) } else { value.replace('T', "  ") };
                let this = cx.entity();
                let (fid2, include_time2) = (id.clone(), *include_time);
                let header = Self::box_style(focused)
                    .id(("date", index))
                    .track_focus(&self.handles[&id])
                    .flex()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .child(div().text_color(if value.is_empty() { theme::text_faint() } else { theme::text() }).child(shown))
                    .child(gpui::svg().path("icons/calendar.svg").size(px(16.)).text_color(theme::text_dim()))
                    .on_click(move |_, _, cx| {
                        this.update(cx, |this, cx| {
                            if this.open.as_deref() == Some(fid2.as_str()) {
                                this.open = None;
                            } else {
                                let cur = this.form(cx).map(|(_, s)| s.text(&fid2).to_string()).unwrap_or_default();
                                let (date, hour, minute) = if include_time2 {
                                    DateTime::parse(&cur).map(|d| (d.date, d.hour, d.minute)).unwrap_or((today_utc(), 9, 0))
                                } else {
                                    (Date::parse(&cur).unwrap_or_else(today_utc), 0, 0)
                                };
                                this.calendars.insert(fid2.clone(), Calendar { cursor: date, hour, minute, time_stage: None });
                                this.open = Some(fid2.clone());
                            }
                            cx.notify();
                        })
                    });
                let mut col = div().w_full().flex().flex_col().gap_1().child(header);
                if is_open {
                    if let Some(cal) = self.calendars.get(&id).cloned() {
                        col = col.child(self.render_calendar(index, &id, &cal, *include_time, min.as_deref(), max.as_deref(), &value, cx));
                    }
                }
                col.into_any_element()
            }
            FieldKind::FilePicker { .. } => {
                let files = state.list(&id).to_vec();
                let cursor = self.cursor.get(&id).copied().unwrap_or(0);
                let mut chips = div().id(("files", index)).w_full().flex().flex_wrap().gap_1p5();
                for (i, p) in files.iter().enumerate() {
                    let (this, fid3, path) = (cx.entity(), id.clone(), p.clone());
                    let name = std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.clone());
                    chips = chips.child(
                        div()
                            .id(("file", index * 1000 + i))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(if focused && i == cursor { theme::accent() } else { theme::border() })
                            .bg(theme::chip())
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_size(px(13.))
                            .text_color(theme::text())
                            .child(name)
                            .child(
                                div().id(("rm", index * 1000 + i)).cursor_pointer().text_color(theme::text_dim()).child("✕").on_click(move |_, _, cx| {
                                    this.update(cx, |this, cx| {
                                        if let Some((_, st)) = this.form(cx) {
                                            let mut list = st.list(&fid3).to_vec();
                                            list.retain(|x| x != &path);
                                            this.set(&fid3, FieldValue::List(list), cx);
                                        }
                                    })
                                }),
                            ),
                    );
                }
                let (this, field) = (cx.entity(), f.clone());
                let browse = div()
                    .id(("browse", index))
                    .px_2p5()
                    .py_1()
                    .rounded_md()
                    .bg(theme::chip())
                    .text_size(px(12.))
                    .text_color(theme::text())
                    .cursor_pointer()
                    .child("Browse…")
                    .on_click(move |_, _, cx| this.update(cx, |this, cx| this.browse(&field, cx)));
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .when(!files.is_empty(), |d| d.child(chips))
                    .child(div().flex().gap_2().items_center().child(div().flex_1().child(Self::box_style(focused).child(self.path_inputs[&id].clone()))).child(browse))
                    .into_any_element()
            }
            FieldKind::Separator => div().w_full().h(px(1.)).my_1().bg(theme::divider()).into_any_element(),
            FieldKind::Description { text } => div().w_full().text_size(px(13.)).text_color(theme::text_dim()).child(text.clone()).into_any_element(),
        };

        let note = self.notes.get(&id).cloned();
        let id_for_key = id.clone();
        let interactive = f.kind.has_value();
        div()
            .id(("field", index))
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .py_1p5()
            .when(interactive, |d| {
                d.on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    core.update(cx, |c, cx| {
                        c.form_focus(&fid);
                        cx.notify();
                    })
                })
            })
            .on_key_down(cx.listener(move |this, ev: &KeyDownEvent, _, cx| this.on_field_key(&id_for_key, ev.keystroke.key.as_str(), ev.keystroke.key_char.as_deref(), cx)))
            .children(label)
            .child(body)
            .when_some(f.info.clone(), |d, i| d.child(div().text_size(px(11.)).text_color(theme::text_faint()).child(i)))
            .when_some(error, |d, e| d.child(div().text_size(px(12.)).text_color(theme::error()).child(e)))
            .when_some(note, |d, n| d.child(div().text_size(px(12.)).text_color(theme::text_dim()).child(n)))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_calendar(&self, index: usize, id: &str, cal: &Calendar, include_time: bool, min: Option<&str>, max: Option<&str>, value: &str, cx: &mut Context<Self>) -> AnyElement {
        let selected = value.get(..10).and_then(Date::parse);
        let today = today_utc();
        let month = cal.cursor;
        let mut grid = div().flex().flex_col().gap_0p5();
        grid = grid.child(
            div()
                .flex()
                .children(["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"].iter().map(|d| div().w(px(34.)).flex().justify_center().text_size(px(11.)).text_color(theme::text_faint()).child(*d))),
        );
        for (w, week) in month.month_grid().into_iter().enumerate() {
            let mut row = div().flex();
            for (d, day) in week.into_iter().enumerate() {
                let in_month = day.month == month.month;
                let iso = day.iso();
                let allowed = min.is_none_or(|m| iso.as_str() >= &m[..10.min(m.len())]) && max.is_none_or(|m| iso.as_str() <= &m[..10.min(m.len())]);
                let (this, fid, include_time2) = (cx.entity(), id.to_string(), include_time);
                row = row.child(
                    div()
                        .id(("day", index * 100 + w * 7 + d))
                        .w(px(34.))
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .text_size(px(13.))
                        .text_color(if !allowed { theme::text_faint() } else if in_month { theme::text() } else { theme::text_dim() })
                        .when(day == today, |d| d.border_1().border_color(theme::border()))
                        .when(Some(day) == selected, |d| d.bg(theme::selection_text()))
                        .when(day == cal.cursor && cal.time_stage.is_none(), |d| d.bg(theme::accent()).text_color(gpui::white()))
                        .when(allowed, |d| d.cursor_pointer())
                        .child(day.day.to_string())
                        .on_click(move |_, _, cx| {
                            if !allowed {
                                return;
                            }
                            this.update(cx, |this, cx| {
                                if let Some(c) = this.calendars.get_mut(&fid) {
                                    c.cursor = day;
                                    if include_time2 {
                                        c.time_stage = Some(0);
                                    }
                                }
                                if !include_time2 {
                                    this.set(&fid, FieldValue::Text(day.iso()), cx);
                                    this.open = None;
                                }
                                cx.notify();
                            })
                        }),
                );
            }
            grid = grid.child(row);
        }
        let (prev, next) = (cx.entity(), cx.entity());
        let (fid_p, fid_n) = (id.to_string(), id.to_string());
        let nav = |label: &'static str, key: (&'static str, usize)| div().id(key).px_2().py_0p5().rounded_sm().cursor_pointer().text_color(theme::text_dim()).child(label);
        let _ = days_in_month;
        let mut panel = div()
            .w(px(260.))
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::panel_background())
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(nav("‹", ("prev", index)).on_click(move |_, _, cx| {
                        prev.update(cx, |this, cx| {
                            if let Some(c) = this.calendars.get_mut(&fid_p) {
                                c.cursor = c.cursor.add_months(-1);
                            }
                            cx.notify();
                        })
                    }))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(format!("{} {}", month.month_name(), month.year)))
                    .child(nav("›", ("next", index)).on_click(move |_, _, cx| {
                        next.update(cx, |this, cx| {
                            if let Some(c) = this.calendars.get_mut(&fid_n) {
                                c.cursor = c.cursor.add_months(1);
                            }
                            cx.notify();
                        })
                    })),
            )
            .child(grid);
        if include_time {
            let cell = |v: String, active: bool| div().px_2().py_0p5().rounded_sm().text_size(px(14.)).when(active, |d| d.bg(theme::accent()).text_color(gpui::white())).child(v);
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_color(theme::text())
                    .child(div().text_size(px(12.)).text_color(theme::text_dim()).child("Time"))
                    .child(cell(format!("{:02}", cal.hour), cal.time_stage == Some(0)))
                    .child(":")
                    .child(cell(format!("{:02}", cal.minute), cal.time_stage == Some(1))),
            );
        }
        panel
            .child(div().text_size(px(11.)).text_color(theme::text_faint()).child(if include_time { "←→↑↓ move · PgUp/PgDn month · ↵ day, then time" } else { "←→↑↓ move · PgUp/PgDn month · ↵ select · Home today" }))
            .into_any_element()
    }
}

impl Focusable for FormUi {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // the launcher keeps its own focus; the form never steals it as a whole
        self.handles.values().next().cloned().unwrap_or_else(|| cx.focus_handle())
    }
}

impl Render for FormUi {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((view, state)) = self.form(cx) else { return div().into_any_element() };
        let focus = state.focus();
        // an open dropdown/calendar needs room below its field: bring the field to the top once
        match (&self.open, &self.scrolled_for) {
            (Some(id), seen) if seen.as_ref() != Some(id) => {
                if let Some(i) = view.fields.iter().position(|f| &f.id == id) {
                    self.scroll.scroll_to_top_of_item(i);
                }
                self.scrolled_for = Some(id.clone());
            }
            (None, Some(_)) => self.scrolled_for = None,
            _ => {}
        }
        if let Some((i, left)) = self.pending_scroll {
            self.scroll.scroll_to_item(i);
            self.pending_scroll = (left > 1).then_some((i, left - 1));
            cx.notify();
        }
        let popup_open = self.open.is_some();
        let fields: Vec<AnyElement> = view.fields.iter().enumerate().map(|(i, f)| self.render_field(i, f, &state, i == focus, cx)).collect();
        let _ = icons::available;
        div()
            .id("form-scroll")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px_5()
            .pt_2()
            .pb(px(if popup_open { 300. } else { 8. }))
            .flex()
            .flex_col()
            .children(fields)
            .into_any_element()
    }
}
