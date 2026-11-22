//! Single-line search editing, independent of platform input callbacks.
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

pub fn scroll_for_cursor(previous: f32, cursor: f32, line_width: f32, viewport: f32) -> f32 {
    let available = (viewport - 3.0).max(0.0);
    let max_scroll = (line_width - available).max(0.0);
    let mut scroll = previous.clamp(0.0, max_scroll);
    if cursor < scroll {
        scroll = cursor;
    }
    if cursor > scroll + available {
        scroll = cursor - available;
    }
    scroll.clamp(0.0, max_scroll)
}

fn utf8_range(text: &str, range: Range<usize>) -> Range<usize> {
    let mut units = 0;
    let mut start = text.len();
    let mut end = text.len();
    for (byte, ch) in text.char_indices() {
        if units <= range.start && range.start < units + ch.len_utf16() {
            start = byte;
        }
        if units >= range.end {
            end = byte;
            break;
        }
        units += ch.len_utf16();
    }
    if range.is_empty() || range.end < range.start { start..start } else { start..end.max(start) }
}

#[derive(Default)]
pub struct Buffer {
    pub content: String,
    pub anchor: usize,
    pub cursor: usize,
    pub marked: Option<Range<usize>>,
}

impl Buffer {
    pub fn selection(&self) -> Range<usize> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor)
    }
    /// `mark` distinguishes committed input from composition with an optional
    /// selection relative to the replacement, in UTF-16 units (the OS contract).
    pub fn replace(&mut self, range: Option<Range<usize>>, text: &str, mark: Option<Option<Range<usize>>>) {
        let range = range.map(|r| self.utf16_range(r)).or_else(|| self.marked.clone()).unwrap_or_else(|| self.selection());
        let text: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        self.content.replace_range(range.clone(), &text);
        self.marked = mark.as_ref().filter(|_| !text.is_empty()).map(|_| range.start..range.start + text.len());
        let selection = mark.flatten().map(|r| utf8_range(&text, r));
        (self.anchor, self.cursor) = selection.map_or((range.start + text.len(), range.start + text.len()), |r| (range.start + r.start, range.start + r.end));
    }
    pub fn move_left(&mut self, extend: bool) {
        let offset = if !extend && self.anchor != self.cursor {
            self.selection().start
        } else {
            self.content.grapheme_indices(true).rev().find_map(|(i, _)| (i < self.cursor).then_some(i)).unwrap_or(0)
        };
        self.cursor = offset;
        if !extend {
            self.anchor = offset;
        }
    }
    pub fn move_right(&mut self, extend: bool) {
        let offset = if !extend && self.anchor != self.cursor {
            self.selection().end
        } else {
            self.content.grapheme_indices(true).find_map(|(i, _)| (i > self.cursor).then_some(i)).unwrap_or(self.content.len())
        };
        self.cursor = offset;
        if !extend {
            self.anchor = offset;
        }
    }
    pub fn delete(&mut self, backward: bool) {
        if self.selection().is_empty() {
            if backward {
                self.move_left(true);
            } else {
                self.move_right(true);
            }
        }
        self.replace(None, "", None);
    }
    pub fn to_utf16(&self, offset: usize) -> usize {
        self.content.char_indices().take_while(|(i, _)| *i < offset).map(|(_, c)| c.len_utf16()).sum()
    }
    pub fn utf16_range(&self, range: Range<usize>) -> Range<usize> {
        utf8_range(&self.content, range)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_scrolled_text_with_a_short_query_reveals_the_whole_query() {
        let old = scroll_for_cursor(0., 1000., 1000., 200.);
        assert!(old > 0.);
        let after_replace = scroll_for_cursor(old, 30., 30., 200.);
        assert_eq!(after_replace, 0.);
        assert_eq!(scroll_for_cursor(old, 500., 1000., 1200.), 0.);
        let at_end = scroll_for_cursor(0., 1000., 1000., 200.);
        assert!((0.0..=197.0).contains(&(1000. - at_end)));
    }

    #[test]
    fn composition_selection_is_relative_to_inserted_text() {
        let mut b = Buffer::default();
        b.replace(None, "先頭末尾", None);
        b.replace(Some(2..2), "仮", Some(Some(0..1)));
        assert_eq!(b.content, "先頭仮末尾");
        assert_eq!(b.selection(), 6..9);
        b.replace(None, "検索🔎", Some(Some(2..4)));
        assert_eq!(b.content, "先頭検索🔎末尾");
        assert_eq!(b.selection(), 12..16);
        b.replace(None, "検索", None);
        assert_eq!(b.content, "先頭検索末尾");
        assert_eq!(b.cursor, 12);
        assert!(b.marked.is_none());
    }

    #[test]
    fn utf16_ranges_never_split_a_surrogate_or_utf8_character() {
        let mut b = Buffer::default();
        b.replace(None, "a🔎日", None);
        assert_eq!(b.utf16_range(1..3), 1..5);
        assert_eq!(b.utf16_range(2..3), 1..5);
        assert_eq!(b.utf16_range(2..2), 1..1);
        assert_eq!(b.utf16_range(20..30), 8..8);
        assert_eq!(b.to_utf16(5), 3);
        b.replace(Some(1..3), "😀", None);
        assert_eq!(b.content, "a😀日");
    }

    #[test]
    fn delete_and_navigation_use_user_perceived_graphemes() {
        let mut b = Buffer::default();
        b.replace(None, "a👩‍💻e\u{301}", None);
        b.move_left(false);
        assert_eq!(&b.content[b.cursor..], "e\u{301}");
        b.delete(true);
        assert_eq!(b.content, "ae\u{301}");
        b.delete(false);
        assert_eq!(b.content, "a");
        b.move_left(true);
        assert_eq!(b.selection(), 0..1);
        b.replace(None, "日", None);
        assert_eq!(b.content, "日");
    }

    #[test]
    fn arrows_collapse_selections_without_skipping_a_character() {
        let mut b = Buffer::default();
        b.replace(None, "abc", None);
        b.anchor = 0;
        b.move_left(false);
        assert_eq!(b.selection(), 0..0);
        b.anchor = 3;
        b.move_right(false);
        assert_eq!(b.selection(), 3..3);
    }

    #[test]
    fn pasted_newlines_and_control_characters_are_single_line() {
        let mut b = Buffer::default();
        b.replace(None, "first\r\nsecond\tlast\u{0}", None);
        assert_eq!(b.content, "first  second last ");
    }
}
