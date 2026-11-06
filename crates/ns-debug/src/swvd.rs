//! Trace window (SWO/SWV) support for Cortex-M targets.
//!
//! Trace data never comes through DAP: the debug probe streams SWV packets, so
//! the IDE needs probe integration (ST-Link/J-Link/OpenOCD) plus ITM/DWT setup
//! on the target. This module owns the *protocol-independent* part:
//!
//! * [`TraceConfig`] — what to enable and which DWT comparators to arm.
//! * DWT comparator encoding (watchpoint on a data address, cycle counter,
//!   exception trace) as `writeMemory` payloads for the DAP adapter.
//! * ITM packet decoding (trace source / stimulus port messages).
//! * Probe discovery, preferring `NS_TRACE_PROBE`, then a listening
//!   `st-link`/`swordfish` GDB server, then OpenOCD on its default port.
//!
//! The probe connection itself (`probe-rs`) is not wired up yet: [`TraceConfig::requests`]
//! emits the target-side setup, and [`package_support`] reports honestly when a
//! session cannot trace (e.g. a hosted x86 debuggee) instead of pretending.
//! See AGENT.md §5.1 — full SWO streaming is an open item.

use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

/// Register addresses on the ARMv7-M debug interface used for tracing.
pub const DEMCR: u32 = 0xE000_EDFC;
pub const DWT_CTRL: u32 = 0xE000_1000;
pub const COMP_BASE: u32 = 0xE000_1020;
pub const MASK_BASE: u32 = 0xE000_1024;
pub const FUNCTION_BASE: u32 = 0xE000_1028;
pub const TPIU_LAR: u32 = 0xE004_0FB0;
pub const TPIU_ACPR: u32 = 0xE004_0010;
pub const TPIU_SPPR: u32 = 0xE004_00F0;
pub const TPIU_FFCR: u32 = 0xE004_0304;
pub const ITM_TER_BASE: u32 = 0xE000_0E00;
pub const ITM_TCR: u32 = 0xE000_0E80;
pub const ITM_LAR: u32 = 0xE000_0FB0;

/// How the trace window is started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceMode {
    /// Trace only when running, buffered while paused (typical SWO usage).
    #[default]
    Running,
    /// Trace continuously, including while paused.
    Always,
}

impl TraceMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "仅运行时",
            Self::Always => "始终",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Running => Self::Always,
            Self::Always => Self::Running,
        }
    }
}

/// Trace window configuration, persisted in the project's IDE state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TraceConfig {
    /// Capture DWT data watchpoint events (writes go to the trace buffer).
    pub data_trace: bool,
    /// DWT comparator for `data_trace` (0–3 on most parts, 0–7 on some).
    pub data_comparator: u8,
    /// Address watched for `data_trace`.
    pub data_address: String,
    /// ITM stimulus port used to drive `printf` redirection.
    pub itm_port: u8,
    /// SWO/core clock ratio expressed as the TPIU prescaler.
    pub swo_prescaler: u32,
    pub mode: TraceMode,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            data_trace: false,
            data_comparator: 0,
            data_address: String::new(),
            itm_port: 0,
            swo_prescaler: 1,
            mode: TraceMode::Running,
        }
    }
}

/// Parse a `0x…`/`0X…` hex address or a bare decimal address.
pub fn parse_address(text: &str) -> Option<u32> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        // An explicit `0x` prefix always means hexadecimal.
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        // Bare digits are decimal; `20000010` is read as 20,000,010, and users
        // who mean hex write `0x20000010`.
        None => trimmed.parse::<u32>().ok(),
    }
}

/// Address of DWT comparator `index`'s compare register.
pub fn dwt_comp_address(index: u8) -> u32 {
    COMP_BASE + index as u32 * 4
}

/// Address of DWT comparator `index`'s mask register.
pub fn dwt_mask_address(index: u8) -> u32 {
    MASK_BASE + index as u32 * 4
}

/// Address of DWT comparator `index`'s function register.
pub fn dwt_function_address(index: u8) -> u32 {
    FUNCTION_BASE + index as u32 * 4
}

/// DWT function value for a watchpoint on a data address.
pub fn dwt_function_watchpoint() -> u32 {
    0x5
}

/// DWT function value for the PC sampler (program counter cycles counter).
pub fn dwt_function_cycles() -> u32 {
    0x4
}

/// DWT function value for exception tracing.
pub fn dwt_function_exception() -> u32 {
    0x3
}

/// Little-endian hex encoding of a 32-bit debug register write.
fn word(value: u32) -> String {
    let bytes = value.to_le_bytes();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A `writeMemory` DAP request targeting a debug register.
pub(crate) fn write_memory(address: u32, value: u32) -> Value {
    json!({
        "memoryReference": format!("0x{address:X}"),
        "data": word(value),
        "allowPartial": false,
    })
}

/// File name of the trace configuration inside `.ns-ide/`.
pub const TRACE_FILE: &str = "trace.json";

impl TraceConfig {
    /// Load the trace configuration saved next to the breakpoints, falling back
    /// to defaults when the file is absent or unreadable.
    pub fn load(root: &std::path::Path) -> Self {
        let path = root.join(".ns-ide").join(TRACE_FILE);
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Persist the trace configuration in `.ns-ide/trace.json`.
    pub fn save(&self, root: &std::path::Path) -> anyhow::Result<()> {
        let path = root.join(".ns-ide").join(TRACE_FILE);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Validate the configuration for the given session; the error is shown in
    /// the trace panel instead of silently doing nothing.
    pub fn validate(&self) -> Result<(), String> {
        if self.swo_prescaler == 0 {
            return Err("SWO 分频不能为 0".to_string());
        }
        if self.swo_prescaler > 1024 {
            return Err("SWO 分频最多 1024".to_string());
        }
        if self.data_comparator > 3 {
            return Err("DWT 比较器只有 0–3".to_string());
        }
        if self.data_trace {
            let address = self
                .data_address
                .trim();
            if address.is_empty() {
                return Err("数据跟踪需要填写被监视变量的地址".to_string());
            }
            if parse_address(address).is_none() {
                return Err(format!("无法解析地址 `{address}`（示例 0x20000010）"));
            }
        }
        if self.itm_port > 31 {
            return Err("ITM 端口取 0–31".to_string());
        }
        Ok(())
    }

    /// The target-side setup, as a sequence of DAP requests (all sent with
    /// [`super::DebugCommand::Disassemble`]-style fire-and-forget semantics).
    ///
    /// Registers are unlocked through the CoreSight Lock Access Register so a
    /// locked debug block does not silently ignore the writes.
    pub fn requests(&self) -> Vec<(String, Value)> {
        let mut requests = Vec::new();
        let mut write = |address: u32, value: u32| {
            requests.push(("writeMemory".to_string(), write_memory(address, value)));
        };

        // DEMCR.TRCENA (bit 24) enables the trace subsystem.
        write(DEMCR, 1 << 24);
        // Unlock + program TPIU for asynchronous SWO.
        write(TPIU_LAR, 0xC5AC_CE55);
        write(TPIU_ACPR, self.swo_prescaler.saturating_sub(1));
        write(TPIU_SPPR, 0x2); // NRZ (Manchester off)
        write(TPIU_FFCR, 0);
        // ITM: unlock, enable the stimulus port, then the ITM master switch.
        write(ITM_LAR, 0xC5AC_CE55);
        let port_bit = 1u32 << self.itm_port;
        write(ITM_TER_BASE + (self.itm_port as u32 / 4) * 4, port_bit);
        write(ITM_TCR, (1 << 0) | (1 << 16)); // ITMENA | TraceBusID=1

        // DWT: cycle counter + exception trace, and the optional data watch.
        let mut dwt_ctrl = 1;
        if self.data_trace {
            let comparator = self.data_comparator.min(3);
            write(dwt_comp_address(comparator), parse_address(&self.data_address).unwrap_or(0));
            write(dwt_mask_address(comparator), 0);
            write(dwt_function_address(comparator), dwt_function_watchpoint());
            dwt_ctrl |= 1 << (comparator as u32 * 4);
        }
        write(DWT_CTRL, dwt_ctrl);
        requests
    }

    /// Requests that disable tracing (used by `StopTrace` and on session exit).
    pub fn stop_requests(&self) -> Vec<(String, Value)> {
        vec![
            ("writeMemory".to_string(), write_memory(ITM_TCR, 0)),
            ("writeMemory".to_string(), write_memory(DWT_CTRL, 0)),
            ("writeMemory".to_string(), write_memory(DEMCR, 0)),
        ]
    }
}

/// Requests that disable tracing regardless of the current configuration.
pub fn stop_requests() -> Vec<(String, Value)> {
    TraceConfig::default().stop_requests()
}

/// `variablesReference` a virtual trace scope is exposed under. DAP references
/// are opaque to the client, so the trace scope simply picks two values that
/// lldb-dap never hands out itself.
pub fn scope_reference(scope: &str) -> u64 {
    match scope {
        "GlobalScope" => 0x7FFF_FFFE,
        _ => 0x7FFF_FFFF,
    }
}

/// One decoded SWV trace packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    /// Instrumentation (printf) message on `port`.
    Stimulus { port: u8, payload: Vec<u8> },
    /// DWT comparator `index` fired (data watchpoint / PC sample).
    Dwt { index: u8, pc: Option<u32> },
    /// Exception entry/exit; `number` is the exception number.
    Exception { number: u16, entered: bool },
    /// Timestamp packet value.
    Timestamp(u32),
}

/// Decode a stream of SWO header + payload bytes.
///
/// Supports ITM pages with `SS=0` (data source 0 = stimulus, source 1 = DWT,
/// source 2 = extension) and source `2` exception packets whose payload holds a
/// 16-bit exception number. Malformed/unknown headers stop decoding of the
/// current buffer; the caller keeps the unconsumed tail for the next read.
pub fn decode(bytes: &[u8]) -> (Vec<Packet>, usize) {
    let mut packets = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let header = bytes[offset];
        match header {
            // Protocol 1 header: ITM data source 0 (stimulus), SS=00 means
            // the port is 0 and the header is 0x00.
            0x00 => {
                let Some(length) = bytes.get(offset + 1) else {
                    break;
                };
                let length = *length as usize;
                let start = offset + 2;
                if start + length > bytes.len() {
                    break;
                }
                packets.push(Packet::Stimulus {
                    port: 0,
                    payload: bytes[start..start + length].to_vec(),
                });
                offset = start + length;
            }
            // Timestamp packet: header 1, then a 4-byte value.
            0x01 => {
                let start = offset + 1;
                if start + 4 > bytes.len() {
                    break;
                }
                let value = u32::from_le_bytes([
                    bytes[start],
                    bytes[start + 1],
                    bytes[start + 2],
                    bytes[start + 3],
                ]);
                packets.push(Packet::Timestamp(value));
                offset = start + 4;
            }
            // Exception trace packet: header 2, then a 1-byte exception header.
            0x02 => {
                let Some(bits) = bytes.get(offset + 1) else {
                    break;
                };
                let entered = bits & 0x01 == 0;
                let number = u16::from(*bits >> 1);
                packets.push(Packet::Exception { number, entered });
                offset += 2;
            }
            // Protocol 2 header: lower bits encode the source and SS.
            header => {
                let source = header >> 3;
                match (header >> 1) & 0x03 {
                    0b00 => {
                        // Data source 0/1 with SS=0: 1-byte header, then the
                        // length byte and the payload.
                        let Some(length) = bytes.get(offset + 1) else {
                            break;
                        };
                        let length = *length as usize;
                        let start = offset + 2;
                        if start + length > bytes.len() {
                            break;
                        }
                        if source == 1 {
                            let index = ((header >> 1) & 0x03) as u8;
                            let value = bytes[start..start + length]
                                .iter()
                                .rev()
                                .fold(0u32, |acc, byte| (acc << 8) | u32::from(*byte));
                            packets.push(Packet::Dwt {
                                index,
                                pc: Some(value),
                            });
                        } else {
                            packets.push(Packet::Stimulus {
                                port: source as u8,
                                payload: bytes[start..start + length].to_vec(),
                            });
                        }
                        offset = start + length;
                    }
                    _ => break,
                }
            }
        }
    }
    (packets, offset)
}

/// Probe a GDB server by name; returns whether it accepts a connection.
fn port_open(port: u16) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&address, Duration::from_millis(150)).is_ok()
}

/// Kind of trace probe the IDE could find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// Command-line tool that streams SWV (st-link/ST-Link GDB server).
    Tool { path: String },
    /// A listening GDB server that multiplexes SWO on a port.
    Server { port: u16 },
}

/// Locate a trace-capable probe. `NS_TRACE_PROBE` wins; otherwise look for the
/// ST-Link GDB server (`st-util`/`ST-LINK_gdbserver`) on `PATH` and then a
/// listening server on the usual ports.
pub fn find_probe(path_var: Option<&str>) -> Option<Probe> {
    if let Ok(explicit) = std::env::var("NS_TRACE_PROBE")
        && Path::new(&explicit).is_file()
    {
        return Some(Probe::Tool { path: explicit });
    }
    if let Some(path_var) = path_var {
        for name in ["st-util", "ST-LINK_gdbserver", "JLinkGDBServerCLExe"] {
            for directory in path_var.split(':') {
                let candidate = Path::new(directory).join(name);
                if candidate.is_file() {
                    return Some(Probe::Tool {
                        path: candidate.display().to_string(),
                    });
                }
            }
        }
    }
    for port in [4242u16, 3333, 61234] {
        if port_open(port) {
            return Some(Probe::Server { port });
        }
    }
    None
}

/// Why a session can't produce trace data. `None` means tracing is available.
pub fn package_support(probe: Option<&Probe>) -> Option<&'static str> {
    if probe.is_none() {
        return Some(
            "未找到支持 SWV 的调试探针（ST-Link GDB server / st-util / J-Link）。\
             嵌入 SWO 流目前尚未接通，见 AGENT.md §5.1。",
        );
    }
    Some(
        "已检测到探针，但 SWO 流尚未接入 IDE 的 trace 面板（探针网络读取未实现）。\
         见 AGENT.md §5.1。",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_and_decimal_addresses() {
        assert_eq!(parse_address("0x20000010"), Some(0x2000_0010));
        assert_eq!(parse_address("20000010"), Some(20_000_010));
        assert_eq!(parse_address("16"), Some(16));
        assert_eq!(parse_address("  0X10 "), Some(16));
        assert_eq!(parse_address(""), None);
        assert_eq!(parse_address("0xZZ"), None);
    }

    #[test]
    fn validates_trace_configuration() {
        let mut config = TraceConfig::default();
        assert!(config.validate().is_ok());
        config.swo_prescaler = 0;
        assert!(config.validate().is_err());
        config.swo_prescaler = 4096;
        assert!(config.validate().is_err());
        let mut config = TraceConfig::default();
        config.data_trace = true;
        assert!(config.validate().is_err());
        config.data_address = "0x20000000".into();
        assert!(config.validate().is_ok());
        config.data_comparator = 9;
        assert!(config.validate().is_err());
    }

    #[test]
    fn emits_target_setup_writes_in_order() {
        let config = TraceConfig {
            data_trace: true,
            data_comparator: 1,
            data_address: "0x20000010".into(),
            itm_port: 0,
            ..TraceConfig::default()
        };
        let requests = config.requests();
        assert!(requests.iter().all(|(command, _)| command == "writeMemory"));
        let first = &requests[0].1;
        assert_eq!(first["memoryReference"], "0xE000EDFC");
        assert_eq!(first["data"], "00000001");
        let addresses: Vec<&str> = requests
            .iter()
            .map(|(_, request)| request["memoryReference"].as_str().unwrap())
            .collect();
        assert!(addresses.contains(&"0xE0001024"), "comparator 1 compare reg");
        assert!(addresses.contains(&"0xE000102C"), "comparator 1 function reg");
        assert_eq!(dwt_comp_address(0), 0xE000_1020);
        assert_eq!(
            addresses.last().copied(),
            Some("0xE0001000"),
            "DWT_CTRL is the last DWT write"
        );
    }

    #[test]
    fn watchpoint_function_says_store() {
        // DWT FUNCTION value 5 = watchpoint on store.
        assert_eq!(dwt_function_watchpoint(), 0x5);
        assert_eq!(dwt_function_exception(), 0x3);
        assert_eq!(dwt_function_cycles(), 0x4);
    }

    #[test]
    fn stop_requests_disable_itm_dwt_and_trace() {
        let requests = stop_requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].1["memoryReference"], "0xE0000E80");
        assert_eq!(requests[0].1["data"], "00000000");
    }

    #[test]
    fn decodes_stimulus_and_exception_packets() {
        // Stimulus on port 0 with payload "hi", an exception entry (number 15),
        // then a timestamp of 0x01020304.
        let bytes = [
            0x00, 0x02, b'h', b'i', // header 0x00: stimulus port 0, length 2
            0x02, 0x1E, // exception header 0x02, E0=0 (entry), number 15
            0x01, 0x04, 0x03, 0x02, 0x01, // timestamp header 0x01 + 0x01020304
        ];
        let (packets, consumed) = decode(&bytes);
        assert_eq!(consumed, bytes.len());
        assert_eq!(
            packets,
            vec![
                Packet::Stimulus {
                    port: 0,
                    payload: b"hi".to_vec()
                },
                Packet::Exception {
                    number: 15,
                    entered: true
                },
                Packet::Timestamp(0x0102_0304),
            ]
        );
    }

    #[test]
    fn decode_stops_at_truncated_packet() {
        let (packets, consumed) = decode(&[0x00, 0x05, b'a', b'b']);
        assert!(packets.is_empty());
        assert_eq!(consumed, 0, "incomplete packet leaves the buffer untouched");
    }

    #[test]
    fn trace_config_roundtrips_through_state_file() {
        let root = std::env::temp_dir().join(format!(
            "ns-trace-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = TraceConfig {
            data_trace: true,
            data_comparator: 2,
            data_address: "0x20000010".into(),
            itm_port: 3,
            swo_prescaler: 8,
            mode: TraceMode::Always,
        };
        config.save(&root).unwrap();
        let loaded = TraceConfig::load(&root);
        assert_eq!(loaded, config);
        assert!(root.join(".ns-ide").join(TRACE_FILE).is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_state_file_falls_back_to_defaults() {
        let root = std::env::temp_dir().join(format!(
            "ns-trace-missing-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(TraceConfig::load(&root), TraceConfig::default());
    }

    #[test]
    fn probe_support_explains_why_tracing_is_unavailable() {
        let reason = package_support(None).expect("hosted sessions cannot trace");
        assert!(reason.contains("AGENT.md"));
        let tool = Probe::Tool {
            path: "/usr/bin/st-util".into(),
        };
        assert!(package_support(Some(&tool)).is_some());
    }
}
