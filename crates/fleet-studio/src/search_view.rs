//! GPUI adapter for the bounded, platform-independent search editor.
use std::ops::Range;

use fleet_studio::search_input::{caret_stops, nearest_caret};
use gpui::{
    App, Bounds, ContentMask, Context, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, PaintQuad, Pixels, Point, ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, fill, point, px, relative, rgb, rgba,
    size,
};

use crate::view::FleetView;

pub(crate) struct SearchLayout {
    text: String,
    stops: Vec<(usize, f32)>,
    bounds: Bounds<Pixels>,
    scroll: f32,
}

impl SearchLayout {
    pub(crate) fn byte_for_point(&self, text: &str, position: Point<Pixels>) -> Option<usize> {
        if text != self.text || position.y < self.bounds.top() || position.y > self.bounds.bottom() {
            return None;
        }
        nearest_caret(&self.stops, f32::from(position.x - self.bounds.left()) + self.scroll)
    }

    fn x_for_byte(&self, byte: usize) -> Option<f32> {
        self.stops.iter().rev().find_map(|(position, x)| (*position <= byte).then_some(*x))
    }

    fn bounds_for(&self, range: Range<usize>) -> Option<Bounds<Pixels>> {
        let start = self.x_for_byte(range.start)? - self.scroll;
        let end = self.x_for_byte(range.end)? - self.scroll;
        Some(Bounds::from_corners(point(self.bounds.left() + px(start), self.bounds.top()), point(self.bounds.left() + px(end), self.bounds.bottom())))
    }
}

// Windows GPUI 0.2.2 obtains an IME context before asking for this geometry.
// Returning None can bypass the matching release, so even stale layout must
// provide a harmless position inside the current input element.
fn ime_bounds(layout: Option<&SearchLayout>, text: &str, range: Option<Range<usize>>, fallback: Bounds<Pixels>) -> Bounds<Pixels> {
    layout
        .filter(|layout| layout.text == text)
        .and_then(|layout| range.and_then(|range| layout.bounds_for(range)))
        .unwrap_or_else(|| Bounds::new(fallback.origin, size(px(1.), fallback.size.height)))
}

impl EntityInputHandler for FleetView {
    fn text_for_range(&mut self, range: Range<usize>, actual: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        if !self.searching {
            return None;
        }
        let bytes = self.search.range_from_utf16(range.clone())?;
        *actual = Some(range);
        Some(self.search.text()[bytes].to_owned())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let (range, reversed) = self.search.selection_utf16();
        Some(UTF16Selection { range, reversed })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.searching.then(|| self.search.marked_utf16()).flatten()
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.search.unmark();
        self.search_changed(cx);
    }

    fn replace_text_in_range(&mut self, range: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        if self.searching {
            if self.search.commit_text(range, text) {
                self.paste_status = None;
            } else if !text.is_empty() {
                self.paste_status = Some("入力を反映できませんでした。検索は 256 バイト以内です。");
            }
            self.search_changed(cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.searching {
            if self.search.mark_text(range, text, selected) {
                self.paste_status = None;
            } else if !text.is_empty() {
                self.paste_status = Some("入力を反映できませんでした。検索は 256 バイト以内です。");
            }
            self.search_changed(cx);
        }
    }

    fn bounds_for_range(&mut self, range: Range<usize>, bounds: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        Some(ime_bounds(self.search_layout.as_ref(), self.search.text(), self.search.range_from_utf16(range), bounds))
    }

    fn character_index_for_point(&mut self, position: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        if !self.searching {
            return None;
        }
        let byte = self.search_layout.as_ref()?.byte_for_point(self.search.text(), position)?;
        Some(self.search.range_to_utf16(byte..byte)?.start)
    }
}

pub(crate) struct SearchElement {
    pub(crate) view: Entity<FleetView>,
}

pub(crate) struct Prepaint {
    line: ShapedLine,
    layout: SearchLayout,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for SearchElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for SearchElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let view = self.view.read(cx);
        let text = view.search.text().to_owned();
        let selection = view.search.selection();
        let cursor = view.search.cursor();
        let style = window.text_style();
        let display: SharedString = if text.is_empty() { "機体・リポジトリ・モデル".into() } else { text.clone().into() };
        let run = TextRun {
            len: display.len(),
            font: style.font(),
            color: if text.is_empty() { rgb(0x9baec4).into() } else { style.color },
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked) = view.search.marked().filter(|_| !text.is_empty()) {
            vec![
                TextRun { len: marked.start, ..run.clone() },
                TextRun { len: marked.len(), underline: Some(UnderlineStyle { color: Some(run.color), thickness: px(1.), wavy: false }), ..run.clone() },
                TextRun { len: display.len() - marked.end, ..run },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };
        let line = window.text_system().shape_line(display, style.font_size.to_pixels(window.rem_size()), &runs, None);
        let stops = if text.is_empty() { Some(vec![(0, 0.)]) } else { caret_stops(&text, |index| f32::from(line.x_for_index(index))) }.unwrap_or_default();
        let cursor_x = stops.iter().rev().find_map(|(byte, x)| (*byte <= cursor).then_some(*x)).unwrap_or(0.);
        let width = f32::from(bounds.size.width).max(1.);
        let previous_scroll = view.search_layout.as_ref().map_or(0., |layout| layout.scroll);
        let scroll = previous_scroll.min(cursor_x).max((cursor_x - width + 4.).max(0.));
        let layout = SearchLayout { text, stops, bounds, scroll };
        let selection = if view.searching && !selection.is_empty() { layout.bounds_for(selection).map(|bounds| fill(bounds, rgba(0x4477cc88))) } else { None };
        let cursor = if view.searching && selection.is_none() {
            Some(fill(Bounds::new(point(bounds.left() + px(cursor_x - scroll), bounds.top()), size(px(2.), bounds.size.height)), rgb(0xbcd8ff)))
        } else {
            None
        };
        Prepaint { line, layout, cursor, selection }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let view = self.view.read(cx);
        let focus = view.focus.clone();
        let searching = view.searching;
        if searching {
            window.handle_input(&focus, ElementInputHandler::new(bounds, self.view.clone()), cx);
        }
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(selection) = prepaint.selection.take() {
                window.paint_quad(selection);
            }
            let _ = prepaint.line.paint(point(bounds.left() - px(prepaint.layout.scroll), bounds.top()), window.line_height(), window, cx);
            if searching
                && focus.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });
        let layout = std::mem::replace(&mut prepaint.layout, SearchLayout { text: String::new(), stops: vec![], bounds, scroll: 0. });
        self.view.update(cx, |view, _| view.search_layout = Some(layout));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ime_geometry_falls_back_when_frame_or_range_is_stale() {
        let bounds = Bounds::new(point(px(10.), px(20.)), size(px(100.), px(24.)));
        let fallback = Bounds::new(bounds.origin, size(px(1.), bounds.size.height));
        let layout = SearchLayout { text: "old".into(), stops: vec![(0, 0.), (3, 30.)], bounds, scroll: 0. };
        assert_eq!(ime_bounds(Some(&layout), "new", Some(0..0), bounds), fallback);
        assert_eq!(ime_bounds(Some(&layout), "old", None, bounds), fallback);
        assert_eq!(ime_bounds(None, "old", Some(0..0), bounds), fallback);
        assert_eq!(ime_bounds(Some(&layout), "old", Some(0..3), bounds).size.width, px(30.));
    }
}
