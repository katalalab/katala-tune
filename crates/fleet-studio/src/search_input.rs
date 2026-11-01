//! Bounded single-line editing state. Search text is never logged.
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

pub const MAX_BYTES: usize = 256;

#[derive(Clone, Copy, Default)]
pub struct KeyModifiers {
    pub control: bool,
    pub platform: bool,
    pub alt: bool,
    pub shift: bool,
    pub function: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyAction {
    Ime,
    Confirm,
    Leave,
    Paste,
    SelectAll,
    Left(bool),
    Right(bool),
    Home(bool),
    End(bool),
    Backspace,
    Delete,
    Text,
}

/// Text is delivered by the platform input handler, not inserted from key events.
pub fn search_key(key: &str, modifiers: KeyModifiers, composing: bool, macos: bool) -> KeyAction {
    if composing {
        return KeyAction::Ime;
    }
    let primary = if macos { modifiers.platform && !modifiers.control } else { modifiers.control && !modifiers.platform };
    if primary && !modifiers.alt && !modifiers.function {
        match key {
            "v" => return KeyAction::Paste,
            "a" => return KeyAction::SelectAll,
            "left" if macos => return KeyAction::Home(modifiers.shift),
            "right" if macos => return KeyAction::End(modifiers.shift),
            _ => return KeyAction::Text,
        }
    }
    if modifiers.control || modifiers.platform || modifiers.alt || modifiers.function {
        return KeyAction::Text;
    }
    match key {
        "enter" => KeyAction::Confirm,
        "escape" => KeyAction::Leave,
        "left" => KeyAction::Left(modifiers.shift),
        "right" => KeyAction::Right(modifiers.shift),
        "home" => KeyAction::Home(modifiers.shift),
        "end" => KeyAction::End(modifiers.shift),
        "backspace" => KeyAction::Backspace,
        "delete" => KeyAction::Delete,
        _ => KeyAction::Text,
    }
}

struct Composition {
    original: String,
    anchor: usize,
    head: usize,
    marked: Range<usize>,
}

#[derive(Default)]
pub struct SearchInput {
    text: String,
    anchor: usize,
    head: usize,
    composition: Option<Composition>,
}

/// Convert only exact UTF-16 boundaries; a surrogate interior is not a valid byte index.
fn byte_at_utf16(text: &str, target: usize) -> Option<usize> {
    let mut count = 0usize;
    for (byte, ch) in text.char_indices() {
        if count == target {
            return Some(byte);
        }
        count += ch.len_utf16();
        if count > target {
            return None;
        }
    }
    (count == target).then_some(text.len())
}

fn byte_range(text: &str, range: Range<usize>) -> Option<Range<usize>> {
    if range.start > range.end {
        return None;
    }
    Some(byte_at_utf16(text, range.start)?..byte_at_utf16(text, range.end)?)
}

fn utf16_at(text: &str, byte: usize) -> usize {
    text[..byte].encode_utf16().count()
}

fn allowed(text: &str) -> bool {
    text.len() <= MAX_BYTES && !text.chars().any(char::is_control)
}

/// GPUI reports one x position per glyph cluster. Interpolate grapheme stops
/// within a ligature so clicking and the visible caret use the same positions.
pub fn caret_stops(text: &str, mut measure: impl FnMut(usize) -> f32) -> Option<Vec<(usize, f32)>> {
    let mut indices: Vec<_> = text.grapheme_indices(true).map(|(index, _)| index).collect();
    indices.push(text.len());
    let mut stops: Vec<_> = indices.into_iter().map(|index| (index, measure(index))).collect();
    if stops.iter().any(|(_, x)| !x.is_finite()) || stops.windows(2).any(|pair| pair[0].1 > pair[1].1) {
        return None;
    }
    let mut start = 1;
    while start < stops.len() {
        let mut end = start;
        while end + 1 < stops.len() && stops[end + 1].1 == stops[start].1 {
            end += 1;
        }
        if end > start && stops[start - 1].1 < stops[start].1 {
            let left = stops[start - 1].1;
            let span = stops[start].1 - left;
            let count = (end - start + 1) as f32;
            for (offset, stop) in stops[start..=end].iter_mut().enumerate() {
                stop.1 = left + span * (offset + 1) as f32 / count;
            }
        }
        start = end + 1;
    }
    Some(stops)
}

pub fn nearest_caret(stops: &[(usize, f32)], x: f32) -> Option<usize> {
    if !x.is_finite() {
        return None;
    }
    stops.iter().min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs())).map(|stop| stop.0)
}

impl SearchInput {
    pub fn new(text: &str) -> Option<Self> {
        allowed(text).then(|| Self { text: text.into(), anchor: text.len(), head: text.len(), composition: None })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Keep rows stable while an IME is revising text that may be cancelled.
    pub fn filter_text(&self) -> &str {
        self.composition.as_ref().map_or(&self.text, |composition| &composition.original)
    }
    pub fn is_composing(&self) -> bool {
        self.composition.is_some()
    }
    pub fn selection(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
    pub fn cursor(&self) -> usize {
        self.head
    }
    pub fn selected_text(&self) -> &str {
        &self.text[self.selection()]
    }
    pub fn marked(&self) -> Option<Range<usize>> {
        self.composition.as_ref().map(|composition| composition.marked.clone())
    }

    pub fn selection_utf16(&self) -> (Range<usize>, bool) {
        let range = self.selection();
        (utf16_at(&self.text, range.start)..utf16_at(&self.text, range.end), self.head < self.anchor)
    }

    pub fn marked_utf16(&self) -> Option<Range<usize>> {
        self.marked().map(|range| utf16_at(&self.text, range.start)..utf16_at(&self.text, range.end))
    }

    pub fn range_from_utf16(&self, range: Range<usize>) -> Option<Range<usize>> {
        byte_range(&self.text, range)
    }
    pub fn range_to_utf16(&self, range: Range<usize>) -> Option<Range<usize>> {
        (range.start <= range.end && self.text.is_char_boundary(range.start) && self.text.is_char_boundary(range.end))
            .then(|| utf16_at(&self.text, range.start)..utf16_at(&self.text, range.end))
    }

    pub fn select_utf16(&mut self, range: Range<usize>, reversed: bool) -> bool {
        let Some(range) = byte_range(&self.text, range) else { return false };
        if self.is_composing() {
            return false;
        }
        self.anchor = if reversed { range.end } else { range.start };
        self.head = if reversed { range.start } else { range.end };
        true
    }

    pub fn select_byte(&mut self, position: usize, extend: bool) -> bool {
        if self.is_composing() || position > self.text.len() || !self.text.is_char_boundary(position) {
            return false;
        }
        let position = self.grapheme_floor(position);
        if !extend {
            self.anchor = position;
        }
        self.head = position;
        true
    }

    pub fn select_all(&mut self) {
        if !self.is_composing() {
            self.anchor = 0;
            self.head = self.text.len();
        }
    }

    pub fn move_left(&mut self, extend: bool) {
        if self.is_composing() {
            return;
        }
        let target = if !extend && self.anchor != self.head { self.selection().start } else { self.previous(self.head) };
        if !extend {
            self.anchor = target;
        }
        self.head = target;
    }

    pub fn move_right(&mut self, extend: bool) {
        if self.is_composing() {
            return;
        }
        let target = if !extend && self.anchor != self.head { self.selection().end } else { self.next(self.head) };
        if !extend {
            self.anchor = target;
        }
        self.head = target;
    }

    pub fn move_home(&mut self, extend: bool) {
        self.move_boundary(0, extend);
    }
    pub fn move_end(&mut self, extend: bool) {
        self.move_boundary(self.text.len(), extend);
    }

    fn move_boundary(&mut self, target: usize, extend: bool) {
        if self.is_composing() {
            return;
        }
        if !extend {
            self.anchor = target;
        }
        self.head = target;
    }

    pub fn backspace(&mut self) -> bool {
        if self.is_composing() {
            return false;
        }
        let selection = self.selection();
        let range = if selection.is_empty() {
            let floor = self.grapheme_floor(self.head);
            if floor != self.head { floor..self.next(self.head) } else { self.previous(self.head)..self.head }
        } else {
            self.grapheme_floor(selection.start)..self.grapheme_ceil(selection.end)
        };
        self.replace(range, "")
    }

    pub fn delete(&mut self) -> bool {
        if self.is_composing() {
            return false;
        }
        let selection = self.selection();
        let range = if selection.is_empty() {
            self.grapheme_floor(self.head)..self.next(self.head)
        } else {
            self.grapheme_floor(selection.start)..self.grapheme_ceil(selection.end)
        };
        self.replace(range, "")
    }

    /// Platform commits replace the marked text first. Windows sends an empty
    /// replacement when IME is cancelled, including when nothing is marked.
    pub fn commit_text(&mut self, range: Option<Range<usize>>, text: &str) -> bool {
        if range.is_none() && text.is_empty() {
            return self.cancel_composition();
        }
        let range = match range {
            Some(range) => match byte_range(&self.text, range) {
                Some(range) => range,
                None => {
                    self.cancel_composition();
                    return false;
                }
            },
            None => self.marked().unwrap_or_else(|| self.selection()),
        };
        if !self.replace(range, text) {
            self.cancel_composition();
            return false;
        }
        self.composition = None;
        true
    }

    /// The selection supplied by GPUI is relative to the newly marked string.
    pub fn mark_text(&mut self, range: Option<Range<usize>>, text: &str, selected: Option<Range<usize>>) -> bool {
        if text.is_empty() {
            return self.cancel_composition();
        }
        if self.composition.is_none() {
            self.composition = Some(Composition { original: self.text.clone(), anchor: self.anchor, head: self.head, marked: self.selection() });
        }
        let range = match range {
            Some(range) => match byte_range(&self.text, range) {
                Some(range) => range,
                None => return false,
            },
            None => self.marked().expect("composition is present"),
        };
        let selected = match selected {
            Some(range) => match byte_range(text, range) {
                Some(range) => range,
                None => return false,
            },
            None => text.len()..text.len(),
        };
        if !allowed(text) || self.text.len().saturating_sub(range.len()).saturating_add(text.len()) > MAX_BYTES {
            return false;
        }
        self.text.replace_range(range.clone(), text);
        self.anchor = range.start + selected.start;
        self.head = range.start + selected.end;
        if let Some(composition) = &mut self.composition {
            composition.marked = range.start..range.start + text.len();
        }
        true
    }

    pub fn cancel_composition(&mut self) -> bool {
        let Some(composition) = self.composition.take() else { return false };
        self.text = composition.original;
        self.anchor = composition.anchor;
        self.head = composition.head;
        true
    }

    pub fn unmark(&mut self) {
        self.composition = None;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.anchor = 0;
        self.head = 0;
        self.composition = None;
    }

    fn replace(&mut self, range: Range<usize>, text: &str) -> bool {
        if !allowed(text) || self.text.len().saturating_sub(range.len()).saturating_add(text.len()) > MAX_BYTES {
            return false;
        }
        if range.is_empty() && text.is_empty() {
            return false;
        }
        self.text.replace_range(range.clone(), text);
        self.anchor = range.start + text.len();
        self.head = self.anchor;
        true
    }

    fn previous(&self, offset: usize) -> usize {
        self.text.grapheme_indices(true).rev().find_map(|(byte, _)| (byte < offset).then_some(byte)).unwrap_or(0)
    }
    fn next(&self, offset: usize) -> usize {
        self.text.grapheme_indices(true).find_map(|(byte, _)| (byte > offset).then_some(byte)).unwrap_or(self.text.len())
    }
    fn grapheme_floor(&self, offset: usize) -> usize {
        if offset == self.text.len() {
            offset
        } else {
            self.text.grapheme_indices(true).map(|(byte, _)| byte).take_while(|byte| *byte <= offset).last().unwrap_or(0)
        }
    }
    fn grapheme_ceil(&self, offset: usize) -> usize {
        if self.grapheme_floor(offset) == offset { offset } else { self.next(offset) }
    }
}
