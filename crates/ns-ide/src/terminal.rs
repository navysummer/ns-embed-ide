//! Embedded system shell terminal backed by a pseudo-terminal (PTY).
//!
//! The terminal spawns the user's login shell in a PTY and streams its output
//! into a [`rio_vt`] VT state machine (`Crosswords`), which parses ANSI/escape
//! sequences into a grid with scrollback. Rendering pulls the visible rows and
//! draws them as styled text runs. This is a full VT100/ANSI terminal core —
//! colors, cursor movement, alternate screen, and scrollback all work.
//!
//! The reader thread is driven by [`rio_vt::performer::Machine`], which wires
//! the PTY to the parser. Input keystrokes are written back through the
//! machine's message channel.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use gpui::prelude::*;
use gpui::{Context, EventEmitter, FocusHandle, Render, SharedString, px};
use rio_vt::ansi::CursorShape;
use rio_vt::config::colors::{AnsiColor, NamedColor};
use rio_vt::crosswords::pos::Column;
use rio_vt::crosswords::style::{Style, StyleFlags};
use rio_vt::crosswords::{Crosswords, CrosswordsSize};
use rio_vt::event::sync::FairMutex;
use rio_vt::event::{EventListener, Msg, RioEvent, WindowId};
use rio_vt::performer::handler::Processor;
use serde::{Deserialize, Serialize};

use crate::theme;

/// Events emitted by a terminal session.
#[derive(Debug, Clone)]
pub enum TerminalEvent {
    /// Output arrived from the shell; the view should scroll to the bottom.
    Output,
    /// The shell process exited.
    Exited(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalKind {
    Shell,
    Serial,
}

/// Configuration for a terminal tab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalConfig {
    pub kind: TerminalKind,
    /// Shell: the program to spawn. Serial: the device path.
    pub program: String,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            kind: TerminalKind::Shell,
            program: default_shell(),
        }
    }
}

fn default_shell() -> String {
    ns_project::default_terminal_shell()
}

/// A listener that forwards terminal damage and lifecycle events to the UI.
struct TermListener {
    tx: Sender<TerminalEvent>,
    route_id: usize,
}

impl EventListener for TermListener {
    fn send_event(&self, event: RioEvent, _window: WindowId) {
        match event {
            RioEvent::TerminalDamaged(route) | RioEvent::RenderRoute(route) => {
                if route == self.route_id {
                    let _ = self.tx.send(TerminalEvent::Output);
                }
            }
            RioEvent::ChildExited(route, code) if route == self.route_id => {
                let _ = self.tx.send(TerminalEvent::Exited(code.unwrap_or(1)));
            }
            _ => {}
        }
    }
}

/// Metadata shared with the reader threads, independent of the grid.
#[derive(Default)]
struct Meta {
    running: bool,
    exit_code: Option<i32>,
    title: String,
    error: Option<String>,
}

pub struct Terminal {
    focus_handle: FocusHandle,
    config: TerminalConfig,
    grid: Arc<FairMutex<Crosswords<TermListener>>>,
    meta: Arc<Mutex<Meta>>,
    /// PTY message channel (input + resize). `None` for serial terminals.
    machine_tx: Option<rio_vt::corcovado::channel::Sender<Msg>>,
    /// Serial reader thread handle.
    _serial_reader: Option<std::thread::JoinHandle<()>>,
    /// PTY reader thread handle (retains the machine + pty for the thread's
    /// lifetime; the tuple is the thread's return value).
    _machine: Option<
        std::thread::JoinHandle<(
            rio_vt::performer::Machine<
                rio_vt::teletypewriter::Pty,
                TermListener,
            >,
            rio_vt::performer::State,
        )>,
    >,
    closed: Arc<AtomicBool>,
    rx: Receiver<TerminalEvent>,
    _tx: Sender<TerminalEvent>,
    route_id: usize,
}

impl Terminal {
    pub fn new(config: TerminalConfig, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let (tx, rx) = channel::<TerminalEvent>();
        let route_id = 0usize;
        let listener = TermListener {
            tx: tx.clone(),
            route_id,
        };
        let size = CrosswordsSize::new(100, 30);
        let grid = Crosswords::new(
            size,
            CursorShape::Block,
            listener,
            WindowId::from(0),
            route_id,
            10_000,
        );
        let grid = Arc::new(FairMutex::new(grid));
        let meta = Arc::new(Mutex::new(Meta {
            running: true,
            title: "终端".to_string(),
            ..Default::default()
        }));

        let mut terminal = Self {
            focus_handle,
            config: config.clone(),
            grid,
            meta,
            machine_tx: None,
            _serial_reader: None,
            _machine: None,
            closed: Arc::new(AtomicBool::new(false)),
            rx,
            _tx: tx,
            route_id,
        };
        terminal.spawn(cx);
        terminal
    }

    fn spawn(&mut self, cx: &mut Context<Self>) {
        if self.config.kind == TerminalKind::Serial {
            self.spawn_serial(cx);
        } else {
            self.spawn_shell(cx);
        }
    }

    fn spawn_shell(&mut self, cx: &mut Context<Self>) {
        let program = self.config.program.clone();
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let pty = match rio_vt::teletypewriter::create_pty_with_spawn(
            Some(&program),
            Vec::new(),
            &Some(cwd),
            None,
            100,
            30,
            0,
            0,
        ) {
            Ok(pty) => pty,
            Err(error) => {
                self.fail(format!("启动 shell「{program}」失败: {error}"), cx);
                return;
            }
        };

        let grid = self.grid.clone();
        let tx = self._tx.clone();
        let route_id = self.route_id;
        let listener = TermListener { tx, route_id };
        let machine = match rio_vt::performer::Machine::new(
            grid,
            pty,
            listener,
            WindowId::from(0),
            route_id,
        ) {
            Ok(machine) => machine,
            Err(error) => {
                self.fail(format!("初始化 PTY 失败: {error}"), cx);
                return;
            }
        };
        self.machine_tx = Some(machine.channel());
        self._machine = Some(machine.spawn());
    }

    fn spawn_serial(&mut self, cx: &mut Context<Self>) {
        let program = self.config.program.clone();
        let mut port = match serialport::new(&program, 115_200).open() {
            Ok(port) => port,
            Err(error) => {
                self.fail(format!("打开串口 {program} 失败: {error}"), cx);
                return;
            }
        };
        let grid = self.grid.clone();
        let tx = self._tx.clone();
        let closed = self.closed.clone();
        let meta = self.meta.clone();
        self._serial_reader = Some(std::thread::spawn(move || {
            let mut parser = Processor::default();
            let mut buf = [0u8; 4096];
            loop {
                if closed.load(Ordering::SeqCst) {
                    break;
                }
                match port.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut term = grid.lock();
                        parser.advance(&mut *term, &buf[..n]);
                        drop(term);
                        let _ = tx.send(TerminalEvent::Output);
                    }
                }
            }
            let mut m = meta.lock().unwrap();
            m.running = false;
            let _ = tx.send(TerminalEvent::Exited(0));
        }));
        let mut m = self.meta.lock().unwrap();
        m.title = format!("串口 {program}");
    }

    fn fail(&mut self, message: String, cx: &mut Context<Self>) {
        let mut m = self.meta.lock().unwrap();
        m.running = false;
        m.error = Some(message.clone());
        drop(m);
        cx.notify();
    }

    pub fn send_text(&self, text: &str) {
        if let Some(tx) = &self.machine_tx {
            let _ = tx.send(Msg::Input(std::borrow::Cow::Owned(
                text.as_bytes().to_vec(),
            )));
        }
    }

    pub fn send_newline(&self) {
        self.send_text("\r");
    }

    pub fn backspace(&self) {
        self.send_text("\x7f");
    }

    pub fn clear(&self) {
        let mut state = self.grid.lock();
        state.clear_saved_history();
        state.reset_damage();
    }

    /// Handle a key-down event: forward the keystroke to the PTY and refresh.
    fn handle_keydown(
        &mut self,
        event: &gpui::KeyDownEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.handle_key(&event.keystroke);
        cx.notify();
    }

    /// Forward a keystroke to the PTY.
    pub fn handle_key(&self, keystroke: &gpui::Keystroke) {
        if keystroke.modifiers.control {
            if keystroke.key.eq_ignore_ascii_case("c") {
                self.send_text("\x03");
                return;
            }
            if keystroke.key.eq_ignore_ascii_case("l") {
                self.send_text("\x0c");
                return;
            }
            if keystroke.key.eq_ignore_ascii_case("d") {
                self.send_text("\x04");
                return;
            }
            return;
        }
        match keystroke.key.as_str() {
            "enter" => self.send_newline(),
            "backspace" => self.backspace(),
            _ => {
                if let Some(ch) = keystroke.key_char.as_ref() {
                    self.send_text(ch);
                }
            }
        }
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }



    pub fn title(&self) -> String {
        self.meta.lock().unwrap().title.clone()
    }

    pub fn error(&self) -> Option<String> {
        self.meta.lock().unwrap().error.clone()
    }

    pub fn size(&self) -> (u16, u16) {
        let state = self.grid.lock();
        (state.columns() as u16, state.screen_lines() as u16)
    }

    /// The working directory reported by the shell (OSC 7), if any.
    pub fn current_dir(&self) -> Option<String> {
        self.grid
            .lock()
            .current_directory
            .as_ref()
            .map(|p| p.display().to_string())
    }


    pub fn resize(&self, cols: u16, rows: u16) {
        if let Some(tx) = &self.machine_tx {
            let _ = tx.send(Msg::Resize(rio_vt::event::WindowSize {
                rows,
                cols,
                width: 0,
                height: 0,
            }));
        }
        let mut state = self.grid.lock();
        state.resize(CrosswordsSize::new(cols as usize, rows as usize));
    }

    /// Scroll the terminal's scrollback viewport by the given line delta.
    /// Positive values scroll up (into history); negative scroll down.
    pub fn scroll_lines(&self, delta: i32) {
        let mut state = self.grid.lock();
        state.scroll_display(rio_vt::crosswords::grid::Scroll::Delta(delta));
    }

    /// Drain pending events. Emits each as a gpui event so subscribers
    /// (the app) refresh immediately, instead of waiting for a poll tick.
    /// Returns true when new output arrived.
    pub fn drain_events(&self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        while let Ok(event) = self.rx.try_recv() {
            changed = true;
            match &event {
                TerminalEvent::Output => {}
                TerminalEvent::Exited(code) => {
                    let mut m = self.meta.lock().unwrap();
                    m.running = false;
                    m.exit_code = Some(*code);
                }
            }
            cx.emit(event);
        }
        changed
    }

    /// Test-only: inject an event into the session channel (as the reader
    /// thread would), so `drain_events` can be exercised without a real PTY.
    #[cfg(test)]
    pub fn inject_event_for_test(&self, event: TerminalEvent) {
        let _ = self._tx.send(event);
    }
}

impl EventEmitter<TerminalEvent> for Terminal {}

impl Render for Terminal {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> impl gpui::IntoElement {
        let (rows, styles, cursor, cols) = {
            let mut state = self.grid.lock();
            let mut dst = Vec::new();
            let mut row_styles = Vec::new();
            let mut extras = rustc_hash::FxHashMap::default();
            let damage = state.peek_damage_event().unwrap_or_default();
            state.snapshot_visible(&damage, &mut dst, &mut row_styles, &mut extras);
            state.reset_damage();
            let cursor = state.cursor();
            let cols = state.columns();
            (dst, row_styles, cursor, cols)
        };

        let line_h = px(17.);
        let mono = theme::MONO_FONT;
        let size = px(12.);
        let mut row_elements: Vec<gpui::AnyElement> = Vec::with_capacity(rows.len());
        for (y, row) in rows.iter().enumerate() {
            let mut text = String::with_capacity(cols);
            let mut highlights: Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> = Vec::new();
            let mut run_start = 0usize;
            let mut run_style: Option<gpui::HighlightStyle> = None;

            for x in 0..cols {
                let cell = &row[Column(x)];
                let ch = cell.c();
                let style = styles
                    .get(y)
                    .and_then(|s| s.get(x))
                    .cloned()
                    .unwrap_or_default();
                let is_cursor = cursor.pos.row.0 as usize == y && cursor.pos.col.0 == x;
                let fg = resolve_fg(&style);
                let bg = if is_cursor {
                    Some(theme::color(theme::ACCENT))
                } else {
                    resolve_bg(&style)
                };
                let fg = if style.flags.contains(StyleFlags::INVERSE) {
                    bg.unwrap_or_else(|| theme::color(theme::BG_0))
                } else {
                    fg
                };
                let highlight = gpui::HighlightStyle {
                    color: Some(fg),
                    background_color: bg,
                    ..Default::default()
                };
                if run_style != Some(highlight) {
                    if let Some(prev) = run_style.take() {
                        highlights.push((run_start..text.len(), prev));
                    }
                    run_start = text.len();
                    run_style = Some(highlight);
                }
                if ch == ' ' || cell.is_spacer() {
                    text.push(' ');
                } else {
                    text.push(ch);
                }
            }
            if let Some(prev) = run_style.take() {
                highlights.push((run_start..text.len(), prev));
            }

            let element = gpui::div()
                .h(line_h)
                .flex_none()
                .font_family(mono)
                .text_size(size)
                .line_height(gpui::DefiniteLength::Absolute(
                    gpui::AbsoluteLength::Pixels(line_h),
                ))
                .text_color(theme::color(theme::TEXT))
                .child(gpui::StyledText::new(text.clone()).with_highlights(highlights))
                .into_any_element();
            row_elements.push(element);
        }

        let error = self.error();
        let title = self.title();
        let cols = self.size().0;
        let rows = self.size().1;
        let cwd = self.current_dir();

        gpui::div()
            .id("terminal-root")
            .flex_1()
            .flex()
            .flex_col()
            .bg(theme::color(theme::BG_0))
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .cursor(gpui::CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::handle_keydown))
            .child(
                gpui::div()
                    .id("terminal-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(10.))
                    .py(px(8.))
                    .flex()
                    .flex_col()
                    .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, _, cx| {
                        let delta = match event.delta {
                            gpui::ScrollDelta::Pixels(p) => -(f32::from(p.y) as i32),
                            gpui::ScrollDelta::Lines(l) => -(l.y as i32),
                        };
                        this.scroll_lines(delta);
                        cx.notify();
                    }))
                    .children(row_elements),
            )
            .child(
                gpui::div()
                    .flex_none()
                    .px(px(10.))
                    .py(px(6.))
                    .border_t_1()
                    .border_color(theme::alpha(theme::STROKE, 0.12))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        gpui::div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(11.))
                            .text_color(theme::color(theme::TEXT_DIM))
                            .font_family(theme::MONO_FONT)
                            .child(SharedString::from(
                                cwd.clone().unwrap_or_else(|| title.clone()),
                            )),
                    )
                    .child(
                        gpui::div()
                            .id("term-clear")
                            .text_size(px(10.))
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .cursor_pointer()
                            .hover(|style| style.text_color(theme::color(theme::TEXT)))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.clear();
                                cx.notify();
                            }))
                            .child("清屏"),
                    )
                    .child(
                        gpui::div()
                            .text_size(px(10.))
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .child(SharedString::from(format!(
                                "{} · {}×{}",
                                title, cols, rows
                            ))),
                    )
                    .when(error.is_some(), |style| {
                        style.child(
                            gpui::div()
                                .text_size(px(10.))
                                .text_color(theme::color(theme::DANGER))
                                .child(SharedString::from(error.unwrap_or_default())),
                        )
                    }),
            )
    }
}

/// Map a style's foreground color to a gpui color.
fn resolve_fg(style: &Style) -> gpui::Hsla {
    match style.fg {
        AnsiColor::Named(NamedColor::Foreground) => theme::color(theme::TEXT),
        AnsiColor::Named(NamedColor::Background) => theme::color(theme::BG_0),
        AnsiColor::Named(named) => named_color(named),
        AnsiColor::Indexed(index) => indexed_color(index),
        AnsiColor::Spec(rgb) => gpui::rgb(rgb_to_hex(rgb.r, rgb.g, rgb.b)).into(),
    }
}

/// Map a style's background color to a gpui color (or `None` for default).
fn resolve_bg(style: &Style) -> Option<gpui::Hsla> {
    match style.bg {
        AnsiColor::Named(NamedColor::Background) => None,
        AnsiColor::Named(NamedColor::Foreground) => Some(theme::color(theme::TEXT)),
        AnsiColor::Named(named) => Some(named_color(named)),
        AnsiColor::Indexed(index) => Some(indexed_color(index)),
        AnsiColor::Spec(rgb) => Some(gpui::rgb(rgb_to_hex(rgb.r, rgb.g, rgb.b)).into()),
    }
}

fn named_color(named: NamedColor) -> gpui::Hsla {
    let hex = match named {
        NamedColor::Black => 0x1d1c1a,
        NamedColor::Red => 0xb0564a,
        NamedColor::Green => 0x6f9b8a,
        NamedColor::Yellow => 0xc9a25f,
        NamedColor::Blue => 0x6b8bb0,
        NamedColor::Magenta => 0xa57a3d,
        NamedColor::Cyan => 0x5f9c9c,
        NamedColor::White => 0xf0e6d2,
        NamedColor::LightBlack => 0x8a7c63,
        NamedColor::LightRed => 0xcf6b5e,
        NamedColor::LightGreen => 0x8fb5a4,
        NamedColor::LightYellow => 0xd8b97f,
        NamedColor::LightBlue => 0x8ba8c8,
        NamedColor::LightMagenta => 0xc9a25f,
        NamedColor::LightCyan => 0x7fb5b5,
        NamedColor::LightWhite => 0xf7efe0,
        _ => theme::TEXT,
    };
    theme::color(hex)
}

fn indexed_color(index: u8) -> gpui::Hsla {
    if (16..232).contains(&index) {
        let i = index - 16;
        let r = i / 36;
        let g = (i % 36) / 6;
        let b = i % 6;
        let val = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
        gpui::rgb(rgb_to_hex(val(r), val(g), val(b))).into()
    } else if index >= 232 {
        let v = 8 + (index - 232) * 10;
        gpui::rgb(rgb_to_hex(v, v, v)).into()
    } else if index < 8 {
        named_color([
            NamedColor::Black,
            NamedColor::Red,
            NamedColor::Green,
            NamedColor::Yellow,
            NamedColor::Blue,
            NamedColor::Magenta,
            NamedColor::Cyan,
            NamedColor::White,
        ][index as usize])
    } else {
        named_color([
            NamedColor::LightBlack,
            NamedColor::LightRed,
            NamedColor::LightGreen,
            NamedColor::LightYellow,
            NamedColor::LightBlue,
            NamedColor::LightMagenta,
            NamedColor::LightCyan,
            NamedColor::LightWhite,
        ][(index - 8) as usize])
    }
}

fn rgb_to_hex(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rio_vt::ansi::CursorShape;
    use rio_vt::crosswords::pos::Column;
    use rio_vt::crosswords::{Crosswords, CrosswordsSize};
    use rio_vt::event::{VoidListener, WindowId};
    use rio_vt::performer::handler::Processor;

    /// End-to-end proof that the rio-vt parser handles a shell's SGR output
    /// and stores it in the grid (mirrors `Terminal::spawn_shell`'s reader).
    #[test]
    fn parser_fills_grid_with_ansi_output() {
        let size = CrosswordsSize::new(80, 24);
        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            10_000,
        );
        let mut parser = Processor::default();
        parser.advance(&mut term, b"\x1b[31mhello\x1b[0m world\r\n");
        let rows = term.visible_rows();
        let cols = term.columns();
        let line: String = (0..cols)
            .map(|x| rows[0][Column(x)].c())
            .take_while(|c| *c != ' ')
            .collect();
        assert_eq!(line, "hello");
    }

    /// Verify SGR color survives parsing into the cell style table.
    #[test]
    fn parser_resolves_style() {
        let size = CrosswordsSize::new(80, 24);
        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            10_000,
        );
        let mut parser = Processor::default();
        parser.advance(&mut term, b"\x1b[31mred");
        let mut dst = Vec::new();
        let mut row_styles = Vec::new();
        let mut extras = rustc_hash::FxHashMap::default();
        let damage = term.peek_damage_event().unwrap_or_default();
        term.snapshot_visible(&damage, &mut dst, &mut row_styles, &mut extras);
        let style = &row_styles[0][0];
        assert_eq!(
            style.fg,
            AnsiColor::Named(NamedColor::Red),
            "expected red foreground, got {:?}",
            style.fg
        );
    }

    /// End-to-end proof that the `Machine` PTY reader spawns a real shell
    /// and streams its output into the grid (mirrors `Terminal::spawn_shell`).
    #[test]
    fn machine_spawns_shell_and_fills_grid() {
        if cfg!(windows) {
            return; // POSIX shell assumptions; Unix CI covers this.
        }
        use rio_vt::performer::Machine;

        let (tx, rx) = channel::<TerminalEvent>();
        let route_id = 0usize;
        let listener = TermListener { tx, route_id };
        let size = CrosswordsSize::new(80, 24);
        let grid = Crosswords::new(
            size,
            CursorShape::Block,
            listener,
            WindowId::from(0),
            route_id,
            10_000,
        );
        let grid = Arc::new(FairMutex::new(grid));

        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let pty = rio_vt::teletypewriter::create_pty_with_spawn(
            Some("sh"),
            vec!["-c".to_string(), "echo hello-rio-vt; exit 0".to_string()],
            &Some(cwd),
            None,
            80,
            24,
            0,
            0,
        )
        .expect("spawn pty");

        let (tx2, _rx2) = channel::<TerminalEvent>();
        let listener2 = TermListener {
            tx: tx2,
            route_id,
        };
        let machine = Machine::new(grid.clone(), pty, listener2, WindowId::from(0), route_id)
            .expect("machine");
        let handle = machine.spawn();

        // Wait for the shell output to arrive in the grid.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut found = false;
        while std::time::Instant::now() < deadline {
            while let Ok(_ev) = rx.try_recv() {}
            let g = grid.lock();
            let rows = g.visible_rows();
            let cols = g.columns();
            let text: String = rows
                .iter()
                .flat_map(|row| (0..cols).map(|x| row[Column(x)].c()))
                .collect();
            if text.contains("hello-rio-vt") {
                found = true;
                break;
            }
            drop(g);
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = handle.join();
        assert!(found, "expected shell output in grid");
    }
    /// Reproduction: an interactive shell must NOT echo/run input before
    /// the newline. Sends a partial command (no `\n`), waits, and asserts
    /// the command's output did not appear yet.
    #[test]
    fn input_is_buffered_until_newline() {
        if cfg!(windows) {
            return;
        }
        use rio_vt::performer::Machine;

        let (tx, _rx) = channel::<TerminalEvent>();
        let route_id = 0usize;
        let grid = Arc::new(FairMutex::new(Crosswords::new(
            CrosswordsSize::new(80, 24),
            CursorShape::Block,
            TermListener { tx, route_id },
            WindowId::from(0),
            route_id,
            10_000,
        )));

        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let pty = rio_vt::teletypewriter::create_pty_with_spawn(
            Some("bash"),
            vec!["-i".to_string(), "--noprofile".to_string()],
            &Some(cwd),
            None,
            80,
            24,
            0,
            0,
        )
        .expect("spawn pty");

        let (tx2, _rx2) = channel::<TerminalEvent>();
        let machine = Machine::new(
            grid.clone(),
            pty,
            TermListener { tx: tx2, route_id },
            WindowId::from(0),
            route_id,
        )
        .expect("machine");
        let channel = machine.channel();
        let _handle = machine.spawn();

        // Give the shell a moment to print its prompt.
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Send a partial command: NO newline yet.
        let _ = channel.send(Msg::Input(std::borrow::Cow::Owned(
            b"echo OUTPUT_BEFORE_NL".to_vec(),
        )));

        // Wait a generous window: if output appears without Enter, it's a bug.
        std::thread::sleep(std::time::Duration::from_millis(600));
        let g = grid.lock();
        let rows = g.visible_rows();
        let cols = g.columns();
        let text: String = rows
            .iter()
            .flat_map(|row| (0..cols).map(|x| row[Column(x)].c()))
            .collect();
        drop(g);

        // The typed text may be echoed, but the command's OUTPUT (a bare
        // `OUTPUT_BEFORE_NL` on its own line) must not run before newline.
        assert!(
            !text.lines().any(|l| l.trim() == "OUTPUT_BEFORE_NL"),
            "command executed before newline:\n{text}"
        );
    }
}

