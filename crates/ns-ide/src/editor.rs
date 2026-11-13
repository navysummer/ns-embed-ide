use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font, GlobalElementId,
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
/// Width of the right-edge minimap in pixels.
pub const MINIMAP_WIDTH: f32 = 88.0;

/// Emitted when the user toggles a breakpoint in the editor gutter.
#[derive(Debug, Clone)]
pub struct BreakpointToggled {
    /// 1-based source line.
    pub line: usize,
    pub enabled: bool,
}

/// Emitted when the user toggles a bookmark in the editor gutter.
#[derive(Debug, Clone)]
pub struct BookmarkToggled {
    /// 1-based source line.
    pub line: usize,
    pub added: bool,
}

/// Emitted when the user Ctrl/Cmd+clicks a symbol in the source area, asking
/// the app to jump to its definition (F3.2 Ctrl+点击 跳转).
#[derive(Debug, Clone)]
pub struct DefinitionRequested {
    /// 1-based source line of the click.
    pub line: usize,
    /// 0-based column of the click (UTF-8 chars), as LSP expects.
    pub column: usize,
}

/// Emitted when the user right-clicks in the editor to open the breakpoint context menu.
#[derive(Debug, Clone)]
pub struct BreakpointContextMenu {
    /// 1-based source line.
    pub line: usize,
    /// Window-relative position for the menu.
    pub x: Pixels,
    pub y: Pixels,
    /// Identifier-like token under the cursor, used to prefill a watchpoint
    /// expression. Empty when the cursor is not on a token.
    pub word: String,
}

/// A rich diagnostic marker for hit-testing and hover tooltips.
#[derive(Debug, Clone, PartialEq)]
pub struct DiagnosticMark {
    /// 0-based source line.
    pub line: usize,
    /// 0-based character column where the underline starts.
    pub start_col: usize,
    /// 0-based character column where the underline ends (exclusive).
    pub end_col: usize,
    /// Human-readable message shown in the hover tooltip.
    pub message: String,
    /// `true` = error, `false` = warning.
    pub is_error: bool,
}

/// Emitted when the mouse hovers a diagnostic underline. `message` is `Some`
/// while over a mark and `None` when leaving all marks; `position` is the
/// window-relative mouse point for anchoring the tooltip.
#[derive(Debug, Clone)]
pub struct DiagnosticHover {
    pub message: Option<String>,
    pub position: Point<Pixels>,
}

/// Emitted when the mouse moves over a different identifier-like word.
/// `word` is `Some` while over a word and `None` on whitespace/punctuation;
/// `position` anchors the hover tooltip. Used for debug variable hover.
#[derive(Debug, Clone)]
pub struct HoverWord {
    pub word: Option<String>,
    pub position: Point<Pixels>,
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
        ToggleBookmark,
        ToggleFold,
        ToggleMinimap,


         AddCursorUp,
         AddCursorDown,
         ClearSecondary,
         SelectNextOccurrence,
        ExpandSelection,
        ShrinkSelection,
         ToggleComment,
        DuplicateLine,
        DeleteLine,
        MoveLineUp,
        MoveLineDown,
        TransformUppercase,
        TransformLowercase,
        JoinLines,
        SortLines,
        TrimTrailingWhitespace,
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
        KeyBinding::new("f2", ToggleBookmark, Some("Editor")),
        KeyBinding::new("ctrl-k ctrl-0", ToggleFold, Some("Editor")),
        KeyBinding::new("cmd-k cmd-m", ToggleMinimap, Some("Editor")),
        KeyBinding::new("ctrl-k ctrl-m", ToggleMinimap, Some("Editor")),
        KeyBinding::new("cmd-alt-up", AddCursorUp, Some("Editor")),
        KeyBinding::new("ctrl-alt-up", AddCursorUp, Some("Editor")),
        KeyBinding::new("cmd-alt-down", AddCursorDown, Some("Editor")),
        KeyBinding::new("ctrl-alt-down", AddCursorDown, Some("Editor")),
        KeyBinding::new("cmd-d", SelectNextOccurrence, Some("Editor")),
        KeyBinding::new("ctrl-d", SelectNextOccurrence, Some("Editor")),
         KeyBinding::new("escape", ClearSecondary, Some("Editor")),
        KeyBinding::new("cmd-/", ToggleComment, Some("Editor")),
        KeyBinding::new("ctrl-/", ToggleComment, Some("Editor")),
        KeyBinding::new("cmd-shift-k", DeleteLine, Some("Editor")),
        KeyBinding::new("ctrl-shift-k", DeleteLine, Some("Editor")),
        KeyBinding::new("cmd-shift-d", DuplicateLine, Some("Editor")),
        KeyBinding::new("ctrl-shift-d", DuplicateLine, Some("Editor")),
         KeyBinding::new("alt-up", MoveLineUp, Some("Editor")),
         KeyBinding::new("alt-down", MoveLineDown, Some("Editor")),
        KeyBinding::new("cmd-alt-shift-right", ExpandSelection, Some("Editor")),
        KeyBinding::new("ctrl-alt-shift-right", ExpandSelection, Some("Editor")),
        KeyBinding::new("cmd-alt-shift-left", ShrinkSelection, Some("Editor")),
        KeyBinding::new("ctrl-alt-shift-left", ShrinkSelection, Some("Editor")),
        KeyBinding::new("cmd-shift-u", TransformUppercase, Some("Editor")),
        KeyBinding::new("ctrl-shift-u", TransformUppercase, Some("Editor")),
        KeyBinding::new("cmd-alt-shift-l", TransformLowercase, Some("Editor")),
        KeyBinding::new("ctrl-alt-shift-l", TransformLowercase, Some("Editor")),
        KeyBinding::new("cmd-j", JoinLines, Some("Editor")),
        KeyBinding::new("ctrl-j", JoinLines, Some("Editor")),
        KeyBinding::new("cmd-shift-s", SortLines, Some("Editor")),
        KeyBinding::new("ctrl-shift-s", SortLines, Some("Editor")),
        KeyBinding::new("cmd-alt-t", TrimTrailingWhitespace, Some("Editor")),
        KeyBinding::new("ctrl-alt-t", TrimTrailingWhitespace, Some("Editor")),
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
    bookmarks: BTreeSet<usize>,
    debug_line: Option<usize>,
    scroll_y: f32,
    viewport_height: f32,
    dirty: bool,
    undo: Vec<String>,
    redo: Vec<String>,
    is_selecting: bool,
    /// In-editor find bar state.
    find_open: bool,
    find_query: String,
    find_replace: String,
    find_matches: Vec<Range<usize>>,
    find_current: usize,
    /// Folded regions: (start line, end line), 0-based inclusive.
    folded: Vec<(usize, usize)>,
    /// Find-bar query/replace text fields.
    find_query_field: Option<gpui::Entity<crate::text_field::TextField>>,
    find_replace_field: Option<gpui::Entity<crate::text_field::TextField>>,
    /// Match case exactly (vs. case-insensitive substring search).
    find_case_sensitive: bool,
    /// Only match whole words (non-alphanumeric/underscore boundaries).
    find_whole_word: bool,
    semantic: Vec<SemanticSpan>,
    /// Inlay hints from the LSP (0-based line, start char, label).
    inlay_hints: Vec<InlayHint>,
    /// Whether the right-edge minimap is rendered.
    show_minimap: bool,
    /// Additional cursors for multi-cursor editing (empty = single cursor).
    /// Each is a selection range (collapsed for a bare cursor).
    secondary: Vec<Range<usize>>,
    /// Diagnostic markers (0-based line + char range + message + severity),
    /// rendered as an underline plus a gutter tick; hovered to show a tooltip.
    diagnostics: Vec<DiagnosticMark>,
    /// Index into `diagnostics` currently under the mouse (for hover events).
    hovered_diagnostic: Option<usize>,
    /// Identifier under the mouse at the last `on_mouse_move` (for HoverWord).
    hovered_word: Option<String>,
    /// Line coverage from gcov (0-based line → hit count). `None`/empty means
    /// no coverage data for this file. Lines with `0` hits are uncovered.
    coverage: Vec<u64>,

    /// Editor zoom factor (1.0 = default). Scales font and line height.
    zoom: f32,
    indent_width: usize,
    /// Word wrap (soft wrap) toggle. When true, long lines break visually at
    /// the editor's right edge instead of scrolling horizontally. Defaults
    /// to false to preserve the existing horizontal-scroll behaviour.
    word_wrap: bool,
    /// Wrap rows: one entry per visual row, in document order. Each maps a
    /// visual row to its document line and the byte span shown on that row.
    /// Empty when wrap is off or not yet computed; rebuilt in prepaint.
    wrap_rows: Vec<WrapRow>,
    /// The viewport width (px) the wrap cache was computed at; a mismatch
    /// forces recomputation on the next prepaint.
    wrap_cache_width: f32,
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
            bookmarks: BTreeSet::new(),
            debug_line: None,
            scroll_y: 0.0,
            viewport_height: 0.0,
            dirty: false,

            zoom: 1.0,
            indent_width: 4,
            undo: Vec::new(),
            redo: Vec::new(),
            is_selecting: false,
            find_open: false,
            find_query: String::new(),
            find_replace: String::new(),
            find_matches: Vec::new(),
            find_current: 0,
            folded: Vec::new(),
            find_query_field: None,
            find_replace_field: None,
            find_case_sensitive: false,
            find_whole_word: false,
            semantic: Vec::new(),
            inlay_hints: Vec::new(),
            show_minimap: true,
             secondary: Vec::new(),
             diagnostics: Vec::new(),
            hovered_diagnostic: None,
            hovered_word: None,
            coverage: Vec::new(),
            word_wrap: false,
            wrap_rows: Vec::new(),
            wrap_cache_width: 0.0,
        }
    }




    /// Replace the whole buffer with `new_text` (e.g. reload from disk after an
    /// external change). Resets cursor/selection and edit history; breakpoints
    /// and diagnostic markers are kept — line values may shift with the reload.
    pub fn reload_text(&mut self, new_text: &str, cx: &mut Context<Self>) {
        let text = new_text.replace("\r\n", "\n").replace('\r', "\n");
        let cursor = text.len();
        self.text = text;
        self.cursor = cursor;
        self.selection = cursor..cursor;
        self.selection_reversed = false;
        self.preferred_column = None;
        self.marked_range.take();
        self.secondary.clear();
        self.undo.clear();
        self.redo.clear();
        self.find_matches.clear();
        self.find_current = 0;
        cx.notify();
    }

    /// Set the diagnostic markers. Markers are deduplicated by
    /// `(line, start_col, end_col)`; an error overrides a warning on the same
    /// range. Resets the hovered-mark state.
    pub fn set_diagnostics(&mut self, marks: Vec<DiagnosticMark>, cx: &mut Context<Self>) {
        self.diagnostics = dedup_diagnostic_marks(marks);
        self.hovered_diagnostic = None;
        cx.notify();
    }

    /// Set line coverage (0-based line → hit count). Lines absent from the
    /// vector are treated as uncovered; an empty vector clears the overlay.
    pub fn set_coverage(&mut self, hits: Vec<u64>, cx: &mut Context<Self>) {
        self.coverage = hits;
        cx.notify();
    }
 
     /// Set the semantic token overlays (0-based line, start char, length, kind index).
    pub fn set_semantic_tokens(&mut self, spans: Vec<SemanticSpan>, cx: &mut Context<Self>) {
        self.semantic = spans;
        cx.notify();
    }

    /// Set the inlay hints (0-based line, start char, label).
    pub fn set_inlay_hints(&mut self, hints: Vec<InlayHint>, cx: &mut Context<Self>) {
        // Deduplicate by (line, start) so overlapping LSP responses (a slow
        // request resolving after a newer one) never render a hint twice.
        let mut seen = std::collections::HashSet::new();
        let mut unique = Vec::with_capacity(hints.len());
        for hint in hints {
            if seen.insert((hint.line, hint.start)) {
                unique.push(hint);
            }
        }
        self.inlay_hints = unique;
        cx.notify();
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Return the text of a 0-based `line` (without the trailing newline), or
    /// `None` if the line is out of bounds.
    pub fn line_text(&self, line: usize) -> Option<&str> {
        let starts = line_starts(&self.text);
        let start = *starts.get(line)?;
        let end = starts.get(line + 1).copied().unwrap_or(self.text.len());
        Some(&self.text[start..end])
    }


    pub fn selected_text(&self) -> String {
        if self.selection.is_empty() {
            String::new()
        } else {
            self.text[self.selection.clone()].to_string()
        }
    }

    /// The identifier-like token under the cursor (letters, digits, `_`, `.`,
    /// `->`, `[`, `]`), trimmed to its edges. Used to propose watchpoint
    /// expressions and watch-expression evaluation.
    pub fn word_under_cursor(&self) -> String {
        let is_word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '.' | '[' | ']' | '-');
        let before = self.text[..self.cursor]
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_word(*c))
            .last()
            .map(|(index, _)| index)
            .unwrap_or(self.cursor);
        let after = self.text[self.cursor..]
            .char_indices()
            .take_while(|(_, c)| is_word(*c))
            .last()
            .map(|(index, c)| self.cursor + index + c.len_utf8())
            .unwrap_or(self.cursor);
        let mut token = &self.text[before..after];
        token = token
            .trim_start_matches(|c: char| matches!(c, '.' | '-' | '[' | ']'))
            .trim_end_matches(|c: char| matches!(c, '.' | '-' | '[' | ']'));
        token.to_string()
    }

    /// Byte range of the word (letters/digits/underscore) at `offset`, or
    /// `None` when the offset lands on whitespace/punctuation. Used for
    /// double-click word selection.
    fn word_range_at(&self, offset: usize) -> Option<Range<usize>> {
        let text = &self.text;
        let bytes = text.as_bytes();
        let offset = offset.min(text.len());
        let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        if offset >= text.len() || !is_word(bytes[offset]) {
            // Only the very end of the document (no trailing newline) may sit
            // one past the last word's final byte; clicking on real whitespace
            // between words selects nothing.
            if offset == text.len() && offset > 0 && is_word(bytes[offset - 1]) {
                let mut start = offset - 1;
                while start > 0 && is_word(bytes[start - 1]) {
                    start -= 1;
                }
                return Some(start..offset);
            }
            return None;
        }
        let mut start = offset;
        while start > 0 && is_word(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = offset + 1;
        while end < text.len() && is_word(bytes[end]) {
            end += 1;
        }
        Some(start..end)
    }

    /// Byte range of the line (including the trailing newline) containing
    /// `offset`. Used for triple-click line selection.
    fn line_range_at(&self, offset: usize) -> Range<usize> {
        let text = &self.text;
        let offset = offset.min(text.len());
        let start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let end = text[offset..]
            .find('\n')
            .map(|i| offset + i + 1)
            .unwrap_or(text.len());
        start..end
    }
    /// Apply an LSP-style text edit (0-based line/character range) to the
    /// document, replacing the range with `new_text`. Used by `WorkspaceEdit`
    /// application after a symbol rename.
    pub fn apply_text_edit(
        &mut self,
        start_line: usize,
        start_char: usize,
        end_line: usize,
        end_char: usize,
        new_text: &str,
    ) {
        let starts = line_starts(&self.text);
        // Reject edits that reference lines past the document.
        if start_line >= starts.len() || end_line >= starts.len() {
            return;
        }
        let start = offset_for_line_char(&self.text, &starts, start_line, start_char);
        let end = offset_for_line_char(&self.text, &starts, end_line, end_char);
        if start > end || end > self.text.len() {
            return;
        }
        self.text = self.text[0..start].to_owned() + new_text + &self.text[end..];
        self.cursor = clamp_boundary(&self.text, start + new_text.len());
        self.selection = self.cursor..self.cursor;
        self.secondary.clear();
        self.dirty = true;
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    /// Current zoom factor (1.0 = default).
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// Set the zoom factor, clamped to 0.5..=2.0, and notify.
    pub fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(0.5, 2.0);
        if (zoom - self.zoom).abs() < f32::EPSILON {
            return;
        }
        self.zoom = zoom;
        cx.notify();
    }

    /// Effective line height at the current zoom.
    fn line_height(&self) -> f32 {
        LINE_HEIGHT * self.zoom
    }

    /// Effective editor font size at the current zoom.
    fn font_size(&self) -> f32 {
        FONT_SIZE * self.zoom
    }

    /// Current indent width in spaces (2/4/8).
    pub fn indent_width(&self) -> usize {
        self.indent_width
    }

    /// Set the indent width (clamped to 2/4/8) and notify. The editor's
    /// existing text is left untouched; only new Tab/auto-indent steps use it.
    pub fn set_indent_width(&mut self, width: usize, cx: &mut Context<Self>) {
        let width = match width {
            2 | 4 | 8 => width,
            _ => 4,
        };
        if width == self.indent_width {
            return;
        }
        self.indent_width = width;
        cx.notify();
    }

    /// Whether soft word wrap is enabled.
    pub fn word_wrap(&self) -> bool {
        self.word_wrap
    }

    /// Enable or disable soft word wrap and invalidate the wrap cache.
    pub fn set_word_wrap(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.word_wrap == enabled {
            return;
        }
        self.word_wrap = enabled;
        self.wrap_rows.clear();
        self.wrap_cache_width = 0.0;
        cx.notify();
    }

    /// The whitespace string inserted for one indent level.
    fn indent_string(&self) -> String {
        " ".repeat(self.indent_width)
    }

    fn content_height(&self) -> f32 {
        self.visual_row_count().max(1) as f32 * self.line_height()
    }

    fn max_scroll(&self) -> f32 {
        (self.content_height() - self.viewport_height).max(0.0)
    }

    fn clamp_scroll(&mut self) {
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll());
    }

    /// Scroll so that `ratio` (0.0-1.0) of the document height is at the top
    /// of the viewport. Used by minimap clicks and drags.
    pub fn scroll_to_ratio(&mut self, ratio: f32, cx: &mut Context<Self>) {
        let target = self.max_scroll() * ratio.clamp(0.0, 1.0);
        self.scroll_y = target;
        self.clamp_scroll();
        cx.notify();
    }

    /// Current vertical scroll position as a 0.0-1.0 ratio of the document
    /// height. Used to persist/restore the viewport across sessions.
    pub fn scroll_ratio(&self) -> f32 {
        let max = self.max_scroll();
        if max <= 0.0 {
            0.0
        } else {
            (self.scroll_y / max).clamp(0.0, 1.0)
        }
    }

    /// Scroll the viewport so `line_index` (0-based) is visible.
    fn reveal_line(&mut self, line_index: usize) {
        let line_top = line_index as f32 * self.line_height();
        if line_top < self.scroll_y {
            self.scroll_y = line_top;
        } else if line_top + self.line_height() > self.scroll_y + self.viewport_height {
            self.scroll_y = (line_top + self.line_height() - self.viewport_height).max(0.0);
        }
        self.clamp_scroll();
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(px(self.line_height()));
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

    /// The set of breakpoint lines (1-based) currently set in this editor.
    pub fn breakpoint_lines(&self) -> BTreeSet<usize> {
        self.breakpoints.clone()
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

    /// Set the bookmark lines (1-based) shown in the gutter.
    pub fn set_bookmarks(
        &mut self,
        lines: impl IntoIterator<Item = usize>,
        cx: &mut Context<Self>,
    ) {
        self.bookmarks = lines.into_iter().collect();
        cx.notify();
    }


    fn toggle_bookmark_line(&mut self, line: usize, cx: &mut Context<Self>) {
        if line == 0 {
            return;
        }
        let added = if self.bookmarks.remove(&line) {
            false
        } else {
            self.bookmarks.insert(line);
            true
        };
        cx.emit(BookmarkToggled { line, added });
        cx.notify();
    }
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let (line, column) = self.line_col(self.cursor);
        (line + 1, column + 1)
    }


    /// Insert a completion at the cursor. When `replace_prefix` is true the
    /// word prefix before the cursor (letters/digits/underscore, as typed by
    /// the user) is removed first, so confirming `printf` over `pri` yields
    /// `printf` not `priprintf`. `cursor_at` gives the byte offset within
    /// `text` where the cursor should end (a snippet `$0` position); `None`
    /// places the cursor after the inserted text.
    pub fn insert_completion(
        &mut self,
        text: &str,
        replace_prefix: bool,
        cursor_at: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        if text.is_empty() {
            return;
        }
        // Collect ranges: the primary cursor (optionally extended to cover the
        // typed word prefix) plus every secondary cursor. Each is replaced by
        // the completion text; the primary cursor ends at `cursor_at`.
        let mut ranges: Vec<Range<usize>> = Vec::new();
        let mut primary_start = self.cursor;
        if replace_prefix {
            let before = &self.text[..self.cursor];
            primary_start = before
                .char_indices()
                .rev()
                .find(|(_, c)| !(c.is_alphanumeric() || *c == '_'))
                .map(|(idx, _)| idx + 1)
                .unwrap_or(0);
        }
        let primary_range = clamp_range(&self.text, primary_start..self.cursor);
        ranges.push(primary_range.clone());
        for extra in &self.secondary {
            let extra = clamp_range(&self.text, extra.clone());
            if !ranges.iter().any(|r| r.start == extra.start && r.end == extra.end) {
                ranges.push(extra);
            }
        }
        self.snapshot();
        ranges.sort_by_key(|r| std::cmp::Reverse(r.start));
        for r in &ranges {
            self.text = self.text[0..r.start].to_owned() + text + &self.text[r.end..];
        }
        let cursor = primary_range.start + cursor_at.unwrap_or(text.len()).min(text.len());
        self.cursor = clamp_boundary(&self.text, cursor);
        self.selection = self.cursor..self.cursor;
        self.secondary.clear();
        self.selection_reversed = false;
        self.preferred_column = None;
        self.marked_range.take();
        self.dirty = true;
        cx.notify();
    }
    pub fn set_cursor_line(&mut self, line_number: usize, cx: &mut Context<Self>) {
        let starts = line_starts(&self.text);
        let line = line_number.saturating_sub(1).min(starts.len().saturating_sub(1));
        let offset = starts.get(line).copied().unwrap_or(0);
        self.reveal_line(line);
        self.move_to(offset, cx);
        cx.notify();
    }

    /// Move the cursor to a 1-based `line` and 0-based `column`, clamping to
    /// the document bounds. Used by navigation-history jumps.
    pub fn set_cursor_line_column(&mut self, line: usize, column: usize, cx: &mut Context<Self>) {
        let starts = line_starts(&self.text);
        let line = line.saturating_sub(1).min(starts.len().saturating_sub(1));
        let line_start = starts.get(line).copied().unwrap_or(0);
        let line_end = starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.text.len());
        // Clamp the column to the line length (excluding the trailing newline).
        let line_len = line_end.saturating_sub(line_start);
        let col = column.min(line_len);
        self.reveal_line(line);
        self.move_to(line_start + col, cx);
        cx.notify();
    }

    /// Toggle the right-edge minimap.
    pub fn toggle_minimap(&mut self, cx: &mut Context<Self>) {
        self.show_minimap = !self.show_minimap;
        cx.notify();
    }

    /// Add a cursor on the line above or below the current one, at the same
    /// character column. Does nothing if the target line is out of bounds.
    pub fn add_cursor_vertical(&mut self, delta: i32, cx: &mut Context<Self>) {
        let starts = line_starts(&self.text);
        let (cur_line, cur_col) = self.line_col(self.cursor);
        let target = cur_line as i32 + delta;
        if target < 0 || target as usize >= starts.len() {
            return;
        }
        let target_line = target as usize;
        let line_start = starts[target_line];
        let line_end = starts
            .get(target_line + 1)
            .copied()
            .unwrap_or(self.text.len());
        // Clamp the column to the target line's character count.
        let col = cur_col.min(line_end.saturating_sub(line_start));
        let offset = self.offset_for_line_char(target_line, col, &starts);
        let offset = clamp_boundary(&self.text, offset);
        // Dedupe: an existing cursor already at this offset (or a selection
        // spanning it) should not be added twice. `Range::contains` is false
        // for empty ranges, so compare endpoints directly.
        let exists = self
            .secondary
            .iter()
            .any(|r| r.start <= offset && offset <= r.end);
        if !exists {
            self.secondary.push(offset..offset);
        }
        cx.notify();
    }

    /// Remove all secondary (multi-) cursors.
    pub fn clear_secondary(&mut self, cx: &mut Context<Self>) {
        if !self.secondary.is_empty() {
            self.secondary.clear();
            cx.notify();
        }
    }
    /// Select the next occurrence of the current word/selection (`Cmd/Ctrl+D`).
    ///
    /// The first press selects the word under the cursor (or the current
    /// selection, when non-empty). Each further press adds the next
    /// non-overlapping occurrence: the previously selected occurrence becomes
    /// a secondary selection and the primary cursor moves onto the new one,
    /// wrapping to the start after the last. Typing then edits every selected
    /// occurrence at once (`replace_text_in_range` applies to `secondary`
    /// ranges too). A selection change is not an edit, so it does not push an
    /// undo entry.
    pub fn select_next_occurrence(&mut self, cx: &mut Context<Self>) {
        let search = if self.selection.is_empty() {
            let word = self.word_under_cursor();
            if word.is_empty() {
                return;
            }
            word
        } else {
            self.text[self.selection.clone()].to_string()
        };
        if search.is_empty() {
            return;
        }
        // Ranges already selected: the primary selection plus any secondary
        // selection that is a real (non-collapsed) range.
        let mut selected: Vec<Range<usize>> = Vec::new();
        if !self.selection.is_empty() {
            selected.push(self.selection.clone());
        }
        for extra in &self.secondary {
            if !extra.is_empty() {
                selected.push(extra.clone());
            }
        }
        let last_end = selected
            .iter()
            .map(|r| r.end)
            .max()
            .unwrap_or(self.cursor);
         let mut found = find_occurrence_after(&self.text, &search, last_end, &selected);
         if found.is_none() {
             found = find_occurrence_after(&self.text, &search, 0, &selected);
         }
         let Some(range) = found else {
            // Every occurrence is already selected: reset to the first one
            // only (VS Code's wrap behaviour), so repeated presses cycle.
            let first = find_occurrence_after(&self.text, &search, 0, &[]);
            if let Some(first) = first {
                self.secondary.clear();
                self.selection = first.clone();
                self.cursor = first.end;
                self.selection_reversed = false;
                self.preferred_column = None;
                self.marked_range = None;
                let (line, _) = self.line_col(first.start);
                self.reveal_line(line);
                cx.notify();
            }
            return;
         };
        // Move the previous primary selection into `secondary` (if any), then
        // make the new occurrence the primary selection/cursor.
        if !self.selection.is_empty() {
            self.secondary.push(self.selection.clone());
        }
        self.selection = range.clone();
        self.cursor = range.end;
        self.selection_reversed = false;
        self.preferred_column = None;
        self.marked_range = None;
        let (line, _) = self.line_col(range.start);
        self.reveal_line(line);
        cx.notify();
    }



    /// Fold or unfold the region at `line` (0-based). Returns whether any fold
    /// state changed.
    pub fn toggle_fold(&mut self, line: usize, cx: &mut Context<Self>) {
        if let Some(pos) = self.folded.iter().position(|(start, _)| *start == line) {
            self.folded.remove(pos);
            cx.notify();
            return;
        }
        for (start, end) in self.fold_regions() {
            if start == line && end > line {
                self.folded.push((start, end));
                cx.notify();
                return;
            }
        }
    }


    /// Map a document line to its visible line, accounting for folded regions.
    /// Returns `(visible_line, is_hidden)`.
    pub fn visible_line(&self, line: usize) -> (usize, bool) {
        visible_line_in(&self.text, &self.folded, line)
    }

    pub fn visible_line_count(&self) -> usize {
        visible_lines_in(&self.text, &self.folded).len()
    }

    /// Total visual rows under word wrap (each wrapped segment is one row).
    /// When wrap is off this equals `visible_line_count()`.
    pub fn visual_row_count(&self) -> usize {
        if !self.word_wrap || self.wrap_rows.is_empty() {
            return self.visible_line_count();
        }
        self.wrap_rows.len()
    }

    /// Map a document byte offset to its visual row index under word wrap, or
    /// the document-line visible row when wrap is off. Returns `(row, in_row)`
    /// where `in_row` is the byte offset relative to the row's slice start.
    pub fn visual_position(&self, offset: usize) -> (usize, usize) {
        if self.word_wrap && !self.wrap_rows.is_empty() {
            for (row, wr) in self.wrap_rows.iter().enumerate() {
                if offset >= wr.byte_start && offset <= wr.byte_end {
                    return (row, offset - wr.byte_start);
                }
            }
            // Beyond the last row: use the final row.
            let last = self.wrap_rows.len() - 1;
            return (last, offset - self.wrap_rows[last].byte_start);
        }
        let (line, column) = line_col_in(&self.text, &line_starts(&self.text), offset);
        let (visible, _) = self.visible_line(line);
        (visible, column)
    }

    /// The visual row index of the first row of a document line, or `None` if
    /// the line is hidden by a fold. Used to place gutter markers (breakpoints,
    /// diagnostics, coverage) on the correct visual row.
    pub fn visual_row_for_line(&self, line: usize) -> Option<usize> {
        if self.word_wrap && !self.wrap_rows.is_empty() {
            return self
                .wrap_rows
                .iter()
                .position(|wr| wr.line == line);
        }
        let (visible, hidden) = self.visible_line(line);
        if hidden {
            None
        } else {
            Some(visible)
        }
    }

    /// Number of logical lines in the document (1-based count; empty text = 1).
    pub fn line_count(&self) -> usize {
        line_starts(&self.text).len().max(1)
    }

    /// Return the list of document line indices that should be drawn.
    pub fn visible_lines(&self) -> Vec<usize> {
        visible_lines_in(&self.text, &self.folded)
    }

    pub fn find_query_field_entity(&self) -> Option<gpui::Entity<crate::text_field::TextField>> {
        self.find_query_field.clone()
    }

    pub fn find_replace_field_entity(&self) -> Option<gpui::Entity<crate::text_field::TextField>> {
        self.find_replace_field.clone()
    }

    pub fn find_case_sensitive(&self) -> bool {
        self.find_case_sensitive
    }

    pub fn find_whole_word(&self) -> bool {
        self.find_whole_word
    }

    pub fn toggle_find_case_sensitive(&mut self, cx: &mut Context<Self>) {
        self.find_case_sensitive = !self.find_case_sensitive;
        self.refresh_find(cx);
        cx.notify();
    }

    pub fn toggle_find_whole_word(&mut self, cx: &mut Context<Self>) {
        self.find_whole_word = !self.find_whole_word;
        self.refresh_find(cx);
        cx.notify();
    }

    pub fn find_open(&self) -> bool {
        self.find_open
    }
    /// Open the in-editor find bar, seeding the query from the current selection.
    pub fn open_find(&mut self, cx: &mut Context<Self>) {
        if !self.selection.is_empty() && self.find_query.is_empty() {
            self.find_query = self.text[self.selection.clone()].to_string();
        }
        if self.find_query_field.is_none() {
            let entity = cx.entity();
            let field = cx.new(|cx| {
                let mut field = crate::text_field::TextField::new("查找", cx);
                field.set_on_change(move |text, _window, _cx| {
                    entity.update(_cx, |editor, cx| {
                        editor.set_find_query(text, cx);
                    });
                });
                field
            });
            field.update(cx, |field, _cx| field.set_text(self.find_query.clone(), _cx));
            self.find_query_field = Some(field);
        }
        if self.find_replace_field.is_none() {
            let entity = cx.entity();
            let field = cx.new(|cx| {
                let mut field = crate::text_field::TextField::new("替换为", cx);
                field.set_on_change(move |text, _window, _cx| {
                    entity.update(_cx, |editor, cx| {
                        editor.set_find_replace(text, cx);
                    });
                });
                field
            });
            self.find_replace_field = Some(field);
        }
        self.find_open = true;
        self.refresh_find(cx);
        cx.notify();
    }

    pub fn close_find(&mut self, cx: &mut Context<Self>) {
        self.find_open = false;
        self.find_matches.clear();
        self.find_current = 0;
        cx.notify();
    }



    pub fn find_match_count(&self) -> usize {
        self.find_matches.len()
    }

    pub fn find_current(&self) -> usize {
        self.find_current
    }

    pub fn set_find_query(&mut self, query: &str, cx: &mut Context<Self>) {
        self.find_query = query.to_string();
        self.refresh_find(cx);
        cx.notify();
    }

    pub fn set_find_replace(&mut self, replacement: &str, cx: &mut Context<Self>) {
        self.find_replace = replacement.to_string();
        cx.notify();
    }

    /// Recompute all match ranges for the current query, in document order,
    /// honouring the case-sensitivity and whole-word options.
    fn refresh_find(&mut self, cx: &mut Context<Self>) {
        self.find_matches.clear();
        self.find_current = 0;
        if self.find_query.is_empty() {
            cx.notify();
            return;
        }
        self.find_matches = find_matches_in(
            &self.text,
            &self.find_query,
            self.find_case_sensitive,
            self.find_whole_word,
        );
    }

    /// Move to the next match (or the previous if `backwards`).
    pub fn find_next(&mut self, backwards: bool, cx: &mut Context<Self>) {
        if self.find_matches.is_empty() {
            return;
        }
        if backwards {
            if self.find_current == 0 {
                self.find_current = self.find_matches.len() - 1;
            } else {
                self.find_current -= 1;
            }
        } else {
            self.find_current = (self.find_current + 1) % self.find_matches.len();
        }
        let range = self.find_matches[self.find_current].clone();
        let (line, _) = self.line_col(range.start);
        self.cursor = range.start;
        self.selection = range;
        self.selection_reversed = false;
        self.preferred_column = None;
        self.reveal_line(line);
        cx.notify();
    }

    /// Replace the currently-selected match with the replacement text.
    pub fn find_replace_current(&mut self, cx: &mut Context<Self>) {
        if self.find_matches.is_empty() {
            return;
        }
        let range = self.find_matches[self.find_current].clone();
        let replacement = self.find_replace.clone();
        self.snapshot();
        self.text = self.text[0..range.start].to_owned()
            + &replacement
            + &self.text[range.end..];
        self.dirty = true;
        self.refresh_find(cx);
        let count = self.find_matches.len();
        if count == 0 {
            self.cursor = clamp_boundary(&self.text, range.start + replacement.len());
            self.selection = self.cursor..self.cursor;
        } else {
            self.find_current = self.find_current.min(count - 1);
            let next = self.find_matches[self.find_current].clone();
            let (line, _) = self.line_col(next.start);
            self.cursor = next.start;
            self.selection = next;
            self.reveal_line(line);
        }
        cx.notify();
    }

    /// Replace every match with the replacement text.
    pub fn find_replace_all(&mut self, cx: &mut Context<Self>) {
        if self.find_matches.is_empty() {
            return;
        }
        let replacement = self.find_replace.clone();
        self.snapshot();
        let mut result = String::with_capacity(self.text.len());
        let mut last = 0;
        for range in self.find_matches.iter() {
            result.push_str(&self.text[last..range.start]);
            result.push_str(&replacement);
            last = range.end;
        }
        result.push_str(&self.text[last..]);
        self.text = result;
        self.dirty = true;
        self.refresh_find(cx);
        self.cursor = clamp_boundary(&self.text, self.cursor);
        self.selection = self.cursor..self.cursor;
        cx.notify();
    }
    /// Offset of the bracket matching the bracket at `offset`, if any.
    ///
    /// `offset` is the byte offset of the opening/closing bracket. Returns the
    /// offset of its partner. Handles `()`, `[]`, `{}` and respects nesting by
    /// scanning outward.
    fn matching_bracket(&self, offset: usize) -> Option<usize> {
        matching_bracket_in(&self.text, offset)
    }

    /// The bracket pair around the cursor (open/close offsets), if the cursor is
    /// adjacent to a bracket or inside a pair.
    pub fn bracket_pair_around_cursor(&self) -> Option<(usize, usize)> {
        // The cursor may sit right after an opening bracket or before a closing one.
        let candidates = [
            self.cursor.checked_sub(1),
            Some(self.cursor),
            Some(self.cursor),
        ];
        for candidate in candidates {
            let Some(offset) = candidate else { continue };
            if offset >= self.text.len() {
                continue;
            }
            if let Some(partner) = self.matching_bracket(offset) {
                return Some((offset.min(partner), offset.max(partner)));
            }
        }
        None
    }

    /// Compute fold regions for the document based on indentation.
    ///
    /// Each region is the 0-based start line (where the fold marker appears) and
    /// the last line contained. Lines that are blank or less indented than the
    /// following non-blank line begin a region.
    pub fn fold_regions(&self) -> Vec<(usize, usize)> {
        fold_regions_in(&self.text)
    }

    /// Current folded regions as `(start, end)` pairs (0-based inclusive).
    pub fn folded_regions(&self) -> Vec<(usize, usize)> {
        self.folded.clone()
    }

    /// Restore folded regions from persisted session state. Regions outside the
    /// document are dropped; the fold state is clamped to the line count.
    pub fn set_folded_regions(&mut self, regions: Vec<(usize, usize)>, cx: &mut Context<Self>) {
        let total = self.text.split('\n').count();
        self.folded = clamp_folded_regions(total, regions);
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
        // Auto-indent: inherit the current line's leading whitespace, plus one
        // indent level if the line ends with an opening brace/paren/bracket.
        let (line, _) = self.line_col(self.cursor);
        let indent = self
            .line_text(line)
            .map(|line| auto_indent_for_line(line, self.indent_width))
            .unwrap_or_default();
        self.replace_text_in_range(None, &format!("\n{indent}"), window, cx);
    }

    fn tab(&mut self, _: &Tab, window: &mut Window, cx: &mut Context<Self>) {
        let selection = clamp_range(&self.text, self.selection.clone());
        if selection.is_empty() {
            self.replace_text_in_range(None, &self.indent_string(), window, cx);
            return;
        }
        let (start_line, _) = self.line_col(selection.start);
        let (end_line, _) = self.line_col(selection.end);
        if start_line == end_line {
            self.replace_text_in_range(None, &self.indent_string(), window, cx);
            return;
        }
        self.indent_lines(start_line, end_line, cx);
    }

    /// Indent the start of every line in `start_line..=end_line` by one level.
    /// Kept separate from `tab` so tests can exercise it without a `Window`.
    fn indent_lines(&mut self, start_line: usize, end_line: usize, cx: &mut Context<Self>) {
        self.snapshot();
        let starts = line_starts(&self.text);
        let mut line_offsets: Vec<usize> = Vec::new();
        for line in start_line..=end_line {
            if let Some(&start) = starts.get(line) {
                line_offsets.push(start);
            }
        }
        line_offsets.sort_by_key(|start| std::cmp::Reverse(*start));
        let indent = self.indent_string();

        for start in &line_offsets {
            self.text.insert_str(*start, &indent);
        }
        self.cursor = (self.cursor + self.indent_width * line_offsets.len()).min(self.text.len());
        self.selection = self.cursor..self.cursor;
         self.dirty = true;
         cx.notify();
     }
 
    /// Toggle line comments over the selection (or the current line when the
    /// selection is empty or single-line). If every targeted line is already
    /// commented, remove the comment prefix; otherwise add it. Uses the
    /// language's `//` prefix; assembly/unknown languages are a no-op.
     fn toggle_comment(&mut self, cx: &mut Context<Self>) {
        let Some(prefix) = self.language.and_then(|lang| lang.line_comment()) else {
            return;
        };
        let selection = clamp_range(&self.text, self.selection.clone());
        let (start_line, _) = self.line_col(selection.start);
        let (end_line, _) = self.line_col(selection.end);
        let starts = self.line_starts();
        let lines: Vec<usize> = (start_line..=end_line)
            .filter(|line| *line < starts.len())
            .collect();
        if lines.is_empty() {
            return;
        }
         // Decide: comment if any targeted line is not already commented.
         // Blank lines are ignored (they are not commented and do not count
         // against an all-commented decision).
         let non_blank: Vec<usize> = lines
             .iter()
            .copied()
            .filter(|line| {
                self.line_text(*line)
                    .map(|text| !text.trim().is_empty())
                    .unwrap_or(false)
            })
             .collect();
        let all_commented = non_blank.iter().all(|line| {
             self.line_text(*line)
                 .map(|text| text.trim_start().starts_with(prefix))
                 .unwrap_or(false)
         });
         self.snapshot();
        if all_commented {
            // Remove `//` from each line (after leading whitespace).
            let mut offsets: Vec<usize> = Vec::new();
            for line in &lines {
                let Some(text) = self.line_text(*line) else {
                    continue;
                };
                let leading = text.len() - text.trim_start().len();
                let at = starts[*line] + leading;
                if text[leading..].starts_with(prefix) {
                    offsets.push(at);
                }
            }
             offsets.sort_by_key(|offset| std::cmp::Reverse(*offset));
             for offset in offsets {
                // Remove the prefix plus one trailing space (the `// ` we add).
                let mut end = (offset + prefix.len()).min(self.text.len());
                if self.text[end..].starts_with(' ') {
                    end += 1;
                }
                 self.text.replace_range(offset..end, "");
             }
            self.cursor = self.cursor.min(self.text.len());
        } else {
            // Insert `// ` at the start of each targeted line (after indent).
             let mut inserts: Vec<usize> = Vec::new();
             for line in &lines {
                 if let Some(start) = starts.get(*line) {
                     let text = self.line_text(*line).unwrap_or("");
                    // Skip blank lines: commenting an empty line would leave a
                    // dangling `// ` with no content.
                    if text.trim().is_empty() {
                        continue;
                    }
                     let leading = text.len() - text.trim_start().len();
                     inserts.push(*start + leading);
                 }
             }
            inserts.sort_by_key(|offset| std::cmp::Reverse(*offset));
            let mut added = 0;
            for offset in inserts {
                self.text.insert_str(offset, &format!("{prefix} "));
                added += prefix.len() + 1;
            }
            self.cursor = (self.cursor + added).min(self.text.len());
        }
        self.selection = self.cursor..self.cursor;
        self.secondary.clear();
        self.dirty = true;
        cx.notify();
    }

    /// Duplicate the current line (or every line covered by the selection),
    /// placing the copy below the original.
     fn duplicate_line(&mut self, cx: &mut Context<Self>) {
        let selection = clamp_range(&self.text, self.selection.clone());
        let (start_line, _) = self.line_col(selection.start);
        let (end_line, _) = self.line_col(selection.end);
        let starts = self.line_starts();
        if start_line >= starts.len() {
            return;
        }
        let first_start = starts[start_line];
        let last_end = self.line_end(end_line, &starts);
        // The duplicated block includes its trailing newline when one exists;
        // a final unterminated line is duplicated with a newline before it.
        let (chunk, insert_at) = if let Some(&next) = starts.get(end_line + 1) {
            if next > last_end && self.text[last_end..next].starts_with('\n') {
                (self.text[first_start..next].to_string(), next)
            } else {
                (self.text[first_start..last_end].to_string(), last_end)
            }
        } else {
            (format!("\n{}", &self.text[first_start..last_end]), last_end)
        };
        self.snapshot();
        self.text.insert_str(insert_at, &chunk);
        self.cursor = insert_at.min(self.text.len());
        self.selection = self.cursor..self.cursor;
        self.secondary.clear();
        self.dirty = true;
        cx.notify();
    }

    /// Delete the current line (or every line covered by the selection).
     fn delete_line(&mut self, cx: &mut Context<Self>) {
        let selection = clamp_range(&self.text, self.selection.clone());
        let (start_line, _) = self.line_col(selection.start);
        let (end_line, _) = self.line_col(selection.end);
        let starts = self.line_starts();
        if start_line >= starts.len() {
            return;
        }
        let first_start = starts[start_line];
        let mut last_end = self.line_end(end_line, &starts);
        // Consume the trailing newline of the last selected line.
        if let Some(&next) = starts.get(end_line + 1) {
            if next > last_end && self.text[last_end..next].starts_with('\n') {
                last_end = next;
            }
        } else if self.text.len() > last_end {
            last_end = self.text.len();
        }
        self.snapshot();
        self.text.replace_range(first_start..last_end, "");
        self.cursor = first_start.min(self.text.len());
        self.selection = self.cursor..self.cursor;
        self.secondary.clear();
        self.dirty = true;
        cx.notify();
    }

    /// Move the current line (or the selected block) up/down by `delta` lines,
    /// swapping with the adjacent line(s). `delta` is -1 (up) or 1 (down).
     fn move_line(&mut self, delta: isize, cx: &mut Context<Self>) {
         let selection = clamp_range(&self.text, self.selection.clone());
         let (start_line, _) = self.line_col(selection.start);
         let (end_line, _) = self.line_col(selection.end);
         let starts = self.line_starts();
        // The trailing newline creates a final empty "line" entry; the last
        // real content line is one before it when the text ends with `\n`.
        let content_last_line = if self.text.ends_with('\n') {
            starts.len().saturating_sub(2)
        } else {
            starts.len().saturating_sub(1)
        };
         if start_line >= starts.len() {
             return;
         }
         if delta < 0 && start_line == 0 {
             return;
         }
        if delta > 0 && end_line >= content_last_line {
             return;
         }
         let first_start = starts[start_line];
         let mut last_end = self.line_end(end_line, &starts);
         if let Some(&next) = starts.get(end_line + 1) {
             if next > last_end && self.text[last_end..next].starts_with('\n') {
                 last_end = next;
             }
         }
         let block = self.text[first_start..last_end].to_string();
         if delta < 0 {
             // Swap with the line above: previous block then moving block.
             let prev_start = starts[start_line - 1];
             let prev_end = starts[start_line];
             let prev_block = self.text[prev_start..prev_end].to_string();
             let moving_end = first_start + block.len();
             self.snapshot();
             let mut new_text = String::with_capacity(self.text.len());
             new_text.push_str(&self.text[..prev_start]);
             new_text.push_str(&block);
             new_text.push_str(&prev_block);
             new_text.push_str(&self.text[moving_end..]);
             self.text = new_text;
             self.cursor = prev_start;
         } else {
             // Swap with the line below: next block then moving block.
             let next_start = starts[end_line + 1];
             let mut next_end = self.line_end(end_line + 1, &starts);
             if let Some(&after) = starts.get(end_line + 2) {
                 if after > next_end && self.text[next_end..after].starts_with('\n') {
                     next_end = after;
                 }
             }
             let next_block = self.text[next_start..next_end].to_string();
             self.snapshot();
             let mut new_text = String::with_capacity(self.text.len());
             new_text.push_str(&self.text[..first_start]);
             new_text.push_str(&next_block);
             new_text.push_str(&block);
            new_text.push_str(&self.text[next_end..]);
             self.text = new_text;
             self.cursor = first_start + next_block.len();
         }
         self.cursor = self.cursor.min(self.text.len());
         self.selection = self.cursor..self.cursor;
         self.secondary.clear();
         self.dirty = true;
         cx.notify();
     }
 
    /// Expand (or shrink) the current selection to the next word boundary on
    /// the active side. `expand` grows toward the cursor's end (right) or
    /// start (left); shrinking pulls the selection back to the prior boundary.
    fn expand_or_shrink_selection(&mut self, expand: bool, cx: &mut Context<Self>) {
        let selection = clamp_range(&self.text, self.selection.clone());
        if expand {
            // Grow the selection by one word boundary outward. If the cursor
            // is at the end, extend right; otherwise extend left.
            let at_end = self.cursor == selection.end && !self.selection_reversed;
            if at_end {
                let new_end = self.next_word_boundary(selection.end);
                self.selection = selection.start..new_end;
                self.cursor = new_end;
            } else {
                let new_start = self.previous_word_boundary(selection.start);
                self.selection = new_start..selection.end;
                self.cursor = new_start;
            }
        } else {
            // Shrink: pull the cursor-side boundary back one word.
            let at_end = self.cursor == selection.end;
             if at_end {
                 let new_end = self.previous_word_end(selection.end);
                 if new_end > selection.start {
                     self.selection = selection.start..new_end;
                     self.cursor = new_end;
                 }
             } else {
                 let new_start = self.next_word_start(selection.start);
                 if new_start < selection.end {
                     self.selection = new_start..selection.end;
                     self.cursor = new_start;
                 }
             }
         }
        self.selection_reversed = false;
        self.secondary.clear();
        cx.notify();
    }

    /// The next word boundary strictly after `offset`: the end of the word
    /// containing `offset`, or the end of the next word when `offset` sits on a
    /// delimiter. Returns the text length when no further word exists.
    fn next_word_boundary(&self, offset: usize) -> usize {
        let bytes = self.text.as_bytes();
        let len = bytes.len();
        let mut i = offset.min(len);
        // Skip a leading delimiter run, then advance through the word.
        while i < len && !is_word_byte(bytes[i]) {
            i += 1;
        }
        while i < len && is_word_byte(bytes[i]) {
            i += 1;
        }
        i
    }

    /// The previous word boundary strictly before `offset`.
    fn previous_word_boundary(&self, offset: usize) -> usize {
        let bytes = self.text.as_bytes();
        let mut i = offset.min(bytes.len());
        while i > 0 && !is_word_byte(bytes[i - 1]) {
            i -= 1;
        }
        while i > 0 && is_word_byte(bytes[i - 1]) {
            i -= 1;
        }
         i
     }
 
    /// The end of the word preceding `offset`, skipping any delimiter between
    /// it and `offset`. Used to shrink a selection from its right edge so the
    /// result excludes the trailing separator (e.g. "hello world" → "hello").
    fn previous_word_end(&self, offset: usize) -> usize {
        let bytes = self.text.as_bytes();
        let mut i = offset.min(bytes.len());
         // Skip a trailing delimiter run, then the word run, then the delimiter
         // before it so the result is the end of the previous word (excludes
         // the separator that the expansion added).
         while i > 0 && !is_word_byte(bytes[i - 1]) {
             i -= 1;
         }
         while i > 0 && is_word_byte(bytes[i - 1]) {
             i -= 1;
         }
        while i > 0 && !is_word_byte(bytes[i - 1]) {
            i -= 1;
        }
         i
     }
 
     /// The start of the word after `offset`, skipping any delimiter between it
    fn next_word_start(&self, offset: usize) -> usize {
        let bytes = self.text.as_bytes();
        let len = bytes.len();
        let mut i = offset.min(len);
        while i < len && !is_word_byte(bytes[i]) {
            i += 1;
        }
        i
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

    /// Public clipboard helper for the editor context menu: copies the current
    /// selection (if any) to the system clipboard.
    pub fn copy_selection_to_clipboard(&self, cx: &mut Context<Self>) {
        if !self.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.text[self.selection.clone()].to_string(),
            ));
        }
    }

    /// Public clipboard helper for the editor context menu: cuts the current
    /// selection (if any) to the system clipboard.
    pub fn cut_selection_to_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cut(&Cut, window, cx);
    }

    /// Public clipboard helper for the editor context menu: pastes the system
    /// clipboard text at the cursor (replacing any selection).
    pub fn paste_from_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.paste(&Paste, window, cx);
    }

    /// Select the entire document (used by the editor context menu).
    pub fn select_all_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.select_all(&SelectAll, window, cx);
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

    fn toggle_bookmark(
        &mut self,
        _: &ToggleBookmark,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (line, _) = self.line_col(self.cursor);
        self.toggle_bookmark_line(line + 1, cx);
    }
    fn toggle_fold_action(&mut self, _: &ToggleFold, _: &mut Window, cx: &mut Context<Self>) {
        let (line, _) = self.line_col(self.cursor);
        self.toggle_fold(line, cx);
    }

    fn toggle_minimap_action(&mut self, _: &ToggleMinimap, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_minimap(cx);
    }

    fn add_cursor_up_action(&mut self, _: &AddCursorUp, _: &mut Window, cx: &mut Context<Self>) {
        self.add_cursor_vertical(-1, cx);
    }

    fn add_cursor_down_action(&mut self, _: &AddCursorDown, _: &mut Window, cx: &mut Context<Self>) {
        self.add_cursor_vertical(1, cx);
    }

    fn clear_secondary_action(&mut self, _: &ClearSecondary, _: &mut Window, cx: &mut Context<Self>) {
        self.clear_secondary(cx);
    }

    fn select_next_occurrence_action(
        &mut self,
        _: &SelectNextOccurrence,
        _: &mut Window,
        cx: &mut Context<Self>,
     ) {
         self.select_next_occurrence(cx);
     }
 
    fn toggle_comment_action(
        &mut self,
        _: &ToggleComment,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_comment(cx);
    }

    fn duplicate_line_action(
        &mut self,
        _: &DuplicateLine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.duplicate_line(cx);
    }

    fn delete_line_action(
        &mut self,
        _: &DeleteLine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_line(cx);
    }

    fn move_line_up_action(
        &mut self,
        _: &MoveLineUp,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_line(-1, cx);
    }

    fn move_line_down_action(
        &mut self,
        _: &MoveLineDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
         self.move_line(1, cx);
     }

    fn transform_uppercase_action(
        &mut self,
        _: &TransformUppercase,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transform_selection_case(true, cx);
    }

    fn transform_lowercase_action(
        &mut self,
        _: &TransformLowercase,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transform_selection_case(false, cx);
    }

    fn join_lines_action(&mut self, _: &JoinLines, _: &mut Window, cx: &mut Context<Self>) {
        self.join_lines(cx);
    }

    fn sort_lines_action(&mut self, _: &SortLines, _: &mut Window, cx: &mut Context<Self>) {
        self.sort_lines(cx);
    }

    fn trim_trailing_whitespace_action(
        &mut self,
        _: &TrimTrailingWhitespace,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.trim_trailing_whitespace(cx);
    }
    fn expand_selection_action(
        &mut self,
        _: &ExpandSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.expand_or_shrink_selection(true, cx);
    }

    fn shrink_selection_action(
        &mut self,
        _: &ShrinkSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.expand_or_shrink_selection(false, cx);
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
        let menu_height = px(200.);
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

        cx.emit(BreakpointContextMenu {
            line,
            x,
            y,
            word: self.word_under_cursor(),
        });
        cx.notify();
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);

        if let Some(bounds) = self.last_bounds {
            // Click on the right-edge minimap: scroll proportionally to the
            // pointer's Y position.
            if event.position.x > bounds.right() + px(2.) {
                let map_top = bounds.top();
                let map_h = bounds.size.height;
                let ratio = ((event.position.y - map_top) / map_h).clamp(0.0, 1.0);
                self.scroll_to_ratio(ratio, cx);
                self.is_selecting = false;
                return;
            }
            if event.position.x < bounds.left() + px(GUTTER_WIDTH) {
                let top = bounds.top() - px(self.scroll_y);
                let line = self.line_for_y(event.position.y, top);
                // Clicking the left fold strip (first ~10px of the gutter) toggles
                // the fold at that line; Option/Cmd+click elsewhere toggles a
                // bookmark; plain click toggles a breakpoint.
                if event.position.x < bounds.left() + px(10.) {
                    self.toggle_fold(line.saturating_sub(1), cx);
                } else if event.modifiers.platform || event.modifiers.control || event.modifiers.alt {
                    self.toggle_bookmark_line(line, cx);
                } else {
                    self.toggle_breakpoint_line(line, cx);
                }
                self.is_selecting = false;
                return;
            }
        }

        self.is_selecting = true;
        let offset = self.index_for_mouse_position(event.position);
        if event.modifiers.platform || event.modifiers.control {
            // Ctrl/Cmd+click on a symbol: report the position so the app can
            // trigger "go to definition" (F3.2). Cursor moves to the click.
            self.move_to(offset, cx);
            let index = offset.min(self.text.len());
            let (row, column) = self.cursor_index_to_row_column(index);
            cx.emit(DefinitionRequested { line: row + 1, column });
            self.is_selecting = false;
            return;
        }
        if event.modifiers.shift {
            self.select_to(offset, cx);
        } else if event.click_count >= 3 {
            // Triple-click selects the whole line (including trailing newline).
            let line = self.line_range_at(offset);
            self.snapshot();
            self.selection = line.clone();
            self.selection_reversed = false;
            self.cursor = line.end.min(self.text.len());
            self.preferred_column = None;
            self.marked_range.take();
            self.dirty = true;
            cx.notify();
        } else if event.click_count == 2 {
            // Double-click selects the word under the click.
            if let Some(word) = self.word_range_at(offset) {
                self.snapshot();
                self.selection = word.clone();
                self.selection_reversed = false;
                self.cursor = word.end.min(self.text.len());
                self.preferred_column = None;
                self.marked_range.take();
                self.dirty = true;
                cx.notify();
            } else {
                self.move_to(offset, cx);
            }
        } else {
            self.move_to(offset, cx);
        }
    }

    /// Convert a byte offset into (0-based line, 0-based UTF-8 column).
    fn cursor_index_to_row_column(&self, index: usize) -> (usize, usize) {
        let starts = self.line_starts();
        let line = match starts.binary_search(&index) {
            Ok(line) => line,
            Err(next) => next.saturating_sub(1),
        };
        (line, index - starts[line])
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            let offset = self.index_for_mouse_position(event.position);
            self.select_to(offset, cx);
        }
        // Diagnostic hover tooltip: emit when the hovered mark changes (or
        // when leaving all marks), so the app shows/hides the tooltip.
        let hovered = self.diagnostic_at_position(event.position);
        if hovered != self.hovered_diagnostic {
            self.hovered_diagnostic = hovered;
            let message = hovered
                .and_then(|i| self.diagnostics.get(i))
                .map(|m| m.message.clone());
            cx.emit(DiagnosticHover {
                message,
                position: event.position,
            });
        }
        // Identifier hover (debug variable value / LSP symbol). Emit only on
        // word change so the app does not re-evaluate on every pixel.
        let word = self.word_at_position(event.position);
        if word != self.hovered_word {
            self.hovered_word = word.clone();
            cx.emit(HoverWord {
                word,
                position: event.position,
            });
        }
    }

    /// The identifier-like word (letters/digits/`_`) under `position`, or
    /// `None` when the mouse is over the gutter, whitespace, or punctuation.
    fn word_at_position(&self, position: Point<Pixels>) -> Option<String> {
        let Some(bounds) = self.last_bounds else {
            return None;
        };
        if position.x < bounds.left() + px(GUTTER_WIDTH) || position.y < bounds.top() {
            return None;
        }
        let visual = ((position.y - bounds.top() + px(self.scroll_y)) / px(self.line_height()))
            .floor()
            .max(0.0) as usize;
        let line = visible_lines_in(&self.text, &self.folded).get(visual).copied()?;
        let local_x = position.x - bounds.left() - px(GUTTER_WIDTH);
        let column = self
            .last_line_shapes
            .get(visual)
            .and_then(|shape| shape.index_for_x(local_x))?;
        let starts = line_starts(&self.text);
        let start = *starts.get(line)?;
        let end = starts.get(line + 1).copied().unwrap_or(self.text.len());
        let offset = (start + column).min(end);
        let word_range = self.word_range_at(offset)?;
        if word_range.is_empty() {
            return None;
        }
        Some(self.text[word_range].to_string())
    }

    /// Return the index of the diagnostic mark under `position` (window-
    /// relative), if any. Uses the visual row to find the document line and
    /// the shaped line to resolve the character column; a mark whose
    /// `start_col..end_col` covers the column (or a whole-line mark) matches.
    fn diagnostic_at_position(&self, position: Point<Pixels>) -> Option<usize> {
        if self.diagnostics.is_empty() {
            return None;
        }
        let Some(bounds) = self.last_bounds else {
            return None;
        };
        if position.x < bounds.left() + px(GUTTER_WIDTH) || position.y < bounds.top() {
            return None;
        }
        let visual = ((position.y - bounds.top() + px(self.scroll_y)) / px(self.line_height()))
            .floor()
            .max(0.0) as usize;
        let line = visible_lines_in(&self.text, &self.folded).get(visual).copied();
        let Some(line) = line else {
            return None;
        };
        let local_x = position.x - bounds.left() - px(GUTTER_WIDTH);
        let column = self
            .last_line_shapes
            .get(visual)
            .and_then(|shape| shape.index_for_x(local_x))
            .unwrap_or(0);
        self.diagnostics
            .iter()
            .position(|m| {
                m.line == line
                    && (m.start_col == m.end_col
                        || (column >= m.start_col && column < m.end_col))
            })
    }

    /// Map a click `y` to a 1-based document line, honouring folded regions so
    /// gutter clicks (fold indicator / breakpoint) target the right line.
    fn line_for_y(&self, y: Pixels, top: Pixels) -> usize {
        let row = ((y - top) / px(self.line_height())).floor().max(0.0) as usize;
        let visible = visible_lines_in(&self.text, &self.folded);
        visible
            .get(row)
            .map(|line| line + 1)
            .unwrap_or(self.line_starts().len().saturating_sub(1) + 1)
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds else {
            return self.cursor;
        };
        let line = ((position.y - bounds.top() + px(self.scroll_y)) / px(self.line_height()))
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
    /// Uppercase (`uppercase = true`) or lowercase the selected text. With a
    /// collapsed cursor, the word under the cursor is transformed instead.
    fn transform_selection_case(&mut self, uppercase: bool, cx: &mut Context<Self>) {
        let range = if self.selection.is_empty() {
            // Word under cursor (or the single char if no word found).
            self.word_range_at(self.cursor).unwrap_or(self.cursor..self.cursor + 1)
        } else {
            self.selection.clone()
        };
        let range = clamp_range(&self.text, range);
        if range.is_empty() {
            return;
        }
        let original = &self.text[range.clone()];
        let transformed = if uppercase {
            original.to_uppercase()
        } else {
            original.to_lowercase()
        };
        if transformed == original {
            return;
        }
        self.snapshot();
        self.text.replace_range(range.clone(), &transformed);
        // Keep the transformed text selected so repeated presses walk the word.
        self.cursor = clamp_boundary(&self.text, range.start + transformed.len());
        self.selection = range.start..self.cursor;
        self.selection_reversed = false;
        self.secondary.clear();
        self.marked_range.take();
        self.dirty = true;
        cx.notify();
    }
    /// Join the selected lines (or the current line and the next) into a single
    /// line, replacing interior whitespace runs with a single space and the
    /// trailing newline with nothing. A trailing line break is dropped.
    fn join_lines(&mut self, cx: &mut Context<Self>) {
        let starts = self.line_starts();
        let (first_line, _) = self.line_col(self.selection.start);
        let (last_line, _) = self.line_col(self.selection.end.max(self.selection.start));
        // Join at least two lines: the selection's first line through the line
        // after the selection end (or the line after the cursor when collapsed).
        let first = first_line.min(starts.len().saturating_sub(1));
        let last = if last_line > first {
            last_line
        } else {
            (first + 1).min(starts.len().saturating_sub(1))
        };
        if first >= last {
            return;
        }
        let start = starts[first];
        let end = self.line_end(last, &starts);
        let mut joined = String::new();
        for line in first..=last {
            let line_start = starts[line];
            let line_end = self.line_end(line, &starts);
            let line_text = self.text[line_start..line_end].trim();
            if !line_text.is_empty() {
                if !joined.is_empty() {
                    joined.push(' ');
                }
                joined.push_str(line_text);
            }
        }
        self.snapshot();
        self.text = self.text[0..start].to_owned() + &joined + &self.text[end..];
        self.cursor = clamp_boundary(&self.text, start + joined.len());
        self.selection = start..self.cursor;
        self.selection_reversed = false;
        self.secondary.clear();
        self.marked_range.take();
        self.dirty = true;
        cx.notify();
    }
    /// Sort the selected lines (or the whole document when nothing is selected)
    /// alphabetically, byte-order. Blank lines sort before content lines.
    fn sort_lines(&mut self, cx: &mut Context<Self>) {
        let starts = self.line_starts();
        let (first_line, _) = self.line_col(self.selection.start);
        let (last_line, _) = self.line_col(self.selection.end.max(self.selection.start));
        let first = first_line.min(starts.len().saturating_sub(1));
        let last = if self.selection.is_empty() {
            starts.len().saturating_sub(1)
        } else if last_line > first {
            last_line
        } else {
            first
        };
        if first >= last {
            return;
        }
        let start = starts[first];
        let end = self.line_end(last, &starts);
        let slice = &self.text[start..end];
        // A trailing `\n` produces a phantom empty element; keep it as the
        // terminator so it does not sort to the front as a blank line.
        let trailing_newline = slice.ends_with('\n');
        let body = if trailing_newline { &slice[..slice.len() - 1] } else { slice };
        let mut lines: Vec<&str> = body.split('\n').collect();
        let stable = lines.clone();
        lines.sort();
        if lines == stable {
            return;
        }
        let joined = lines.join("\n");
        let replacement = if trailing_newline {
            format!("{joined}\n")
        } else {
            joined
        };
        self.snapshot();
        self.text = self.text[0..start].to_owned() + &replacement + &self.text[end..];
        self.cursor = clamp_boundary(&self.text, start);
        self.selection = start..clamp_boundary(&self.text, end);
        self.selection_reversed = false;
        self.secondary.clear();
        self.marked_range.take();
        self.dirty = true;
        cx.notify();
    }

    /// Trim trailing whitespace (spaces/tabs) from every line. When there is a
    /// non-empty selection, only the lines spanned by the selection are trimmed;
    /// otherwise the whole document is trimmed.
    fn trim_trailing_whitespace(&mut self, cx: &mut Context<Self>) {
        let starts = self.line_starts();
        let (first_line, _) = self.line_col(self.selection.start);
        let (last_line, _) = self.line_col(self.selection.end.max(self.selection.start));
        let first = if self.selection.is_empty() { 0 } else { first_line };
        let last = if self.selection.is_empty() {
            starts.len().saturating_sub(1)
        } else {
            last_line
        };
        // Collect trim ranges from the ORIGINAL text first, then apply them in
        // reverse offset order so earlier offsets stay valid.
        let mut trims: Vec<(usize, usize)> = Vec::new();
        for line in first..=last.min(starts.len().saturating_sub(1)) {
            let line_start = starts[line];
            let line_end = self.line_end(line, &starts);
            let trimmed = self.text[line_start..line_end].trim_end();
            if trimmed.len() != line_end - line_start {
                trims.push((trimmed.len() + line_start, line_end));
            }
        }
        if trims.is_empty() {
            return;
        }
        self.snapshot();
        trims.sort_by_key(|(start, _)| std::cmp::Reverse(*start));
        for (start, end) in trims {
            self.text.replace_range(start..end, "");
        }
        self.cursor = clamp_boundary(&self.text, self.cursor);
        self.selection = clamp_range(&self.text, self.selection.clone());
        self.secondary.clear();
        self.marked_range.take();
        self.dirty = true;
        cx.notify();
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
        // Collect the primary plus any secondary cursor ranges.
        let mut ranges = vec![range.clone()];
        for extra in &self.secondary {
            let extra = clamp_range(&self.text, extra.clone());
            if !ranges.iter().any(|r| r.start == extra.start && r.end == extra.end) {
                ranges.push(extra);
            }
        }
        if ranges.iter().all(|r| r.is_empty()) && new_text.is_empty() {
            return;
        }
        // Auto-close brackets/quotes (single opening char, no selection).
        let (insert_text, cursor_offset) = auto_close_insert(
            &self.text,
            &range,
            new_text,
            ranges.len() == 1,
        );
        self.snapshot();
        // Apply edits from the highest offset down so earlier offsets stay valid.
        ranges.sort_by_key(|r| std::cmp::Reverse(r.start));
        for r in &ranges {
            self.text = self.text[0..r.start].to_owned() + &insert_text + &self.text[r.end..];
        }
        // The primary cursor ends after its own insertion (or the skip target).
        let cursor = cursor_offset.unwrap_or(range.start + insert_text.len());
        self.cursor = clamp_boundary(&self.text, cursor);
        self.selection = self.cursor..self.cursor;
        self.secondary.clear();
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
        self.secondary.clear();
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
                top + px(line as f32 * self.line_height()),
            ),
            Point::new(
                bounds.left() + shape.x_for_index(local_end),
                top + px((line as f32 + 1.0) * self.line_height()),
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
impl EventEmitter<BookmarkToggled> for Editor {}
impl EventEmitter<BreakpointContextMenu> for Editor {}
impl EventEmitter<DefinitionRequested> for Editor {}
impl EventEmitter<DiagnosticHover> for Editor {}
impl EventEmitter<HoverWord> for Editor {}

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
            .on_action(cx.listener(Self::toggle_bookmark))
            .on_action(cx.listener(Self::toggle_fold_action))
            .on_action(cx.listener(Self::toggle_minimap_action))
            .on_action(cx.listener(Self::add_cursor_up_action))
            .on_action(cx.listener(Self::add_cursor_down_action))
             .on_action(cx.listener(Self::clear_secondary_action))
             .on_action(cx.listener(Self::select_next_occurrence_action))
            .on_action(cx.listener(Self::toggle_comment_action))
            .on_action(cx.listener(Self::duplicate_line_action))
            .on_action(cx.listener(Self::delete_line_action))
            .on_action(cx.listener(Self::move_line_up_action))
             .on_action(cx.listener(Self::move_line_down_action))
            .on_action(cx.listener(Self::expand_selection_action))
            .on_action(cx.listener(Self::shrink_selection_action))
            .on_action(cx.listener(Self::transform_uppercase_action))
            .on_action(cx.listener(Self::transform_lowercase_action))
            .on_action(cx.listener(Self::join_lines_action))
            .on_action(cx.listener(Self::sort_lines_action))
            .on_action(cx.listener(Self::trim_trailing_whitespace_action))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(
                div()
                    .id("editor-viewport")
                    .flex_1()
                    .w_full()
                    .overflow_hidden()
                    .flex()
                    .flex_row()
                    .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
                    .child(
                        div()
                            .flex_1()
                            .w_full()
                            .overflow_hidden()
                            .child(CodeElement {
                                editor: cx.entity(),
                            }),
                    )
                    .children(if self.show_minimap {
                        Some(MinimapElement {
                            editor: cx.entity(),
                        })
                    } else {
                        None
                    }),
            )
    }
}

/// The right-edge minimap: a compact, clickable overview of the whole file.
struct MinimapElement {
    editor: Entity<Editor>,
}


/// A semantic token overlay from the LSP: 0-based line, start character,
/// length in characters, and a semantic token type index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticSpan {
    pub line: usize,
    pub start: usize,
    pub length: usize,
    pub token_type: usize,
}

/// An inlay hint from the LSP: 0-based line, start character, label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHint {
    pub line: usize,
    pub start: usize,
    pub label: String,
}


struct CodeElement {
    editor: Entity<Editor>,
}

struct PrepaintState {
    lines: Vec<ShapedLine>,
    line_numbers: Vec<ShapedLine>,
    breakpoints: Vec<PaintQuad>,
    bookmarks: Vec<PaintQuad>,
    selections: Vec<PaintQuad>,
    find_matches: Vec<PaintQuad>,
      debug_line: Option<PaintQuad>,
    debug_marker: Option<ShapedLine>,
     cursor: Option<PaintQuad>,
    /// Vertical bars for secondary (multi-) cursors.
    secondary_cursors: Vec<PaintQuad>,
    fold_quads: Vec<PaintQuad>,
    bracket_quads: Vec<PaintQuad>,
    /// Inlay hint labels positioned after their anchor column.
    inlays: Vec<(Pixels, Pixels, ShapedLine)>,
    /// Diagnostic underlines (line bottom bar) — error/warning color per row.
    diagnostics: Vec<PaintQuad>,
     /// Diagnostic gutter ticks (small square left of the line number).
     diagnostic_gutter: Vec<PaintQuad>,
    /// Coverage markers (uncovered lines get a red gutter bar; covered lines a
    /// green bar). Drawn left of the line number, inside the gutter.
    coverage_quads: Vec<PaintQuad>,
    /// Subtle background quad highlighting the row under the cursor.
    current_line: Option<PaintQuad>,
    /// The wrap model computed this frame (visual row → document line + byte
    /// span). Written back to the editor in `paint` for coordinate mapping.
    wrap_rows: Vec<WrapRow>,
    wrap_cache_width: f32,
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
        let find_matches = editor.find_matches.clone();
        let find_current = editor.find_current;
          let secondary = editor.secondary.clone();
        let breakpoints = editor.breakpoints.clone();
        let debug_line = editor.debug_line;
        let mut inlays = Vec::new();


        let code_left = bounds.left() + px(GUTTER_WIDTH);
        let visible_lines = editor.visible_lines();
        let viewport: f32 = bounds.size.height.into();
        let line_height = editor.line_height();
        let font_size = editor.font_size();
        let code_font = font(theme::MONO_FONT);
        let number_font = font(theme::UI_FONT);
        let folded = editor.folded.clone();
        let starts = line_starts(&text);

        // Word wrap: compute the visual rows (document line + byte span) once
        // for the current viewport width, then shape each row's slice.
        let wrap_width = bounds.size.width - px(GUTTER_WIDTH) - px(MINIMAP_WIDTH);
        let wrap_on = editor.word_wrap;
        let wrap_rows = if wrap_on {
            let mut rows = Vec::new();
            for &line_index in &visible_lines {
                for (byte_start, byte_end) in wrap_line_segments(
                    &text,
                    line_index,
                    wrap_width,
                    font_size,
                    &code_font,
                    window,
                ) {
                    rows.push(WrapRow { line: line_index, byte_start, byte_end });
                }
            }
            rows
        } else {
            Vec::new()
        };
        // The wrap model is carried in the prepaint state; `paint` writes it
        // back into the editor for input/scroll coordinate mapping.

        let total_lines = if wrap_on {
            wrap_rows.len().max(1) as f32
        } else {
            visible_lines.len().max(1) as f32
        };
        let scroll_y = editor
            .scroll_y
            .clamp(0.0, (total_lines * line_height - viewport).max(0.0));
        let top = bounds.top() - px(scroll_y);
        let mut line_shapes = Vec::new();
        let mut line_numbers = Vec::new();
        let mut selections = Vec::new();
        let mut find_match_quads = Vec::new();
        let mut block_comment = false;

        // Iterate visual rows. Without wrap, one row per visible document line;
        // with wrap, one row per wrapped segment. `line_index` is the document
        // line, `slice` the bytes drawn on this visual row, `row_in_line` the
        // segment index within that line (0 = first, gets the gutter number).
        let visual_rows: Vec<(usize, &str, usize)> = if wrap_on {
            wrap_rows
                .iter()
                .enumerate()
                .map(|(row, wr)| {
                    let row_in_line = wrap_rows[..row]
                        .iter()
                        .rev()
                        .take_while(|r| r.line == wr.line)
                        .count();
                    (wr.line, &text[wr.byte_start..wr.byte_end], row_in_line)
                })
                .collect()
        } else {
            visible_lines
                .iter()
                .copied()
                .map(|line_index| {
                    (line_index, text.split('\n').nth(line_index).unwrap_or(""), 0)
                })
                .collect()
        };

        for (row, (line_index, line, row_in_line)) in visual_rows.into_iter().enumerate() {
            // `line` is the slice drawn on this visual row; its byte offset
            // within the document is `slice_start` (used for x-for-index).
            let slice_start = starts[line_index];
            let slice_end = if line_index + 1 < starts.len() {
                starts[line_index + 1].saturating_sub(1)
            } else {
                text.len()
            };
            // In wrap mode the slice is a sub-range of the line; recompute the
            // actual byte offset by locating the slice in the document.
            let (slice_start, slice_end) = if wrap_on {
                let wr = &wrap_rows[row];
                (wr.byte_start, wr.byte_end)
            } else {
                (slice_start, slice_end)
            };
            let is_first_row = row_in_line == 0;
            let spans = match language {
                Some(lang) => highlight_line(line, lang, &mut block_comment),
                None => Vec::new(),
            };
            let line_semantic: Vec<SemanticSpan> = editor
                .semantic
                .iter()
                .filter(|token| token.line == line_index)
                .copied()
                .collect();
            let runs = build_runs_with_semantic(line, &spans, &line_semantic, &code_font);
            let shape = window.text_system().shape_line(
                SharedString::from(line.to_string()),
                px(font_size),
                &runs,
                None,
            );

            // Only the first visual row of a wrapped line carries the gutter
            // number; continuation rows keep an empty gutter slot.
            if is_first_row {
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
            } else {
                line_numbers.push(window.text_system().shape_line(
                    SharedString::from(""),
                    px(10.5),
                    &[],
                    None,
                ));
            }

            let sel_visible = |r: &Range<usize>| {
                r.start < slice_end && r.end > slice_start
            };
            if selection.start < selection.end && sel_visible(&selection) {
                let sel_start = selection.start.max(slice_start);
                let sel_end = selection.end.min(slice_end);
                let x_start = shape.x_for_index(sel_start - slice_start);
                let x_end = shape.x_for_index(sel_end - slice_start);
                selections.push(fill(
                    Bounds::new(
                        Point::new(
                            code_left + x_start,
                            top + px(row as f32 * line_height),
                        ),
                        size((x_end - x_start).max(px(2.)), px(line_height)),
                    ),
                    theme::alpha(theme::ACCENT, 0.2),
                ));
            }
            for extra in &secondary {
                if extra.start >= extra.end || !sel_visible(extra) {
                    continue;
                }
                let sel_start = extra.start.max(slice_start);
                let sel_end = extra.end.min(slice_end);
                let x_start = shape.x_for_index(sel_start - slice_start);
                let x_end = shape.x_for_index(sel_end - slice_start);
                selections.push(fill(
                    Bounds::new(
                        Point::new(
                            code_left + x_start,
                            top + px(row as f32 * line_height),
                        ),
                        size((x_end - x_start).max(px(2.)), px(line_height)),
                    ),
                    theme::alpha(theme::ACCENT, 0.2),
                ));
            }

            for (match_index, range) in find_matches.iter().enumerate() {
                if range.start >= slice_end || range.end <= slice_start {
                    continue;
                }
                let match_start = range.start.max(slice_start);
                let match_end = range.end.min(slice_end);
                if match_start >= match_end {
                    continue;
                }
                let x_start = shape.x_for_index(match_start - slice_start);
                let x_end = shape.x_for_index(match_end - slice_start);
                let is_current = match_index == find_current;
                find_match_quads.push(fill(
                    Bounds::new(
                        Point::new(
                            code_left + x_start,
                            top + px(row as f32 * line_height),
                        ),
                        size((x_end - x_start).max(px(2.)), px(line_height)),
                    ),
                    if is_current {
                        theme::alpha(theme::ACCENT, 0.42)
                    } else {
                        theme::alpha(theme::ACCENT, 0.18)
                    },
                ));
            }

            // Inlay hints only on the first visual row of their line.
            if is_first_row {
                for hint in editor.inlay_hints.iter().filter(|hint| hint.line == line_index) {
                    let hint_x = shape.x_for_index(hint.start.min(line.len()));
                    let hint_run = TextRun {
                        len: hint.label.len(),
                        font: code_font.clone(),
                        color: theme::color(theme::TEXT_FAINT),
                        background_color: Some(theme::color(theme::BG_1)),
                        underline: None,
                        strikethrough: None,
                    };
                    let hint_shape = window.text_system().shape_line(
                        SharedString::from(hint.label.clone()),
                        px(font_size),
                        &[hint_run],
                        None,
                    );
                    inlays.push((
                        code_left + hint_x,
                        top + px(row as f32 * line_height),
                        hint_shape,
                    ));
                }
            }

            line_shapes.push(shape);
        }

        let breakpoint_quads = breakpoints
            .iter()
            .filter(|line| **line >= 1)
            .filter_map(|line| {
                let visible = editor.visual_row_for_line(*line - 1)?;
                let y = top + px(visible as f32 * line_height);
                Some(
                    fill(
                        Bounds::new(
                            Point::new(bounds.left() + px(13.), y + px((line_height - 8.) / 2.)),
                            size(px(8.), px(8.)),
                        ),
                        theme::color(theme::CINNABAR),
                    )
                    .corner_radii(px(4.)),
                )
            })
            .collect();

        let bookmark_quads = editor
            .bookmarks
            .iter()
            .filter(|line| **line >= 1)
            .filter_map(|line| {
                let visible = editor.visual_row_for_line(*line - 1)?;
                let y = top + px(visible as f32 * line_height);
                let bookmark_size = px(7.);
                let x = bounds.left() + px(5.);
                let cy = y + px(line_height / 2.);
                let half = bookmark_size / 2.;
                Some(
                    fill(
                        Bounds::new(
                            Point::new(x + half, cy - half),
                            size(bookmark_size, bookmark_size),
                        ),
                        theme::color(theme::ACCENT),
                    )
                    .corner_radii(px(1.5)),
                )
            })
            .collect::<Vec<_>>();

        let (debug_line, debug_marker) = debug_line
            .filter(|line| *line >= 1)
            .and_then(|line| {
                let visible = editor.visual_row_for_line(line - 1)?;
                 let y = top + px(visible as f32 * line_height);
                let quad = fill(
                    Bounds::new(
                        Point::new(bounds.left(), y),
                        size(bounds.size.width, px(line_height)),
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
                Some((Some(quad), Some(marker)))
            })
            .unwrap_or((None, None));

        let (cursor_visible, cursor_in_row) = editor.visual_position(cursor);
        let cursor_quad = line_shapes.get(cursor_visible).map(|shape| {
            fill(
                Bounds::new(
                    Point::new(
                        code_left + shape.x_for_index(cursor_in_row),
                        top + px(cursor_visible as f32 * line_height),
                    ),
                    size(px(2.0), px(line_height)),
                ),
                theme::color(theme::ACCENT_3),
            )
        });

        // Highlight the row under the cursor with a subtle background band.
        let current_line = Some(fill(
            Bounds::new(
                Point::new(code_left, top + px(cursor_visible as f32 * line_height)),
                size(bounds.size.width - px(GUTTER_WIDTH), px(line_height)),
            ),
            theme::alpha(theme::ACCENT, 0.06),
        ));

        // Vertical bars for secondary (multi-) cursors.
        let mut secondary_cursors = Vec::new();
        for extra in &editor.secondary {
            let (visible, in_row) = editor.visual_position(extra.start);
            if let Some(shape) = line_shapes.get(visible) {
                secondary_cursors.push(fill(
                    Bounds::new(
                        Point::new(
                            code_left + shape.x_for_index(in_row),
                            top + px(visible as f32 * line_height),
                        ),
                        size(px(2.0), px(line_height)),
                    ),
                    theme::color(theme::ACCENT_3).opacity(0.6),
                ));
            }
        }

        // Fold marker: a small triangle drawn at the left edge of each folded

        // region's start line, plus a horizontal indicator line.
        let mut fold_quads = Vec::new();



        for (start, _end) in folded.iter() {
            let Some(visible) = editor.visual_row_for_line(*start) else {
                continue;
            };
             let y = top + px(visible as f32 * line_height);
            fold_quads.push(fill(
                Bounds::new(
                    Point::new(bounds.left() + px(2.), y + px((line_height - 8.) / 2.)),
                    size(px(7.), px(8.)),
                ),
                theme::color(theme::TEXT_FAINT).opacity(0.7),
            ));
            fold_quads.push(fill(
                Bounds::new(
                    Point::new(code_left, y + px(line_height - 1.)),
                    size(bounds.size.width, px(1.)),
                ),
                theme::alpha(theme::STROKE, 0.25),
            ));
        }

        // Bracket match highlight: draw a subtle box around both brackets when
        // the cursor is adjacent to a pair.
        let mut bracket_quads = Vec::new();
        if let Some((open, close)) = editor.bracket_pair_around_cursor() {
            for offset in [open, close] {
                let (visible, column) = editor.visual_position(offset);
                if let Some(shape) = line_shapes.get(visible) {
                    let x = code_left + shape.x_for_index(column);
                    let y = top + px(visible as f32 * line_height);
                    let width = (shape.x_for_index(column + 1) - shape.x_for_index(column))
                        .max(px(6.));
                    bracket_quads.push(fill(
                        Bounds::new(
                            Point::new(x - px(1.), y),
                            size(width, px(line_height)),
                        ),
                        theme::alpha(theme::ACCENT, 0.18),
                    ));
                }
            }
        }

        // Diagnostic underlines + gutter ticks: markers are mapped to the
        // visible viewport with the same folding-aware mapping used for
        // breakpoints. The underline spans the mark's char range (or the full
        // line for whole-line marks), so hovering the squiggle hit-tests.
        let mut diagnostic_quads = Vec::new();
        let mut diagnostic_gutter = Vec::new();
        let diagnostics = editor.diagnostics.clone();
        for mark in diagnostics {
            let Some(visible) = editor.visual_row_for_line(mark.line) else {
                continue;
            };
            let y = top + px(visible as f32 * line_height);
            let color = if mark.is_error {
                theme::color(theme::DANGER)
            } else {
                theme::color(theme::ACCENT)
            };
            // Underline spans the mark's char range when known, else the row.
            let (start_x, width) = if mark.start_col == mark.end_col {
                (code_left, bounds.size.width - px(GUTTER_WIDTH))
            } else if let Some(shape) = line_shapes.get(visible) {
                let sx = code_left + shape.x_for_index(mark.start_col);
                let ex = code_left + shape.x_for_index(mark.end_col);
                (sx, (ex - sx).max(px(4.)))
            } else {
                (code_left, bounds.size.width - px(GUTTER_WIDTH))
            };
            diagnostic_quads.push(fill(
                Bounds::new(
                    Point::new(start_x, y + px(line_height - 2.0)),
                    size(width, px(2.0)),
                ),
                theme::alpha(if mark.is_error { theme::DANGER } else { theme::ACCENT }, 0.7),
            ));
            // Gutter tick left of the line number for quick scanning.
            diagnostic_gutter.push(
                fill(
                    Bounds::new(
                        Point::new(bounds.left() + px(2.0), y + px((line_height - 8.0) / 2.0)),
                        size(px(4.0), px(8.0)),
                    ),
                    color,
                )
                .corner_radii(px(2.0)),
            );
        }

        // Coverage markers: a thin gutter bar colored by hit state (red for
        // uncovered executable lines, green for covered). Drawn left of the
        // line number, distinct from the wider diagnostic tick.
        let mut coverage_quads = Vec::new();
        let coverage = editor.coverage.clone();
        if !coverage.is_empty() {
            for (line, hits) in coverage.iter().enumerate() {
                let Some(visible) = editor.visual_row_for_line(line) else {
                    continue;
                };
                let y = top + px(visible as f32 * line_height);
                let color = if *hits == 0 { theme::DANGER } else { theme::JADE };
                coverage_quads.push(
                    fill(
                        Bounds::new(
                            Point::new(bounds.left() + px(7.0), y + px((line_height - 6.0) / 2.0)),
                            size(px(2.0), px(6.0)),
                        ),
                        theme::alpha(color, 0.85),
                    )
                    .corner_radii(px(1.0)),
                );
            }
        }
 
         PrepaintState {
            lines: line_shapes,
            line_numbers,
            breakpoints: breakpoint_quads,
            bookmarks: bookmark_quads,
            selections,
            find_matches: find_match_quads,
             debug_line,
            debug_marker,
            cursor: cursor_quad,
            secondary_cursors,
            fold_quads,
            bracket_quads,
            inlays,
             diagnostics: diagnostic_quads,
             diagnostic_gutter,
            coverage_quads,
            current_line,
            wrap_rows,
            wrap_cache_width: wrap_width.into(),
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
        let line_height = self.editor.read(cx).line_height();
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

        for bracket in prepaint.bracket_quads.drain(..) {
            window.paint_quad(bracket);
        }

        // Current-line highlight sits under selections but above the editor
        // background, so it never obscures the caret or selected text.
        if let Some(current_line) = prepaint.current_line.take() {
            window.paint_quad(current_line);
        }

        if let Some(debug_line) = prepaint.debug_line.take() {
            window.paint_quad(debug_line);
        }

        for diagnostic in prepaint.diagnostics.drain(..) {
            window.paint_quad(diagnostic);
        }

         for marker in prepaint.diagnostic_gutter.drain(..) {
             window.paint_quad(marker);
         }

        for quad in prepaint.coverage_quads.drain(..) {
            window.paint_quad(quad);
        }
         for selection in prepaint.selections.drain(..) {
             window.paint_quad(selection);
         }

        for find_match in prepaint.find_matches.drain(..) {
            window.paint_quad(find_match);
        }
 
        for breakpoint in prepaint.breakpoints.drain(..) {
            window.paint_quad(breakpoint);
        }

        for bookmark in prepaint.bookmarks.drain(..) {
            window.paint_quad(bookmark);
        }
        for fold in prepaint.fold_quads.drain(..) {
            window.paint_quad(fold);
        }

        if let Some(marker) = prepaint.debug_marker.take() {
            if let Some(line) = self.editor.read(cx).debug_line {
                if let Some(visible) = self.editor.read(cx).visual_row_for_line(line.saturating_sub(1)) {
                    let y = top + px(visible as f32 * line_height);
                    marker
                        .paint(
                            Point::new(bounds.left() + px(27.), y + px((line_height - 10.) / 2.)),
                            px(line_height),
                            window,
                            cx,
                        )
                        .ok();
                }
            }
        }

        for (index, number) in prepaint.line_numbers.iter().enumerate() {
            let line_top = top + px((index as f32 + 0.5) * line_height - line_height / 2.);
            if line_top + px(line_height) <= bounds.top() || line_top >= bounds.bottom() {
                continue;
            }
            number
                .paint(
                    Point::new(
                        code_left - px(12.) - number.width,
                        line_top,
                    ),
                    px(line_height),
                    window,
                    cx,
                )
                .ok();
        }

        for (index, line) in prepaint.lines.iter().enumerate() {
            let line_top = top + px(index as f32 * line_height);
            if line_top + px(line_height) <= bounds.top() || line_top >= bounds.bottom() {
                continue;
            }
            line.paint(
                Point::new(code_left, line_top),
                px(line_height),
                window,
                cx,
            )
            .ok();
        }

        // Draw inlay hints on top of the line text.
        for (x, y, hint) in prepaint.inlays.drain(..) {
            hint.paint(Point::new(x, y), px(line_height), window, cx).ok();
        }


        if focus_handle.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
            for extra in prepaint.secondary_cursors.drain(..) {
                window.paint_quad(extra);
            }
        }

        let lines = std::mem::take(&mut prepaint.lines);
        let wrap_rows = std::mem::take(&mut prepaint.wrap_rows);
        let wrap_cache_width = prepaint.wrap_cache_width;
        self.editor.update(cx, |editor, _cx| {
            editor.last_line_shapes = lines;
            editor.last_bounds = Some(bounds);
            editor.viewport_height = bounds.size.height.into();
            editor.wrap_rows = wrap_rows;
            editor.wrap_cache_width = wrap_cache_width;
            editor.clamp_scroll();
        });
    }
}

impl IntoElement for MinimapElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for MinimapElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

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
        style.size.width = px(MINIMAP_WIDTH).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        ()
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let editor = self.editor.read(cx);
        let text = editor.text.clone();
        let scroll_y = editor.scroll_y;
        let viewport_h: f32 = editor.viewport_height;
        let line_count = text.split('\n').count().max(1);
        let map_h: f32 = bounds.size.height.into();
        // Each document line maps to a fixed minimap slot; the whole file fits
        // the minimap height.
        let slot_h = map_h / line_count as f32;
        let line_h = slot_h.max(1.0);

        // Background strip.
        window.paint_quad(fill(
            Bounds::new(bounds.origin, bounds.size),
            theme::color(theme::BG_2).opacity(0.55),
        ));

        // One thin bar per line, width proportional to its content length.
        let max_len = text
            .split('\n')
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        for (index, line) in text.split('\n').enumerate() {
            let y = bounds.top() + px(index as f32 * line_h);
            let len = line.chars().count() as f32;
            let w = px(MINIMAP_WIDTH - 8.0) * (len / max_len).max(0.08);
            window.paint_quad(fill(
                Bounds::new(Point::new(bounds.left() + px(4.), y), size(w, px(line_h))),
                theme::color(theme::TEXT_FAINT).opacity(0.35),
            ));
        }

        // Viewport indicator: the region of the document currently visible.
        let total_h = line_count as f32 * line_h;
        if viewport_h > 0.0 {
            let content_h = editor.content_height();
            let ratio = (scroll_y / content_h.max(1.0)).clamp(0.0, 1.0);
            let view_ratio = (viewport_h / content_h.max(1.0)).clamp(0.0, 1.0);
            let top = bounds.top() + px(ratio * total_h);
            let h = (view_ratio * total_h).max(8.0);
            window.paint_quad(fill(
                Bounds::new(
                    Point::new(bounds.left(), top),
                    size(bounds.size.width, px(h)),
                ),
                theme::alpha(theme::ACCENT, 0.22),
            ));
            // Top and bottom edge lines of the viewport indicator.
            for edge_y in [top, top + px(h) - px(1.0)] {
                window.paint_quad(fill(
                    Bounds::new(
                        Point::new(bounds.left(), edge_y),
                        size(bounds.size.width, px(1.0)),
                    ),
                    theme::color(theme::ACCENT),
                ));
            }
        }

        // Click to scroll: compute the ratio from the pointer Y.
        let editor_entity = self.editor.clone();
        window.handle_input(
            &editor_entity.read(cx).focus_handle.clone(),
            ElementInputHandler::new(bounds, editor_entity.clone()),
            cx,
        );
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

/// Build text runs for a line, overlaying semantic token colors where the LSP
/// reports a token. Characters outside any semantic token keep the syntax color.
fn build_runs_with_semantic(
    line: &str,
    spans: &[HighlightSpan],
    semantic: &[SemanticSpan],
    font: &gpui::Font,
) -> Vec<TextRun> {
    if line.is_empty() {
        return vec![TextRun {
            len: 0,
            font: font.clone(),
            color: theme::color(theme::TEXT),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
    }
    if semantic.is_empty() {
        return build_runs(line, spans, font);
    }

    // Build per-character colors: start from the syntax span color, then overlay.
    let mut char_colors = vec![None; line.chars().count()];
    let mut char_index = 0usize;
    for span in spans {
        let color = token_color(span.kind);
        for _ in 0..span.len {
            if char_index < char_colors.len() {
                char_colors[char_index] = Some(color);
            }
            char_index += 1;
        }
    }
    for token in semantic {
        let color = semantic_token_color(token.token_type);
        let start = token.start.min(char_colors.len());
        let end = (token.start + token.length).min(char_colors.len());
        for slot in &mut char_colors[start..end] {
            *slot = Some(color);
        }
    }

    // Collapse into runs.
    let mut runs = Vec::new();
    let mut index = 0usize;
    while index < char_colors.len() {
        let color = char_colors[index].unwrap_or(theme::color(theme::TEXT));
        let mut len = 1usize;
        while index + len < char_colors.len() && char_colors[index + len] == char_colors[index] {
            len += 1;
        }
        runs.push(TextRun {
            len,
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
        index += len;
    }
    if runs.is_empty() {
        runs.push(TextRun {
            len: 0,
            font: font.clone(),
            color: theme::color(theme::TEXT),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    runs
}

fn semantic_token_color(token_type: usize) -> gpui::Hsla {
    match token_type {
        1 => theme::color(theme::JADE),          // type
        2 | 4 | 5 => theme::color(0xe8d5ab),     // enum/struct/function/method
        7 => theme::color(theme::ACCENT),        // keyword
        9 => theme::color(theme::TEXT_FAINT),    // comment
        10 => theme::color(0xa8b57a),            // string
        11 => theme::color(theme::ACCENT_3),     // number
        _ => theme::color(theme::TEXT),          // plain
    }
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

/// Find all non-overlapping occurrences of `query` in `text`, in document
/// order, honouring `case_sensitive` (exact vs. ASCII case-insensitive) and
/// `whole_word` (non-alphanumeric/`_` boundaries on both sides).
fn find_matches_in(
    text: &str,
    query: &str,
    case_sensitive: bool,
    whole_word: bool,
) -> Vec<std::ops::Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    let mut search_from = 0;
    while search_from <= text.len() {
        let relative = if case_sensitive {
            text[search_from..].find(query).map(|r| search_from + r)
        } else {
            (search_from..=text.len().saturating_sub(query.len())).find(|&offset| {
                text.as_bytes()
                    .get(offset..offset + query.len())
                    .is_some_and(|window| window.eq_ignore_ascii_case(query.as_bytes()))
            })
        };
        let Some(start) = relative else {
            break;
        };
        let end = start + query.len();
        if end > text.len() {
            break;
        }
        let is_whole = if whole_word {
            let bytes = text.as_bytes();
            let before_ok =
                start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
            let after_ok =
                end >= bytes.len() || !(bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_');
            before_ok && after_ok
        } else {
            true
        };
        if is_whole {
            matches.push(start..end);
        }
        search_from = if end == search_from { end + 1 } else { end };
    }
    matches
}

/// Find the first occurrence of `query` at or after `from`, excluding any
/// range already in `selected`. Non-overlapping: overlapping matches of an
/// already-selected range are skipped (searching the next whole occurrence).
fn find_occurrence_after(
    text: &str,
    query: &str,
    from: usize,
    selected: &[Range<usize>],
) -> Option<Range<usize>> {
    if query.is_empty() {
        return None;
    }
    let mut search_from = from;
    while search_from <= text.len() {
        let start = text[search_from..].find(query).map(|r| search_from + r)?;
        let end = start + query.len();
        if end > text.len() {
            return None;
        }
        let overlaps = selected
            .iter()
            .any(|r| start < r.end && end > r.start);
        if !overlaps {
            return Some(start..end);
        }
        search_from = if end == search_from { end + 1 } else { end };
    }
    None
}

/// Keep only folded regions that fit inside a `total`-line document, with a
/// valid `start <= end` ordering. Used when restoring persisted fold state.
fn clamp_folded_regions(total: usize, regions: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    regions
        .into_iter()
        .filter(|(start, end)| *start < total && *end < total && *end >= *start)
        .collect()
}

/// Return the closing character that pairs with an opening bracket/quote,
/// or `None` for non-auto-closable input. Only single-character openings are
/// handled; `"` and `'` close themselves.
fn auto_close_pair(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        '\'' => Some('\''),
        _ => None,
    }
}

/// Decide how a single typed character should be inserted, applying
/// bracket/quote auto-close. Returns the text to insert and the absolute
/// cursor offset after insertion (or `None` to use the default after-text).
fn auto_close_insert(
    text: &str,
    range: &Range<usize>,
    new_text: &str,
    single_range: bool,
) -> (String, Option<usize>) {
    if !range.is_empty() || !single_range || new_text.chars().count() != 1 {
        return (new_text.to_string(), None);
    }
    let ch = new_text.chars().next().unwrap();
    let after = text[range.end..].chars().next();
    // A closing char that already follows the cursor: skip over it rather
    // than inserting a duplicate (the auto-inserted close).
    if is_close_char(ch) && after == Some(ch) {
        return (new_text.to_string(), Some(range.end + ch.len_utf8()));
    }
    if let Some(close) = auto_close_pair(ch) {
        if after == Some(close) {
            // Skip over the already-present closing char.
            (new_text.to_string(), Some(range.end + ch.len_utf8()))
        } else {
            // Insert the pair; cursor lands between open and close.
            (format!("{ch}{close}"), Some(range.start + ch.len_utf8()))
        }
    } else {
        (new_text.to_string(), None)
    }
}

fn is_close_char(ch: char) -> bool {
    matches!(ch, ')' | ']' | '}' | '"' | '\'')
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

/// Convert a 0-based (line, character) position to a byte offset, clamping to
/// the document and line bounds. Mirrors `line_col_in`.
fn offset_for_line_char(text: &str, starts: &[usize], line: usize, character: usize) -> usize {
    let line = line.min(starts.len().saturating_sub(1));
    let line_start = starts[line];
    let line_end = starts
        .get(line + 1)
        .map(|next| next.saturating_sub(1))
        .unwrap_or(text.len());
    // The byte offset of the `character`-th char in the line; clamps to the
    // line end when the column is past the line.
    let offset = match text[line_start..line_end].char_indices().nth(character) {
        Some((index, _)) => line_start + index,
        None => line_end,
    };
    clamp_boundary(text, offset)
}

fn clamp_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
 }
 
/// Whether a byte is part of a word (alphanumeric or underscore). Word
/// boundaries for selection expansion/shrink are delimited by these.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

 fn clamp_range(text: &str, range: Range<usize>) -> Range<usize> {
    let start = clamp_boundary(text, range.start);
    let end = clamp_boundary(text, range.end.max(start));
    start..end
}

/// Deduplicate diagnostic marks by `(line, start_col, end_col)`. Multiple
/// marks covering the same range collapse into one; an error wins over a
/// warning on the same range.
fn dedup_diagnostic_marks(marks: Vec<DiagnosticMark>) -> Vec<DiagnosticMark> {
    let mut map: BTreeMap<(usize, usize, usize), DiagnosticMark> = BTreeMap::new();
    for mark in marks {
        let key = (mark.line, mark.start_col, mark.end_col);
        let is_error = mark.is_error;
        map.entry(key)
            .and_modify(|existing| existing.is_error |= is_error)
            .or_insert(mark);
    }
    map.into_values().collect()
}

fn matching_bracket_in(text: &str, offset: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let ch = *bytes.get(offset)?;
    let (open, close, direction) = match ch {
        b'(' => (b'(', b')', 1),
        b'[' => (b'[', b']', 1),
        b'{' => (b'{', b'}', 1),
        b')' => (b'(', b')', -1),
        b']' => (b'[', b']', -1),
        b'}' => (b'{', b'}', -1),
        _ => return None,
    };
    let (seek, nest) = if direction == 1 { (close, open) } else { (open, close) };
    let mut depth = 0i32;
    let mut index = offset as isize;
    loop {
        index += direction;
        if index < 0 || index as usize >= bytes.len() {
            return None;
        }
        let byte = bytes[index as usize];
        if byte == nest {
            depth += 1;
        } else if byte == seek {
            if depth == 0 {
                return Some(index as usize);
            }
            depth -= 1;
        }
    }
}

/// The indentation a newline should produce after `line`: its leading
/// whitespace, plus one level if the trimmed line ends with an opening
/// brace, paren, or bracket. Empty/whitespace-only lines yield no extra
/// indent beyond the inherited whitespace.
fn auto_indent_for_line(line: &str, indent_width: usize) -> String {
    let trimmed = line.trim_start();
    let base = &line[..line.len() - trimmed.len()];
    if line.trim_end().ends_with(['{', '(', '[']) {
        format!("{base}{}", " ".repeat(indent_width))
    } else {
        base.to_string()
    }
}

fn fold_regions_in(text: &str) -> Vec<(usize, usize)> {
    // Find brace pairs line by line: a `{` opens a region, its matching `}`
    // closes it. This is robust to comments and strings at the cost of only
    // handling `{}` — good enough for fold markers in C-like code.
    let lines: Vec<&str> = text.split('\n').collect();
    let mut regions = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let mut block_comment = false;
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if block_comment {
                if let Some(rel) = line[i..].find("*/") {
                    i += rel + 2;
                    block_comment = false;
                    continue;
                }
                break;
            }
            if line[i..].starts_with("/*") {
                block_comment = true;
                i += 2;
                continue;
            }
            if line[i..].starts_with("//") {
                break;
            }
            match bytes[i] {
                b'"' | b'\'' => {
                    let quote = bytes[i];
                    i += 1;
                    while i < bytes.len() && bytes[i] != quote {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                    i += 1;
                }
                b'{' => stack.push(index),
                b'}' => {
                    if let Some(start) = stack.pop() {
                        if start < index {
                            regions.push((start, index));
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
    regions.sort_unstable();
    regions
}

fn visible_line_in(text: &str, folded: &[(usize, usize)], line: usize) -> (usize, bool) {
    let total = text.split('\n').count();
    if line >= total {
        return (0, false);
    }
    // A folded region `(start, end)` keeps its anchor line `start` visible (the
    // fold indicator is drawn there) and hides `start + 1..=end`. A hidden line
    // maps to the anchor's visible position so callers can still locate it.
    let hidden_at = folded
        .iter()
        .find(|(start, end)| line > *start && line <= *end)
        .map(|(start, _)| *start);
    let map_to = hidden_at.unwrap_or(line);
    let mut visible = 0usize;
    for index in 0..map_to {
        if !folded.iter().any(|(start, end)| index > *start && index <= *end) {
            visible += 1;
        }
    }
    (visible, hidden_at.is_some())
}

fn visible_lines_in(text: &str, folded: &[(usize, usize)]) -> Vec<usize> {
    let total = text.split('\n').count();
    let mut lines = Vec::new();
    for index in 0..total {
        if folded.iter().any(|(start, end)| index > *start && index <= *end) {
            continue;
        }
        lines.push(index);
    }
    lines
}

#[derive(Clone)]
struct WrapRow {
    line: usize,
    byte_start: usize,
    byte_end: usize,
}

/// Compute the wrapped visual rows for a single document line at the given
/// pixel width. Returns `(byte_start, byte_end)` per visual row, row 0 first.
/// The returned vector is never empty (a blank line yields one empty row).
fn wrap_line_segments(
    text: &str,
    line: usize,
    width: Pixels,
    font_size: f32,
    font: &Font,
    window: &mut Window,
) -> Vec<(usize, usize)> {
    let starts = line_starts(text);
    let start = starts[line];
    let end = if line + 1 < starts.len() {
        starts[line + 1].saturating_sub(1)
    } else {
        text.len()
    };
    let slice = &text[start..end];
    if slice.is_empty() {
        return vec![(start, end)];
    }
    // Use gpui's line wrapper to compute the byte index of each visual break.
    // `Boundary.ix` is the index of the last character before a wrap.
    let mut wrapper = window.text_system().line_wrapper(font.clone(), px(font_size));
    let boundaries: Vec<usize> = wrapper
        .wrap_line(&[gpui::LineFragment::text(slice)], width)
        .map(|b| b.ix)
        .collect();
    let mut segments = Vec::with_capacity(boundaries.len() + 1);
    let mut byte = 0usize;
    for boundary in boundaries.iter().chain(std::iter::once(&slice.len())) {
        let end_byte = (*boundary).min(slice.len());
        if end_byte > byte {
            segments.push((start + byte, start + end_byte));
            byte = end_byte;
        }
    }
    if segments.is_empty() || byte < slice.len() {
        segments.push((start + byte, start + slice.len()));
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_marks_dedup_and_error_wins() {
        assert_eq!(dedup_diagnostic_marks(vec![]), vec![]);
        let mk = |line, start, end, is_error| DiagnosticMark {
            line,
            start_col: start,
            end_col: end,
            message: "m".to_string(),
            is_error,
        };
        let rows = dedup_diagnostic_marks(vec![
            mk(3, 0, 4, false),
            mk(1, 2, 5, true),
            mk(3, 0, 4, true),
            mk(1, 2, 5, false),
            mk(3, 0, 4, false),
        ]);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|m| m.line == 1 && m.is_error));
        assert!(rows.iter().any(|m| m.line == 3 && m.is_error));
    }

    /// `diagnostic_at_position` maps a window point to a document line (via
    /// the visual row) and matches whole-line marks (start == end), returning
    /// the hovered mark index. Outside the gutter/above the top returns None.
    #[test]
    fn diagnostic_hover_hit_tests_visual_line() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| {
            let mut e = Editor::new("a\nb\nc\n".to_string(), None, cx);
            e.last_bounds =
                Some(Bounds::new(Point::new(px(0.), px(0.)), size(px(600.), px(200.))));
            e.diagnostics = vec![DiagnosticMark {
                line: 1,
                start_col: 0,
                end_col: 0,
                message: "line b".to_string(),
                is_error: true,
            }];
            e
        });
        cx.update(|cx| {
            // Row 1 (0-based) = document line "b" at y = 1 * line_height + a bit.
            let line_height = editor.read(cx).line_height();
            let y = px(line_height * 1.0 + 2.0);
            let hit = editor
                .read(cx)
                .diagnostic_at_position(Point::new(px(200.), y));
            assert_eq!(hit, Some(0));
            // Above the editor top: no hit.
            let miss = editor
                .read(cx)
                .diagnostic_at_position(Point::new(px(200.), px(0.)));
            assert_eq!(miss, None);
        });
    }

    /// `set_diagnostics` dedups and clears the hovered index; the hovered
    /// state resets so a stale tooltip does not linger after a re-analysis.
    #[test]
    fn set_diagnostics_resets_hovered_mark() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| {
            let mut e = Editor::new("a\nb\n".to_string(), None, cx);
            e.hovered_diagnostic = Some(0);
            e
        });
        cx.update(|cx| {
            editor.update(cx, |e, cx| {
                e.set_diagnostics(
                    vec![DiagnosticMark {
                        line: 0,
                        start_col: 0,
                        end_col: 0,
                        message: "m".to_string(),
                        is_error: false,
                    }],
                    cx,
                );
                assert_eq!(e.hovered_diagnostic, None);
                assert_eq!(e.diagnostics.len(), 1);
            });
        });
    }

    /// `word_at_position` returns None over the gutter or above the editor,
    /// and the identifier under a valid code position when shaped lines are
    /// available (whole-word range derived from the document).
    #[test]
    fn word_at_position_extracts_identifier() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| {
            let mut e = Editor::new("int counter = 5;\n".to_string(), None, cx);
            e.last_bounds =
                Some(Bounds::new(Point::new(px(0.), px(0.)), size(px(600.), px(200.))));
            e
        });
        cx.update(|cx| {
            let editor = editor.read(cx);
            // Over the gutter (x < GUTTER_WIDTH): no word.
            assert_eq!(
                editor.word_at_position(Point::new(px(GUTTER_WIDTH - 4.0), px(10.))),
                None
            );
            // Above the editor top: no word.
            assert_eq!(editor.word_at_position(Point::new(px(200.), px(0.))), None);
            // Without shaped lines (no layout yet) the column cannot resolve.
            assert_eq!(editor.word_at_position(Point::new(px(200.), px(10.))), None);
        });
    }

    /// `set_coverage` stores per-line hit counts and clears when given an
    /// empty vector; uncovered lines are retained as `0`.
    #[test]
    fn editor_coverage_stores_and_clears() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("a\nb\nc\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.set_coverage(vec![2, 0, 1], cx);
                assert_eq!(editor.coverage, vec![2, 0, 1]);
                editor.set_coverage(vec![], cx);
                assert!(editor.coverage.is_empty());
            });
         });
    }
 
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

    #[test]
    fn word_under_cursor_trims_member_and_index_edges() {
        // Helper mirroring the token scan so the cursor placement can be tested
        // without constructing a full view.
        fn word(text: &str, cursor: usize) -> String {
            let is_word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '.' | '[' | ']' | '-');
            let before = text[..cursor]
                .char_indices()
                .rev()
                .take_while(|(_, c)| is_word(*c))
                .last()
                .map(|(index, _)| index)
                .unwrap_or(cursor);
            let after = text[cursor..]
                .char_indices()
                .take_while(|(_, c)| is_word(*c))
                .last()
                .map(|(index, c)| cursor + index + c.len_utf8())
                .unwrap_or(cursor);
            text[before..after]
                .trim_start_matches(|c: char| matches!(c, '.' | '-' | '[' | ']'))
                .trim_end_matches(|c: char| matches!(c, '.' | '-' | '[' | ']'))
                .to_string()
        }
        let text = "if (flags.bits && obj->count) {";
        assert_eq!(word(text, 8), "flags.bits");
        // `->` splits the member access, so only the member name is returned.
        assert_eq!(word(text, 24), "count");
        assert_eq!(word(text, 4), "flags.bits");
    }

    #[test]
    fn word_range_at_selects_identifier_edges() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("foo_bar baz-42\nx = y;\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, _| {
                // Mid-word: expands both directions.
                assert_eq!(editor.word_range_at(4), Some(0..7));
                // On a separator: `baz-42` — hyphen is a separator here, so the
                // word is just `baz`.
                assert_eq!(editor.word_range_at(9), Some(8..11));
                // On whitespace: no word.
                assert_eq!(editor.word_range_at(7), None);
                // On the digit tail of `42`.
                assert_eq!(editor.word_range_at(13), Some(12..14));
            });
        });
    }

    #[test]
    fn line_range_at_spans_full_lines() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("alpha\nbeta\ngamma".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, _| {
                // Middle line includes its trailing newline: beta = 6..10, \n at 10.
                assert_eq!(editor.line_range_at(7), 6..11);
                // Last line runs to EOF: gamma = 11..16.
                assert_eq!(editor.line_range_at(13), 11..16);
                // First line: alpha = 0..4, \n at 5.
                assert_eq!(editor.line_range_at(2), 0..6);
            });
        });
    }

    #[test]
    fn double_click_selects_word_and_triple_selects_line() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("hello world\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, _cx| {
                // Double-click on "world" (byte 6..11): select that word.
                editor.selection = 0..0;
                editor.cursor = 0;
                let word = editor.word_range_at(7).unwrap();
                editor.selection = word.clone();
                editor.cursor = word.end;
                assert_eq!(&editor.text[editor.selection.clone()], "world");
                // Triple-click: select the whole line.
                let line = editor.line_range_at(3);
                assert_eq!(&editor.text[line.clone()], "hello world\n");
            });
        });
    }



    #[test]
    fn bracket_matching_pairs() {
        let text = "fn main() { let x = (1 + 2); }";
        // `(` at 7 pairs with `)` at 8.
        assert_eq!(matching_bracket_in(text, 7), Some(8));
        assert_eq!(matching_bracket_in(text, 8), Some(7));
        // `{` at 10 pairs with `}` at 29.
        assert_eq!(matching_bracket_in(text, 10), Some(29));
        assert_eq!(matching_bracket_in(text, 29), Some(10));
        // Not a bracket.
        assert_eq!(matching_bracket_in(text, 4), None);
    }

    #[test]
    fn bracket_matching_nested_ignores_inner() {
        let text = "f(g(h(x)))";
        // `(` at 1 pairs with `)` at 9; `(` at 3 pairs with `)` at 8.
        assert_eq!(matching_bracket_in(text, 1), Some(9));
        assert_eq!(matching_bracket_in(text, 3), Some(8));
    }

    #[test]
    fn fold_regions_indentation() {
        let text = "fn main() {\n    let x = 1;\n    if x > 0 {\n        y();\n    }\n}\n";
        let regions = fold_regions_in(text);
        assert!(regions.contains(&(0, 5)), "function block: {regions:?}");
        assert!(regions.contains(&(2, 4)), "if block: {regions:?}");
    }

    #[test]
    fn auto_indent_inherits_leading_whitespace() {
        // A line indented four spaces keeps that indent on the next line.
        assert_eq!(auto_indent_for_line("    let x = 1;", 4), "    ");
        // A line opening a block adds one level.
        assert_eq!(auto_indent_for_line("    if x > 0 {", 4), "        ");
        // Paren/bracket also open a continuation indent.
        assert_eq!(auto_indent_for_line("fn call(a, b) {", 4), "    ");
        // A closing brace keeps the enclosing indentation (no extra).
        assert_eq!(auto_indent_for_line("    }", 4), "    ");
        // An empty line yields no indent.
        assert_eq!(auto_indent_for_line("", 4), "");
        // Whitespace-only line keeps its whitespace.
        assert_eq!(auto_indent_for_line("  \t", 4), "  \t");
        // A two-space indent width produces a smaller continuation level.
        assert_eq!(auto_indent_for_line("fn call(a, b) {", 2), "  ");
        assert_eq!(auto_indent_for_line("fn call(a, b) {", 8), "        ");
    }

    #[test]
    fn indent_lines_indents_each_selected_line() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("a\nb\nc\nd\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                // Indent lines 1..=2 (the "b" and "c" lines).
                editor.indent_lines(1, 2, cx);
                assert_eq!(&editor.text, "a\n    b\n    c\nd\n");
                // The cursor advanced by 4 spaces per indented line (2 lines),
                // from its initial position at the end of the document (8).
                assert_eq!(editor.cursor, 16);
            });
        });
    }

    #[test]
    fn indent_lines_indents_single_line() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("x\ny\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.indent_lines(0, 0, cx);
                assert_eq!(&editor.text, "    x\ny\n");
            });
        });
    }

    /// `set_indent_width` clamps to 2/4/8, and `indent_lines`/`tab` insert the
    /// configured number of spaces per level. Auto-indent also uses the width.
    #[test]
    fn indent_width_configures_tab_and_auto_indent() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("a\nb\nc\nd\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                // Invalid widths fall back to 4.
                editor.set_indent_width(3, cx);
                assert_eq!(editor.indent_width(), 4);
                editor.set_indent_width(2, cx);
                assert_eq!(editor.indent_width(), 2);
                assert_eq!(editor.indent_string(), "  ");

                // `indent_lines` inserts two spaces per line and advances the
                // cursor by two per indented line.
                editor.indent_lines(0, 1, cx);
                assert_eq!(&editor.text, "  a\n  b\nc\nd\n");
                assert_eq!(editor.cursor, 12); // 8 bytes + 2 spaces * 2 lines

                // Auto-indent on a block opener uses the configured width.
                assert_eq!(auto_indent_for_line("fn f() {", 2), "  ");
            });
        });
    }

    #[test]
    fn visible_lines_skip_folded() {
        let folded = vec![(1, 2)];
        // Folded (1,2) hides lines 2..=3 (0-based 1..=2 inclusive), keeping the
        // anchor line 1 visible. Visible lines are 0, 1, 3, 4.
        assert_eq!(visible_lines_in("a\nb\nc\nd\n", &folded), vec![0, 1, 3, 4]);
        assert_eq!(visible_line_in("a\nb\nc\nd\n", &folded, 0), (0, false));
        assert_eq!(visible_line_in("a\nb\nc\nd\n", &folded, 1), (1, false));
        assert_eq!(visible_line_in("a\nb\nc\nd\n", &folded, 2), (1, true));
        assert_eq!(visible_line_in("a\nb\nc\nd\n", &folded, 3), (2, false));
        assert_eq!(visible_line_in("a\nb\nc\nd\n", &folded, 4), (3, false));
        assert_eq!(visible_lines_in("a\nb\nc\nd\n", &folded).len(), 4);
    }

    #[test]
    fn find_matches_case_sensitive_and_insensitive() {
        let text = "int main() { return main; }";
        // Case-sensitive: only the exact `main` tokens.
        let sensitive = find_matches_in(text, "main", true, false);
        assert_eq!(sensitive, vec![4..8, 20..24]);
        // Case-insensitive: also matches `Main`/`MAIN` if present.
        let text = "Main main MAIN";
        assert_eq!(find_matches_in(text, "main", false, false).len(), 3);
        assert_eq!(find_matches_in(text, "MAIN", false, false).len(), 3);
    }

    #[test]
    fn find_matches_whole_word_respects_boundaries() {
        let text = "foo foobar foo_bar _foo foo_";
        // Whole word: only standalone `foo`, not `foobar`, `foo_bar`, `_foo`, `foo_`.
        let whole = find_matches_in(text, "foo", true, true);
        assert_eq!(whole, vec![0..3]);
        // Substring still finds all five occurrences.
        assert_eq!(find_matches_in(text, "foo", true, false).len(), 5);
    }

    #[test]
    fn find_matches_skips_overlapping_and_empty() {
        // Non-overlapping: "aaaa" with "aa" yields two matches (0..2, 2..4).
        assert_eq!(find_matches_in("aaaa", "aa", true, false), vec![0..2, 2..4]);
        assert!(find_matches_in("text", "", true, false).is_empty());
     }

    /// Setting a find query populates the match ranges the renderer highlights,
    /// and `find_next` cycles the current match (rendered stronger).
    #[test]
    fn find_query_populates_highlight_matches_and_cycles() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("let a = 1; let b = a + a;\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.set_find_query("a", cx);
                 assert_eq!(editor.find_matches, vec![4..5, 19..20, 23..24]);
                assert_eq!(editor.find_current, 0);

                 editor.find_next(false, cx);
                 assert_eq!(editor.find_current, 1);
                 editor.find_next(false, cx);
                 assert_eq!(editor.find_current, 2);
                 // Forward wraps back to the first match (3 matches, index 2 -> 0).
                 editor.find_next(false, cx);
                 assert_eq!(editor.find_current, 0);

                 // Backwards from the first match wraps to the last match.
                 editor.find_next(true, cx);
                 assert_eq!(editor.find_current, 2);
            });
        });
    }
 
     #[test]
    fn clamps_folded_regions_to_document() {
        // total = 4 lines: only regions fully inside 0..=3 survive.
        let regions = vec![(0, 3), (1, 2), (3, 4), (2, 1), (5, 6)];
        assert_eq!(clamp_folded_regions(4, regions), vec![(0, 3), (1, 2)]);
    }

    /// The minimap toggle must flip its visibility state and notify.
    #[test]
    fn toggles_minimap_visibility() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("a\nb\nc\n".to_string(), None, cx));
        cx.update(|cx| {
            assert!(editor.read(cx).show_minimap);
            editor.update(cx, |editor, cx| editor.toggle_minimap(cx));
            assert!(!editor.read(cx).show_minimap);
            editor.update(cx, |editor, cx| editor.toggle_minimap(cx));
            assert!(editor.read(cx).show_minimap);
        });
    }

    /// `scroll_to_ratio` must clamp to [0, 1] and respect the scroll bounds.
    #[test]
    fn scroll_to_ratio_clamps_and_scrolls() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| {
            let mut e = Editor::new("line0\nline1\nline2\n".to_string(), None, cx);
            e.viewport_height = 22.0; // one line visible
            e
        });
        cx.update(|cx| {
            editor.update(cx, |editor, cx| editor.scroll_to_ratio(0.5, cx));
            // 4 lines (trailing newline yields an empty last line) at 22px,
            // minus a 22px viewport = 66px of scrollable range.
            let expected = 66.0 * 0.5;
            assert!((editor.read(cx).scroll_y - expected).abs() < 0.01);
            editor.update(cx, |editor, cx| editor.scroll_to_ratio(5.0, cx));
            assert!((editor.read(cx).scroll_y - 66.0).abs() < 0.01);
            editor.update(cx, |editor, cx| editor.scroll_to_ratio(-1.0, cx));
            assert!(editor.read(cx).scroll_y.abs() < 0.01);
        });
    }

    /// Adding a cursor vertically must place it on the adjacent line at the
    /// same column, and a subsequent insert must apply at every cursor.
    #[test]
    fn add_cursor_vertical_and_multi_insert() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| {
            let mut e = Editor::new("abc\ndef\nghi\n".to_string(), None, cx);
            // Move the primary cursor to line 1, column 1 (before "abc").
            e.move_to(0, cx);
            e
        });
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.add_cursor_vertical(1, cx);
                assert_eq!(editor.secondary.len(), 1);
                assert_eq!(editor.secondary[0], 4..4); // start of "def"
                // Insert "X" -> must appear at both cursors.
                editor.insert_completion("X", false, None, cx);
                assert_eq!(&editor.text, "Xabc\nXdef\nghi\n");
                assert!(editor.secondary.is_empty());
            });
        });
    }

    /// Adding a cursor beyond the first/last line must be a no-op, and adding
    /// the same offset twice must not duplicate it.
    #[test]
    fn add_cursor_vertical_bounds_and_dedupe() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| {
            let mut e = Editor::new("abc\ndef\n".to_string(), None, cx);
            e.move_to(0, cx);
            e
        });
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.add_cursor_vertical(-1, cx); // above first line: no-op
                assert!(editor.secondary.is_empty());
                editor.add_cursor_vertical(1, cx);
                assert_eq!(editor.secondary.len(), 1);
                editor.add_cursor_vertical(1, cx); // same offset: deduped
                assert_eq!(editor.secondary.len(), 1);
                editor.add_cursor_vertical(1, cx); // past last line: no-op
                assert_eq!(editor.secondary.len(), 1);
            });
        });
    }

    /// Clearing secondary cursors must remove them and leave the primary alone.
    #[test]
    fn clear_secondary_removes_extras() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| {
            let mut e = Editor::new("abc\ndef\nghi\n".to_string(), None, cx);
            e.move_to(0, cx);
            e
        });
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.add_cursor_vertical(1, cx);
                editor.add_cursor_vertical(2, cx);
                assert_eq!(editor.secondary.len(), 2);
                editor.clear_secondary(cx);
                assert!(editor.secondary.is_empty());
                // The primary cursor is untouched.
                assert_eq!(editor.cursor, 0);
            });
        });
    }

    /// `Cmd/Ctrl+D` selects the word, then adds the next non-overlapping
    /// occurrence as the primary selection (prior ones move to `secondary`),
    /// wrapping to the start after the last. The search is a literal substring
    /// match; already-selected ranges are skipped.
    #[test]
    fn select_next_occurrence_accumulates_and_wraps() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| {
            let mut e = Editor::new("foo bar foo baz foo\n".to_string(), None, cx);
            e.move_to(0, cx); // cursor on the first `foo`
            e
        });
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                // Press 1: select the word at the cursor.
                editor.select_next_occurrence(cx);
                assert_eq!(&editor.text[editor.selection.clone()], "foo");
                assert_eq!(editor.selection, 0..3);
                assert!(editor.secondary.is_empty());
                // Press 2: the first selection becomes secondary; primary moves on.
                editor.select_next_occurrence(cx);
                assert_eq!(editor.selection, 8..11);
                assert_eq!(editor.secondary, vec![0..3]);
                // Press 3: the third (last) match.
                editor.select_next_occurrence(cx);
                assert_eq!(editor.selection, 16..19);
                assert_eq!(editor.secondary.len(), 2);
                // All three `foo` occurrences are now selected. The next
                // press finds none and resets to the first one only.
                editor.select_next_occurrence(cx);
                assert_eq!(editor.selection, 0..3);
                assert!(editor.secondary.is_empty(), "reset clears secondary");
                // A further press selects the second match again.
                editor.select_next_occurrence(cx);
                assert_eq!(editor.selection, 8..11);
                assert_eq!(editor.secondary, vec![0..3]);
            });
        });
    }

    /// A collapsed cursor (no selection) on a non-word char must be a no-op,
    /// and a selection must be reused verbatim as the search string.
    #[test]
    fn select_next_occurrence_uses_selection_and_skips_nonwords() {
         let mut cx = gpui::TestAppContext::single();
         let editor = cx.new(|cx| {
            let mut e = Editor::new("foo  foo\n".to_string(), None, cx);
            e.move_to(4, cx); // on the space between the two words
             e
         });
         cx.update(|cx| {
             editor.update(cx, |editor, cx| {
                 editor.select_next_occurrence(cx);
                // A collapsed cursor on a space has no word to select.
                 assert!(editor.selection.is_empty());
                 // Seed an explicit selection and search for it.
                 editor.selection = 0..3;
                 editor.cursor = 3;
                 editor.select_next_occurrence(cx);
                assert_eq!(editor.selection, 5..8);
                 assert_eq!(editor.secondary, vec![0..3]);
             });
         });
     }

    /// `apply_text_edit` must replace a 0-based LSP range and mark the document
    /// dirty, leaving a single collapsed cursor after the inserted text.
    #[test]
    fn applies_lsp_text_edit() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("let x = 1;\nlet y = 2;\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, _cx| {
                editor.apply_text_edit(0, 4, 0, 5, "value");
                assert_eq!(editor.text, "let value = 1;\nlet y = 2;\n");
                assert!(editor.dirty);
                assert_eq!(editor.cursor, 9); // after "let value"
                assert_eq!(editor.selection, 9..9);
            });
        });
    }

    /// Out-of-bounds edit ranges must be rejected (document unchanged).
    #[test]
    fn rejects_out_of_bounds_lsp_edit() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("a\nb\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, _cx| {
                let before = editor.text.clone();
                editor.apply_text_edit(99, 0, 99, 1, "x");
                assert_eq!(editor.text, before);
            });
        });
    }


    #[test]
    fn auto_close_inserts_pair_and_places_cursor_inside() {
        let (insert, cursor) = auto_close_insert("", &(0..0), "(", true);
        assert_eq!(insert, "()");
        assert_eq!(cursor, Some(1));

        // Nested: typing '[' at offset 1 inside "()" inserts inside.
        let (insert, cursor) = auto_close_insert("()", &(1..1), "[", true);
        assert_eq!(insert, "[]");
        assert_eq!(cursor, Some(2));

        // Quote pairs close themselves.
        let (insert, cursor) = auto_close_insert("", &(0..0), "\"", true);
        assert_eq!(insert, "\"\"");
        assert_eq!(cursor, Some(1));
    }

    #[test]
    fn auto_close_skips_existing_close() {
        // Cursor at 1 in "()", typing ')' skips over the existing close.
        let (insert, cursor) = auto_close_insert("()", &(1..1), ")", true);
        assert_eq!(insert, ")");
        assert_eq!(cursor, Some(2));
        // Quote: skip over the auto-inserted closing quote.
        let (insert, cursor) = auto_close_insert("\"\"", &(1..1), "\"", true);
        assert_eq!(insert, "\"");
        assert_eq!(cursor, Some(2));
    }

    #[test]
    fn auto_close_disabled_for_selection_and_multi_range() {
        // Non-empty selection: plain replacement, no expansion.
        let (insert, cursor) = auto_close_insert("abc", &(0..3), "(", true);
        assert_eq!(insert, "(");
        assert_eq!(cursor, None);
        // Multi-range (secondary cursors): no auto-close.
        let (insert, cursor) = auto_close_insert("", &(0..0), "(", false);
        assert_eq!(insert, "(");
        assert_eq!(cursor, None);
        // Multi-char paste: no auto-close.
        let (insert, cursor) = auto_close_insert("", &(0..0), "(x)", true);
        assert_eq!(insert, "(x)");
        assert_eq!(cursor, None);
    }

    #[test]
    fn word_wrap_coordinate_mapping() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("aaaa bbbb cccc\ndd\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.set_word_wrap(true, cx);
                assert!(editor.word_wrap());
                // Simulate a wrap model: line 0 wraps into two rows at byte 9
                // ("aaaa bbbb" | " cccc"), line 1 is one row.
                editor.wrap_rows = vec![
                    WrapRow { line: 0, byte_start: 0, byte_end: 9 },
                    WrapRow { line: 0, byte_start: 9, byte_end: 14 },
                    WrapRow { line: 1, byte_start: 15, byte_end: 17 },
                ];
                assert_eq!(editor.visual_row_count(), 3);
                // Offset 3 (inside "aaaa") → row 0, in-row byte 3.
                assert_eq!(editor.visual_position(3), (0, 3));
                // Offset 10 (inside " cccc") → row 1, in-row byte 1.
                assert_eq!(editor.visual_position(10), (1, 1));
                // Offset 16 (line 1) → row 2.
                assert_eq!(editor.visual_position(16), (2, 1));
                // Gutter markers: line 0 first row is row 0; line 1 is row 2.
                assert_eq!(editor.visual_row_for_line(0), Some(0));
                assert_eq!(editor.visual_row_for_line(1), Some(2));
                // Toggling off clears the model and falls back to lines.
                editor.set_word_wrap(false, cx);
                assert!(!editor.word_wrap());
                assert!(editor.wrap_rows.is_empty());
                assert_eq!(editor.visual_row_count(), 3);
            });
        });
    }

    /// Overlapping inlay-hint responses must not render the same hint twice:
    /// `set_inlay_hints` deduplicates by (line, start).
    #[test]
    fn inlay_hints_dedup_by_position() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("fn main() {}\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.set_inlay_hints(
                    vec![
                        InlayHint { line: 0, start: 4, label: "a".into() },
                        InlayHint { line: 0, start: 4, label: "a".into() },
                        InlayHint { line: 0, start: 9, label: "b".into() },
                    ],
                    cx,
                );
                assert_eq!(editor.inlay_hints.len(), 2);
                assert_eq!(editor.inlay_hints[0], InlayHint { line: 0, start: 4, label: "a".into() });
                assert_eq!(editor.inlay_hints[1], InlayHint { line: 0, start: 9, label: "b".into() });
            });
        });
    }
     /// `toggle_comment` adds `// ` to a single line and removes it on the
     /// second invocation (round trip).
     #[test]
     fn toggle_comment_round_trips() {
         let mut cx = gpui::TestAppContext::single();
         let editor = cx.new(|cx| Editor::new("foo\nbar\n".to_string(), Some(Language::Rust), cx));
         cx.update(|cx| {
             editor.update(cx, |editor, cx| {
                 editor.cursor = 0;
                 editor.selection = 0..0;
                 editor.toggle_comment(cx);
                 assert_eq!(editor.text, "// foo\nbar\n");
                 editor.toggle_comment(cx);
                 assert_eq!(editor.text, "foo\nbar\n");
             });
         });
     }
 
     /// `toggle_comment` over a multi-line selection comments every line and
     /// uncomments them on the second invocation.
     #[test]
     fn toggle_comment_multi_line() {
         let mut cx = gpui::TestAppContext::single();
         let editor = cx.new(|cx| Editor::new("a\nb\nc\n".to_string(), Some(Language::C), cx));
         cx.update(|cx| {
             editor.update(cx, |editor, cx| {
                 editor.cursor = 4;
                 editor.selection = 2..6; // spans lines 1..=2 (b, c)
                 editor.toggle_comment(cx);
                 assert_eq!(editor.text, "a\n// b\n// c\n");
                // The first pass grew the text; reset the selection so the
                // uncomment pass covers both commented lines.
                editor.selection = 2..12;
                 editor.toggle_comment(cx);
                 assert_eq!(editor.text, "a\nb\nc\n");
             });
         });
     }
 
     /// `toggle_comment` is a no-op for assembly (no `//` line comment).
     #[test]
     fn toggle_comment_noop_for_asm() {
         let mut cx = gpui::TestAppContext::single();
         let editor = cx.new(|cx| Editor::new("ldr r0, r1\n".to_string(), Some(Language::Asm), cx));
         cx.update(|cx| {
             editor.update(cx, |editor, cx| {
                 editor.cursor = 0;
                 editor.selection = 0..0;
                 editor.toggle_comment(cx);
                 assert_eq!(editor.text, "ldr r0, r1\n");
             });
         });
     }
 
     /// `duplicate_line` copies the current line below itself.
     #[test]
     fn duplicate_line_copies_below() {
         let mut cx = gpui::TestAppContext::single();
         let editor = cx.new(|cx| Editor::new("a\nb\n".to_string(), None, cx));
         cx.update(|cx| {
             editor.update(cx, |editor, cx| {
                 editor.cursor = 2; // on "b"
                 editor.selection = 2..2;
                 editor.duplicate_line(cx);
                 assert_eq!(editor.text, "a\nb\nb\n");
                 assert_eq!(editor.cursor, 4);
             });
         });
     }
 
     /// `delete_line` removes the current line including its newline.
     #[test]
     fn delete_line_removes_line() {
         let mut cx = gpui::TestAppContext::single();
         let editor = cx.new(|cx| Editor::new("a\nb\nc\n".to_string(), None, cx));
         cx.update(|cx| {
             editor.update(cx, |editor, cx| {
                 editor.cursor = 2; // on "b"
                 editor.selection = 2..2;
                 editor.delete_line(cx);
                 assert_eq!(editor.text, "a\nc\n");
             });
         });
     }
 
     /// `move_line` swaps the current line with the one above/below.
     #[test]
     fn move_line_swaps() {
         let mut cx = gpui::TestAppContext::single();
         let editor = cx.new(|cx| Editor::new("a\nb\nc\n".to_string(), None, cx));
         cx.update(|cx| {
             editor.update(cx, |editor, cx| {
                 editor.cursor = 2; // on "b"
                 editor.selection = 2..2;
                 editor.move_line(-1, cx);
                 assert_eq!(editor.text, "b\na\nc\n");
                 editor.move_line(1, cx);
                 assert_eq!(editor.text, "a\nb\nc\n");
             });
         });
     }
 
     /// `move_line` refuses to move the first line up or the last line down.
     #[test]
     fn move_line_respects_bounds() {
         let mut cx = gpui::TestAppContext::single();
         let editor = cx.new(|cx| Editor::new("a\nb\n".to_string(), None, cx));
         cx.update(|cx| {
             editor.update(cx, |editor, cx| {
                 editor.cursor = 0;
                 editor.selection = 0..0;
                 editor.move_line(-1, cx);
                 assert_eq!(editor.text, "a\nb\n");
                 editor.cursor = 2;
                editor.selection = 2..2;
                 editor.move_line(1, cx);
                 assert_eq!(editor.text, "a\nb\n");
             });
         });
     }

    /// `expand_or_shrink_selection` grows the selection to word boundaries and
    /// shrinks it back.
    #[test]
    fn expand_and_shrink_selection_at_word_boundaries() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("hello world foo\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.cursor = 0;
                editor.selection = 0..0;
                editor.expand_or_shrink_selection(true, cx);
                assert_eq!(editor.selection, 0..5, "first word 'hello'");
                editor.expand_or_shrink_selection(true, cx);
                assert_eq!(editor.selection, 0..11, "through ' world'");
                editor.expand_or_shrink_selection(false, cx);
                assert_eq!(editor.selection, 0..5, "shrink back to 'hello'");
            });
        });
    }

    /// Zoom clamps to 0.5..=2.0 and scales the effective line height and font
    /// size proportionally; reset returns to exactly 100%.
    #[test]
    fn editor_zoom_scales_and_clamps() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("x\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                assert_eq!(editor.zoom(), 1.0);
                let base_lh = editor.line_height();
                let base_fs = editor.font_size();
                assert!((base_lh - LINE_HEIGHT).abs() < 1e-4);
                assert!((base_fs - FONT_SIZE).abs() < 1e-4);

                editor.set_zoom(1.5, cx);
                assert!((editor.zoom() - 1.5).abs() < 1e-4);
                assert!((editor.line_height() - LINE_HEIGHT * 1.5).abs() < 1e-3);
                assert!((editor.font_size() - FONT_SIZE * 1.5).abs() < 1e-3);

                // Clamp to the upper bound.
                editor.set_zoom(3.0, cx);
                assert!((editor.zoom() - 2.0).abs() < 1e-4);
                // Clamp to the lower bound.
                editor.set_zoom(0.2, cx);
                assert!((editor.zoom() - 0.5).abs() < 1e-4);
                // Reset to 100%.
                editor.set_zoom(1.0, cx);
                assert!((editor.zoom() - 1.0).abs() < 1e-4);
                assert!((editor.line_height() - LINE_HEIGHT).abs() < 1e-4);
            });
        });
    }


    /// Toggling a bookmark line flips membership in the editor's bookmark set;
    /// line 0 is a no-op.
    #[test]
    fn toggle_bookmark_line_flips_membership() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("a\nb\nc\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.toggle_bookmark_line(2, cx);
                assert!(editor.bookmarks.contains(&2));
                editor.toggle_bookmark_line(2, cx);
                assert!(!editor.bookmarks.contains(&2));
                editor.toggle_bookmark_line(0, cx); // no-op
                assert!(editor.bookmarks.is_empty());
            });
        });
    }

    /// `transform_selection_case` uppercases/lowercases the selected text and
    /// keeps it selected; with a collapsed cursor it transforms the word under
    /// the cursor.
    #[test]
    fn transform_selection_case_upper_and_lower() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("hello world\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                // Select "hello" (offsets 0..5).
                editor.selection = 0..5;
                editor.cursor = 5;
                editor.transform_selection_case(true, cx);
                assert_eq!(editor.text(), "HELLO world\n");
                assert_eq!(editor.selection, 0..5);

                editor.transform_selection_case(false, cx);
                assert_eq!(editor.text(), "hello world\n");
            });
        });
    }

    /// `join_lines` collapses the selected (or current + next) lines into one,
    /// trimming line text and separating with single spaces.
    #[test]
    fn join_lines_merges_with_spaces() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| {
            Editor::new("alpha\n  beta  \n\ngamma\n".to_string(), None, cx)
        });
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.selection = 0..18; // covers alpha..gamma
                editor.cursor = 18;
                editor.join_lines(cx);
                assert_eq!(editor.text(), "alpha beta gamma\n");
            });
        });
    }

    /// `sort_lines` sorts the selected lines (or the whole document) by byte
    /// order and leaves already-sorted text untouched.
    #[test]
    fn sort_lines_orders_and_skips_sorted() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| {
            Editor::new("banana\napple\ncherry\n".to_string(), None, cx)
        });
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.selection = 0..editor.text().len();
                editor.cursor = editor.text().len();
                editor.sort_lines(cx);
                assert_eq!(editor.text(), "apple\nbanana\ncherry\n");
            });
        });
    }

    /// `trim_trailing_whitespace` removes spaces/tabs at line ends; with a
    /// selection only the spanned lines are trimmed, otherwise the document.
    #[test]
    fn trim_trailing_whitespace_trims_lines() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| {
            Editor::new("a  \nb\t\nc\n".to_string(), None, cx)
        });
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.trim_trailing_whitespace(cx);
                assert_eq!(editor.text(), "a\nb\nc\n");
            });
        });
    }

    /// The context-menu helpers expose selection/clipboard operations: copying
    /// a selection puts its text on the clipboard, and `selected_text` returns
    /// the selected span.
    #[test]
    fn editor_context_menu_helpers() {
        let mut cx = gpui::TestAppContext::single();
        let editor = cx.new(|cx| Editor::new("a\nb\nc\n".to_string(), None, cx));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                // Select the whole "b" line (offsets 2..3).
                editor.selection = 2..3;
                editor.cursor = 3;
                assert_eq!(editor.selected_text(), "b");
                editor.copy_selection_to_clipboard(cx);
                let clip = cx.read_from_clipboard().and_then(|item| item.text());
                assert_eq!(clip.as_deref(), Some("b"));
            });
        });
    }
 }
