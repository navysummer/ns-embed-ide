//! Embedded system shell terminal backed by a pseudo-terminal (PTY).
//!
//! The terminal spawns the user's login shell in a PTY, streams its output
//! into a scrollback buffer on a worker thread, and forwards keystrokes to the
//! PTY master. Rendering is line-based: the scrollback is shown as text lines
//! with a live input line. This is a functional terminal (output + input +
//! resize), not a full VT100 emulator — escape sequences are stripped.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use gpui::prelude::*;
use gpui::{Context, EventEmitter, FocusHandle, Render, SharedString, px};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
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

/// Shared state between the view and the PTY worker thread.
struct SessionInner {
    scrollback: String,
    input: String,
    running: bool,
    exit_code: Option<i32>,
    cols: u16,
    rows: u16,
    title: String,
    error: Option<String>,
}

pub struct Terminal {
    focus_handle: FocusHandle,
    config: TerminalConfig,
    inner: Arc<Mutex<SessionInner>>,
    closed: Arc<AtomicBool>,
    _reader: Option<std::thread::JoinHandle<()>>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    _master: Option<Box<dyn MasterPty + Send>>,
    rx: Receiver<TerminalEvent>,
    _tx: Sender<TerminalEvent>,
}

impl Terminal {
    pub fn new(config: TerminalConfig, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let (tx, rx) = channel::<TerminalEvent>();
        let inner = Arc::new(Mutex::new(SessionInner {
            scrollback: String::new(),
            input: String::new(),
            running: true,
            exit_code: None,
            cols: 100,
            rows: 30,
            title: title_for(&config),
            error: None,
        }));

        let mut terminal = Self {
            focus_handle,
            config,
            inner,
            closed: Arc::new(AtomicBool::new(false)),
            _reader: None,
            writer: Mutex::new(None),
            _master: None,
            rx,
            _tx: tx,
        };
        terminal.spawn(cx);
        terminal
    }

    fn spawn(&mut self, cx: &mut Context<Self>) {
        let pair = match native_pty_system().openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            Ok(pair) => pair,
            Err(error) => {
                self.fail(format!("打开 PTY 失败: {error}"), cx);
                return;
            }
        };
        let master = pair.master;
        let slave = pair.slave;

        let (tx, inner, closed) = (self._tx.clone(), self.inner.clone(), self.closed.clone());
        let program = self.config.program.clone();
        let is_serial = self.config.kind == TerminalKind::Serial;

        if is_serial {
            match serialport::new(&program, 115_200).open() {
                Ok(port) => {
                    let read_port = match port.try_clone() {
                        Ok(clone) => clone,
                        Err(error) => {
                            self.fail(format!("串口克隆失败: {error}"), cx);
                            return;
                        }
                    };
                    let (tx2, inner2, closed2) = (tx, inner, closed);
                    let reader = std::thread::spawn(move || {
                        let mut port = read_port;
                        let mut buf = [0u8; 4096];
                        loop {
                            if closed2.load(Ordering::SeqCst) {
                                break;
                            }
                            match port.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    let text = String::from_utf8_lossy(&buf[..n]).to_string();
                                    push_output(&inner2, &text, &tx2);
                                }
                            }
                        }
                    });
                    *self.writer.lock().unwrap() = Some(Box::new(port));
                    self._reader = Some(reader);
                    {
                        let mut state = self.inner.lock().unwrap();
                        state.title = format!("串口 {program}");
                    }
                    return;
                }
                Err(error) => {
                    self.fail(format!("打开串口 {program} 失败: {error}"), cx);
                    return;
                }
            }
        }

        let mut cmd = CommandBuilder::new(&program);
        if let Ok(current_dir) = std::env::current_dir() {
            cmd.cwd(current_dir);
        }
        cmd.env("TERM", "xterm-256color");
        let child = match slave.spawn_command(cmd) {
            Ok(child) => child,
            Err(error) => {
                self.fail(format!("启动 shell「{program}」失败: {error}"), cx);
                return;
            }
        };

        let mut reader = match master.try_clone_reader() {
            Ok(reader) => reader,
            Err(error) => {
                self.fail(format!("读取 PTY 失败: {error}"), cx);
                return;
            }
        };
        let writer = match master.take_writer() {
            Ok(writer) => writer,
            Err(error) => {
                self.fail(format!("写入 PTY 失败: {error}"), cx);
                return;
            }
        };

        self._master = Some(master);
        *self.writer.lock().unwrap() = Some(writer);

        let reader_handle = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                if closed.load(Ordering::SeqCst) {
                    break;
                }
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&buf[..n]).to_string();
                        push_output(&inner, &text, &tx);
                    }
                }
            }
            let _ = tx.send(TerminalEvent::Exited(0));
        });
        self._reader = Some(reader_handle);

        let (inner2, tx2, closed2) = (self.inner.clone(), self._tx.clone(), self.closed.clone());
        std::thread::spawn(move || {
            let mut child = child;
            let exit = child.wait().map(|status| status.exit_code() as i32);
            let code = exit.unwrap_or(1);
            if !closed2.load(Ordering::SeqCst) {
                let mut state = inner2.lock().unwrap();
                state.running = false;
                state.exit_code = Some(code);
                drop(state);
                let _ = tx2.send(TerminalEvent::Exited(code));
            }
        });
    }

    fn fail(&mut self, message: String, cx: &mut Context<Self>) {
        let mut state = self.inner.lock().unwrap();
        state.running = false;
        state.error = Some(message.clone());
        state.scrollback.push_str(&format!("\r\n{message}\r\n"));
        drop(state);
        cx.notify();
    }

    pub fn send_text(&self, text: &str) {
        if let Some(writer) = self.writer.lock().unwrap().as_mut() {
            let _ = writer.write_all(text.as_bytes());
            let _ = writer.flush();
        }
    }


    pub fn send_newline(&self) {
        self.send_text("\r");
    }

    pub fn backspace(&self) {
        self.send_text("\x7f");
    }


    pub fn clear(&self) {
        let mut state = self.inner.lock().unwrap();
        state.scrollback.clear();
        state.input.clear();
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

    /// Forward a keystroke to the PTY. Printable characters are echoed by the
    /// shell; Enter sends CR; Backspace sends DEL; Ctrl+C sends interrupt.
    pub fn handle_key(&self, keystroke: &gpui::Keystroke) {
        if keystroke.modifiers.control {
            if keystroke.key.eq_ignore_ascii_case("c") {
                self.send_text("\x03");
                let mut state = self.inner.lock().unwrap();
                state.input.clear();
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
            "enter" => {
                self.send_newline();
                let mut state = self.inner.lock().unwrap();
                let line = state.input.clone();
                state.scrollback.push_str(&line);
                state.scrollback.push('\n');
                state.input.clear();
            }
            "backspace" => {
                self.backspace();
                let mut state = self.inner.lock().unwrap();
                state.input.pop();
            }
            _ => {
                if let Some(ch) = keystroke.key_char.as_ref() {
                    self.send_text(ch);
                    let mut state = self.inner.lock().unwrap();
                    state.input.push_str(ch);
                }
            }
        }
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub fn scrollback(&self) -> String {
        self.inner.lock().unwrap().scrollback.clone()
    }

    pub fn input(&self) -> String {
        self.inner.lock().unwrap().input.clone()
    }

    pub fn is_running(&self) -> bool {
        self.inner.lock().unwrap().running
    }

    pub fn title(&self) -> String {
        self.inner.lock().unwrap().title.clone()
    }

    pub fn error(&self) -> Option<String> {
        self.inner.lock().unwrap().error.clone()
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.inner.lock().unwrap().exit_code
    }

    pub fn size(&self) -> (u16, u16) {
        let state = self.inner.lock().unwrap();
        (state.cols, state.rows)
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        {
            let mut state = self.inner.lock().unwrap();
            state.cols = cols;
            state.rows = rows;
        }
        if let Some(master) = self._master.as_ref() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    /// Drain pending events. Called from the view when it observes the
    /// terminal; returns true when new output arrived so the caller can notify.
    pub fn drain_events(&self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.rx.try_recv() {
            changed = true;
            match event {
                TerminalEvent::Output => {}
                TerminalEvent::Exited(code) => {
                    let mut state = self.inner.lock().unwrap();
                    state.running = false;
                    state.exit_code = Some(code);
                }
            }
        }
        changed
    }

}

fn title_for(config: &TerminalConfig) -> String {
    match config.kind {
        TerminalKind::Shell => "终端".to_string(),
        TerminalKind::Serial => format!("串口 {}", config.program),
    }
}

/// Append raw output to the scrollback, filtering ANSI escape sequences for
/// display, and notify the view that new output arrived.
fn push_output(inner: &Arc<Mutex<SessionInner>>, text: &str, tx: &Sender<TerminalEvent>) {
    let clean = strip_ansi(text);
    {
        let mut state = inner.lock().unwrap();
        state.scrollback.push_str(&clean);
        const MAX_BYTES: usize = 1 << 20; // 1 MiB
        if state.scrollback.len() > MAX_BYTES {
            let cut = state.scrollback.len() - MAX_BYTES;
            let start = state
                .scrollback
                .find('\n')
                .map(|index| index + 1)
                .unwrap_or(cut);
            state.scrollback.drain(..start.min(cut));
        }
    }
    let _ = tx.send(TerminalEvent::Output);
}

/// Remove common ANSI escape sequences (SGR color, cursor movement, erase,
/// OSC title) so the line-based renderer shows plain text.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&next) {
                        break;
                    }
                }
            } else if chars.peek() == Some(&']') {
                chars.next();
                for next in chars.by_ref() {
                    if next == '\x07' || next == '\x1b' {
                        if next == '\x1b' {
                            let _ = chars.next();
                        }
                        break;
                    }
                }
            } else {
                let _ = chars.next();
            }
        } else {
            out.push(ch);
        }
    }
    out
}

impl EventEmitter<TerminalEvent> for Terminal {}

impl Render for Terminal {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let scrollback = self.scrollback();
        let input = self.input();
        let running = self.is_running();
        let error = self.error();
        let title = self.title();
        let lines: Vec<String> = scrollback.split('\n').map(str::to_string).collect();

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
                    .children(
                        lines
                            .into_iter()
                            .enumerate()
                            .map(|(index, line)| {
                                gpui::div()
                                    .id(("term-line", index))
                                    .h(px(16.))
                                    .whitespace_nowrap()
                                    .font_family(theme::MONO_FONT)
                                    .text_size(px(12.))
                                    .text_color(gpui::rgb(theme::TEXT))
                                    .child(SharedString::from(line))
                            }),
                    ),
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
                            .text_size(px(11.))
                            .text_color(gpui::rgb(theme::TEXT_FAINT))
                            .child(if running {
                                SharedString::from("$")
                            } else {
                                match self.exit_code() {
                                    Some(code) => SharedString::from(format!("✕ {code}")),
                                    None => SharedString::from("✕"),
                                }
                            }),
                    )
                    .child(
                        gpui::div()
                            .flex_1()
                            .text_size(px(12.))
                            .text_color(gpui::rgb(theme::TEXT))
                            .child(SharedString::from(input)),
                    )
                    .child(
                        gpui::div()
                            .id("term-clear")
                            .text_size(px(10.))
                            .text_color(gpui::rgb(theme::TEXT_FAINT))
                            .cursor_pointer()
                            .hover(|style| style.text_color(gpui::rgb(theme::TEXT)))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.clear();
                                cx.notify();
                            }))
                            .child("清屏"),
                    )
                    .child(
                        gpui::div()
                            .text_size(px(10.))
                            .text_color(gpui::rgb(theme::TEXT_FAINT))
                            .child(SharedString::from(format!(
                                "{} · {}×{}",
                                title,
                                self.size().0,
                                self.size().1
                            ))),
                    )
                    .when(error.is_some(), |style| {
                        style.child(
                            gpui::div()
                                .text_size(px(10.))
                                .text_color(gpui::rgb(theme::DANGER))
                                .child(SharedString::from(error.unwrap_or_default())),
                        )
                    }),
            )
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_sgr_and_cursor_moves() {
        let input = "\x1b[31mred\x1b[0m plain \x1b[2Jclear";
        let cleaned = strip_ansi(input);
        assert_eq!(cleaned, "red plain clear");
    }

    #[test]
    fn strip_ansi_keeps_plain_text() {
        assert_eq!(strip_ansi("hello\nworld"), "hello\nworld");
    }

    #[test]
    fn strip_ansi_handles_osc_title() {
        let input = "\x1b]0;my title\x07text";
        assert_eq!(strip_ansi(input), "text");
    }

    /// End-to-end proof that the PTY mechanism the terminal relies on spawns a
    /// real shell and streams its output. Mirrors `Terminal::spawn`.
    #[test]
    fn pty_spawns_shell_and_streams_output() {
        if cfg!(windows) {
            return; // `sh` is not available on Windows; CI covers Unix.
        }
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open pty");
        let mut cmd = CommandBuilder::new("sh");
        cmd.arg("-c");
        cmd.arg("echo hello-pty; exit 0");
        let mut child = pair.slave.spawn_command(cmd).expect("spawn shell");
        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        let mut output = String::new();
        let mut buf = [0u8; 1024];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => output.push_str(&String::from_utf8_lossy(&buf[..n])),
            }
            if output.contains("hello-pty") {
                break;
            }
        }
        let _ = child.wait();
        assert!(
            output.contains("hello-pty"),
            "expected shell output, got: {output:?}"
        );
    }
}
