//! Serial assistant (串口助手) panel.
//!
//! Enumerate serial ports, choose baud/parity/data-bits/stop-bits, open/close
//! the port, send text or hex bytes, and display received data with optional
//! hex formatting, timestamps, and a clear button. A background thread reads
//! from the port into a receive buffer surfaced to the UI.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use gpui::prelude::*;
use gpui::{Context, Entity, EventEmitter, Render, Window, div, px, rgb};
use serde::{Deserialize, Serialize};

use crate::text_field::TextField;
use crate::{theme, widgets};

/// A serial port on the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    pub name: String,
    pub description: String,
}

/// Events emitted by the serial monitor.
#[derive(Debug, Clone)]
pub enum SerialEvent {
    Data,
    Error(String),
}

/// Parity setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Parity {
    None,
    Even,
    Odd,
}


/// Flow control setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlowControl {
    None,
    Hardware,
    Software,
}

/// Configuration for the serial session.
#[derive(Debug, Clone)]
pub struct SerialConfig {
    pub port: String,
    pub baud: u32,
    pub data_bits: u8,
    pub stop_bits: u8,
    pub parity: Parity,
    pub flow: FlowControl,
}

impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            port: String::new(),
            baud: 115_200,
            data_bits: 8,
            stop_bits: 1,
            parity: Parity::None,
            flow: FlowControl::None,
        }
    }
}

struct SessionInner {
    receive: String,
    hex_receive: bool,
    hex_send: bool,
    show_timestamps: bool,
    open: bool,
    error: Option<String>,
    rx_bytes: u64,
    tx_bytes: u64,
    config: SerialConfig,
}

pub struct SerialMonitor {
    inner: Arc<Mutex<SessionInner>>,
    closed: Arc<AtomicBool>,
    port: Arc<Mutex<Option<Box<dyn serialport::SerialPort + Send>>>>,
    send_field: Option<Entity<TextField>>,
    _tx: Sender<SerialEvent>,
    _reader: Option<std::thread::JoinHandle<()>>,
}

impl SerialMonitor {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (tx, _rx) = channel::<SerialEvent>();
        let send_field = cx.new(|cx| TextField::new("发送内容…", cx));
        let inner = Arc::new(Mutex::new(SessionInner {
            receive: String::new(),
            hex_receive: false,
            hex_send: false,
            show_timestamps: true,
            open: false,
            error: None,
            rx_bytes: 0,
            tx_bytes: 0,
            config: SerialConfig::default(),
        }));
        Self {
            inner,
            closed: Arc::new(AtomicBool::new(false)),
            port: Arc::new(Mutex::new(None)),
            send_field: Some(send_field),
            _tx: tx,
            _reader: None,
        }
    }

    /// Enumerate available serial ports.
    pub fn list_ports() -> Vec<PortInfo> {
        serialport::available_ports()
            .map(|ports| {
                ports
                    .into_iter()
                    .map(|port| PortInfo {
                        name: port.port_name,
                        description: match port.port_type {
                            serialport::SerialPortType::UsbPort(info) => {
                                info.product.unwrap_or_default()
                            }
                            serialport::SerialPortType::BluetoothPort => "Bluetooth".to_string(),
                            _ => String::new(),
                        },
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn open_port(&mut self, cx: &mut Context<Self>) {
        let config = self.inner.lock().unwrap().config.clone();
        if config.port.trim().is_empty() {
            self.inner.lock().unwrap().error = Some("请先选择串口设备".to_string());
            cx.notify();
            return;
        }
        let builder = serialport::new(&config.port, config.baud)
            .data_bits(match config.data_bits {
                5 => serialport::DataBits::Five,
                6 => serialport::DataBits::Six,
                7 => serialport::DataBits::Seven,
                _ => serialport::DataBits::Eight,
            })
            .stop_bits(match config.stop_bits {
                2 => serialport::StopBits::Two,
                _ => serialport::StopBits::One,
            })
            .parity(match config.parity {
                Parity::Even => serialport::Parity::Even,
                Parity::Odd => serialport::Parity::Odd,
                Parity::None => serialport::Parity::None,
            })
            .flow_control(match config.flow {
                FlowControl::Hardware => serialport::FlowControl::Hardware,
                FlowControl::Software => serialport::FlowControl::Software,
                FlowControl::None => serialport::FlowControl::None,
            });
        match builder.open() {
            Ok(mut port) => {
                let _ = port.set_timeout(std::time::Duration::from_millis(50));
                *self.port.lock().unwrap() = Some(port);
                {
                    let mut inner = self.inner.lock().unwrap();
                    inner.open = true;
                    inner.error = None;
                    inner.rx_bytes = 0;
                    inner.tx_bytes = 0;
                }
                let inner_clone = self.inner.clone();
                let closed = self.closed.clone();
                let port_clone = self.port.clone();
                let tx = self._tx.clone();
                self._reader = Some(std::thread::spawn(move || {
                    let mut buffer = [0u8; 2048];
                    loop {
                        if closed.load(Ordering::Relaxed) {
                            break;
                        }
                        let read_result = {
                            let mut guard = port_clone.lock().unwrap();
                            match guard.as_mut() {
                                Some(port) => port.read(&mut buffer),
                                None => break,
                            }
                        };
                        match read_result {
                            Ok(0) => {
                                std::thread::sleep(std::time::Duration::from_millis(20));
                            }
                            Ok(count) => {
                                let mut inner = inner_clone.lock().unwrap();
                                let is_hex = inner.hex_receive;
                                let ts = inner.show_timestamps;
                                let text = String::from_utf8_lossy(&buffer[..count]).into_owned();
                                if is_hex {
                                    let hex: String = buffer[..count]
                                        .iter()
                                        .map(|b| format!("{b:02X} "))
                                        .collect();
                                    if ts {
                                        inner.receive.push_str(&format!("{}", timestamp()));
                                    }
                                    inner.receive.push_str(&hex);
                                    inner.receive.push('\n');
                                } else {
                                    if ts {
                                        inner.receive.push_str(&timestamp());
                                    }
                                    inner.receive.push_str(&text);
                                }
                                inner.rx_bytes += count as u64;
                                drop(inner);
                                let _ = tx.send(SerialEvent::Data);
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                                std::thread::sleep(std::time::Duration::from_millis(10));
                            }
                            Err(e) => {
                                let _ = tx.send(SerialEvent::Error(format!("读取失败: {e}")));
                                break;
                            }
                        }
                    }
                }));
                cx.notify();
            }
            Err(error) => {
                self.inner.lock().unwrap().error = Some(format!("打开串口失败: {error}"));
                cx.notify();
            }
        }
    }

    fn close_port(&mut self, cx: &mut Context<Self>) {
        *self.port.lock().unwrap() = None;
        self.inner.lock().unwrap().open = false;
        cx.notify();
    }

    fn send_bytes(&mut self, cx: &mut Context<Self>) {
        let (text, hex_send) = {
            let field = self.send_field.clone();
            let text = field
                .map(|f| f.read(cx).text())
                .unwrap_or_default();
            let hex = self.inner.lock().unwrap().hex_send;
            (text, hex)
        };
        let payload = if hex_send {
            parse_hex(&text)
        } else {
            text.into_bytes()
        };
        if payload.is_empty() {
            return;
        }
        let mut guard = self.port.lock().unwrap();
        match guard.as_mut() {
            Some(port) => match port.write_all(&payload).and_then(|_| port.flush()) {
                Ok(()) => {
                    let mut inner = self.inner.lock().unwrap();
                    inner.tx_bytes += payload.len() as u64;
                    drop(inner);
                }
                Err(error) => {
                    self.inner.lock().unwrap().error = Some(format!("发送失败: {error}"));
                }
            },
            None => {
                self.inner.lock().unwrap().error = Some("串口未打开".to_string());
            }
        }
        cx.notify();
    }

    /// Set the selected port (from the app's dropdown).
    pub fn set_port(&mut self, name: String, cx: &mut Context<Self>) {
        self.inner.lock().unwrap().config.port = name;
        cx.notify();
    }

}

impl EventEmitter<SerialEvent> for SerialMonitor {}

impl Render for SerialMonitor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let inner = self.inner.lock().unwrap();
        let config = inner.config.clone();
        let is_open = inner.open;
        let error = inner.error.clone();
        let hex_receive = inner.hex_receive;
        let hex_send = inner.hex_send;
        let ts = inner.show_timestamps;
        let rx_bytes = inner.rx_bytes;
        let tx_bytes = inner.tx_bytes;
        let receive = inner.receive.clone();
        drop(inner);

        div()
            .id("serial-monitor")
            .flex_1()
            .flex()
            .flex_col()
            .p(px(8.))
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .child(
                                div()
                                    .px(px(8.))
                                    .py(px(3.))
                                    .rounded(px(5.))
                                    .bg(theme::alpha(theme::STROKE, 0.08))
                                    .text_size(px(10.))
                                    .text_color(rgb(theme::TEXT))
                                    .child(if config.port.is_empty() {
                                        "选择串口设备…".to_string()
                                    } else {
                                        config.port.clone()
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(3.))
                                    .children(
                                        SerialMonitor::list_ports()
                                            .into_iter()
                                            .map(|port| {
                                                port_chip(port.name, &config.port, cx)
                                                    .into_any_element()
                                            })
                                            .collect::<Vec<_>>(),
                                    ),
                            ),
                    )
                    .child(baud_selector(config.baud, is_open, cx))
                    .child(
                        widgets::ghost_button("serial-open", if is_open { "关闭" } else { "打开" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.inner.lock().unwrap().open {
                                    this.close_port(cx);
                                } else {
                                    this.open_port(cx);
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .id("serial-receive")
                    .flex_1()
                    .overflow_y_scroll()
                    .rounded(px(6.))
                    .bg(theme::alpha(theme::STROKE, 0.04))
                    .p(px(6.))
                    .text_size(px(10.))
                    .text_color(rgb(theme::TEXT))
                    .child(if receive.is_empty() {
                        "（接收区为空）".to_string()
                    } else {
                        receive.clone()
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(9.))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(format!("RX {rx_bytes} B · TX {tx_bytes} B"))
                    .child(toggle("hex-recv", "HEX 显示", hex_receive, cx))
                    .child(toggle("ts", "时间戳", ts, cx))
                    .child(
                        widgets::ghost_button("serial-clear", "清空").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.inner.lock().unwrap().receive.clear();
                                this.inner.lock().unwrap().rx_bytes = 0;
                                cx.notify();
                            },
                        )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .child(
                                self.send_field
                                    .clone()
                                    .map(|field| field.clone().into_any_element())
                                    .unwrap_or_else(|| div().into_any_element()),
                            ),
                    )
                    .child(toggle("hex-send", "HEX 发送", hex_send, cx))
                    .child(
                        widgets::ghost_button("serial-send", "发送").on_click(cx.listener(
                            |this, _, _, cx| this.send_bytes(cx),
                        )),
                    ),
            )
            .when(error.is_some(), |style| {
                style.child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(theme::DANGER))
                        .child(error.unwrap_or_default()),
                )
            })
    }
}

fn baud_selector(current: u32, _is_open: bool, cx: &mut Context<SerialMonitor>) -> impl IntoElement {
    let rates = [9600u32, 19200, 38400, 57600, 115200, 230400, 460800, 921600];
    let mut cells = Vec::new();
    for rate in rates {
        let selected = rate == current;
        cells.push(
            div()
                .id(("serial-baud", rate))
                .px(px(6.))
                .py(px(2.))
                .rounded(px(4.))
                .bg(if selected {
                    theme::alpha(theme::ACCENT, 0.12)
                } else {
                    theme::alpha(theme::STROKE, 0.05)
                })
                .text_size(px(9.))
                .text_color(if selected { rgb(theme::ACCENT) } else { rgb(theme::TEXT_DIM) })
                .child(rate.to_string())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.inner.lock().unwrap().config.baud = rate;
                    cx.notify();
                })),
        );
    }
    div().flex().gap(px(3.)).children(cells)
}
fn port_chip(name: String, current: &str, cx: &mut Context<SerialMonitor>) -> impl IntoElement {
    let selected = name == current;
    let id = gpui::SharedString::from(format!("serial-port-{name}"));
    div()
        .id(id)
        .px(px(6.))
        .py(px(2.))
        .rounded(px(4.))
        .bg(if selected {
            theme::alpha(theme::ACCENT, 0.12)
        } else {
            theme::alpha(theme::STROKE, 0.05)
        })
        .text_size(px(9.))
        .text_color(if selected { rgb(theme::ACCENT) } else { rgb(theme::TEXT_DIM) })
        .child(name.clone())
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_port(name.clone(), cx);
        }))
}


fn toggle(
    id: &'static str,
    label: &str,
    checked: bool,
    cx: &mut Context<SerialMonitor>,
) -> impl IntoElement {
    div()
        .id(id)
        .px(px(6.))
        .py(px(2.))
        .rounded(px(4.))
        .bg(if checked {
            theme::alpha(theme::ACCENT, 0.12)
        } else {
            theme::alpha(theme::STROKE, 0.05)
        })
        .text_size(px(9.))
        .text_color(if checked { rgb(theme::ACCENT) } else { rgb(theme::TEXT_FAINT) })
        .child(label.to_string())
        .on_click(cx.listener(move |this, _, _, cx| {
            let mut inner = this.inner.lock().unwrap();
            match id {
                "hex-recv" => inner.hex_receive = !inner.hex_receive,
                "hex-send" => inner.hex_send = !inner.hex_send,
                "ts" => inner.show_timestamps = !inner.show_timestamps,
                _ => {}
            }
            cx.notify();
        }))
}

/// Parse a hex string like `AA BB 0C` into bytes.
fn parse_hex(input: &str) -> Vec<u8> {
    let cleaned: String = input.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let mut bytes = Vec::new();
    let chars: Vec<char> = cleaned.chars().collect();
    for pair in chars.chunks(2) {
        if pair.len() == 2 {
            if let Ok(value) = u8::from_str_radix(&pair.iter().collect::<String>(), 16) {
                bytes.push(value);
            }
        }
    }
    bytes
}

fn timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    format!("[{:02}:{:02}:{:02}.{:03}] ", secs / 3600, (secs / 60) % 60, secs % 60, millis)
}
