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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// Fixed row height used for every terminal cell, in logical pixels.
/// The grid is fitted to the viewport using this value.
const TERM_LINE_H: f32 = 17.0;
/// Fixed font size used for terminal cells.
const TERM_FONT_SIZE: f32 = 12.0;
/// Horizontal padding of the viewport (px(10.) on each side).
const TERM_PAD_X: f32 = 20.0;
/// Vertical padding of the viewport (px(8.) on each side).
const TERM_PAD_Y: f32 = 16.0;

/// Configuration for a terminal tab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalConfig {
    pub kind: TerminalKind,
    /// Shell: the program to spawn. Serial: the device path.
    pub program: String,
    /// Working directory for the spawned shell. `None` = the IDE's current
    /// directory. Used by "open terminal in directory" tree actions.
    pub cwd: Option<String>,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            kind: TerminalKind::Shell,
            program: default_shell(),
            cwd: None,
        }
    }
}

fn default_shell() -> String {
    ns_project::default_terminal_shell()
}

/// Compute the grid size that fits a viewport of the given content size.
/// Returns `None` when the viewport is too small to be useful. The result is
/// clamped so a collapsed panel never shrinks the shell to something unusable.
fn fit_cells(width: f32, height: f32, ch_width: f32) -> Option<(u16, u16)> {
    if width < 40.0 || height < TERM_LINE_H || ch_width <= 0.0 {
        return None;
    }
    let cols = (width / ch_width).floor().clamp(20.0, 500.0) as u16;
    let rows = (height / TERM_LINE_H).floor().clamp(4.0, 200.0) as u16;
    Some((cols, rows))
}

/// Decide whether the viewport should keep following new output after a user
/// scroll. Scrolling up (`delta > 0`) into history stops following; scrolling
/// back down to the newest line (`display_offset == 0`) re-arms it. Scrolling
/// down while history remains leaves the current state untouched.
fn follow_after_scroll(follow: bool, delta: i32, at_bottom: bool) -> bool {
    if delta > 0 {
        false
    } else if at_bottom {
        true
    } else {
        follow
    }
}

/// Convert a pixel wheel delta (positive = scroll up, as reported by the
/// platform) into whole terminal lines, carrying the sub-line remainder over to
/// the next event so slow trackpad scrolling still moves the viewport.
fn wheel_delta_lines(remainder: &mut f32, delta_y: f32) -> i32 {
    *remainder += delta_y;
    let lines = (*remainder / TERM_LINE_H).trunc();
    *remainder -= lines * TERM_LINE_H;
    lines as i32
}

/// Shell family, used to pick how the IDE prompt is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellFamily {
    Bash,
    Zsh,
    Sh,
    Other,
}

impl ShellFamily {
    fn from_program(program: &str) -> Self {
        let lower = Path::new(program)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(program)
            .to_ascii_lowercase();
        let base = lower.strip_suffix(".exe").unwrap_or(&lower);
        match base {
            "bash" => ShellFamily::Bash,
            "zsh" => ShellFamily::Zsh,
            "sh" | "dash" | "ash" | "ksh" => ShellFamily::Sh,
            _ => ShellFamily::Other,
        }
    }
}

/// Shell function that prints the IDE prompt: the final component of the
/// working directory plus `#`, or plus `(git:<branch>)#` inside a git
/// work tree. Kept POSIX-compatible so bash/zsh/sh can all use it.
const PROMPT_FUNCTION: &str = r#"__ns_prompt() {
  local d="${PWD##*/}"
  [ -z "$d" ] && d="/"
  local b
  b=$(git -C "$PWD" branch --show-current 2>/dev/null)
  if [ -n "$b" ]; then
    printf '%s(git:%s)# ' "$d" "$b"
  else
    printf '%s# ' "$d"
  fi
}
"#;

/// Everything needed to launch a shell with the IDE prompt installed:
/// extra argv, extra environment, and the temporary init directory (removed
/// when the terminal is dropped).
struct ShellLaunch {
    args: Vec<String>,
    env: Option<Vec<(String, String)>>,
    init_dir: Option<PathBuf>,
}

fn shell_init_dir() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("ns-ide-shell-{}-{seq}-{nanos}", std::process::id()))
}

/// Build the init script that installs the IDE prompt. For bash this is passed
/// via `--rcfile`; for zsh the directory is handed over as `ZDOTDIR`; for
/// POSIX sh it is passed as `$ENV`. Unknown shells (fish, PowerShell, cmd) are
/// left untouched and keep their own prompt.
fn prepare_shell_launch(program: &str) -> ShellLaunch {
    let family = ShellFamily::from_program(program);
    if cfg!(windows) || family == ShellFamily::Other {
        return ShellLaunch {
            args: Vec::new(),
            env: None,
            init_dir: None,
        };
    }

    let dir = shell_init_dir();
    let rc_name = match family {
        ShellFamily::Bash => ".bashrc",
        ShellFamily::Zsh => ".zshrc",
        _ => ".profile",
    };
    let rc = dir.join(rc_name);

    let mut script =
        String::from("# Generated by ns-embed-ide for the embedded terminal prompt.\n");
    match family {
        ShellFamily::Bash => {
            script.push_str(
                "if [ -f \"$HOME/.bashrc\" ]; then\n  . \"$HOME/.bashrc\"\nelif [ -f \"$HOME/.bash_profile\" ]; then\n  . \"$HOME/.bash_profile\"\nfi\n",
            );
            script.push_str(PROMPT_FUNCTION);
            script.push_str("PS1='$(__ns_prompt)'\n");
        }
        ShellFamily::Zsh => {
            // Restore the caller's ZDOTDIR, source its .zshrc, then install
            // our prompt so distro/oh-my-zsh prompts do not override it.
            script.push_str(
                "if [ -n \"$__NS_ORIG_ZDOTDIR\" ]; then\n  ZDOTDIR=\"$__NS_ORIG_ZDOTDIR\"\nelse\n  ZDOTDIR=\"$HOME\"\nfi\nif [ -f \"$ZDOTDIR/.zshrc\" ]; then\n  . \"$ZDOTDIR/.zshrc\"\nfi\nsetopt PROMPT_SUBST 2>/dev/null\n",
            );
            script.push_str(PROMPT_FUNCTION);
            script.push_str("PROMPT='$(__ns_prompt)'\n");
        }
        _ => {
            script.push_str("if [ -f \"$HOME/.profile\" ]; then\n  . \"$HOME/.profile\"\nfi\n");
            script.push_str(PROMPT_FUNCTION);
            script.push_str("PS1='$(__ns_prompt)'\n");
        }
    }

    if std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(&rc, script))
        .is_err()
    {
        return ShellLaunch {
            args: Vec::new(),
            env: None,
            init_dir: None,
        };
    }

    match family {
        ShellFamily::Bash => ShellLaunch {
            args: vec![
                "--rcfile".to_string(),
                rc.display().to_string(),
                "-i".to_string(),
            ],
            env: None,
            init_dir: Some(dir),
        },
        ShellFamily::Zsh => {
            let original = std::env::var("ZDOTDIR")
                .ok()
                .filter(|value| !value.is_empty())
                .or_else(|| std::env::var("HOME").ok())
                .unwrap_or_default();
            ShellLaunch {
                args: vec!["-i".to_string()],
                env: Some(vec![
                    ("ZDOTDIR".to_string(), dir.display().to_string()),
                    ("__NS_ORIG_ZDOTDIR".to_string(), original),
                ]),
                init_dir: Some(dir),
            }
        }
        _ => ShellLaunch {
            args: vec!["-i".to_string()],
            env: Some(vec![("ENV".to_string(), rc.display().to_string())]),
            init_dir: Some(dir),
        },
    }
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
    /// Whether new output should keep the viewport pinned to the bottom.
    /// The user scrolling up clears it; scrolling back to the bottom re-arms it.
    follow_output: bool,
    /// Pixel wheel delta not yet applied as whole lines, so slow trackpad
    /// scrolling accumulates instead of being truncated away.
    scroll_remainder: f32,
    /// Handle for the scrollable terminal viewport, so new output can be
    /// scrolled into view without waiting for a manual wheel/keys.
    scroll_handle: gpui::ScrollHandle,
    /// Temporary directory holding the generated shell init file. Removed when
    /// the terminal is dropped.
    shell_init_dir: Option<PathBuf>,
    /// PTY message channel (input + resize). `None` for serial terminals.
    machine_tx: Option<rio_vt::corcovado::channel::Sender<Msg>>,
    /// Serial reader thread handle.
    _serial_reader: Option<std::thread::JoinHandle<()>>,
    /// PTY reader thread handle (retains the machine + pty for the thread's
    /// lifetime; the tuple is the thread's return value).
    _machine: Option<
        std::thread::JoinHandle<(
            rio_vt::performer::Machine<rio_vt::teletypewriter::Pty, TermListener>,
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
            follow_output: false,
            scroll_remainder: 0.0,
            scroll_handle: gpui::ScrollHandle::new(),
            shell_init_dir: None,
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
        let cwd = self
            .config
            .cwd
            .clone()
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.display().to_string())
            })
            .unwrap_or_default();
        let launch = prepare_shell_launch(&program);
        self.shell_init_dir = launch.init_dir.clone();
        let pty = match rio_vt::teletypewriter::create_pty_with_spawn(
            Some(&program),
            launch.args,
            &Some(cwd),
            launch.env,
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
        let machine =
            match rio_vt::performer::Machine::new(grid, pty, listener, WindowId::from(0), route_id)
            {
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

    /// Apply a user scroll: move the scrollback viewport and update whether new
    /// output should keep pulling the view to the bottom.
    pub fn user_scroll(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        self.scroll_lines(delta);
        let at_bottom = self.grid.lock().display_offset() == 0;
        self.follow_output = follow_after_scroll(self.follow_output, delta, at_bottom);
    }

    /// Convert a pixel wheel delta into scrollback lines. Positive values
    /// scroll up into history, matching the platform's wheel convention.
    fn wheel_pixels(&mut self, delta_y: f32) -> i32 {
        wheel_delta_lines(&mut self.scroll_remainder, delta_y)
    }

    /// Resume or pause following new output, pinning the view to the newest
    /// line when resumed.
    pub fn set_follow_output(&mut self, follow: bool) {
        self.follow_output = follow;
        if follow {
            self.scroll_handle.scroll_to_bottom();
            self.grid
                .lock()
                .scroll_display(rio_vt::crosswords::grid::Scroll::Bottom);
        }
    }

    /// Fit the terminal grid to the rendered viewport so the shell always has
    /// exactly as many rows/columns as the panel can show. Without this the
    /// grid stays at its spawn size (100×30): the prompt and cursor then sit on
    /// a row below the visible panel and only appear after an extra Enter.
    fn fit_to_viewport(
        &mut self,
        bounds: gpui::Bounds<gpui::Pixels>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let width = f32::from(bounds.size.width) - TERM_PAD_X;
        let height = f32::from(bounds.size.height) - TERM_PAD_Y;
        let font_id = window
            .text_system()
            .resolve_font(&gpui::font(theme::MONO_FONT));
        // Use the advance width (not the glyph ink bounds): the grid must fit
        // the horizontal distance each character actually occupies.
        let ch_width = window
            .text_system()
            .ch_advance(font_id, px(TERM_FONT_SIZE))
            .map(f32::from)
            .unwrap_or(7.2);
        let Some((cols, rows)) = fit_cells(width, height, ch_width) else {
            return;
        };
        let (current_cols, current_rows) = self.size();
        if cols == current_cols && rows == current_rows {
            return;
        }
        self.resize(cols, rows);
        self.scroll_handle.scroll_to_bottom();
        cx.notify();
    }

    /// Drain pending events. Emits each as a gpui event so subscribers
    /// (the app) refresh immediately, instead of waiting for a poll tick.
    /// Returns true when new output arrived.
    pub fn drain_events(&self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        while let Ok(event) = self.rx.try_recv() {
            changed = true;
            match &event {
                TerminalEvent::Output => {
                    // Keep the newest output (including the freshly printed
                    // prompt) visible, unless the user scrolled away.
                    if self.follow_output {
                        self.scroll_handle.scroll_to_bottom();
                    }
                }
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

        let line_h = px(TERM_LINE_H);
        let mono = theme::MONO_FONT;
        let size = px(TERM_FONT_SIZE);
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
        let entity = cx.entity();

        gpui::div()
            // Resize the grid to the panel after layout, so the shell's prompt
            // and cursor always land on a visible row.
            .on_children_prepainted(move |bounds, window, app| {
                let Some(bounds) = bounds.first().cloned() else {
                    return;
                };
                entity.update(app, |this, cx| this.fit_to_viewport(bounds, window, cx));
            })
            .id("terminal-root")
            .flex_1()
            // Allow the panel to shrink the terminal below its content height;
            // otherwise the grid's own row height wins and the prompt is pushed
            // out of view instead of the grid being re-fitted.
            .min_h(px(0.))
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
                    .track_scroll(&self.scroll_handle)
                    .px(px(10.))
                    .py(px(8.))
                    .flex()
                    .flex_col()
                    .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, _, cx| {
                        // Positive delta = scroll up into history, matching the
                        // platform's wheel/trackpad direction (same convention as
                        // the editor's scroll handling).
                        let delta = match event.delta {
                            gpui::ScrollDelta::Pixels(p) => this.wheel_pixels(f32::from(p.y)),
                            gpui::ScrollDelta::Lines(l) => l.y.round() as i32,
                        };
                        this.user_scroll(delta);
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
                            .id("term-follow")
                            .text_size(px(10.))
                            .text_color(if self.follow_output {
                                theme::color(theme::JADE)
                            } else {
                                theme::color(theme::TEXT_FAINT)
                            })
                            .cursor_pointer()
                            .hover(|style| style.text_color(theme::color(theme::TEXT)))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                let next = !this.follow_output;
                                this.set_follow_output(next);
                                cx.notify();
                            }))
                            .child(if self.follow_output {
                                "跟随输出"
                            } else {
                                "已暂停跟随"
                            }),
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
                            .child(SharedString::from(format!("{} · {}×{}", title, cols, rows))),
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
        named_color(
            [
                NamedColor::Black,
                NamedColor::Red,
                NamedColor::Green,
                NamedColor::Yellow,
                NamedColor::Blue,
                NamedColor::Magenta,
                NamedColor::Cyan,
                NamedColor::White,
            ][index as usize],
        )
    } else {
        named_color(
            [
                NamedColor::LightBlack,
                NamedColor::LightRed,
                NamedColor::LightGreen,
                NamedColor::LightYellow,
                NamedColor::LightBlue,
                NamedColor::LightMagenta,
                NamedColor::LightCyan,
                NamedColor::LightWhite,
            ][(index - 8) as usize],
        )
    }
}

fn rgb_to_hex(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Some(dir) = &self.shell_init_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
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

    /// Placeholder window root used by render tests so the window frame does not
    /// interfere with elements drawn explicitly at a fixed size.
    struct EmptyView;

    impl gpui::Render for EmptyView {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div()
        }
    }

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
        let listener2 = TermListener { tx: tx2, route_id };
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

    #[test]
    fn terminal_config_round_trips_cwd() {
        // Default has no cwd; a directory-based config preserves it.
        let default = TerminalConfig::default();
        assert!(default.cwd.is_none());
        let with_cwd = TerminalConfig {
            kind: TerminalKind::Shell,
            program: "bash".to_string(),
            cwd: Some("/tmp/project".to_string()),
        };
        let json = serde_json::to_string(&with_cwd).unwrap();
        let back: TerminalConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cwd.as_deref(), Some("/tmp/project"));
        assert_eq!(back.kind, TerminalKind::Shell);
    }

    #[test]
    fn shell_family_detects_supported_shells() {
        assert_eq!(ShellFamily::from_program("/bin/bash"), ShellFamily::Bash);
        assert_eq!(ShellFamily::from_program("bash.exe"), ShellFamily::Bash);
        assert_eq!(ShellFamily::from_program("/bin/zsh"), ShellFamily::Zsh);
        assert_eq!(ShellFamily::from_program("/bin/dash"), ShellFamily::Sh);
        assert_eq!(
            ShellFamily::from_program("/usr/bin/fish"),
            ShellFamily::Other
        );
        assert_eq!(ShellFamily::from_program("pwsh"), ShellFamily::Other);
    }

    /// The grid must fit the panel it is drawn into. A fixed 30-row grid in a
    /// shorter panel pushes the prompt and cursor below the visible area, so
    /// the terminal only looks like it works after an extra Enter.
    #[test]
    fn fit_cells_matches_viewport() {
        // 800×340 of content at 7.2px per cell → 111 columns, 20 rows.
        assert_eq!(fit_cells(800.0, 340.0, 7.2), Some((111, 20)));
        // Tiny panels clamp to a still-usable shell size.
        assert_eq!(fit_cells(100.0, 34.0, 7.2), Some((20, 4)));
        // Absurdly large panels are capped.
        assert_eq!(fit_cells(10_000.0, 10_000.0, 7.2), Some((500, 200)));
        // Degenerate inputs leave the grid untouched.
        assert_eq!(fit_cells(39.0, 340.0, 7.2), None);
        assert_eq!(fit_cells(800.0, 10.0, 7.2), None);
        assert_eq!(fit_cells(800.0, 340.0, 0.0), None);
    }

    /// Scrolling up into history pauses auto-follow; returning to the newest
    /// line re-arms it, and scrolling down mid-history changes nothing.
    #[test]
    fn follow_after_scroll_tracks_bottom_state() {
        assert!(!follow_after_scroll(true, 1, true), "scrolling up pauses");
        assert!(!follow_after_scroll(false, 1, false), "stays paused");
        assert!(
            !follow_after_scroll(false, -1, false),
            "scrolling down mid-history stays paused"
        );
        assert!(
            follow_after_scroll(false, -1, true),
            "back at bottom resumes"
        );
        assert!(follow_after_scroll(true, -1, true), "stays following");
    }

    /// Wheel pixels accumulate across events, and the sign matches the
    /// platform: positive (scroll up) goes into history instead of being
    /// inverted.
    #[test]
    fn wheel_pixels_accumulate_and_keep_direction() {
        let mut remainder = 0.0;
        // Small trackpad steps do not reach a full 17px row yet.
        assert_eq!(wheel_delta_lines(&mut remainder, 8.0), 0);
        assert_eq!(wheel_delta_lines(&mut remainder, 8.0), 0);
        // 24px accumulated → one row up, 7px carried over.
        assert_eq!(wheel_delta_lines(&mut remainder, 8.0), 1);
        assert!(
            (remainder - 7.0).abs() < 0.01,
            "remainder carried: {remainder}"
        );
        // Downward motion is negative (toward the newest line).
        assert_eq!(wheel_delta_lines(&mut remainder, -34.0), -1);
    }

    /// `user_scroll` wires the decision into the terminal state: scroll up,
    /// then back down to the newest line.
    #[test]
    fn user_scroll_pauses_and_resumes_follow() {
        if cfg!(windows) {
            return;
        }
        let mut cx = gpui::TestAppContext::single();
        let terminal = cx.new(|cx| {
            Terminal::new(
                TerminalConfig {
                    kind: TerminalKind::Shell,
                    program: "sh".to_string(),
                    cwd: None,
                },
                cx,
            )
        });
        cx.update(|cx| {
            terminal.update(cx, |terminal, _cx| {
                terminal.set_follow_output(true);
                assert!(terminal.follow_output);
                terminal.user_scroll(3);
                assert!(!terminal.follow_output, "scrolling up pauses follow");
                terminal.user_scroll(-3);
                assert!(terminal.follow_output, "back at bottom resumes follow");
            });
        });
    }

    /// Drawing the terminal into a 640×300 panel must resize the grid to that
    /// panel. Before, the grid kept its 100×30 spawn size and the prompt row
    /// ended up below the visible area.
    #[test]
    fn terminal_grid_fits_drawn_panel() {
        if cfg!(windows) {
            return;
        }
        let mut cx = gpui::TestAppContext::single();
        // The window root is an empty view: otherwise the window frame draw
        // would re-fit the terminal to the whole window and hide the result of
        // the explicit 640×300 draw below.
        let (_root, cx) = cx.add_window_view(|_window, _cx| EmptyView);
        let terminal = cx.new(|cx| {
            Terminal::new(
                TerminalConfig {
                    kind: TerminalKind::Shell,
                    program: "sh".to_string(),
                    cwd: None,
                },
                cx,
            )
        });
        cx.draw(
            gpui::point(gpui::px(0.), gpui::px(0.)),
            gpui::size(gpui::px(640.), gpui::px(300.)),
            |_window, _cx| {
                // A definite-height panel constrains the terminal the same way
                // the app's output panel does.
                gpui::div()
                    .flex()
                    .flex_col()
                    .w(gpui::px(640.))
                    .h(gpui::px(300.))
                    .child(terminal.clone())
            },
        );
        let (cols, rows) = cx.update(|_window, cx| terminal.read(cx).size());
        // The panel is 300px tall: at 17px per row the grid must hold roughly
        // 15 rows (minus the footer), never the 30 rows it was spawned with.
        assert!(
            (8..=17).contains(&rows),
            "grid rows must fit the 300px panel, got {rows}"
        );
        assert!(cols >= 40, "grid columns must stay usable, got {cols}");
        assert_ne!((cols, rows), (100, 30), "grid no longer keeps spawn size");
    }

    /// The generated bash init file must install our prompt after sourcing the
    /// user's rc, so the `dir#` / `dir(git:branch)#` format wins.
    #[test]
    fn prepare_shell_launch_installs_bash_prompt() {
        if cfg!(windows) {
            return;
        }
        let launch = prepare_shell_launch("/bin/bash");
        let dir = launch.init_dir.clone().expect("init dir");
        assert_eq!(
            launch.args,
            vec![
                "--rcfile".to_string(),
                dir.join(".bashrc").display().to_string(),
                "-i".to_string(),
            ]
        );
        let script = std::fs::read_to_string(dir.join(".bashrc")).expect("rc file");
        assert!(script.contains("$HOME/.bashrc"));
        assert!(script.contains("PS1='$(__ns_prompt)'"));
        assert!(script.contains("(git:%s)#"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// End-to-end: a real bash session prints `zr#` outside a repo and
    /// `repo(git:main)#` after `cd` into a git work tree.
    #[test]
    fn shell_prompt_shows_dir_and_git_suffix() {
        if cfg!(windows) {
            return;
        }
        let bash = if Path::new("/bin/bash").exists() {
            "/bin/bash"
        } else {
            "bash"
        };
        if !Path::new("/usr/bin/git").exists()
            && ns_project::toolchain::find_command("git").is_none()
        {
            return;
        }

        use rio_vt::performer::Machine;

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let base =
            std::env::temp_dir().join(format!("ns-ide-prompt-test-{}-{nanos}", std::process::id()));
        let plain = base.join("zr");
        let repo = base.join("repo");
        std::fs::create_dir_all(&plain).expect("plain dir");
        std::fs::create_dir_all(&repo).expect("repo dir");
        let git = ns_project::toolchain::find_command("git")
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "git".to_string());
        let _ = std::process::Command::new(git)
            .arg("-C")
            .arg(&repo)
            .args(["init", "-b", "main"])
            .output();

        let launch = prepare_shell_launch(bash);
        let init_dir = launch.init_dir.clone();
        let pty = rio_vt::teletypewriter::create_pty_with_spawn(
            Some(bash),
            launch.args,
            &Some(plain.display().to_string()),
            launch.env,
            100,
            30,
            0,
            0,
        )
        .expect("spawn bash");

        let (tx, _rx) = channel::<TerminalEvent>();
        let route_id = 0usize;
        let grid = Arc::new(FairMutex::new(Crosswords::new(
            CrosswordsSize::new(100, 30),
            CursorShape::Block,
            TermListener { tx, route_id },
            WindowId::from(0),
            route_id,
            10_000,
        )));
        let machine = Machine::new(
            grid.clone(),
            pty,
            TermListener {
                tx: channel::<TerminalEvent>().0,
                route_id,
            },
            WindowId::from(0),
            route_id,
        )
        .expect("machine");
        let input = machine.channel();
        let _handle = machine.spawn();

        let grid_text = |grid: &Arc<FairMutex<Crosswords<TermListener>>>| -> String {
            let state = grid.lock();
            let rows = state.visible_rows();
            let cols = state.columns();
            rows.iter()
                .flat_map(|row| (0..cols).map(|x| row[Column(x)].c()))
                .collect()
        };
        let wait_for = |needle: &str| -> bool {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if grid_text(&grid).contains(needle) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            false
        };

        assert!(
            wait_for("zr# "),
            "expected `zr# ` prompt, got:\n{}",
            grid_text(&grid)
        );

        let _ = input.send(Msg::Input(std::borrow::Cow::Owned(
            format!("cd \"{}\"\n", repo.display()).into_bytes(),
        )));
        assert!(
            wait_for("repo(git:main)# "),
            "expected git prompt, got:\n{}",
            grid_text(&grid)
        );

        let _ = input.send(Msg::Input(std::borrow::Cow::Owned(b"exit\n".to_vec())));
        std::thread::sleep(std::time::Duration::from_millis(200));
        std::fs::remove_dir_all(&base).ok();
        if let Some(dir) = init_dir {
            std::fs::remove_dir_all(dir).ok();
        }
    }
}
