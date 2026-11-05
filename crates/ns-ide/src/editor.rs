use std::collections::BTreeSet;
use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId,
    InspectorElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, Point, Render, ScrollWheelEvent, ShapedLine, SharedString,
    Style, TextRun, UTF16Selection, Window, actions, div, fill, font, prelude::*, px,
    relative, size,
};
use ns_project::Language;
use unicode_segmentation::UnicodeSegmentation;

use crate::highlight::{HighlightSpan, TokenKind, highlight_line};
use crate::theme;

pub const LINE_HEIGHT: f32 = 22.0;
pub const FONT_SIZE: f32 = 13.5;
pub const GUTTER_WIDTH: f32 = 58.0;

/// Emitted when the user toggles a breakpoint in the editor gutter.
#[derive(Debug, Clone)]
pub struct BreakpointToggled {
    /// 1-based source line.
    pub line: usize,
    pub enabled: bool,
}

/// Emitted when the user right-clicks in the editor to open the breakpoint context menu.
#[derive(Debug, Clone)]
pub struct BreakpointContextMenu {
    /// 1-based source line.
    pub line: usize,
    /// Window-relative position for the menu.
    pub x: Pixels,
    pub y: Pixels,
}

actions!(
    editor,
    [
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Home,
        End,
        Backspace,
        Delete,
        Newline,
        Tab,
        Undo,
        Redo,
        Paste,
        Cut,
        Copy,
        ToggleBreakpoint,
    ]
);

pub fn bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("left", Left, Some("Editor")),
        KeyBinding::new("right", Right, Some("Editor")),
        KeyBinding::new("up", Up, Some("Editor")),
        KeyBinding::new("down", Down, Some("Editor")),
        KeyBinding::new("shift-left", SelectLeft, Some("Editor")),
        KeyBinding::new("shift-right", SelectRight, Some("Editor")),
        KeyBinding::new("shift-up", SelectUp, Some("Editor")),
        KeyBinding::new("shift-down", SelectDown, Some("Editor")),
        KeyBinding::new("cmd-a", SelectAll, Some("Editor")),
        KeyBinding::new("ctrl-a", SelectAll, Some("Editor")),
        KeyBinding::new("home", Home, Some("Editor")),
        KeyBinding::new("end", End, Some("Editor")),
        KeyBinding::new("cmd-home", Home, Some("Editor")),
        KeyBinding::new("cmd-end", End, Some("Editor")),
        KeyBinding::new("backspace", Backspace, Some("Editor")),
        KeyBinding::new("delete", Delete, Some("Editor")),
        KeyBinding::new("enter", Newline, Some("Editor")),
        KeyBinding::new("tab", Tab, Some("Editor")),
        KeyBinding::new("cmd-z", Undo, Some("Editor")),
        KeyBinding::new("ctrl-z", Undo, Some("Editor")),
        KeyBinding::new("cmd-shift-z", Redo, Some("Editor")),
        KeyBinding::new("ctrl-shift-z", Redo, Some("Editor")),
        KeyBinding::new("cmd-v", Paste, Some("Editor")),
        KeyBinding::new("ctrl-v", Paste, Some("Editor")),
        KeyBinding::new("cmd-c", Copy, Some("Editor")),
        KeyBinding::new("ctrl-c", Copy, Some("Editor")),
        KeyBinding::new("cmd-x", Cut, Some("Editor")),
        KeyBinding::new("ctrl-x", Cut, Some("Editor")),
        KeyBinding::new("f9", ToggleBreakpoint, Some("Editor")),
    ]
}

pub struct Editor {
    focus_handle: FocusHandle,
    text: String,
    language: Option<Language>,
    cursor: usize,
    selection: Range<usize>,
    selection_reversed: bool,
    preferred_column: Option<usize>,
    marked_range: Option<Range<usize>>,
    last_line_shapes: Vec<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    breakpoints: BTreeSet<usize>,
    debug_line: Option<usize>,
    scroll_y: f32,
    viewport_height: f32,
    dirty: bool,
    undo: Vec<String>,
    redo: Vec<String>,
    is_selecting: bool,
}

impl Editor {
    pub fn new(
        text: String,
        language: Option<Language>,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let cursor = text.len();
        Self {
            focus_handle: cx.focus_handle(),
            text,
            language,
            cursor,
            selection: cursor..cursor,
            selection_reversed: false,
            preferred_column: None,
            marked_range: None,
            last_line_shapes: Vec::new(),
            last_bounds: None,
            breakpoints: BTreeSet::new(),
            debug_line: None,
            scroll_y: 0.0,
            viewport_height: 0.0,
            dirty: false,
            undo: Vec::new(),
            redo: Vec::new(),
            is_selecting: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn selected_text(&self) -> String {
        if self.selection.is_empty() {
            String::new()
        } else {
            self.text[self.selection.clone()].to_string()
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    fn content_height(&self) -> f32 {
        self.text.split('\n').count().max(1) as f32 * LINE_HEIGHT
    }

    fn max_scroll(&self) -> f32 {
        (self.content_height() - self.viewport_height).max(0.0)
    }

    fn clamp_scroll(&mut self) {
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll());
    }

    /// Scroll the viewport so `line_index` (0-based) is visible.
    fn reveal_line(&mut self, line_index: usize) {
        let line_top = line_index as f32 * LINE_HEIGHT;
        if line_top < self.scroll_y {
            self.scroll_y = line_top;
        } else if line_top + LINE_HEIGHT > self.scroll_y + self.viewport_height {
            self.scroll_y = (line_top + LINE_HEIGHT - self.viewport_height).max(0.0);
        }
        self.clamp_scroll();
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(px(LINE_HEIGHT));
        let delta_y: f32 = delta.y.into();
        if delta_y == 0.0 {
            return;
        }
        self.scroll_y -= delta_y;
        self.clamp_scroll();
        cx.notify();
    }

    pub fn set_debug_line(&mut self, line: Option<usize>, cx: &mut Context<Self>) {
        if self.debug_line != line {
            self.debug_line = line;
            cx.notify();
        }
    }

    pub fn set_breakpoints(
        &mut self,
        lines: impl IntoIterator<Item = usize>,
        cx: &mut Context<Self>,
    ) {
        self.breakpoints = lines.into_iter().collect();
        cx.notify();
    }

    fn toggle_breakpoint_line(&mut self, line: usize, cx: &mut Context<Self>) {
        if line == 0 {
            return;
        }
        let enabled = if self.breakpoints.remove(&line) {
            false
        } else {
            self.breakpoints.insert(line);
            true
        };
        cx.emit(BreakpointToggled { line, enabled });
        cx.notify();
    }

    pub fn cursor_line_col(&self) -> (usize, usize) {
        let (line, column) = self.line_col(self.cursor);
        (line + 1, column + 1)
    }

    pub fn set_cursor_line(&mut self, line_number: usize, cx: &mut Context<Self>) {
        let starts = line_starts(&self.text);
        let line = line_number.saturating_sub(1).min(starts.len().saturating_sub(1));
        let offset = starts.get(line).copied().unwrap_or(0);
        self.reveal_line(line);
        self.move_to(offset, cx);
        cx.notify();
    }

    fn line_starts(&self) -> Vec<usize> {
        line_starts(&self.text)
    }

    fn line_col(&self, offset: usize) -> (usize, usize) {
        let offset = clamp_boundary(&self.text, offset);
        let starts = self.line_starts();
        let line = starts.partition_point(|start| *start <= offset).saturating_sub(1);
        let start = starts[line];
        let line_end = self.line_end(line, &starts);
        (line, offset.min(line_end) - start)
    }

    fn line_end(&self, line: usize, starts: &[usize]) -> usize {
        starts
            .get(line + 1)
            .map(|next| next.saturating_sub(1))
            .unwrap_or(self.text.len())
    }

    fn offset_for_line_char(&self, line: usize, char_column: usize, starts: &[usize]) -> usize {
        let start = starts[line];
        let end = self.line_end(line, starts);
        let mut offset = start;
        for (count, (index, ch)) in self.text[start..end].char_indices().enumerate() {
            if count >= char_column {
                break;
            }
            offset = start + index + ch.len_utf8();
        }
        offset.min(end)
    }

    fn snapshot(&mut self) {
        self.undo.push(self.text.clone());
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = clamp_boundary(&self.text, offset);
        self.cursor = offset;
        self.selection = offset..offset;
        self.selection_reversed = false;
        self.preferred_column = None;
        self.marked_range = None;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = clamp_boundary(&self.text, offset);
        if self.selection_reversed {
            self.selection.start = offset;
        } else {
            self.selection.end = offset;
        }
        if self.selection.end < self.selection.start {
            self.selection_reversed = !self.selection_reversed;
            self.selection = self.selection.end..self.selection.start;
        }
        self.cursor = if self.selection_reversed {
            self.selection.start
        } else {
            self.selection.end
        };
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        self.cursor
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.text.len())
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selection.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            self.move_to(self.next_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selection.end, cx);
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(-1, false, cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(1, false, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(-1, true, cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(1, true, cx);
    }

    fn move_vertical(&mut self, direction: isize, extend: bool, cx: &mut Context<Self>) {
        let starts = self.line_starts();
        let (line, _) = self.line_col(self.cursor);
        let column = self
            .preferred_column
            .unwrap_or_else(|| self.line_col(self.cursor).1);
        let target = line as isize + direction;
        if target < 0 || target as usize >= starts.len() {
            return;
        }
        let offset = self.offset_for_line_char(target as usize, column, &starts);
        if extend {
            self.select_to(offset, cx);
        } else {
            self.cursor = offset;
            self.selection = offset..offset;
            self.selection_reversed = false;
            self.preferred_column = Some(column);
            cx.notify();
        }
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let (line, _) = self.line_col(self.cursor);
        let starts = self.line_starts();
        self.move_to(starts[line], cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let (line, _) = self.line_col(self.cursor);
        let starts = self.line_starts();
        self.move_to(self.line_end(line, &starts), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.cursor = self.text.len();
        self.selection = 0..self.text.len();
        self.selection_reversed = false;
        cx.notify();
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            let start = self.previous_boundary(self.cursor_offset());
            self.selection = start..self.cursor_offset();
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            let end = self.next_boundary(self.cursor_offset());
            self.selection = self.cursor_offset()..end;
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx);
    }

    fn tab(&mut self, _: &Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "    ", window, cx);
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        let Some(previous) = self.undo.pop() else {
            return;
        };
        self.redo.push(std::mem::replace(&mut self.text, previous));
        self.cursor = self.cursor.min(self.text.len());
        self.selection = self.cursor..self.cursor;
        self.dirty = true;
        cx.notify();
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        let Some(next) = self.redo.pop() else {
            return;
        };
        self.undo.push(std::mem::replace(&mut self.text, next));
        self.cursor = self.cursor.min(self.text.len());
        self.selection = self.cursor..self.cursor;
        self.dirty = true;
        cx.notify();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.text[self.selection.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.text[self.selection.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn toggle_breakpoint(
        &mut self,
        _: &ToggleBreakpoint,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (line, _) = self.line_col(self.cursor);
        self.toggle_breakpoint_line(line + 1, cx);
    }

    fn on_right_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);

        let Some(bounds) = self.last_bounds else {
            return;
        };
        let top = bounds.top() - px(self.scroll_y);
        let line = self.line_for_y(event.position.y, top);

        let menu_width = px(220.);
        let menu_height = px(170.);
        let right_edge = bounds.left() + bounds.size.width;
        let bottom_edge = bounds.top() + bounds.size.height;
        let mut x = event.position.x;
        let mut y = event.position.y;
        if x + menu_width > right_edge {
            x = (right_edge - menu_width).max(px(0.));
        }
        if y + menu_height > bottom_edge {
            y = (bottom_edge - menu_height).max(px(0.));
        }

        cx.emit(BreakpointContextMenu { line, x, y });
        cx.notify();
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);

        if let Some(bounds) = self.last_bounds {
            if event.position.x < bounds.left() + px(GUTTER_WIDTH) {
                let top = bounds.top() - px(self.scroll_y);
        let line = self.line_for_y(event.position.y, top);
                self.toggle_breakpoint_line(line, cx);
                self.is_selecting = false;
                return;
            }
        }

        self.is_selecting = true;
        let offset = self.index_for_mouse_position(event.position);
        if event.modifiers.shift {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            let offset = self.index_for_mouse_position(event.position);
            self.select_to(offset, cx);
        }
    }

    fn line_for_y(&self, y: Pixels, top: Pixels) -> usize {
        let line = ((y - top) / px(LINE_HEIGHT)).floor().max(0.0) as usize;
        line.min(self.line_starts().len().saturating_sub(1)) + 1
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds else {
            return self.cursor;
        };
        let line = ((position.y - bounds.top() + px(self.scroll_y)) / px(LINE_HEIGHT))
            .floor()
            .max(0.0) as usize;
        let line = line.min(self.line_starts().len().saturating_sub(1));
        let starts = self.line_starts();
        let line_start = starts[line];
        let line_len = self.line_end(line, &starts) - line_start;
        let local_x = position.x - bounds.left() - px(GUTTER_WIDTH);
        if local_x <= px(0.) {
            return clamp_boundary(&self.text, line_start);
        }
        let column = self
            .last_line_shapes
            .get(line)
            .and_then(|shape| shape.index_for_x(local_x))
            .unwrap_or(0)
            .min(line_len);
        clamp_boundary(&self.text, line_start + column)
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.text.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.text.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.text[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selection),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or(self.selection.clone());
        let range = clamp_range(&self.text, range);
        if range.is_empty() && new_text.is_empty() {
            return;
        }
        self.snapshot();
        self.text = self.text[0..range.start].to_owned()
            + new_text
            + &self.text[range.end..];
        let cursor = range.start + new_text.len();
        self.cursor = clamp_boundary(&self.text, cursor);
        self.selection = self.cursor..self.cursor;
        self.selection_reversed = false;
        self.preferred_column = None;
        self.marked_range.take();
        self.dirty = true;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or(self.selection.clone());
        let range = clamp_range(&self.text, range);
        self.snapshot();
        self.text = self.text[0..range.start].to_owned()
            + new_text
            + &self.text[range.end..];
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.cursor = new_selected_range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .map(|range| clamp_boundary(&self.text, range.start + new_text.len()))
            .unwrap_or_else(|| clamp_boundary(&self.text, range.start + new_text.len()));
        self.selection = self.cursor..self.cursor;
        self.preferred_column = None;
        self.dirty = true;
        cx.notify();
        let _ = window;
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let starts = self.line_starts();
        let (line, _) = self.line_col(range.start);
        let shape = self.last_line_shapes.get(line)?;
        let line_start = starts[line];
        let line_end = self.line_end(line, &starts);
        let local_start = range.start.saturating_sub(line_start).min(line_end - line_start);
        let local_end = range.end.saturating_sub(line_start).min(line_end - line_start);
        let top = bounds.top() - px(self.scroll_y);
        Some(Bounds::from_corners(
            Point::new(
                bounds.left() + shape.x_for_index(local_start),
                top + px(line as f32 * LINE_HEIGHT),
            ),
            Point::new(
                bounds.left() + shape.x_for_index(local_end),
                top + px((line as f32 + 1.0) * LINE_HEIGHT),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let offset = self.index_for_mouse_position(point);
        Some(self.offset_to_utf16(offset))
    }
}

impl EventEmitter<BreakpointToggled> for Editor {}
impl EventEmitter<BreakpointContextMenu> for Editor {}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Editor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::color(theme::BG_0))
            .key_context("Editor")
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::tab))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::toggle_breakpoint))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(
                div()
                    .id("editor-viewport")
                    .flex_1()
                    .w_full()
                    .overflow_hidden()
                    .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
                    .child(CodeElement {
                        editor: cx.entity(),
                    }),
            )
    }
}

struct CodeElement {
    editor: Entity<Editor>,
}

struct PrepaintState {
    lines: Vec<ShapedLine>,
    line_numbers: Vec<ShapedLine>,
    breakpoints: Vec<PaintQuad>,
    selections: Vec<PaintQuad>,
    debug_line: Option<PaintQuad>,
    debug_marker: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
}

impl IntoElement for CodeElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for CodeElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let editor = self.editor.read(cx);
        let text = editor.text.clone();
        let language = editor.language;
        let selection = editor.selection.clone();
        let cursor = editor.cursor;
        let breakpoints = editor.breakpoints.clone();
        let debug_line = editor.debug_line;

        let code_left = bounds.left() + px(GUTTER_WIDTH);
        let total_lines = text.split('\n').count().max(1) as f32;
        let viewport: f32 = bounds.size.height.into();
        let scroll_y = editor
            .scroll_y
            .clamp(0.0, (total_lines * LINE_HEIGHT - viewport).max(0.0));
        let top = bounds.top() - px(scroll_y);
        let mut line_shapes = Vec::new();
        let mut line_numbers = Vec::new();
        let mut selections = Vec::new();
        let mut block_comment = false;
        let starts = line_starts(&text);
        let code_font = font(theme::MONO_FONT);
        let number_font = font(theme::UI_FONT);

        for (line_index, line) in text.split('\n').enumerate() {
            let spans = highlight_line(line, language.unwrap_or(Language::C), &mut block_comment);
            let runs = build_runs(line, &spans, &code_font);
            let shape = window.text_system().shape_line(
                SharedString::from(line.to_string()),
                px(FONT_SIZE),
                &runs,
                None,
            );

            let number_text = (line_index + 1).to_string();
            let number_run = TextRun {
                len: number_text.len(),
                font: number_font.clone(),
                color: theme::color(theme::TEXT_FAINT),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            line_numbers.push(window.text_system().shape_line(
                SharedString::from(number_text),
                px(10.5),
                &[number_run],
                None,
            ));

            let start = starts[line_index];
            let end = if line_index + 1 < starts.len() {
                starts[line_index + 1].saturating_sub(1)
            } else {
                text.len()
            };

            if selection.start < selection.end && selection.start < end && selection.end > start {
                let sel_start = selection.start.max(start);
                let sel_end = selection.end.min(end);
                let x_start = shape.x_for_index(sel_start - start);
                let x_end = shape.x_for_index(sel_end - start);
                selections.push(fill(
                    Bounds::new(
                        Point::new(
                            code_left + x_start,
                            top + px(line_index as f32 * LINE_HEIGHT),
                        ),
                        size((x_end - x_start).max(px(2.)), px(LINE_HEIGHT)),
                    ),
                    theme::alpha(theme::ACCENT, 0.2),
                ));
            }

            line_shapes.push(shape);
        }

        let breakpoint_quads = breakpoints
            .iter()
            .filter(|line| **line >= 1 && **line <= line_shapes.len())
            .map(|line| {
                let y = top + px((*line as f32 - 1.0) * LINE_HEIGHT);
                fill(
                    Bounds::new(
                        Point::new(bounds.left() + px(13.), y + px((LINE_HEIGHT - 8.) / 2.)),
                        size(px(8.), px(8.)),
                    ),
                    theme::color(theme::CINNABAR),
                )
                .corner_radii(px(4.))
            })
            .collect();

        let (debug_line, debug_marker) = debug_line
            .filter(|line| *line >= 1 && *line <= line_shapes.len())
            .map(|line| {
                let y = top + px((line as f32 - 1.0) * LINE_HEIGHT);
                let quad = fill(
                    Bounds::new(
                        Point::new(bounds.left(), y),
                        size(bounds.size.width, px(LINE_HEIGHT)),
                    ),
                    theme::alpha(theme::JADE, 0.16),
                );
                let marker_text = "▶";
                let marker_run = TextRun {
                    len: marker_text.len(),
                    font: font(theme::UI_FONT),
                    color: theme::color(theme::JADE),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let marker = window.text_system().shape_line(
                    SharedString::from(marker_text),
                    px(10.),
                    &[marker_run],
                    None,
                );
                (Some(quad), Some(marker))
            })
            .unwrap_or((None, None));

        let (cursor_line, cursor_column) = line_col_in(&text, &starts, cursor);
        let cursor_quad = line_shapes.get(cursor_line).map(|shape| {
            fill(
                Bounds::new(
                    Point::new(
                        code_left + shape.x_for_index(cursor_column),
                        top + px(cursor_line as f32 * LINE_HEIGHT),
                    ),
                    size(px(2.0), px(LINE_HEIGHT)),
                ),
                theme::color(theme::ACCENT_3),
            )
        });

        PrepaintState {
            lines: line_shapes,
            line_numbers,
            breakpoints: breakpoint_quads,
            selections,
            debug_line,
            debug_marker,
            cursor: cursor_quad,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.editor.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );

        let scroll_y = self.editor.read(cx).scroll_y.clamp(0.0, self.editor.read(cx).max_scroll());
        let top = bounds.top() - px(scroll_y);
        let code_left = bounds.left() + px(GUTTER_WIDTH);
        window.paint_quad(fill(
            Bounds::new(bounds.origin, size(px(GUTTER_WIDTH), bounds.size.height)),
            theme::color(theme::BG_2).opacity(0.55),
        ));
        window.paint_quad(fill(
            Bounds::new(
                Point::new(code_left - px(1.), bounds.top()),
                size(px(1.), bounds.size.height),
            ),
            theme::alpha(theme::STROKE, 0.09),
        ));

        if let Some(debug_line) = prepaint.debug_line.take() {
            window.paint_quad(debug_line);
        }

        for selection in prepaint.selections.drain(..) {
            window.paint_quad(selection);
        }

        for breakpoint in prepaint.breakpoints.drain(..) {
            window.paint_quad(breakpoint);
        }

        if let Some(marker) = prepaint.debug_marker.take() {
            if let Some(line) = self.editor.read(cx).debug_line {
                let y = top + px((line as f32 - 1.0) * LINE_HEIGHT);
                marker
                    .paint(
                        Point::new(bounds.left() + px(27.), y + px((LINE_HEIGHT - 10.) / 2.)),
                        px(LINE_HEIGHT),
                        window,
                        cx,
                    )
                    .ok();
            }
        }

        for (index, number) in prepaint.line_numbers.iter().enumerate() {
            let line_top = top + px((index as f32 + 0.5) * LINE_HEIGHT - LINE_HEIGHT / 2.);
            if line_top + px(LINE_HEIGHT) <= bounds.top() || line_top >= bounds.bottom() {
                continue;
            }
            number
                .paint(
                    Point::new(
                        code_left - px(12.) - number.width,
                        line_top,
                    ),
                    px(LINE_HEIGHT),
                    window,
                    cx,
                )
                .ok();
        }

        for (index, line) in prepaint.lines.iter().enumerate() {
            let line_top = top + px(index as f32 * LINE_HEIGHT);
            if line_top + px(LINE_HEIGHT) <= bounds.top() || line_top >= bounds.bottom() {
                continue;
            }
            line.paint(
                Point::new(code_left, line_top),
                px(LINE_HEIGHT),
                window,
                cx,
            )
            .ok();
        }

        if focus_handle.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }

        let lines = std::mem::take(&mut prepaint.lines);
        self.editor.update(cx, |editor, _cx| {
            editor.last_line_shapes = lines;
            editor.last_bounds = Some(bounds);
            editor.viewport_height = bounds.size.height.into();
            editor.clamp_scroll();
        });
    }
}

fn build_runs(line: &str, spans: &[HighlightSpan], font: &gpui::Font) -> Vec<TextRun> {
    if line.is_empty() || spans.is_empty() {
        return vec![TextRun {
            len: line.len(),
            font: font.clone(),
            color: theme::color(theme::TEXT),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
    }

    spans
        .iter()
        .map(|span| TextRun {
            len: span.len,
            font: font.clone(),
            color: token_color(span.kind),
            background_color: None,
            underline: None,
            strikethrough: None,
        })
        .collect()
}

fn token_color(kind: TokenKind) -> gpui::Hsla {
    match kind {
        TokenKind::Plain => theme::color(theme::TEXT),
        TokenKind::Keyword => theme::color(theme::ACCENT),
        TokenKind::Type => theme::color(theme::JADE),
        TokenKind::Function => theme::color(0xe8d5ab),
        TokenKind::String => theme::color(0xa8b57a),
        TokenKind::Number => theme::color(theme::ACCENT_3),
        TokenKind::Comment => theme::color(theme::TEXT_FAINT),
        TokenKind::Preprocessor => theme::color(theme::CINNABAR),
        TokenKind::Punctuation => theme::color(theme::TEXT_DIM),
    }
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, ch) in text.char_indices() {
        if ch == '\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn line_col_in(text: &str, starts: &[usize], offset: usize) -> (usize, usize) {
    let offset = clamp_boundary(text, offset);
    let line = starts.partition_point(|start| *start <= offset).saturating_sub(1);
    let start = starts[line];
    let end = starts
        .get(line + 1)
        .map(|next| next.saturating_sub(1))
        .unwrap_or(text.len());
    (line, offset.min(end) - start)
}

fn clamp_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn clamp_range(text: &str, range: Range<usize>) -> Range<usize> {
    let start = clamp_boundary(text, range.start);
    let end = clamp_boundary(text, range.end.max(start));
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_starts_and_columns() {
        let text = "abc\ndef\n";
        let starts = line_starts(text);
        assert_eq!(starts, vec![0, 4, 8]);
        assert_eq!(line_col_in(text, &starts, 0), (0, 0));
        assert_eq!(line_col_in(text, &starts, 4), (1, 0));
        assert_eq!(line_col_in(text, &starts, 7), (1, 3));
        assert_eq!(line_col_in(text, &starts, 8), (2, 0));
    }

    #[test]
    fn clamps_to_char_boundary() {
        let text = "a你b";
        assert_eq!(clamp_boundary(text, 2), 1);
        assert_eq!(clamp_boundary(text, 4), 4);
    }
}
