//! Firmware file handling: detect the format and load the raw payload for
//! flashing. Supports ELF (binary extraction), Intel HEX and raw BIN.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareFormat {
    Elf,
    IntelHex,
    Bin,
}

impl FirmwareFormat {
    pub fn label(self) -> &'static str {
        match self {
            FirmwareFormat::Elf => "ELF",
            FirmwareFormat::IntelHex => "Intel HEX",
            FirmwareFormat::Bin => "BIN",
        }
    }
}

/// A loaded firmware image: the raw bytes plus the load address and format.
#[derive(Debug, Clone)]
pub struct FirmwareImage {
    pub bytes: Vec<u8>,
    pub base: u64,
    pub format: FirmwareFormat,
    pub path: PathBuf,
}

/// Detect the format from the file extension and/or content magic.
pub fn detect_format(path: &Path) -> FirmwareFormat {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("hex") | Some("ihex") => FirmwareFormat::IntelHex,
        Some("elf") => FirmwareFormat::Elf,
        Some("bin") => FirmwareFormat::Bin,
        _ => {
            // Fall back to content sniffing.
            if let Ok(text) = fs::read_to_string(path) {
                if text.starts_with(':') {
                    return FirmwareFormat::IntelHex;
                }
            }
            FirmwareFormat::Bin
        }
    }
}

/// Load a firmware image from `path`, extracting the payload in the format
/// expected by the flasher. Intel HEX is decoded; ELF is read as-is (adapters
/// that accept ELF use the original file); BIN is read raw.
pub fn load_firmware(path: &Path) -> Result<FirmwareImage> {
    let format = detect_format(path);
    let bytes = fs::read(path).with_context(|| format!("读取固件 {} 失败", path.display()))?;
    let (payload, base) = match format {
        FirmwareFormat::IntelHex => {
            let text = String::from_utf8(bytes.clone())
                .map_err(|_| anyhow::anyhow!("Intel HEX 文件不是有效 UTF-8: {}", path.display()))?;
            let decoded = decode_intel_hex(&text)?;
            (decoded.bytes, decoded.base)
        }
        FirmwareFormat::Elf => {
            // Adapters that accept ELF take the file directly; keep bytes and
            // a conservative base of 0 (the ELF header carries the real entry).
            (bytes, 0)
        }
        FirmwareFormat::Bin => (bytes, 0),
    };
    Ok(FirmwareImage {
        bytes: payload,
        base,
        format,
        path: path.to_path_buf(),
    })
}

/// Memory-usage breakdown of a firmware image, split by memory region.
///
/// ELF PT_LOAD segments are attributed to `flash` or `ram` based on whether
/// their virtual address falls inside the target's flash or RAM window (from
/// `ns_device::memory_map`). Intel HEX / BIN payloads have a single contiguous
/// base and are attributed to whichever region contains `base`.
 pub struct FirmwareSize {
     /// Bytes occupying the target's flash region (code + rodata).
     pub flash: u64,
     /// Bytes occupying the target's RAM region (data + bss initializers).
     pub ram: u64,
    /// Bytes in the `.text` section (code), from the `size` tool. `0` when
    /// the tool is unavailable or the artifact is not ELF.
    pub text: u64,
    /// Bytes in the `.data` section (initialized globals), from `size`.
    pub data: u64,
    /// Bytes in the `.bss` section (zeroed globals), from `size`.
    pub bss: u64,
     /// Total payload bytes loaded (flash + ram).
     pub total: u64,
    /// The lowest load address observed.
    pub base: u64,
    /// Largest symbols (by size) in the firmware, from `nm`. Empty when the
    /// symbol table is unavailable (non-ELF artifact or no `nm` tool).
    pub symbols: Vec<SymbolSize>,
 }
 
 impl Default for FirmwareSize {
    fn default() -> Self {
         FirmwareSize {
            flash: 0,
             ram: 0,
            text: 0,
            data: 0,
            bss: 0,
             total: 0,
             base: 0,
             symbols: Vec::new(),
         }
     }
 }

/// One symbol's size, name and section from `nm --size-sort`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolSize {
    pub name: String,
    pub size: u64,
    pub address: u64,
 }

/// Compute the memory-usage breakdown of the firmware at `path`, given the
/// target's flash and RAM windows. Returns `Ok(None)` when the file is not a
/// firmware artifact, so callers can degrade gracefully.
///
/// `flash_windows`/`ram_windows` are `(start, size)` spans from the target
/// memory map; segments whose address falls in neither window are attributed
/// to RAM (they still count toward `total`).
pub fn analyze_firmware_size(
    path: &Path,
    flash_windows: &[(u64, u64)],
    _ram_windows: &[(u64, u64)],
) -> Result<Option<FirmwareSize>> {
    let format = detect_format(path);
    // Only firmware artifacts (ELF / Intel HEX / raw firmware BIN) are
    // analyzed. A `.txt`/`.s`/source file that sniffed as BIN is not firmware;
    // require the extension to be a known artifact to avoid misattribution.
    let is_artifact = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "elf" | "hex" | "ihex" | "bin" | "ihx"
            )
        })
        .unwrap_or(false);
    if !is_artifact {
        return Ok(None);
    }
    let bytes = fs::read(path).with_context(|| format!("读取固件 {} 失败", path.display()))?;
    let mut size = FirmwareSize::default();
    let mut first_addr: Option<u64> = None;
    match format {
        FirmwareFormat::Elf => {
            let segments = elf_load_segments(&bytes)?;
            if segments.is_empty() {
                return Ok(None);
            }
            for (addr, data) in &segments {
                first_addr = Some(first_addr.map_or(*addr, |cur| cur.min(*addr)));
                let len = data.len() as u64;
                size.total += len;
                if in_window(*addr, flash_windows) {
                    size.flash += len;
                } else {
                    // RAM (or neither window): data + bss initializers.
                    size.ram += len;
                }
            }
        }
        FirmwareFormat::IntelHex | FirmwareFormat::Bin => {
            let (payload, base) = match format {
                FirmwareFormat::IntelHex => {
                    let text = String::from_utf8(bytes.clone()).map_err(|_| {
                        anyhow::anyhow!("Intel HEX 文件不是有效 UTF-8: {}", path.display())
                    })?;
                    let decoded = decode_intel_hex(&text)?;
                    (decoded.bytes, decoded.base)
                }
                _ => (bytes, 0),
            };
            if payload.is_empty() {
                return Ok(None);
            }
            first_addr = Some(base);
            size.total = payload.len() as u64;
            if in_window(base, flash_windows) {
                size.flash = size.total;
            } else {
                size.ram = size.total;
            }
        }
    }
     size.base = first_addr.unwrap_or(0);
     if size.total == 0 {
         return Ok(None);
     }
     // For ELF artifacts, enrich with the largest symbols via `nm`.
     if format == FirmwareFormat::Elf {
         size.symbols = analyze_symbols(path);
        let (text, data, bss) = analyze_size_breakdown(path);
        size.text = text;
        size.data = data;
        size.bss = bss;
     }
     Ok(Some(size))
 }

/// Run `nm --size-sort --radix=d` on an ELF and return the largest symbols.
/// Returns an empty vector when `nm` is unavailable or the file is not ELF.
pub fn analyze_symbols(path: &Path) -> Vec<SymbolSize> {
    let nm = match ns_project::toolchain::find_command("nm") {
        Some(nm) => nm,
        None => return Vec::new(),
    };
    let output = match std::process::Command::new(&nm)
        .arg("--size-sort")
        .arg("--radix=d")
        .arg(path)
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    parse_nm_symbols(&text)
 }
 
 /// Run `size` on an ELF and return the classic `text`/`data`/`bss` byte
 /// counts. Returns `(0, 0, 0)` when `size` is unavailable or the artifact is
 /// not an ELF. The `size` tool's default output has three columns
 /// (`text`, `data`, `bss`) followed by the file name; the GNU `size` also
 /// supports `--format=sysv` for a per-section listing, but the plain three
 /// column form is what cross toolchains (e.g. `arm-none-eabi-size`) emit by
 /// default and is the most portable.
 pub fn analyze_size_breakdown(path: &Path) -> (u64, u64, u64) {
     let size = match ns_project::toolchain::find_command("size") {
        Some(size) => size,
        None => return (0, 0, 0),
    };
    let output = match std::process::Command::new(&size).arg(path).output() {
        Ok(output) if output.status.success() => output,
        _ => return (0, 0, 0),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    parse_size_breakdown(&text)
}

/// Parse the three-column `text data bss` line from `size` output. The header
/// line (`text    data     bss     dec     hex filename`) and the data line
/// follow the same layout, so the first numeric line wins.
pub fn parse_size_breakdown(text: &str) -> (u64, u64, u64) {
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            if let (Ok(t), Ok(d), Ok(b)) = (
                parts[0].parse::<u64>(),
                parts[1].parse::<u64>(),
                parts[2].parse::<u64>(),
            ) {
                return (t, d, b);
            }
        }
    }
    (0, 0, 0)
}
/// Parse `nm --size-sort --radix=d` output: each line is
/// `<address> <size> <type> <name>` (decimal address and size). Only
/// non-empty, real (non-absolute, non-undefined) symbols are kept; the result
/// is sorted by size descending so callers can show the largest first.
pub fn parse_nm_symbols(text: &str) -> Vec<SymbolSize> {
    let mut symbols: Vec<SymbolSize> = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }
        let address = match parts[0].parse::<u64>() {
            Ok(addr) => addr,
            Err(_) => continue,
        };
        let size = match parts[1].parse::<u64>() {
            Ok(size) => size,
            Err(_) => continue,
        };
        let name = parts[3..].join(" ");
        // Skip absolute (`a`/`A`), undefined (`u`/`U`) and debug (`n`/`N`)
        // symbols; they occupy no flash/RAM footprint.
        let kind = parts[2].chars().next().unwrap_or(' ');
        if matches!(kind, 'a' | 'A' | 'u' | 'U' | 'n' | 'N') {
            continue;
        }
        if size == 0 {
            continue;
        }
        symbols.push(SymbolSize { name, size, address });
    }
    symbols.sort_by(|a, b| b.size.cmp(&a.size));
    symbols
}

/// Whether an address falls in any `(start, size)` window.
fn in_window(addr: u64, windows: &[(u64, u64)]) -> bool {
    windows.iter().any(|(start, size)| {
        let end = start.saturating_add(*size);
        addr >= *start && addr < end
    })
}

/// Decoded Intel HEX payload.
pub struct DecodedHex {
    pub bytes: Vec<u8>,
    pub base: u64,
}

/// Parse Intel HEX records into a contiguous byte image. Supports data records
/// (00), EOF (01), extended segment address (02) and extended linear address
/// (04). The lowest loaded address becomes `base`.
pub fn decode_intel_hex(text: &str) -> Result<DecodedHex> {
    let mut segments: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut base: u64 = 0;
    let mut saw_eof = false;

    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if !line.starts_with(':') {
            bail!("第 {} 行不是合法 Intel HEX 记录: {}", index + 1, line);
        }
        let record = parse_hex_record(line)
            .with_context(|| format!("解析 Intel HEX 第 {} 行失败", index + 1))?;
        match record.record_type {
            0x00 => {
                let addr = base + record.address as u64;
                if let Some((_, seg_bytes)) = segments
                    .iter_mut()
                    .find(|(seg_addr, _)| *seg_addr == addr)
                {
                    seg_bytes.extend_from_slice(&record.data);
                } else {
                    segments.push((addr, record.data));
                }
            }
            0x01 => {
                saw_eof = true;
            }
            0x02 => {
                if record.data.len() != 2 {
                    bail!("扩展段地址记录长度应为 2");
                }
                base = u16::from_be_bytes([record.data[0], record.data[1]]) as u64 * 16;
            }
            0x04 => {
                if record.data.len() != 2 {
                    bail!("扩展线性地址记录长度应为 2");
                }
                base = u16::from_be_bytes([record.data[0], record.data[1]]) as u64 * 65536;
            }
            other => {
                let _ = other;
            }
        }
        if saw_eof {
            break;
        }
    }

    if !saw_eof {
        bail!("Intel HEX 文件缺少 EOF 记录");
    }

    segments.sort_by_key(|(addr, _)| *addr);
    let base_addr = segments
        .first()
        .map(|(addr, _)| *addr)
        .unwrap_or(0);
    let mut bytes = Vec::new();
    for (addr, data) in segments {
        let offset = (addr - base_addr) as usize;
        if offset > bytes.len() {
            bytes.resize(offset, 0);
        }
        bytes.extend_from_slice(&data);
    }
    Ok(DecodedHex {
        bytes,
        base: base_addr,
    })
}

struct HexRecord {
    address: u16,
    record_type: u8,
    data: Vec<u8>,
}

fn parse_hex_record(line: &str) -> Result<HexRecord> {
    let hex = line.strip_prefix(':').unwrap_or(line);
    if hex.len() < 10 || hex.len() % 2 != 0 {
        bail!("记录长度无效");
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| anyhow::anyhow!("非法十六进制字符"))?;
    let byte_count = bytes[0] as usize;
    if bytes.len() != byte_count + 5 {
        bail!("记录长度与字节数不符");
    }
    let address = u16::from_be_bytes([bytes[1], bytes[2]]);
    let record_type = bytes[3];
    let data = bytes[4..4 + byte_count].to_vec();
    let checksum = bytes[4 + byte_count];
    let sum: u8 = bytes[..4 + byte_count].iter().fold(0, |acc, b| acc.wrapping_add(*b));
    if sum.wrapping_add(checksum) != 0 {
        bail!("校验和错误");
    }
    Ok(HexRecord {
        address,
        record_type,
        data,
    })
}

/// Convert `bytes` (an ELF image) to Intel HEX or raw BIN.
///
/// Extracts the loadable program segments (PT_LOAD) and emits them. For BIN,
/// the segments are concatenated from the lowest load address; the caller
/// learns the base address so it can pass it to the flasher. For Intel HEX,
/// data records (00) plus extended linear address records (04) are emitted so
/// the payload can be loaded above 64 KiB.
pub fn convert_firmware(
    bytes: &[u8],
    format: FirmwareFormat,
) -> Result<FirmwareImage> {
    let segments = elf_load_segments(bytes)?;
    if segments.is_empty() {
        bail!("ELF 没有可加载的段（PT_LOAD）");
    }
    let base = segments
        .iter()
        .map(|(addr, _)| *addr)
        .min()
        .unwrap_or(0);
    let payload = match format {
        FirmwareFormat::IntelHex => encode_intel_hex(&segments, base),
        FirmwareFormat::Bin => {
            let mut bin = Vec::new();
            for (addr, data) in &segments {
                let offset = (*addr - base) as usize;
                if offset > bin.len() {
                    bin.resize(offset, 0);
                }
                bin.extend_from_slice(data);
            }
            bin
        }
        FirmwareFormat::Elf => {
            // No conversion needed; return the original bytes.
            bytes.to_vec()
        }
    };
    Ok(FirmwareImage {
        bytes: payload,
        base,
        format,
        path: PathBuf::new(),
    })
}

/// Extract `(address, data)` loadable segments from an ELF image.
fn elf_load_segments(elf: &[u8]) -> Result<Vec<(u64, Vec<u8>)>> {
    if elf.len() < 52 || &elf[0..4] != b"\x7fELF" {
        bail!("不是有效的 ELF 文件");
    }
    let class = elf[4];
    let data = elf[5];
    let little = match data {
        1 => true,
        2 => false,
        _ => bail!("不支持的 ELF 数据编码: {data}"),
    };
    let (phoff, phentsize, phnum) = match class {
        1 => {
            let phoff = read_u32(elf, 28, little) as usize;
            let phentsize = read_u16(elf, 42, little) as usize;
            let phnum = read_u16(elf, 44, little) as usize;
            (phoff, phentsize, phnum)
        }
        2 => {
            let phoff = read_u64(elf, 32, little) as usize;
            let phentsize = read_u16(elf, 54, little) as usize;
            let phnum = read_u16(elf, 56, little) as usize;
            (phoff, phentsize, phnum)
        }
        _ => bail!("不支持的 ELF 类别: {class}"),
    };
    if phoff == 0 || phentsize == 0 || phnum == 0 {
        return Ok(Vec::new());
    }
    let mut segments = Vec::new();
    for index in 0..phnum {
        let off = phoff + index * phentsize;
        if off + phentsize > elf.len() {
            break;
        }
        let (p_type, p_offset, p_filesz, p_addr) = match class {
            1 => {
                let p_type = read_u32(elf, off, little);
                let p_offset = read_u32(elf, off + 4, little) as usize;
                let p_addr = read_u32(elf, off + 8, little) as u64;
                let p_filesz = read_u32(elf, off + 16, little) as usize;
                (p_type, p_offset, p_filesz, p_addr)
            }
            2 => {
                let p_type = read_u32(elf, off, little);
                let p_offset = read_u64(elf, off + 8, little) as usize;
                let p_addr = read_u64(elf, off + 16, little);
                let p_filesz = read_u64(elf, off + 32, little) as usize;
                (p_type, p_offset, p_filesz, p_addr)
            }
            _ => unreachable!(),
        };
        if p_type != 1 {
            // PT_LOAD
            continue;
        }
        let start = p_offset.min(elf.len());
        let end = (p_offset + p_filesz).min(elf.len());
        if end <= start {
            continue;
        }
        segments.push((p_addr, elf[start..end].to_vec()));
    }
    segments.sort_by_key(|(addr, _)| *addr);
    Ok(segments)
}

fn read_u32(bytes: &[u8], offset: usize, little: bool) -> u32 {
    let mut buf = [0u8; 4];
    let start = offset.min(bytes.len());
    let len = (offset + 4).min(bytes.len()) - start;
    buf[..len].copy_from_slice(&bytes[start..start + len]);
    if little {
        u32::from_le_bytes(buf)
    } else {
        u32::from_be_bytes(buf)
    }
}

fn read_u64(bytes: &[u8], offset: usize, little: bool) -> u64 {
    let mut buf = [0u8; 8];
    let start = offset.min(bytes.len());
    let len = (offset + 8).min(bytes.len()) - start;
    buf[..len].copy_from_slice(&bytes[start..start + len]);
    if little {
        u64::from_le_bytes(buf)
    } else {
        u64::from_be_bytes(buf)
    }
}

fn read_u16(bytes: &[u8], offset: usize, little: bool) -> u16 {
    let mut buf = [0u8; 2];
    let start = offset.min(bytes.len());
    let len = (offset + 2).min(bytes.len()) - start;
    buf[..len].copy_from_slice(&bytes[start..start + len]);
    if little {
        u16::from_le_bytes(buf)
    } else {
        u16::from_be_bytes(buf)
    }
}

/// Encode segments as Intel HEX. `base` is the lowest load address; records
/// above 64 KiB are emitted with extended linear address (04) records.
fn encode_intel_hex(segments: &[(u64, Vec<u8>)], base: u64) -> Vec<u8> {
    let mut out = String::new();
    let mut upper: u64 = u64::MAX;
    for (seg_addr, data) in segments {
        let mut offset = 0usize;
        while offset < data.len() {
            let addr = *seg_addr + offset as u64;
            let chunk = &data[offset..(offset + 16).min(data.len())];
            // Emit an extended linear address record when the upper 16 bits
            // change (only for addresses above 64 KiB).
            let seg_upper = addr >> 16;
            if seg_upper != upper {
                let ua = (seg_upper & 0xffff) as u16;
                out.push_str(&encode_hex_record(0x0000, 0x04, &ua.to_be_bytes()));
                upper = seg_upper;
            }
            let low = (addr & 0xffff) as u16;
            out.push_str(&encode_hex_record(low, 0x00, chunk));
            offset += chunk.len();
        }
    }
    out.push_str(&encode_hex_record(0x0000, 0x01, &[]));
    let _ = base;
    out.into_bytes()
}

fn encode_hex_record(address: u16, record_type: u8, data: &[u8]) -> String {
    let mut bytes = Vec::with_capacity(5 + data.len());
    bytes.push(data.len() as u8);
    bytes.extend_from_slice(&address.to_be_bytes());
    bytes.push(record_type);
    bytes.extend_from_slice(data);
    let sum: u8 = bytes.iter().fold(0, |acc, b| acc.wrapping_add(*b));
    let checksum = sum.wrapping_neg();
    let mut line = String::from(":");
    for b in bytes.iter().chain(std::iter::once(&checksum)) {
        line.push_str(&format!("{b:02X}"));
    }
    line.push('\n');
    line
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_intel_hex_with_linear_address() {
        let hex = ":020000040000FA\n\
                  :0400000001020304F2\n\
                  :00000001FF";
        let decoded = decode_intel_hex(hex).unwrap();
        assert_eq!(decoded.bytes, vec![0x01, 0x02, 0x03, 0x04]);
        assert_eq!(decoded.base, 0);
    }

    #[test]
    fn rejects_bad_checksum() {
         let hex = ":0400000001020304FF\n:00000001FF";
         assert!(decode_intel_hex(hex).is_err());
     }

    /// `parse_nm_symbols` keeps real, sized symbols, drops absolute/undefined/
    /// debug and zero-sized entries, and sorts by size descending.
    #[test]
     fn parses_nm_symbols_and_sorts_by_size() {
         let text = "\
            0000000010000000 0000000000000012 T main\n\
            0000000010000010 0000000000000032 t helper\n\
             0000000008000000 0000000000000000 A absolute\n\
             0000000000000000 0000000000000008 U undefined\n\
             0000000000000000 0000000000000004 n debug_symbol\n\
            0000000010000100 0000000000000024 T big_function\n";
         let symbols = parse_nm_symbols(text);
         assert_eq!(symbols.len(), 3);
        // Sorted descending by size: helper (32) > big_function (24) > main (18).
         assert_eq!(symbols[0].name, "helper");
        assert_eq!(symbols[0].size, 32);
         assert_eq!(symbols[1].name, "big_function");
        assert_eq!(symbols[1].size, 24);
         assert_eq!(symbols[2].name, "main");
        assert_eq!(symbols[2].size, 12);
     }

    #[test]
    fn missing_eof_is_error() {
        let hex = ":0400000001020304F2";
        assert!(decode_intel_hex(hex).is_err());
    }

    #[test]
    fn detects_format_by_extension() {
        assert_eq!(detect_format(Path::new("fw.hex")), FirmwareFormat::IntelHex);
        assert_eq!(detect_format(Path::new("fw.elf")), FirmwareFormat::Elf);
        assert_eq!(detect_format(Path::new("fw.bin")), FirmwareFormat::Bin);
    }

    /// Build a minimal little-endian ELF32 with one PT_LOAD segment.
    fn elf32_with_load(p_addr: u32, data: &[u8]) -> Vec<u8> {
        // ELF header (52 bytes) + one program header (32 bytes).
        let mut elf = vec![0u8; 52 + 32];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 1; // ELFCLASS32
        elf[5] = 1; // little-endian
        elf[6] = 1; // EV_CURRENT
        elf[16] = 2; // e_type = ET_EXEC
        elf[18] = 40; // e_machine = ARM
        elf[20..24].copy_from_slice(&1u32.to_le_bytes()); // e_version
        elf[28..32].copy_from_slice(&52u32.to_le_bytes()); // e_phoff
        elf[42..44].copy_from_slice(&32u16.to_le_bytes()); // e_phentsize
        elf[44..46].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        // Program header (PT_LOAD = 1).
        let ph = 52;
        elf[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes()); // p_type
        elf[ph + 4..ph + 8].copy_from_slice(&84u32.to_le_bytes()); // p_offset (after both headers)
        elf[ph + 8..ph + 12].copy_from_slice(&p_addr.to_le_bytes()); // p_vaddr
        elf[ph + 12..ph + 16].copy_from_slice(&p_addr.to_le_bytes()); // p_paddr
        elf[ph + 16..ph + 20].copy_from_slice(&(data.len() as u32).to_le_bytes()); // p_filesz
        elf[ph + 20..ph + 24].copy_from_slice(&(data.len() as u32).to_le_bytes()); // p_memsz
        elf[ph + 24..ph + 28].copy_from_slice(&1u32.to_le_bytes()); // p_flags
        elf[ph + 28..ph + 32].copy_from_slice(&4u32.to_le_bytes()); // p_align
        // Append the payload at offset 84 (after both headers).
        elf.extend_from_slice(data);
        elf
    }

    #[test]
    fn converts_elf_to_bin() {
        let elf = elf32_with_load(0x0800_0000, &[0x01, 0x02, 0x03, 0x04]);
        let image = convert_firmware(&elf, FirmwareFormat::Bin).unwrap();
        assert_eq!(image.base, 0x0800_0000);
        assert_eq!(image.bytes, vec![0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn converts_elf_to_hex_roundtrip() {
        let elf = elf32_with_load(0x0800_0000, &[0xDE, 0xAD, 0xBE, 0xEF, 0x01]);
        let image = convert_firmware(&elf, FirmwareFormat::IntelHex).unwrap();
        let text = String::from_utf8(image.bytes).unwrap();
        let decoded = decode_intel_hex(&text).unwrap();
        assert_eq!(decoded.base, 0x0800_0000);
        assert_eq!(decoded.bytes, vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01]);
    }

    #[test]
    fn rejects_non_elf() {
        assert!(convert_firmware(b"not an elf", FirmwareFormat::Bin).is_err());
    }
#[test]
fn analyzes_elf_split_flash_and_ram() {
    let dir = std::env::temp_dir().join("ns_flash_size_test");
    std::fs::create_dir_all(&dir).unwrap();
    let elf_path = dir.join("fw.elf");
    // Build a 2-segment ELF32: flash @0x0800_0000 (3 bytes) + RAM @0x2000_0000 (2 bytes).
    // Layout: ELF header(52) + phdr1(32) + phdr2(32) = 116; flash payload @116, RAM @119.
    let mut out = vec![0u8; 52];
    out[0..4].copy_from_slice(b"\x7fELF");
    out[4] = 1; // ELFCLASS32
    out[5] = 1; // little-endian
    out[6] = 1;
    out[16] = 2; // ET_EXEC
    out[18] = 40; // e_machine = ARM
    out[20..24].copy_from_slice(&1u32.to_le_bytes());
    out[28..32].copy_from_slice(&52u32.to_le_bytes()); // e_phoff
    out[42..44].copy_from_slice(&32u16.to_le_bytes()); // e_phentsize
    out[44..46].copy_from_slice(&2u16.to_le_bytes()); // e_phnum = 2
    // Program header #1: flash @ 0x0800_0000, payload at 116, 3 bytes.
    out.extend_from_slice(&{
        let mut h = vec![0u8; 32];
        h[0..4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        h[4..8].copy_from_slice(&116u32.to_le_bytes());
        h[8..12].copy_from_slice(&0x0800_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&0x0800_0000u32.to_le_bytes());
        h[16..20].copy_from_slice(&3u32.to_le_bytes()); // filesz
        h[20..24].copy_from_slice(&3u32.to_le_bytes()); // memsz
        h
    });
    // Program header #2: RAM @ 0x2000_0000, payload at 119, 2 bytes.
    out.extend_from_slice(&{
        let mut h = vec![0u8; 32];
        h[0..4].copy_from_slice(&1u32.to_le_bytes());
        h[4..8].copy_from_slice(&119u32.to_le_bytes());
        h[8..12].copy_from_slice(&0x2000_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&0x2000_0000u32.to_le_bytes());
        h[16..20].copy_from_slice(&2u32.to_le_bytes());
        h[20..24].copy_from_slice(&2u32.to_le_bytes());
        h
    });
    // Payloads: flash at 116 (3), RAM at 119 (2) — contiguous, no padding.
    out.extend_from_slice(&[0x01, 0x02, 0x03]);
    out.extend_from_slice(&[0xAA, 0xBB]);
    std::fs::write(&elf_path, &out).unwrap();
    let flash = vec![(0x0800_0000u64, 0x1000u64)];
    let ram = vec![(0x2000_0000u64, 0x2000u64)];
    let size = analyze_firmware_size(&elf_path, &flash, &ram).unwrap().unwrap();
    assert_eq!(size.flash, 3, "flash bytes");
    assert_eq!(size.ram, 2, "ram bytes");
    assert_eq!(size.total, 5, "total bytes");
    assert_eq!(size.base, 0x0800_0000, "lowest load address");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn analyzes_hex_by_base_region() {
    let hex = ":020000040800F2\n\
              :0400000001020304F2\n\
              :00000001FF";
    let dir = std::env::temp_dir().join("ns_flash_size_test_hex");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fw.hex");
    std::fs::write(&path, hex).unwrap();
    let flash = vec![(0x0800_0000u64, 0x1000u64)];
    let ram = vec![(0x2000_0000u64, 0x2000u64)];
    let size = analyze_firmware_size(&path, &flash, &ram).unwrap().unwrap();
    assert_eq!(size.flash, 4);
    assert_eq!(size.ram, 0);
    assert_eq!(size.total, 4);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn unknown_format_returns_none() {
    let dir = std::env::temp_dir().join("ns_flash_size_test_none");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fw.txt");
    std::fs::write(&path, b"hello").unwrap();
    let size = analyze_firmware_size(&path, &[], &[]).unwrap();
    assert!(size.is_none());
    std::fs::remove_dir_all(&dir).ok();
 }

#[test]
fn parses_size_three_column_breakdown() {
    // GNU `size` default output: `text data bss dec hex filename`.
    let report = "   text    data     bss     dec     hex filename\n\
                  1024       64     512    1600     640 fw.elf\n";
    assert_eq!(parse_size_breakdown(report), (1024, 64, 512));
    // A cross toolchain may omit the header; the first numeric line still wins.
    let bare = "  2048      128    1024    3200     c80 fw.elf\n";
    assert_eq!(parse_size_breakdown(bare), (2048, 128, 1024));
    // Non-numeric or empty output degrades to zeros.
    assert_eq!(parse_size_breakdown(""), (0, 0, 0));
     assert_eq!(parse_size_breakdown("text data bss\n"), (0, 0, 0));
 }

}
