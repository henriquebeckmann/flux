//! Text editing model (single-line by default, optionally multi-line): content, selection, IME marked text.
//!
//! Pure Rust (no GPUI) so Unicode and IME behaviour can be unit-tested. All offsets are
//! UTF-8 byte offsets on grapheme boundaries unless a method says UTF-16 (the IME protocol).

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

pub const MASK_CHAR: &str = "•";

#[derive(Debug, Default, Clone)]
pub struct TextBuffer {
    content: String,
    selected: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    /// Keeps line breaks (text areas); otherwise they become spaces.
    multiline: bool,
}

impl TextBuffer {
    pub fn multiline() -> Self {
        Self { multiline: true, ..Default::default() }
    }

    pub fn is_multiline(&self) -> bool {
        self.multiline
    }

    /// Byte range of the line containing `offset` (without its line break).
    pub fn line_range(&self, offset: usize) -> Range<usize> {
        let o = offset.min(self.content.len());
        let start = self.content[..o].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let end = self.content[o..].find('\n').map(|i| o + i).unwrap_or(self.content.len());
        start..end
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    pub fn selected(&self) -> Range<usize> {
        self.selected.clone()
    }

    pub fn selection_reversed(&self) -> bool {
        self.reversed
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub fn selected_text(&self) -> &str {
        &self.content[self.selected.clone()]
    }

    pub fn cursor(&self) -> usize {
        if self.reversed { self.selected.start } else { self.selected.end }
    }

    /// Replaces everything (programmatic change); the cursor goes to the end.
    pub fn set_text(&mut self, text: &str) {
        self.content = self.clean(text);
        let end = self.content.len();
        self.selected = end..end;
        self.reversed = false;
        self.marked = None;
    }

    // ----------------------------------------------------------- boundaries

    pub fn prev_boundary(&self, offset: usize) -> usize {
        self.content.grapheme_indices(true).rev().find_map(|(i, _)| (i < offset).then_some(i)).unwrap_or(0)
    }

    pub fn next_boundary(&self, offset: usize) -> usize {
        self.content.grapheme_indices(true).find_map(|(i, _)| (i > offset).then_some(i)).unwrap_or(self.content.len())
    }

    /// Start of the previous word (skipping punctuation/space), or 0.
    pub fn prev_word_boundary(&self, offset: usize) -> usize {
        self.content.unicode_word_indices().rev().find_map(|(i, _)| (i < offset).then_some(i)).unwrap_or(0)
    }

    /// End of the next word, or the end of the text.
    pub fn next_word_boundary(&self, offset: usize) -> usize {
        self.content
            .unicode_word_indices()
            .find_map(|(i, w)| (i + w.len() > offset).then_some(i + w.len()))
            .unwrap_or(self.content.len())
    }

    // ------------------------------------------------------------ selection

    pub fn move_to(&mut self, offset: usize) {
        let o = offset.min(self.content.len());
        self.selected = o..o;
        self.reversed = false;
    }

    pub fn select_to(&mut self, offset: usize) {
        let o = offset.min(self.content.len());
        if self.reversed {
            self.selected.start = o;
        } else {
            self.selected.end = o;
        }
        if self.selected.end < self.selected.start {
            self.reversed = !self.reversed;
            self.selected = self.selected.end..self.selected.start;
        }
    }

    pub fn select_all(&mut self) {
        self.move_to(0);
        self.select_to(self.content.len());
    }

    fn go(&mut self, offset: usize, extend: bool) {
        if extend { self.select_to(offset) } else { self.move_to(offset) }
    }

    pub fn move_left(&mut self, extend: bool) {
        if !extend && !self.selected.is_empty() {
            return self.move_to(self.selected.start);
        }
        self.go(self.prev_boundary(self.cursor()), extend);
    }

    pub fn move_right(&mut self, extend: bool) {
        if !extend && !self.selected.is_empty() {
            return self.move_to(self.selected.end);
        }
        self.go(self.next_boundary(self.cursor()), extend);
    }

    pub fn move_word_left(&mut self, extend: bool) {
        self.go(self.prev_word_boundary(self.cursor()), extend);
    }

    pub fn move_word_right(&mut self, extend: bool) {
        self.go(self.next_word_boundary(self.cursor()), extend);
    }

    pub fn home(&mut self, extend: bool) {
        let to = if self.multiline { self.line_range(self.cursor()).start } else { 0 };
        self.go(to, extend);
    }

    pub fn end(&mut self, extend: bool) {
        let to = if self.multiline { self.line_range(self.cursor()).end } else { self.content.len() };
        self.go(to, extend);
    }

    // -------------------------------------------------------------- editing

    /// Inserts `text` over `range` (default: marked text, else the selection) and puts the
    /// cursor after it. Newlines become spaces (single-line field).
    pub fn replace(&mut self, range: Option<Range<usize>>, text: &str) {
        let text = self.clean(text);
        let range = self.clamp(range.or_else(|| self.marked.clone()).unwrap_or_else(|| self.selected.clone()));
        self.content.replace_range(range.clone(), &text);
        let end = range.start + text.len();
        self.selected = end..end;
        self.reversed = false;
        self.marked = None;
    }

    /// IME composition: replaces like [`replace`](Self::replace) but keeps the text "marked"
    /// (underlined, still being composed). `new_selection_utf16` is relative to the inserted text.
    pub fn replace_and_mark(&mut self, range: Option<Range<usize>>, text: &str, new_selection_utf16: Option<Range<usize>>) {
        let text = self.clean(text);
        let range = self.clamp(range.or_else(|| self.marked.clone()).unwrap_or_else(|| self.selected.clone()));
        self.content.replace_range(range.clone(), &text);
        self.marked = (!text.is_empty()).then(|| range.start..range.start + text.len());
        let start16 = self.offset_to_utf16(range.start);
        let len16 = text.encode_utf16().count();
        self.selected = match new_selection_utf16 {
            Some(sel) => self.range_from_utf16(&(start16 + sel.start.min(len16)..start16 + sel.end.min(len16))),
            None => range.start + text.len()..range.start + text.len(),
        };
        self.reversed = false;
    }

    pub fn unmark(&mut self) {
        self.marked = None;
    }

    /// Backspace. Returns false if nothing could be deleted.
    pub fn backspace(&mut self) -> bool {
        if self.selected.is_empty() {
            let prev = self.prev_boundary(self.cursor());
            if prev == self.cursor() {
                return false;
            }
            self.select_to(prev);
        }
        self.replace(None, "");
        true
    }

    pub fn delete(&mut self) -> bool {
        if self.selected.is_empty() {
            let next = self.next_boundary(self.cursor());
            if next == self.cursor() {
                return false;
            }
            self.select_to(next);
        }
        self.replace(None, "");
        true
    }

    pub fn delete_word_left(&mut self) -> bool {
        if self.selected.is_empty() {
            let prev = self.prev_word_boundary(self.cursor());
            if prev == self.cursor() {
                return false;
            }
            self.select_to(prev);
        }
        self.replace(None, "");
        true
    }

    pub fn delete_word_right(&mut self) -> bool {
        if self.selected.is_empty() {
            let next = self.next_word_boundary(self.cursor());
            if next == self.cursor() {
                return false;
            }
            self.select_to(next);
        }
        self.replace(None, "");
        true
    }

    /// Ctrl+U: delete everything before the cursor (or the selection).
    pub fn delete_to_start(&mut self) -> bool {
        if self.selected.is_empty() {
            let start = if self.multiline { self.line_range(self.cursor()).start } else { 0 };
            if self.cursor() == start {
                return false;
            }
            self.select_to(start);
        }
        self.replace(None, "");
        true
    }

    fn clamp(&self, r: Range<usize>) -> Range<usize> {
        let len = self.content.len();
        let (mut s, mut e) = (r.start.min(len), r.end.min(len));
        if s > e {
            std::mem::swap(&mut s, &mut e);
        }
        let empty = s == e;
        // Never split a UTF-8 sequence: an empty range snaps back to a boundary (pure insertion),
        // a non-empty one widens to whole characters.
        while !self.content.is_char_boundary(s) { s -= 1 }
        if empty {
            return s..s;
        }
        while !self.content.is_char_boundary(e) { e += 1 }
        s..e
    }

    // ---------------------------------------------------------------- UTF-16

    pub fn offset_from_utf16(&self, offset: usize) -> usize {
        let (mut utf8, mut utf16) = (0, 0);
        for ch in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    pub fn offset_to_utf16(&self, offset: usize) -> usize {
        let (mut utf16, mut utf8) = (0, 0);
        for ch in self.content.chars() {
            if utf8 >= offset {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    pub fn range_to_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(r.start)..self.offset_to_utf16(r.end)
    }

    pub fn range_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(r.start)..self.offset_from_utf16(r.end)
    }

    pub fn utf16_len(&self) -> usize {
        self.content.encode_utf16().count()
    }

    // --------------------------------------------------------------- masking

    /// The text shown for password fields: one bullet per grapheme.
    pub fn masked(&self) -> String {
        MASK_CHAR.repeat(self.content.graphemes(true).count())
    }

    pub fn to_masked_index(&self, idx: usize) -> usize {
        self.content[..idx.min(self.content.len())].graphemes(true).count() * MASK_CHAR.len()
    }

    pub fn from_masked_index(&self, masked: usize) -> usize {
        let n = masked / MASK_CHAR.len();
        self.content.grapheme_indices(true).nth(n).map(|(i, _)| i).unwrap_or(self.content.len())
    }
}

impl TextBuffer {
    /// Single-line fields turn line breaks and tabs into spaces; text areas keep `\n`
    /// (`\r\n` and `\r` are normalised) and expand tabs.
    fn clean(&self, text: &str) -> String {
        if !self.multiline {
            return text.chars().map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c }).collect();
        }
        text.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &str) -> TextBuffer {
        let mut b = TextBuffer::default();
        b.set_text(s);
        b
    }

    #[test]
    fn multiline_keeps_line_breaks_and_moves_by_line() {
        let mut b = TextBuffer::multiline();
        b.replace(None, "one\r\ntwo\tx\rthree");
        assert_eq!(b.content(), "one\ntwo    x\nthree");
        b.move_to(6); // inside "two"
        b.home(false);
        assert_eq!(b.cursor(), 4);
        b.end(false);
        assert_eq!(b.cursor(), 12);
        assert_eq!(b.line_range(13), 13..18);
        assert_eq!(b.line_range(3), 0..3);
        b.move_to(8);
        assert!(b.delete_to_start(), "Ctrl+U deletes to the start of the *line*");
        assert_eq!(b.content(), "one\n   x\nthree");
        // single-line buffers still flatten everything
        let mut s = TextBuffer::default();
        s.replace(None, "a\nb\tc");
        assert_eq!(s.content(), "a b c");
    }

    #[test]
    fn typing_inserts_at_the_cursor_anywhere() {
        let mut b = buf("held");
        b.move_to(2);
        b.replace(None, "l");
        b.move_to(b.content().len());
        b.replace(None, "o");
        assert_eq!(b.content(), "helldo");
        b.move_to(2);
        b.replace(None, "XY");
        assert_eq!((b.content(), b.cursor()), ("heXYlldo", 4));
    }

    #[test]
    fn grapheme_clusters_are_edited_as_one_unit() {
        // "é" as e + combining acute, a flag (two regional indicators), a ZWJ family emoji
        let mut b = buf("e\u{301}🇧🇷👨‍👩‍👧x");
        b.move_to(0);
        b.move_right(false);
        assert_eq!(b.cursor(), "e\u{301}".len(), "combining mark stays with its base");
        b.move_right(false);
        assert_eq!(b.cursor(), "e\u{301}🇧🇷".len(), "flag is one grapheme");
        b.end(false);
        assert!(b.backspace());
        assert!(b.backspace());
        assert_eq!(b.content(), "e\u{301}🇧🇷", "the whole family emoji is removed at once");
        b.move_to(0);
        assert!(b.delete());
        assert_eq!(b.content(), "🇧🇷");
    }

    #[test]
    fn selection_extends_both_ways_and_replace_overwrites_it() {
        let mut b = buf("hello world");
        b.move_to(5);
        b.move_left(true);
        b.move_left(true);
        assert_eq!((b.selected(), b.selection_reversed(), b.selected_text()), (3..5, true, "lo"));
        b.move_right(true);
        b.move_right(true);
        b.move_right(true);
        assert_eq!((b.selected(), b.selection_reversed()), (5..6, false));
        b.select_all();
        assert_eq!(b.selected_text(), "hello world");
        b.replace(None, "x");
        assert_eq!((b.content(), b.cursor()), ("x", 1));
    }

    #[test]
    fn arrows_collapse_a_selection_to_its_edge() {
        let mut b = buf("abcdef");
        b.move_to(1);
        b.select_to(4);
        b.move_left(false);
        assert_eq!(b.selected(), 1..1);
        b.select_to(4);
        b.move_right(false);
        assert_eq!(b.selected(), 4..4);
    }

    #[test]
    fn home_end_and_shifted_variants() {
        let mut b = buf("abc def");
        b.home(false);
        assert_eq!(b.cursor(), 0);
        b.end(true);
        assert_eq!(b.selected_text(), "abc def");
        b.home(true);
        assert!(b.selected().is_empty());
        b.move_to(4);
        b.home(true);
        assert_eq!(b.selected_text(), "abc ");
    }

    #[test]
    fn backspace_and_delete_at_the_edges_report_failure() {
        let mut b = buf("a");
        b.home(false);
        assert!(!b.backspace());
        b.end(false);
        assert!(!b.delete());
        assert!(b.backspace());
        assert!(b.is_empty());
    }

    #[test]
    fn word_navigation_and_deletion() {
        let mut b = buf("foo bar-baz  qux");
        b.end(false);
        b.move_word_left(false);
        assert_eq!(b.cursor(), 13);
        b.move_word_left(false);
        assert_eq!(b.cursor(), 8);
        b.move_word_right(false);
        assert_eq!(b.cursor(), 11);
        b.home(false);
        b.move_word_right(false);
        assert_eq!(b.cursor(), 3);
        b.end(false);
        assert!(b.delete_word_left());
        assert_eq!(b.content(), "foo bar-baz  ");
        assert!(b.delete_to_start());
        assert!(b.is_empty());
        let mut b = buf("one two");
        b.home(false);
        assert!(b.delete_word_right());
        assert_eq!(b.content(), " two");
    }

    #[test]
    fn utf16_conversion_handles_astral_characters() {
        let b = buf("a😀b");
        assert_eq!(b.utf16_len(), 4);
        assert_eq!(b.offset_to_utf16("a😀".len()), 3);
        assert_eq!(b.offset_from_utf16(3), "a😀".len());
        assert_eq!(b.range_from_utf16(&(1..3)), 1.."a😀".len());
    }

    #[test]
    fn ime_composition_marks_then_commits_text() {
        let mut b = buf("ab");
        b.move_to(1);
        // preedit "に" is shown underlined...
        b.replace_and_mark(None, "に", Some(1..1));
        assert_eq!(b.content(), "aにb");
        assert_eq!(b.marked(), Some(1.."aに".len()));
        // ...the composition grows (replaces the marked range, not the cursor position)
        b.replace_and_mark(None, "にほ", None);
        assert_eq!(b.content(), "aにほb");
        assert_eq!(b.marked(), Some(1.."aにほ".len()));
        assert_eq!(b.cursor(), "aにほ".len());
        // commit: final text replaces the marked range and the mark disappears
        b.replace(None, "日本");
        assert_eq!((b.content(), b.marked(), b.cursor()), ("a日本b", None, "a日本".len()));
    }

    #[test]
    fn ime_selection_inside_the_marked_text_is_respected() {
        let mut b = buf("");
        b.replace_and_mark(None, "abc", Some(1..2));
        assert_eq!((b.selected(), b.marked()), (1..2, Some(0..3)));
        b.replace_and_mark(None, "", None); // composition cancelled
        assert_eq!((b.content(), b.marked()), ("", None));
    }

    #[test]
    fn replace_with_an_explicit_range_uses_utf16_converted_offsets() {
        let mut b = buf("a😀b");
        let r = b.range_from_utf16(&(1..3));
        b.replace(Some(r), "-");
        assert_eq!(b.content(), "a-b");
    }

    #[test]
    fn ranges_never_split_utf8_sequences() {
        let mut b = buf("é"); // 2 bytes
        b.replace(Some(1..1), "x");
        assert_eq!(b.content().chars().count(), 2);
        assert!(b.content().is_char_boundary(1));
    }

    #[test]
    fn newlines_and_tabs_become_spaces() {
        let mut b = buf("a\nb");
        b.end(false);
        b.replace(None, "\r\n\tz");
        assert_eq!(b.content(), "a b   z", "CR, LF and TAB each become one space");
    }

    #[test]
    fn masking_maps_indices_per_grapheme() {
        let b = buf("e\u{301}x🇧🇷");
        assert_eq!(b.masked(), "•••");
        assert_eq!(b.to_masked_index("e\u{301}".len()), 3);
        assert_eq!(b.to_masked_index(b.content().len()), 9);
        assert_eq!(b.from_masked_index(3), "e\u{301}".len());
        assert_eq!(b.from_masked_index(9), b.content().len());
    }
}
