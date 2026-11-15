//! Single-line text field built on GPUI's input abstractions (adapted from GPUI's
//! `examples/input.rs`, Apache-2.0). It implements [`EntityInputHandler`], which is what makes
//! IME composition, dead keys and the platform's text services work. Editing logic lives in
//! [`TextBuffer`]; this file is the GPUI glue: actions, mouse, painting.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div,
    fill, point, prelude::*, px, relative, size,
};

use super::{text_buffer::TextBuffer, theme};

actions!(
    flux_text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteToStart,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        Paste,
        Cut,
        Copy,
    ]
);

pub const KEY_CONTEXT: &str = "TextInput";
/// Same editing keys, but Left/Right are left to the screen (grid navigation).
pub const KEY_CONTEXT_GRID: &str = "TextInputGrid";

pub fn bind_keys(cx: &mut App) {
    for (context, arrows) in [(KEY_CONTEXT, true), (KEY_CONTEXT_GRID, false)] {
        let c = Some(context);
        cx.bind_keys([
            KeyBinding::new("backspace", Backspace, c),
            KeyBinding::new("delete", Delete, c),
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
        if arrows {
            cx.bind_keys([
                KeyBinding::new("left", Left, c),
                KeyBinding::new("right", Right, c),
                KeyBinding::new("shift-left", SelectLeft, c),
                KeyBinding::new("shift-right", SelectRight, c),
            ]);
        }
    }
}

pub enum TextInputEvent {
    /// The user changed the text (typing, IME, paste, delete…). Not emitted for `set_text`.
    Changed,
}

pub struct TextInput {
    focus_handle: FocusHandle,
    buffer: TextBuffer,
    placeholder: SharedString,
    secure: bool,
    context: &'static str,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    scroll_x: Pixels,
    is_selecting: bool,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl TextInput {
    pub fn new(placeholder: &str, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            buffer: TextBuffer::default(),
            placeholder: placeholder.to_string().into(),
            secure: false,
            context: KEY_CONTEXT,
            last_layout: None,
            last_bounds: None,
            scroll_x: px(0.),
            is_selecting: false,
        }
    }

    pub fn text(&self) -> &str {
        self.buffer.content()
    }

    /// Programmatic change (does not emit `Changed`).
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buffer.set_text(text);
        self.scroll_x = px(0.);
        cx.notify();
    }

    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.buffer.select_all();
        cx.notify();
    }

    pub fn set_placeholder(&mut self, placeholder: &str, cx: &mut Context<Self>) {
        if self.placeholder.as_ref() != placeholder {
            self.placeholder = placeholder.to_string().into();
            cx.notify();
        }
    }

    /// Grid screens take the arrow keys for themselves.
    pub fn set_arrows_navigate(&mut self, on: bool, cx: &mut Context<Self>) {
        let c = if on { KEY_CONTEXT_GRID } else { KEY_CONTEXT };
        if self.context != c {
            self.context = c;
            cx.notify();
        }
    }

    pub fn set_secure(&mut self, secure: bool, cx: &mut Context<Self>) {
        if self.secure != secure {
            self.secure = secure;
            cx.notify();
        }
    }

    // ------------------------------------------------------------ helpers

    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.notify_text_input_changed(&self.focus_handle);
        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    fn selection_moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn edit(&mut self, ok: bool, window: &mut Window, cx: &mut Context<Self>) {
        if ok {
            self.changed(window, cx);
        } else {
            window.play_system_bell();
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.buffer.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref()) else { return 0 };
        let idx = line.closest_index_for_x(position.x - bounds.left() + self.scroll_x);
        if self.secure { self.buffer.from_masked_index(idx) } else { idx }
    }

    // ------------------------------------------------------------ actions

    fn left(&mut self, _: &Left, w: &mut Window, cx: &mut Context<Self>) { self.buffer.move_left(false); self.selection_moved(w, cx) }
    fn right(&mut self, _: &Right, w: &mut Window, cx: &mut Context<Self>) { self.buffer.move_right(false); self.selection_moved(w, cx) }
    fn select_left(&mut self, _: &SelectLeft, w: &mut Window, cx: &mut Context<Self>) { self.buffer.move_left(true); self.selection_moved(w, cx) }
    fn select_right(&mut self, _: &SelectRight, w: &mut Window, cx: &mut Context<Self>) { self.buffer.move_right(true); self.selection_moved(w, cx) }
    fn word_left(&mut self, _: &WordLeft, w: &mut Window, cx: &mut Context<Self>) { self.buffer.move_word_left(false); self.selection_moved(w, cx) }
    fn word_right(&mut self, _: &WordRight, w: &mut Window, cx: &mut Context<Self>) { self.buffer.move_word_right(false); self.selection_moved(w, cx) }
    fn select_word_left(&mut self, _: &SelectWordLeft, w: &mut Window, cx: &mut Context<Self>) { self.buffer.move_word_left(true); self.selection_moved(w, cx) }
    fn select_word_right(&mut self, _: &SelectWordRight, w: &mut Window, cx: &mut Context<Self>) { self.buffer.move_word_right(true); self.selection_moved(w, cx) }
    fn home(&mut self, _: &Home, w: &mut Window, cx: &mut Context<Self>) { self.buffer.home(false); self.selection_moved(w, cx) }
    fn end(&mut self, _: &End, w: &mut Window, cx: &mut Context<Self>) { self.buffer.end(false); self.selection_moved(w, cx) }
    fn select_home(&mut self, _: &SelectHome, w: &mut Window, cx: &mut Context<Self>) { self.buffer.home(true); self.selection_moved(w, cx) }
    fn select_end(&mut self, _: &SelectEnd, w: &mut Window, cx: &mut Context<Self>) { self.buffer.end(true); self.selection_moved(w, cx) }
    fn select_all(&mut self, _: &SelectAll, w: &mut Window, cx: &mut Context<Self>) { self.buffer.select_all(); self.selection_moved(w, cx) }

    fn backspace(&mut self, _: &Backspace, w: &mut Window, cx: &mut Context<Self>) { let ok = self.buffer.backspace(); self.edit(ok, w, cx) }
    fn delete(&mut self, _: &Delete, w: &mut Window, cx: &mut Context<Self>) { let ok = self.buffer.delete(); self.edit(ok, w, cx) }
    fn delete_word_left(&mut self, _: &DeleteWordLeft, w: &mut Window, cx: &mut Context<Self>) { let ok = self.buffer.delete_word_left(); self.edit(ok, w, cx) }
    fn delete_word_right(&mut self, _: &DeleteWordRight, w: &mut Window, cx: &mut Context<Self>) { let ok = self.buffer.delete_word_right(); self.edit(ok, w, cx) }
    fn delete_to_start(&mut self, _: &DeleteToStart, w: &mut Window, cx: &mut Context<Self>) { let ok = self.buffer.delete_to_start(); self.edit(ok, w, cx) }

    fn paste(&mut self, _: &Paste, w: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.buffer.replace(None, &text);
            self.changed(w, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        // Never copy the contents of a password field.
        if !self.secure && !self.buffer.selected().is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.buffer.selected_text().to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, w: &mut Window, cx: &mut Context<Self>) {
        if !self.secure && !self.buffer.selected().is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.buffer.selected_text().to_string()));
            self.buffer.replace(None, "");
            self.changed(w, cx);
        }
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = true;
        window.request_virtual_keyboard();
        let idx = self.index_for_mouse_position(event.position);
        if event.modifiers.shift { self.buffer.select_to(idx) } else { self.buffer.move_to(idx) }
        self.selection_moved(window, cx);
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            let idx = self.index_for_mouse_position(event.position);
            self.buffer.select_to(idx);
            self.selection_moved(window, cx);
        }
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(&mut self, range_utf16: Range<usize>, actual_range: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let range = self.buffer.range_from_utf16(&range_utf16);
        actual_range.replace(self.buffer.range_to_utf16(&range));
        Some(self.buffer.content()[range].to_string())
    }

    fn selected_text_range(&mut self, _ignore_disabled_input: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.buffer.range_to_utf16(&self.buffer.selected()), reversed: self.buffer.selection_reversed() })
    }

    fn set_selected_text_range(&mut self, range_utf16: Range<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let r = self.buffer.range_from_utf16(&range_utf16);
        self.buffer.move_to(r.start);
        self.buffer.select_to(r.end);
        self.selection_moved(window, cx);
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

    fn replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let range = range_utf16.map(|r| self.buffer.range_from_utf16(&r));
        self.buffer.replace(range, new_text);
        self.changed(window, cx);
    }

    fn replace_and_mark_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, new_selected_range_utf16: Option<Range<usize>>, window: &mut Window, cx: &mut Context<Self>) {
        let range = range_utf16.map(|r| self.buffer.range_from_utf16(&r));
        self.buffer.replace_and_mark(range, new_text, new_selected_range_utf16);
        self.changed(window, cx);
    }

    fn bounds_for_range(&mut self, range_utf16: Range<usize>, bounds: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let line = self.last_layout.as_ref()?;
        let r = self.buffer.range_from_utf16(&range_utf16);
        let (s, e) = if self.secure { (self.buffer.to_masked_index(r.start), self.buffer.to_masked_index(r.end)) } else { (r.start, r.end) };
        Some(Bounds::from_corners(
            point(bounds.left() + line.x_for_index(s) - self.scroll_x, bounds.top()),
            point(bounds.left() + line.x_for_index(e) - self.scroll_x, bounds.bottom()),
        ))
    }

    fn selection_bounds_for_range(&mut self, range_utf16: Range<usize>, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> Vec<Bounds<Pixels>> {
        self.bounds_for_range(range_utf16, bounds, window, cx).into_iter().collect()
    }

    fn character_index_for_point(&mut self, point: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let line_point = self.last_bounds?.localize(&point)?;
        let line = self.last_layout.as_ref()?;
        let idx = line.closest_index_for_x(line_point.x + self.scroll_x);
        let idx = if self.secure { self.buffer.from_masked_index(idx) } else { idx };
        Some(self.buffer.offset_to_utf16(idx))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    scroll_x: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _: &mut Self::RequestLayoutState, window: &mut Window, cx: &mut App) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let buf = &input.buffer;
        let style = window.text_style();

        let (display, color) = if buf.is_empty() {
            (input.placeholder.to_string(), theme::text_faint())
        } else if input.secure {
            (buf.masked(), theme::text())
        } else {
            (buf.content().to_string(), theme::text())
        };
        let map = |i: usize| if input.secure { buf.to_masked_index(i) } else { i };

        let run = TextRun { len: display.len(), font: style.font(), color: color.into(), background_color: None, underline: None, strikethrough: None };
        let runs = match buf.marked().filter(|_| !input.secure && !buf.is_empty()) {
            Some(m) => vec![
                TextRun { len: m.start, ..run.clone() },
                TextRun { len: m.end - m.start, underline: Some(UnderlineStyle { color: Some(run.color), thickness: px(1.0), wavy: false }), ..run.clone() },
                TextRun { len: display.len() - m.end, ..run },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            None => vec![run],
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window.text_system().shape_line(display.into(), font_size, &runs, None);

        // Keep the cursor visible when the text is wider than the field.
        let cursor_x = if buf.is_empty() { px(0.) } else { line.x_for_index(map(buf.cursor())) };
        let mut scroll_x = input.scroll_x;
        let width = bounds.size.width;
        if cursor_x - scroll_x > width - px(2.) {
            scroll_x = cursor_x - width + px(2.);
        }
        if cursor_x < scroll_x {
            scroll_x = cursor_x;
        }
        if line.width <= width {
            scroll_x = px(0.);
        }

        let sel = buf.selected();
        let (selection, cursor) = if sel.is_empty() {
            (None, Some(fill(Bounds::new(point(bounds.left() + cursor_x - scroll_x, bounds.top()), size(px(2.), bounds.size.height)), theme::accent())))
        } else {
            let (s, e) = (line.x_for_index(map(sel.start)), line.x_for_index(map(sel.end)));
            (Some(fill(Bounds::from_corners(point(bounds.left() + s - scroll_x, bounds.top()), point(bounds.left() + e - scroll_x, bounds.bottom())), theme::selection_text())), None)
        };
        PrepaintState { line: Some(line), cursor, selection, scroll_x }
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _: &mut Self::RequestLayoutState, prepaint: &mut Self::PrepaintState, window: &mut Window, cx: &mut App) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.input.clone()), cx);

        let line = prepaint.line.take().unwrap();
        let scroll_x = prepaint.scroll_x;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(selection) = prepaint.selection.take() {
                window.paint_quad(selection);
            }
            line.paint(point(bounds.origin.x - scroll_x, bounds.origin.y), window.line_height(), gpui::TextAlign::Left, None, window, cx).unwrap();
            if focus_handle.is_focused(window) && let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        });

        self.input.update(cx, |input, _| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
            input.scroll_x = scroll_x;
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .w_full()
            .key_context(self.context)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::delete_to_start))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
