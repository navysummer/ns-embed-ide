//! Keil/ARM FLM flash-algorithm metadata parsing.
//!
//! A `.FLM` file is a small binary that describes how to program a device's
//! flash: the total device size, the programming page size, the erased byte
//! value, the base address, and a function descriptor table whose entries
//! point into the algorithm code. This module parses the metadata header so
//! the IDE can use an external flash algorithm's geometry (device size, page
//! size, base) to configure OpenOCD/pyOCD/J-Link flashing — the same layout
//! Keil and pyOCD read.
//!
//! ## Layout (all little-endian)
//!
//! | offset | field                        |
//! |--------|------------------------------|
//! | 0      | device size (u32)            |
//! | 4      | programming page size (u32) |
//! | 8      | erased byte value (u8)      |
//! | 12     | base address (u32)          |
//! | 16     | function descriptors        |
//!
//! Function descriptors are `{ u32 short, u32 long }` pairs (offset, size),
//! terminated by an all-zero pair. The algorithm code/data follows.

use anyhow::{Context, Result, bail};

/// Parsed FLM flash-algorithm metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashAlgorithm {
    /// Total device flash size in bytes.
    pub device_size: u32,
    /// Programming page size in bytes (0 if not present).
    pub page_size: u32,
    /// Value of an erased byte (usually 0xFF).
    pub erased_value: u8,
    /// Base address of the flash (usually `0x0800_0000` for Cortex-M).
    pub base_address: u32,
    /// Function descriptors: (offset into algorithm, size in bytes).
    pub functions: Vec<(u32, u32)>,
    /// True when the buffer is long enough to be a plausible FLM header.
    pub valid: bool,
}

/// Parse a FLM header from a byte buffer.
///
/// Returns an error when the buffer is too short for the header. The `valid`
/// flag distinguishes a well-formed header (a terminating all-zero function
/// descriptor is present) from a truncated-but-readable one.
pub fn parse_flm(bytes: &[u8]) -> Result<FlashAlgorithm> {
    if bytes.len() < 16 {
        bail!("FLM 数据过短（{} 字节，至少需要 16 字节头）", bytes.len());
    }
    let device_size = read_u32(bytes, 0).context("读取 device_size 失败")?;
    let page_size = read_u32(bytes, 4).context("读取 page_size 失败")?;
    let erased_value = bytes[8];
    let base_address = read_u32(bytes, 12).context("读取 base_address 失败")?;
    let mut functions = Vec::new();
    let mut offset = 16;
    let mut valid = false;
    loop {
        if offset + 8 > bytes.len() {
            break;
        }
        let short = read_u32(bytes, offset).context("读取函数描述符失败")?;
        let long = read_u32(bytes, offset + 4).context("读取函数描述符失败")?;
        if short == 0 && long == 0 {
            valid = true;
            break;
        }
        functions.push((short, long));
        offset += 8;
    }
    Ok(FlashAlgorithm {
        device_size,
        page_size,
        erased_value,
        base_address,
        functions,
        valid,
    })
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    bytes
        .get(offset..offset + 4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().expect("4 bytes")))
        .with_context(|| format!("FLM 偏移 {offset:#x} 越界"))
}

/// Load and parse a FLM file from disk.
pub fn load_flm(path: &std::path::Path) -> Result<FlashAlgorithm> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("读取 FLM 文件 {} 失败", path.display()))?;
    parse_flm(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal plausible FLM buffer and parse it back.
    fn sample_flm() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&128u32.to_le_bytes()); // device_size
        bytes.extend_from_slice(&256u32.to_le_bytes()); // page_size
        bytes.push(0xFF); // erased value
        bytes.extend_from_slice(&[0, 0, 0]); // padding to offset 8
        bytes.extend_from_slice(&0x0800_0000u32.to_le_bytes()); // base
        // Two function descriptors then the terminator.
        bytes.extend_from_slice(&0u32.to_le_bytes()); // offset
        bytes.extend_from_slice(&64u32.to_le_bytes()); // size
        bytes.extend_from_slice(&64u32.to_le_bytes()); // offset
        bytes.extend_from_slice(&32u32.to_le_bytes()); // size
        bytes.extend_from_slice(&0u32.to_le_bytes()); // terminator
        bytes.extend_from_slice(&0u32.to_le_bytes());
        // Trailing algorithm bytes.
        bytes.extend_from_slice(&[0x00, 0xBF, 0x70, 0x47]);
        bytes
    }

    #[test]
    fn parses_flm_header_and_functions() {
        let flm = parse_flm(&sample_flm()).unwrap();
        assert_eq!(flm.device_size, 128);
        assert_eq!(flm.page_size, 256);
        assert_eq!(flm.erased_value, 0xFF);
        assert_eq!(flm.base_address, 0x0800_0000);
        assert_eq!(flm.functions.len(), 2);
        assert_eq!(flm.functions[0], (0, 64));
        assert_eq!(flm.functions[1], (64, 32));
        assert!(flm.valid);
    }

    #[test]
    fn rejects_short_flm() {
        assert!(parse_flm(&[0u8; 8]).is_err());
    }

    #[test]
    fn handles_unterminated_functions() {
        // No terminating zero pair: header parses but `valid` is false.
        let mut bytes = sample_flm();
        let len = bytes.len();
        bytes.truncate(len - 8); // drop the terminator + trailing bytes
        let flm = parse_flm(&bytes).unwrap();
        assert_eq!(flm.functions.len(), 2);
        assert!(!flm.valid);
    }
}
