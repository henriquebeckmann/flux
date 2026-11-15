//! Multi-line text field (soft-wrapped), the widget behind `ui.Form.TextArea`. Same editing
//! model and IME support as [`TextInput`](super::text_input) (it shares [`TextBuffer`]); this file
//! is the multi-line layout: wrapping, vertical cursor movement, scrolling.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, GlobalElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString,
    Style, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine, actions, div, fill, point, prelude::*, px, relative, size,
};

use super::{text_buffer::TextBuffer, text_input::TextInputEvent, theme};

actions!(flux_text_area, [Newline, Up, Down, SelectUp, SelectDown, DocStart, DocEnd]);

pub const KEY_CONTEXT: &str = "TextArea";

pub fn bind_keys(cx: &mut App) {
    use super::text_input::{Backspace, Copy, Cut, Delete, DeleteToStart, DeleteWordLeft, DeleteWordRight, End, Home, Left, Paste, Right, SelectAll, SelectEnd, SelectHome, SelectLeft, SelectRight, SelectWordLeft, SelectWordRight, WordLeft, WordRight};
    let c = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("enter", Newline, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("shift-up", SelectUp, c),
        KeyBinding::new("shift-down", SelectDown, c),
        KeyBinding::new("ctrl-home", DocStart, c),
        KeyBinding::new("ctrl-end", DocEnd, c),
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("ctrl-left", WordLeft, c),
        KeyBinding::new("ctrl-right", WordRight, c),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, c),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, c),
        KeyBinding::new("ctrl-backspace", DeleteWordLeft, c),
        KeyBinding::new("ctrl-w", DeleteWordLeft, c),
        KeyBinding::new("ctrl-delete", DeleteWordRight, c),
        KeyBinding::new("ctrl-u", DeleteToStart, c),
        KeyBinding::new("ctrl-a", SelectAll, c),
        KeyBinding::new("home", Home, c),
        KeyBinding::new("end", End, c),
        KeyBinding::new("shift-home", SelectHome, c),
        KeyBinding::new("shift-end", SelectEnd, c),
        KeyBinding::new("ctrl-v", Paste, c),
        KeyBinding::new("shift-insert", Paste, c),
        KeyBinding::new("ctrl-c", Copy, c),
        KeyBinding::new("ctrl-x", Cut, c),
    ]);
}

/// One laid-out paragraph and where it starts in the buffer.
struct Para {
    line: WrappedLine,
    start: usize,
    y: Pixels,
}

pub struct TextArea {
    focus_handle: FocusHandle,
    buffer: TextBuffer,
    placeholder: SharedString,
    rows: u32,
    paras: Vec<Para>,
    line_height: Pixels,
    bounds: Option<Bounds<Pixels>>,
    scroll_y: Pixels,
    /// x to keep while moving vertically through lines of different lengths.
    goal_x: Option<Pixels>,
    is_selecting: bool,
}

impl EventEmitter<TextInputEvent> for TextArea {}

impl TextArea {
    pub fn new(placeholder: &str, rows: u32, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            buffer: TextBuffer::multiline(),
            placeholder: placeholder.to_string().into(),
            rows: rows.max(1),
            paras: vec![],
            line_height: px(20.),
            bounds: None,
            scroll_y: px(0.),
            goal_x: None,
            is_selecting: false,
        }
    }

    pub fn text(&self) -> &str {
        self.buffer.content()
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.buffer.content() != text {
            self.buffer.set_text(text);
            self.scroll_y = px(0.);
            cx.notify();
        }
    }

    pub fn set_placeholder(&mut self, p: &str, cx: &mut Context<Self>) {
        if self.placeholder.as_ref() != p {
            self.placeholder = p.to_string().into();
            cx.notify();
        }
    }

    pub fn set_rows(&mut self, rows: u32, cx: &mut Context<Self>) {
        if self.rows != rows.max(1) {
            self.rows = rows.max(1);
            cx.notify();
        }
    }

    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    fn moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn edit(&mut self, ok: bool, window: &mut Window, cx: &mut Context<Self>) {
        if ok { self.changed(window, cx) } else { window.play_system_bell() }
    }

    /// Position of byte `offset` relative to the text origin (scroll not applied).
    fn position_of(&self, offset: usize) -> Point<Pixels> {
        for p in self.paras.iter().rev() {
            if offset >= p.start {
                let local = (offset - p.start).min(p.line.len());
                let pos = p.line.position_for_index(local, self.line_height).unwrap_or_default();
                return point(pos.x, p.y + pos.y);
            }
        }
        point(px(0.), px(0.))
    }

    /// Byte offset closest to a point relative to the text origin.
    fn offset_at(&self, pt: Point<Pixels>) -> usize {
        let Some(p) = self.paras.iter().rev().find(|p| pt.y >= p.y).or(self.paras.first()) else { return 0 };
        let local = point(pt.x, (pt.y - p.y).max(px(0.)));
        let i = match p.line.closest_index_for_position(local, self.line_height) {
            Ok(i) | Err(i) => i,
        };
        p.start + i.min(p.line.len())
    }

    fn move_vertical(&mut self, delta: f32, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        let cur = self.position_of(self.buffer.cursor());
        let goal = *self.goal_x.get_or_insert(cur.x);
        let target = point(goal, cur.y + self.line_height * delta + self.line_height * 0.5);
        let total = self.paras.last().map(|p| p.y + p.line.size(self.line_height).height).unwrap_or_default();
        let to = if target.y < px(0.) {
            0
        } else if target.y >= total {
            self.buffer.content().len()
        } else {
            self.offset_at(target)
        };
        if extend { self.buffer.select_to(to) } else { self.buffer.move_to(to) }
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn mouse_offset(&self, position: Point<Pixels>) -> usize {
        let Some(b) = self.bounds else { return 0 };
        self.offset_at(point(position.x - b.left(), position.y - b.top() + self.scroll_y))
    }
}

macro_rules! simple_actions {
    ($($name:ident : $action:ty => $body:expr),* $(,)?) => {
        impl TextArea {
            $(fn $name(&mut self, _: &$action, w: &mut Window, cx: &mut Context<Self>) { #[allow(clippy::redundant_closure_call)] ($body)(self, w, cx) })*
        }
    };
}

use super::text_input as ti;

simple_actions! {
    left: ti::Left => |s: &mut Self, w, cx| { s.buffer.move_left(false); s.moved(w, cx) },
    right: ti::Right => |s: &mut Self, w, cx| { s.buffer.move_right(false); s.moved(w, cx) },
    select_left: ti::SelectLeft => |s: &mut Self, w, cx| { s.buffer.move_left(true); s.moved(w, cx) },
    select_right: ti::SelectRight => |s: &mut Self, w, cx| { s.buffer.move_right(true); s.moved(w, cx) },
    word_left: ti::WordLeft => |s: &mut Self, w, cx| { s.buffer.move_word_left(false); s.moved(w, cx) },
    word_right: ti::WordRight => |s: &mut Self, w, cx| { s.buffer.move_word_right(false); s.moved(w, cx) },
    select_word_left: ti::SelectWordLeft => |s: &mut Self, w, cx| { s.buffer.move_word_left(true); s.moved(w, cx) },
    select_word_right: ti::SelectWordRight => |s: &mut Self, w, cx| { s.buffer.move_word_right(true); s.moved(w, cx) },
    home: ti::Home => |s: &mut Self, w, cx| { s.buffer.home(false); s.moved(w, cx) },
    end: ti::End => |s: &mut Self, w, cx| { s.buffer.end(false); s.moved(w, cx) },
    select_home: ti::SelectHome => |s: &mut Self, w, cx| { s.buffer.home(true); s.moved(w, cx) },
    select_end: ti::SelectEnd => |s: &mut Self, w, cx| { s.buffer.end(true); s.moved(w, cx) },
    select_all: ti::SelectAll => |s: &mut Self, w, cx| { s.buffer.select_all(); s.moved(w, cx) },
    doc_start: DocStart => |s: &mut Self, w, cx| { s.buffer.move_to(0); s.moved(w, cx) },
    doc_end: DocEnd => |s: &mut Self, w, cx| { let n = s.buffer.content().len(); s.buffer.move_to(n); s.moved(w, cx) },
    up: Up => |s: &mut Self, w, cx| s.move_vertical(-1., false, w, cx),
    down: Down => |s: &mut Self, w, cx| s.move_vertical(1., false, w, cx),
    select_up: SelectUp => |s: &mut Self, w, cx| s.move_vertical(-1., true, w, cx),
    select_down: SelectDown => |s: &mut Self, w, cx| s.move_vertical(1., true, w, cx),
    backspace: ti::Backspace => |s: &mut Self, w, cx| { let ok = s.buffer.backspace(); s.edit(ok, w, cx) },
    delete: ti::Delete => |s: &mut Self, w, cx| { let ok = s.buffer.delete(); s.edit(ok, w, cx) },
    delete_word_left: ti::DeleteWordLeft => |s: &mut Self, w, cx| { let ok = s.buffer.delete_word_left(); s.edit(ok, w, cx) },
    delete_word_right: ti::DeleteWordRight => |s: &mut Self, w, cx| { let ok = s.buffer.delete_word_right(); s.edit(ok, w, cx) },
    delete_to_start: ti::DeleteToStart => |s: &mut Self, w, cx| { let ok = s.buffer.delete_to_start(); s.edit(ok, w, cx) },
    newline: Newline => |s: &mut Self, w, cx| { s.buffer.replace(None, "\n"); s.changed(w, cx) },
    paste: ti::Paste => |s: &mut Self, w, cx: &mut Context<Self>| {
        if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
            s.buffer.replace(None, &text);
            s.changed(w, cx);
        }
    },
    copy: ti::Copy => |s: &mut Self, _w, cx: &mut Context<Self>| {
        if !s.buffer.selected().is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(s.buffer.selected_text().to_string()));
        }
    },
    cut: ti::Cut => |s: &mut Self, w, cx: &mut Context<Self>| {
        if !s.buffer.selected().is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(s.buffer.selected_text().to_string()));
            s.buffer.replace(None, "");
            s.changed(w, cx);
        }
    },
}

impl TextArea {
    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = true;
        let idx = self.mouse_offset(event.position);
        if event.modifiers.shift { self.buffer.select_to(idx) } else { self.buffer.move_to(idx) }
        self.moved(window, cx);
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            let idx = self.mouse_offset(event.position);
            self.buffer.select_to(idx);
            self.moved(window, cx);
        }
    }
}

impl EntityInputHandler for TextArea {
    fn text_for_range(&mut self, r: Range<usize>, actual: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let range = self.buffer.range_from_utf16(&r);
        actual.replace(self.buffer.range_to_utf16(&range));
        Some(self.buffer.content()[range].to_string())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.buffer.range_to_utf16(&self.buffer.selected()), reversed: self.buffer.selection_reversed() })
    }

    fn set_selected_text_range(&mut self, r: Range<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let r = self.buffer.range_from_utf16(&r);
        self.buffer.move_to(r.start);
        self.buffer.select_to(r.end);
        self.moved(window, cx);
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.buffer.utf16_len())
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer.marked().map(|r| self.buffer.range_to_utf16(&r))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.unmark();
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn replace_text_in_range(&mut self, r: Option<Range<usize>>, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let range = r.map(|r| self.buffer.range_from_utf16(&r));
        self.buffer.replace(range, text);
        self.changed(window, cx);
    }

    fn replace_and_mark_text_in_range(&mut self, r: Option<Range<usize>>, text: &str, sel: Option<Range<usize>>, window: &mut Window, cx: &mut Context<Self>) {
        let range = r.map(|r| self.buffer.range_from_utf16(&r));
        self.buffer.replace_and_mark(range, text, sel);
        self.changed(window, cx);
    }

    fn bounds_for_range(&mut self, r: Range<usize>, bounds: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let r = self.buffer.range_from_utf16(&r);
        let (a, b) = (self.position_of(r.start), self.position_of(r.end));
        let top = bounds.top() - self.scroll_y;
        Some(Bounds::from_corners(point(bounds.left() + a.x, top + a.y), point(bounds.left() + b.x.max(a.x + px(1.)), top + b.y + self.line_height)))
    }

    fn selection_bounds_for_range(&mut self, r: Range<usize>, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> Vec<Bounds<Pixels>> {
        self.bounds_for_range(r, bounds, window, cx).into_iter().collect()
    }

    fn character_index_for_point(&mut self, p: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let b = self.bounds?;
        b.localize(&p)?;
        Some(self.buffer.offset_to_utf16(self.mouse_offset(p)))
    }
}

struct AreaElement {
    input: Entity<TextArea>,
}

struct Prepaint {
    paras: Vec<Para>,
    line_height: Pixels,
    cursor: Point<Pixels>,
    scroll_y: Pixels,
}

impl IntoElement for AreaElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for AreaElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let rows = self.input.read(cx).rows;
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (window.line_height() * rows as f32).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> Prepaint {
        let input = self.input.read(cx);
        let buf = &input.buffer;
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let (text, color) = if buf.is_empty() { (input.placeholder.to_string(), theme::text_faint()) } else { (buf.content().to_string(), theme::text()) };

        // Paragraph by paragraph, so each keeps its byte offset; runs carry selection and IME marks.
        let sel = buf.selected();
        let marked = buf.marked();
        let mut paras = Vec::new();
        let (mut start, mut y) = (0usize, px(0.));
        for piece in text.split('\n') {
            let end = start + piece.len();
            let mut cuts = vec![0usize, piece.len()];
            if !buf.is_empty() {
                for r in [Some(sel.clone()), marked.clone()].into_iter().flatten() {
                    for p in [r.start, r.end] {
                        if p > start && p < end {
                            cuts.push(p - start);
                        }
                    }
                }
            }
            cuts.sort_unstable();
            cuts.dedup();
            let runs: Vec<TextRun> = cuts
                .windows(2)
                .filter(|w| w[1] > w[0])
                .map(|w| {
                    let (a, b) = (start + w[0], start + w[1]);
                    let selected = !buf.is_empty() && sel.start < b && sel.end > a;
                    let is_marked = marked.as_ref().is_some_and(|m| m.start < b && m.end > a);
                    TextRun {
                        len: w[1] - w[0],
                        font: style.font(),
                        color: color.into(),
                        background_color: selected.then(|| theme::selection_text().into()),
                        underline: is_marked.then(|| UnderlineStyle { color: Some(color.into()), thickness: px(1.), wavy: false }),
                        strikethrough: None,
                    }
                })
                .collect();
            let line = window
                .text_system()
                .shape_text(piece.to_string().into(), font_size, &runs, Some(bounds.size.width), None)
                .ok()
                .and_then(|mut v| if v.is_empty() { None } else { Some(v.remove(0)) })
                .unwrap_or_default();
            let h = line.size(line_height).height.max(line_height);
            paras.push(Para { line, start, y });
            y += h;
            start = end + 1;
        }

        // Cursor, and scrolling that keeps it visible.
        let cursor_off = if buf.is_empty() { 0 } else { buf.cursor() };
        let cursor = paras
            .iter()
            .rev()
            .find(|p| cursor_off >= p.start)
            .map(|p| {
                let pos = p.line.position_for_index((cursor_off - p.start).min(p.line.len()), line_height).unwrap_or_default();
                point(pos.x, p.y + pos.y)
            })
            .unwrap_or_default();
        let mut scroll_y = input.scroll_y;
        if cursor.y < scroll_y {
            scroll_y = cursor.y;
        }
        if cursor.y + line_height > scroll_y + bounds.size.height {
            scroll_y = cursor.y + line_height - bounds.size.height;
        }
        if y <= bounds.size.height {
            scroll_y = px(0.);
        }
        Prepaint { paras, line_height, cursor, scroll_y }
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), pre: &mut Prepaint, window: &mut Window, cx: &mut App) {
        let focus = self.input.read(cx).focus_handle.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.input.clone()), cx);
        let (lh, scroll_y) = (pre.line_height, pre.scroll_y);
        let paras = std::mem::take(&mut pre.paras);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for p in &paras {
                let origin = point(bounds.left(), bounds.top() + p.y - scroll_y);
                if origin.y + p.line.size(lh).height < bounds.top() || origin.y > bounds.bottom() {
                    continue;
                }
                let _ = p.line.paint_background(origin, lh, gpui::TextAlign::Left, Some(bounds), window, cx);
                let _ = p.line.paint(origin, lh, gpui::TextAlign::Left, Some(bounds), window, cx);
            }
            if focus.is_focused(window) {
                let c = point(bounds.left() + pre.cursor.x, bounds.top() + pre.cursor.y - scroll_y);
                window.paint_quad(fill(Bounds::new(c, size(px(2.), lh)), theme::accent()));
            }
        });
        self.input.update(cx, |i, _| {
            i.paras = paras;
            i.line_height = lh;
            i.bounds = Some(bounds);
            i.scroll_y = scroll_y;
        });
    }
}

impl Render for TextArea {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_full()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::delete_to_start))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(AreaElement { input: cx.entity() })
    }
}

impl Focusable for TextArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
