//! Import existing embedded IDE projects into a ns-embed-ide project.
//!
//! Each parser converts a vendor project file into a [`ProjectConfig`] plus a
//! list of source files (relative to the project root) that should be copied
//! into `src/`. The IDE entry point ("导入工程") picks the parser by file
//! extension, then writes `project.toml` and the imported sources.
//!
//! Supported formats:
//! * IAR Embedded Workbench `.ewp` (XML; source files + include dirs).
//! * Keil MDK `.uvprojx` (XML; source groups + include paths + target MCU).
//! * STM32CubeMX `.ioc` (INI; MCU name, per-peripheral config).
//!
//! Anything the parser cannot map is reported honestly (unknown fields are
//! skipped, but the import still succeeds with a best-effort config).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::project::{
    BuildSection, CompilerSection, DebuggerSection, FlasherSection, ProjectConfig, ProjectSection,
    SimSection,
};
use crate::Language;

 /// The result of parsing a vendor project file.
 #[derive(Debug, Clone)]
 pub struct ImportedProject {
     /// Best-effort `project.toml` config derived from the vendor file.
     pub config: ProjectConfig,
    /// Absolute paths of source files found in the vendor project.
     pub sources: Vec<PathBuf>,
     /// Human-readable notes about what was inferred / left at defaults.
     pub notes: Vec<String>,
 }

/// Detect the vendor format from a project-file extension and parse it.
pub fn parse_import(path: &Path) -> Result<ImportedProject> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "ewp" => parse_ewp(path),
        "uvprojx" => parse_uvprojx(path),
        "ioc" => parse_ioc(path),
        _ => anyhow::bail!(
            "无法识别的工程文件 `{}`：支持 .ewp (IAR)、.uvprojx (Keil)、.ioc (STM32CubeMX)",
            path.display()
        ),
    }
}

/// Extract the MCU target name from a vendor file's hints (best effort).
pub fn detect_target(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let lower = text.to_lowercase();
    // Common Cortex-M parts appear verbatim (stm32f103c8, stm32f407vg, nrf52840).
    for key in [
        "stm32f103c8",
        "stm32f407vg",
        "stm32g431rb",
        "stm32f103",
        "stm32f407",
        "nrf52840",
        "rp2040",
        "esp32c3",
        "esp32",
    ] {
        if lower.contains(key) {
            return Some(key.to_string());
        }
    }
    None
}

/// A simple non-allocating XML tag scanner used by the IAR/Keil parsers.
/// Returns (tag, inner) for the first tag matching `name` at depth 1 after a
/// given byte offset. Used by the parsers to walk `<name>...</name>` blocks.
 fn find_tag<'a>(text: &'a str, name: &str, from: usize) -> Option<(usize, &'a str)> {
     let open = format!("<{name}");
     let close = format!("</{name}>");
     let mut pos = from;
     while pos < text.len() {
         let rest = &text[pos..];
         let Some(start) = rest.find(&open) else { return None };
         let abs_start = pos + start;
         let after_open = abs_start + open.len();
        // Require a tag boundary after the name: `<File>` matches, but
        // `<FilePath>` does not (the next char must be `>`, space, `/`, or end).
        let boundary = text[after_open..].chars().next();
        if let Some(ch) = boundary {
            if ch != '>' && ch != ' ' && ch != '/' {
                pos = after_open;
                continue;
            }
        }
         // Skip self-closing tags (`<name/>`).
         let Some(gt) = text[after_open..].find('>') else {
             return None;
         };
         let abs_gt = after_open + gt;
         if text[abs_gt - 1..abs_gt].as_bytes().first() == Some(&b'/') {
             pos = abs_gt + 1;
             continue;
         }
         // Find the matching close tag from the end of the open tag.
         let Some(end) = text[abs_gt..].find(&close) else { return None };
         let inner_start = abs_gt + 1;
         let inner_end = abs_gt + end;
         return Some((abs_gt + 1, &text[inner_start..inner_end]));
     }
     None
 }

/// Walk every `inner` of a repeated tag (e.g. all `<file>` entries).
fn find_all_tags<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some((after, inner)) = find_tag(text, name, pos) {
        out.push(inner);
        pos = after;
    }
    out
}

 /// Extract the text of a child element within `inner`, e.g. `<name>foo</name>`.
 fn child_text<'a>(inner: &'a str, child: &str) -> Option<&'a str> {
    let (_, text) = find_tag(inner, child, 0)?;
    Some(text.trim())
}

 /// Normalize a backslash path to forward slashes.
 fn norm_path(raw: &str) -> String {
     raw.trim().replace('\\', "/")
 }

/// Strip a `$PROJ_DIR$` prefix and leading `./` / `/` from a normalized path.
/// `$PROJ_DIR$` is IAR's project-directory variable; Keil uses `..\` relative
/// paths that are kept (lexical_join resolves them).
fn strip_proj_dir(normalized: &str) -> String {
    let mut s = normalized;
    if let Some(rest) = s.strip_prefix("$PROJ_DIR$") {
        s = rest;
    }
    let s = s.trim_start_matches("./");
    let s = s.trim_start_matches('/');
    s.to_string()
}


/// Strip leading `../` / `..\` segments from a normalized path, so a vendor
/// include dir like `..\include` becomes `include` (relative to the project
/// root where the imported sources land).
fn strip_leading_dotdot(normalized: &str) -> String {
    let mut s = normalized;
    loop {
        if let Some(rest) = s.strip_prefix("../") {
            s = rest;
        } else if let Some(rest) = s.strip_prefix("..\\") {
            s = rest;
        } else if let Some(rest) = s.strip_prefix("./") {
            s = rest;
        } else {
            break;
        }
    }
    s.trim_start_matches('/').to_string()
}
/// Join `base` with a possibly-`..`-containing relative path, normalizing the
/// result lexically (no filesystem access). The result is absolute when `base`
/// is absolute.
fn lexical_join(base: &Path, rel: &str) -> PathBuf {
    let mut result = base.to_path_buf();
    for component in Path::new(rel).components() {
        match component {
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => result.push(part),
            _ => {}
        }
    }
    result
}

/// Parse an IAR Embedded Workbench `.ewp` file.
fn parse_ewp(path: &Path) -> Result<ImportedProject> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取 {} 失败", path.display()))?;
    let mut config = base_config();

    let mut notes = Vec::new();
    let target = detect_target(path);
    if let Some(target) = &target {
        config.project.target = target.clone();
        config.project.svd = target.clone();
    } else {
        notes.push("未识别目标 MCU（project.toml 的 target 留空，可在项目设置中选择）".to_string());
    }

    // IAR projects are XML: sources live under `<file>` with a `<name>` child,
    // include dirs under `<option><state>...` with `$PROJ_DIR$` paths.
     let root = path.parent().unwrap_or(Path::new("."));
     let mut sources = Vec::new();
     for file in find_all_tags(&text, "file") {
         if let Some(name) = child_text(file, "name") {
             let normalized = norm_path(name);
             let cleaned = strip_proj_dir(&normalized);
             if is_source(&cleaned) {
                 let rel = lexical_join(root, &cleaned);
                 if rel.exists() {
                     sources.push(rel);
                 }
             }
         }
     }
 
     // Include paths: `<state>$PROJ_DIR$\include</state>`.
    let mut includes = Vec::new();
    for state in find_all_tags(&text, "state") {
        let value = norm_path(state);
         if value.starts_with("$PROJ_DIR$") || value.starts_with("..") || value.starts_with("./") {
            let cleaned = strip_leading_dotdot(&strip_proj_dir(&value));
            if !cleaned.is_empty() && !includes.contains(&cleaned) {
                includes.push(cleaned);
             }
         }

     }
    if !includes.is_empty() {
        config.compiler.includes = includes;
    }

    notes.push("IAR .ewp 已解析：编译器/调试器/烧写器需在 project.toml 选择可用工具（如 arm-none-eabi-gcc + openocd）".to_string());
    let mut imported = ImportedProject {
        config,
        sources,
        notes,
    };
    imported.config.build.system = "auto".to_string();
    Ok(imported)
}

/// Parse a Keil MDK `.uvprojx` file.
fn parse_uvprojx(path: &Path) -> Result<ImportedProject> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取 {} 失败", path.display()))?;
    let mut config = base_config();
    let mut notes = Vec::new();

    let target = detect_target(path);
    if let Some(target) = &target {
        config.project.target = target.clone();
        config.project.svd = target.clone();
    } else {
        notes.push("未识别目标 MCU（project.toml 的 target 留空，可在项目设置中选择）".to_string());
    }

    let root = path.parent().unwrap_or(Path::new("."));
    let mut sources = Vec::new();
    // Keil groups sources as `<File><FileName>...</FileName><FilePath>...</FilePath></File>`.
    for file in find_all_tags(&text, "File") {
        let file_path = child_text(file, "FilePath").or_else(|| child_text(file, "FileName"));
        if let Some(file_path) = file_path {
             let normalized = norm_path(file_path);
            let cleaned = strip_proj_dir(&normalized);
            if is_source(&cleaned) {
                let rel = lexical_join(root, &cleaned);
                 if rel.exists() {
                    sources.push(rel);
                 }
             }
        }
    }

    // Include paths in `<IncludePath>...</IncludePath>` (semicolon-separated).
    let mut includes = Vec::new();
    for inc in find_all_tags(&text, "IncludePath") {
         for part in inc.split(';') {
            let normalized = norm_path(part);
            let cleaned = strip_leading_dotdot(&normalized);
            if !cleaned.is_empty() && !includes.contains(&cleaned) {
                includes.push(cleaned);
             }
         }
     }
     if !includes.is_empty() {
        config.compiler.includes = includes;
     }

    notes.push("Keil .uvprojx 已解析：编译器/调试器/烧写器需在 project.toml 选择可用工具（如 arm-none-eabi-gcc + openocd）".to_string());
    Ok(ImportedProject {
        config,
        sources,
        notes,
    })
}

/// Parse an STM32CubeMX `.ioc` file (INI-like key=value lines).
fn parse_ioc(path: &Path) -> Result<ImportedProject> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取 {} 失败", path.display()))?;
    let mut config = base_config();
    let mut notes = Vec::new();

    let mut mcu: Option<String> = None;
    let mut peripheral_lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim();
            if key == "Mcu.Name" || key == "Mcu.Family" {
                if value.contains("STM32") {
                    mcu = Some(value.to_lowercase().replace('-', ""));
                }
            } else if key.starts_with("Mcu.IP") && !value.is_empty() {
                peripheral_lines.push(value.to_string());
            }
        }
    }

    if let Some(mcu) = &mcu {
        // Map the CubeMX name (e.g. STM32F103C8Tx) to a catalog-friendly target.
        let target = mcu_to_target(mcu);
        config.project.target = target.clone();
        config.project.svd = target;
    } else {
        notes.push("未识别 MCU（.ioc 的 Mcu.Name 缺失；target 留空）".to_string());
    }

    if !peripheral_lines.is_empty() {
        notes.push(format!(
            "识别到 {} 个外设配置行（Mcu.IP），可在「外设生成」中用于生成初始化代码",
            peripheral_lines.len()
        ));
    }

    notes.push("STM32CubeMX .ioc 已解析：CubeMX 生成的工程建议用「导入工程」后重新编译（工具链为 arm-none-eabi-gcc + openocd）".to_string());
    Ok(ImportedProject {
        config,
        sources: Vec::new(),
        notes,
    })
}

/// Map a CubeMX MCU name (e.g. `STM32F103C8Tx`) to a ns-device catalog key.
fn mcu_to_target(mcu: &str) -> String {
    let lower = mcu.to_lowercase();
    for key in [
        "stm32f103c8",
        "stm32f407vg",
        "stm32g431rb",
        "stm32f103",
        "stm32f407",
        "nrf52840",
        "rp2040",
        "esp32c3",
        "esp32",
    ] {
        if lower.contains(key) {
            return key.to_string();
        }
    }
    // Best effort: strip the package suffix (Tx/...).
    lower.trim_end_matches(|c: char| c.is_ascii_alphabetic() && c.is_ascii_lowercase())
        .to_string()
}

/// Whether a path looks like a source file (C/C++/assembly).
fn is_source(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".c")
        || lower.ends_with(".cpp")
        || lower.ends_with(".cc")
        || lower.ends_with(".h")
        || lower.ends_with(".s")
        || lower.ends_with(".S")
        || lower.ends_with(".asm")
}

/// A base `ProjectConfig` for an imported C/C++ project.
fn base_config() -> ProjectConfig {
    let mut config = ProjectConfig {
        project: ProjectSection {
            name: String::new(),
            version: "0.1.0".to_string(),
            language: Language::C,
            target: String::new(),
            svd: String::new(),
            compiler: "arm-none-eabi-gcc".to_string(),
            simulator: "ns-sim".to_string(),
            debugger: "gdb".to_string(),
            flasher: "openocd".to_string(),
        },
        compiler: CompilerSection::default(),
        debugger: DebuggerSection::default(),
        flasher: FlasherSection::default(),
        build: BuildSection::default(),
        sim: SimSection::default(),
        launch: Default::default(),
        targets: Vec::new(),
    };
    // Imported projects use the MCU toolchain by default; keep C std at c11.
    config.compiler.std = "c11".to_string();
    config.project.name = "imported".to_string();
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iar_ewp_sources_includes_and_target() {
        let dir = std::env::temp_dir().join(format!("ns-import-ewp-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("include")).unwrap();
        std::fs::write(dir.join("src/main.c"), "int main(void){return 0;}\n").unwrap();
        std::fs::write(dir.join("include/board.h"), "#pragma once\n").unwrap();
        let ewp = r#"<?xml version="1.0" encoding="UTF-8"?>
<project>
  <file>
    <name>$PROJ_DIR$\src\main.c</name>
  </file>
  <file>
    <name>$PROJ_DIR$\include\board.h</name>
  </file>
  <option>
    <state>$PROJ_DIR$\include</state>
  </option>
</project>"#;
        let ewp_path = dir.join("app.ewp");
        std::fs::write(&ewp_path, ewp).unwrap();

        let imported = parse_import(&ewp_path).unwrap();
        assert!(imported.sources.iter().any(|s| s.ends_with("src/main.c")));
        assert!(imported.config.compiler.includes.contains(&"include".to_string()));
        assert_eq!(imported.config.project.compiler, "arm-none-eabi-gcc");
        assert_eq!(imported.config.build.system, "auto");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_keil_uvprojx_sources_and_includes() {
         let dir = std::env::temp_dir().join(format!("ns-import-keil-{}", std::process::id()));
         std::fs::create_dir_all(dir.join("MDK-ARM")).unwrap();
         std::fs::create_dir_all(dir.join("src")).unwrap();
         std::fs::write(dir.join("src/app.c"), "void app(void){}\n").unwrap();


         let uvprojx = r#"<?xml version="1.0" encoding="UTF-8"?>
<Project>
  <Groups>
    <Group>
      <Files>
        <File>
          <FileName>app.c</FileName>
          <FilePath>..\src\app.c</FilePath>
        </File>
      </Files>
    </Group>
  </Groups>
  <Targets>
    <Target>
      <TargetOption>
        <IncludePath>..\include;..\drivers</IncludePath>
      </TargetOption>
    </Target>
  </Targets>
</Project>"#;
        let path = dir.join("MDK-ARM").join("app.uvprojx");
        std::fs::write(&path, uvprojx).unwrap();

        let imported = parse_import(&path).unwrap();
        assert!(imported.sources.iter().any(|s| s.ends_with("src/app.c")));
        assert!(imported.config.compiler.includes.contains(&"include".to_string()));
        assert!(imported.config.compiler.includes.contains(&"drivers".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_ioc_mcu_and_peripherals() {
        let dir = std::env::temp_dir().join(format!("ns-import-ioc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ioc = "#MicroXplorer Configuration settings - do not modify\n\
Mcu.Name=STM32F103C8Tx\n\
Mcu.IP0=GPIO\n\
Mcu.IP1=USART1\n";
        let path = dir.join("app.ioc");
        std::fs::write(&path, ioc).unwrap();

        let imported = parse_import(&path).unwrap();
        assert_eq!(imported.config.project.target, "stm32f103c8");
        assert!(imported.notes.iter().any(|n| n.contains("2 个外设")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_target_from_text() {
        let dir = std::env::temp_dir().join(format!("ns-import-detect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.uvprojx");
        std::fs::write(&path, "<Device>STM32F407VGTx</Device>").unwrap();
        assert_eq!(detect_target(&path).as_deref(), Some("stm32f407vg"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_unknown_extension() {
        let dir = std::env::temp_dir().join(format!("ns-import-unknown-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "x").unwrap();
        assert!(parse_import(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
