//! A single-line native search field. GPUI's platform input handler provides IME
//! composition and UTF-16 selection; editing itself lives in the tested Buffer.
use crate::text::Buffer;
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    GlobalElementId, KeyBinding, LayoutId, MouseButton, Pixels, Point, ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window,
    actions, div, fill, point, prelude::*, px, relative, rgb, size,
};
use std::ops::Range;

actions!(search, [Backspace, Delete, Left, Right, SelectLeft, SelectRight, SelectAll, Home, End, Paste, Cut, Copy, Submit]);
pub struct Submitted;

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("SearchInput")),
        KeyBinding::new("delete", Delete, Some("SearchInput")),
        KeyBinding::new("left", Left, Some("SearchInput")),
        KeyBinding::new("right", Right, Some("SearchInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("SearchInput")),
        KeyBinding::new("shift-right", SelectRight, Some("SearchInput")),
        KeyBinding::new("home", Home, Some("SearchInput")),
        KeyBinding::new("end", End, Some("SearchInput")),
        KeyBinding::new("enter", Submit, Some("SearchInput")),
    ]);
    for modifier in ["cmd", "ctrl"] {
        cx.bind_keys([
            KeyBinding::new(format!("{modifier}-a").as_str(), SelectAll, Some("SearchInput")),
            KeyBinding::new(format!("{modifier}-c").as_str(), Copy, Some("SearchInput")),
            KeyBinding::new(format!("{modifier}-x").as_str(), Cut, Some("SearchInput")),
            KeyBinding::new(format!("{modifier}-v").as_str(), Paste, Some("SearchInput")),
        ]);
    }
}

pub struct SearchInput {
    buffer: Buffer,
    focus: FocusHandle,
    placeholder: SharedString,
    layout: Option<ShapedLine>,
    bounds: Option<Bounds<Pixels>>,
    scroll_x: Pixels,
    selecting: bool,
}
impl EventEmitter<Submitted> for SearchInput {}
impl Focusable for SearchInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl SearchInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            buffer: Buffer::default(), focus: cx.focus_handle(), placeholder: "検索".into(), layout: None, bounds: None, scroll_x: px(0.), selecting: false
        }
    }
    pub fn text(&self) -> &str {
        &self.buffer.content
    }
    pub fn is_composing(&self) -> bool {
        self.buffer.marked.is_some()
    }
    pub fn reset(&mut self, placeholder: &str, cx: &mut Context<Self>) {
        self.buffer = Buffer::default();
        self.placeholder = placeholder.to_owned().into();
        self.layout = None;
        self.scroll_x = px(0.);
        cx.notify();
    }
    fn index_at(&self, point: Point<Pixels>) -> usize {
        if self.buffer.content.is_empty() {
            return 0;
        }
        match (&self.layout, self.bounds) {
            (Some(line), Some(bounds)) if line.text.as_ref() == self.buffer.content => line.closest_index_for_x(point.x - bounds.left() + self.scroll_x),
            _ => 0,
        }
    }
}
impl EntityInputHandler for SearchInput {
    fn text_for_range(&mut self, range: Range<usize>, actual: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let range = self.buffer.utf16_range(range);
        *actual = Some(self.buffer.to_utf16(range.start)..self.buffer.to_utf16(range.end));
        Some(self.buffer.content[range].into())
    }
    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let s = self.buffer.selection();
        Some(UTF16Selection { range: self.buffer.to_utf16(s.start)..self.buffer.to_utf16(s.end), reversed: self.buffer.cursor < self.buffer.anchor })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer.marked.as_ref().map(|r| self.buffer.to_utf16(r.start)..self.buffer.to_utf16(r.end))
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.marked = None;
        cx.notify();
    }
    fn replace_text_in_range(&mut self, range: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.replace(range, text, None);
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.replace(range, text, Some(selected));
        cx.notify();
    }
    fn bounds_for_range(&mut self, range: Range<usize>, _: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let line = self.layout.as_ref()?;
        let bounds = self.bounds?;
        let range = self.buffer.utf16_range(range);
        Some(Bounds::from_corners(
            point(bounds.left() + line.x_for_index(range.start) - self.scroll_x, bounds.top()),
            point(bounds.left() + line.x_for_index(range.end) - self.scroll_x, bounds.bottom()),
        ))
    }
    fn character_index_for_point(&mut self, point: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        self.bounds?.localize(&point)?;
        Some(self.buffer.to_utf16(self.index_at(point)))
    }
}

struct InputLine(Entity<SearchInput>);
struct PaintedLine {
    line: ShapedLine,
    origin: Point<Pixels>,
    selection: Option<Bounds<Pixels>>,
    cursor: Bounds<Pixels>,
    scroll_x: Pixels,
}
impl IntoElement for InputLine {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for InputLine {
    type RequestLayoutState = ();
    type PrepaintState = PaintedLine;
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> PaintedLine {
        let input = self.0.read(cx);
        let style = window.text_style();
        let content: SharedString = if input.buffer.content.is_empty() { input.placeholder.clone() } else { input.buffer.content.clone().into() };
        let run = TextRun {
            len: content.len(),
            font: style.font(),
            color: if input.buffer.content.is_empty() { rgb(0x8b93a3).into() } else { style.color },
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(r) = input.buffer.marked.as_ref() {
            vec![
                TextRun { len: r.start, ..run.clone() },
                TextRun { len: r.len(), underline: Some(UnderlineStyle { color: Some(run.color), thickness: px(1.), wavy: false }), ..run.clone() },
                TextRun { len: content.len() - r.end, ..run },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect()
        } else {
            vec![run]
        };
        let line = window.text_system().shape_line(content, style.font_size.to_pixels(window.rem_size()), &runs, None);
        let cursor_x = line.x_for_index(input.buffer.cursor);
        let scroll_x = px(crate::text::scroll_for_cursor(input.scroll_x.into(), cursor_x.into(), line.width.into(), bounds.size.width.into()));
        let origin = point(bounds.left() - scroll_x, bounds.top());
        let s = input.buffer.selection();
        let selection = (!s.is_empty()).then(|| {
            Bounds::from_corners(point(origin.x + line.x_for_index(s.start), bounds.top()), point(origin.x + line.x_for_index(s.end), bounds.bottom()))
        });
        let cursor = Bounds::new(point(origin.x + cursor_x, bounds.top()), size(px(1.), bounds.size.height));
        PaintedLine { line, origin, selection, cursor, scroll_x }
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        state: &mut PaintedLine,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.0.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.0.clone()), cx);
        if let Some(selection) = state.selection {
            window.paint_quad(fill(selection, gpui::rgba(0x4f8cff40)));
        }
        let _ = state.line.paint(state.origin, window.line_height(), window, cx);
        if focus.is_focused(window) {
            window.paint_quad(fill(state.cursor, rgb(0x8bb5ff)));
        }
        self.0.update(cx, |input, _| {
            input.layout = Some(state.line.clone());
            input.bounds = Some(bounds);
            input.scroll_x = state.scroll_x;
        });
    }
}
impl Render for SearchInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("SearchInput")
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .w_full()
            .overflow_hidden()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(0x1e222b))
            .border_1()
            .border_color(rgb(0x3a4352))
            .text_sm()
            .line_height(px(20.))
            .on_action(cx.listener(|this, _: &Backspace, _, cx| {
                this.buffer.delete(true);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Delete, _, cx| {
                this.buffer.delete(false);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Left, _, cx| {
                this.buffer.move_left(false);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Right, _, cx| {
                this.buffer.move_right(false);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectLeft, _, cx| {
                this.buffer.move_left(true);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectRight, _, cx| {
                this.buffer.move_right(true);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.buffer.anchor = 0;
                this.buffer.cursor = this.buffer.content.len();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Home, _, cx| {
                this.buffer.cursor = 0;
                this.buffer.anchor = 0;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &End, _, cx| {
                this.buffer.cursor = this.buffer.content.len();
                this.buffer.anchor = this.buffer.cursor;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Copy, _, cx| {
                let r = this.buffer.selection();
                if !r.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(this.buffer.content[r].into()));
                }
            }))
            .on_action(cx.listener(|this, _: &Cut, _, cx| {
                let r = this.buffer.selection();
                if !r.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(this.buffer.content[r].into()));
                    this.buffer.replace(None, "", None);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &Paste, _, cx| {
                if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                    this.buffer.replace(None, &text, None);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &Submit, _, cx| {
                if !this.is_composing() {
                    cx.emit(Submitted);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.focus.focus(window);
                    let i = this.index_at(event.position);
                    this.buffer.cursor = i;
                    if !event.modifiers.shift {
                        this.buffer.anchor = i;
                    }
                    this.selecting = true;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if this.selecting {
                    this.buffer.cursor = this.index_at(event.position);
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.selecting = false))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, _| this.selecting = false))
            .child(InputLine(cx.entity()))
    }
}
