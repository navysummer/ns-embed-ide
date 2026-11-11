//! Memory-layout visualization support.
//!
//! Parses the project's linker script (`MEMORY { ... }` blocks) into named
//! regions, and combines that with section sizes read from the built firmware
//! (via `size`/`nm`) so the IDE can render a region-occupancy map.
 /// A named memory region from the linker script (`MEMORY { NAME (attrs) : ORIGIN = ..., LENGTH = ... }`).
 #[derive(Debug, Clone, PartialEq, Eq)]
 pub struct MemoryRegion {
    pub name: String,
    /// Attributes in parentheses, e.g. `rx`, `rwx`.
    pub attributes: String,
    pub origin: u64,
    pub length: u64,
    /// Bytes used in this region (sum of sections placed here). 0 = unknown.
    pub used: u64,
}

/// A section symbol with its size and load address (from `nm`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionUsage {
    pub name: String,
    pub address: u64,
    pub size: u64,
}

/// The full memory-layout model for a firmware.
#[derive(Debug, Clone, Default)]
pub struct MemoryLayout {
    pub regions: Vec<MemoryRegion>,
    pub sections: Vec<SectionUsage>,
}

/// Parse `MEMORY { ... }` blocks from a linker-script text.
///
/// Handles both `ORIGIN = 0x08000000, LENGTH = 64K` and the shorter
/// `ORIGIN = ..., LENGTH = ...` forms with `K`/`M`/`G` suffixes. Lines that do
/// not parse are skipped (the caller sees a best-effort region list).
pub fn parse_linker_memory(text: &str) -> Vec<MemoryRegion> {
    let mut regions = Vec::new();
    let mut in_memory = false;
     for raw_line in text.lines() {
         let line = raw_line.trim();
         if !in_memory && line.starts_with("MEMORY") {
             in_memory = true;
             continue;
         }
         if in_memory && line == "{" {
             continue;
         }
         if !in_memory {
             continue;
         }
         if line.starts_with('}') {
             in_memory = false;
             continue;
         }
        let Some(open) = line.find('(') else { continue };
        let Some(close) = line.find(')') else { continue };
        let name = line[..open].trim().to_string();
        if name.is_empty() {
            continue;
        }
        let attributes = line[open + 1..close].trim().to_string();
        let origin = extract_kv(line, "ORIGIN");
        let length = extract_kv(line, "LENGTH");
        if let (Some(origin), Some(length)) = (origin, length) {
            regions.push(MemoryRegion {
                name,
                attributes,
                origin,
                length,
                used: 0,
            });
        }
    }
    regions
}

/// Extract a `KEY = value` (case-insensitive) from a line, parsing the numeric
/// value with `K`/`M`/`G`/`k`/`m`/`g` and `0x`/decimal support.
fn extract_kv(line: &str, key: &str) -> Option<u64> {
    let upper = line.to_ascii_uppercase();
    let key_upper = key.to_ascii_uppercase();
    let idx = upper.find(&key_upper)?;
    let rest = &upper[idx + key_upper.len()..];
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
     let mut num = String::new();
     let mut rest_after = "";
     for (i, ch) in rest.char_indices() {
         if ch.is_ascii_hexdigit() || ch == 'x' || ch == 'X' || ch == '_' {
             num.push(ch);
         } else {
             rest_after = &rest[i..];
             break;
         }
     }
     let (value, suffix) = split_num_suffix(&num);
     let number = if value.starts_with("0x") || value.starts_with("0X") {
         u64::from_str_radix(&value[2..], 16)
     } else {
         value.parse::<u64>()
     }
     .ok()?;
     // Prefer the explicit suffix captured from the source; `split_num_suffix`
     // is a fallback for numbers that already carry a trailing letter.
     let suffix = if suffix.is_empty() {
         rest_after
             .trim_start()
             .chars()
             .next()
             .map(|c| c.to_string())
             .unwrap_or_default()
     } else {
         suffix
     };
     Some(match suffix.as_str() {
         "K" | "k" => number * 1024,
         "M" | "m" => number * 1024 * 1024,
         "G" | "g" => number * 1024 * 1024 * 1024,
         _ => number,
     })
 }

 /// Split a numeric string into its numeric part and trailing alpha suffix.
 fn split_num_suffix(num: &str) -> (String, String) {
    let split_at = num
        .find(|c: char| !c.is_ascii_hexdigit() && c != 'x' && c != 'X' && c != '_')
        .unwrap_or(num.len());
    (num[..split_at].to_string(), num[split_at..].to_string())
}

/// Assign section usages to regions by address and mark each region's `used`
/// bytes. Sections whose address falls outside every region are attributed to
/// the nearest region (by start address) so nothing is silently dropped.
pub fn assign_sections(regions: &mut [MemoryRegion], sections: &[SectionUsage]) {
    for section in sections {
        if let Some(region) = regions
            .iter_mut()
            .find(|region| section.address >= region.origin && section.address < region.origin + region.length)
        {
            region.used += section.size;
        } else {
            // Nearest region by origin (for sections at/below a region start).
            if let Some(region) = regions.iter_mut().min_by_key(|region| {
                section.address.abs_diff(region.origin)
            }) {
                region.used += section.size;
            }
        }
    }
}

/// Parse a GNU `size` report (`text data bss dec hex filename`) into section
/// usages (text→0x0, data→0x20000000, bss→0x20000000 are approximations; the
/// `nm` path is preferred when available).
pub fn parse_size_report(text: &str) -> Vec<SectionUsage> {
    let mut sections = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 5 {
            if let (Ok(text_size), Ok(data_size), Ok(bss_size)) = (
                parts[0].parse::<u64>(),
                parts[1].parse::<u64>(),
                parts[2].parse::<u64>(),
            ) {
                sections.push(SectionUsage {
                    name: ".text".to_string(),
                    address: 0x0800_0000,
                    size: text_size,
                });
                sections.push(SectionUsage {
                    name: ".data".to_string(),
                    address: 0x2000_0000,
                    size: data_size,
                });
                sections.push(SectionUsage {
                    name: ".bss".to_string(),
                    address: 0x2000_0000,
                    size: bss_size,
                });
                break;
            }
        }
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = r#"MEMORY
{
  FLASH (rx) : ORIGIN = 0x08000000, LENGTH = 64K
  RAM (rwx)  : ORIGIN = 0x20000000, LENGTH = 20K
}
SECTIONS
{
  .text : { *(.text*) } > FLASH
  .data : { *(.data*) } > RAM
}"#;

    #[test]
    fn parses_linker_memory_regions() {
        let regions = parse_linker_memory(SCRIPT);
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].name, "FLASH");
        assert_eq!(regions[0].origin, 0x0800_0000);
        assert_eq!(regions[0].length, 64 * 1024);
        assert_eq!(regions[0].attributes, "rx");
        assert_eq!(regions[1].name, "RAM");
        assert_eq!(regions[1].origin, 0x2000_0000);
        assert_eq!(regions[1].length, 20 * 1024);
    }

    #[test]
    fn assigns_sections_to_regions() {
        let mut regions = parse_linker_memory(SCRIPT);
        let sections = vec![
            SectionUsage {
                name: ".text".to_string(),
                address: 0x0800_0000,
                size: 0x2000,
            },
            SectionUsage {
                name: ".data".to_string(),
                address: 0x2000_0000,
                size: 0x100,
            },
            SectionUsage {
                name: ".bss".to_string(),
                address: 0x2000_0100,
                size: 0x50,
            },
        ];
        assign_sections(&mut regions, &sections);
        assert_eq!(regions[0].used, 0x2000);
        assert_eq!(regions[1].used, 0x150);
    }

    #[test]
    fn parses_size_report() {
        let text = "   text    data     bss     dec     hex filename\n  32768     512    2048   35328   8a00 app.elf\n";
        let sections = parse_size_report(text);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].name, ".text");
        assert_eq!(sections[0].size, 32768);
    }
}
