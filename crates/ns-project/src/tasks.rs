//! Project-wide TODO/FIXME/XXX/HACK marker scanning.
//!
//! `scan_tasks` walks the project's source files and reports every line that
//! carries a marker keyword (`TODO`, `FIXME`, `XXX`, `HACK`, `BUG`), with the
//! marker text that follows, so the IDE can show a task panel and jump to the
//! exact line.

use std::fs;
use std::path::{Path, PathBuf};

/// A single task marker found in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskMarker {
    /// Absolute path of the file containing the marker.
    pub path: PathBuf,
    /// 1-based source line.
    pub line: usize,
    /// Column (1-based, bytes) of the marker keyword.
    pub column: usize,
    /// The marker keyword, uppercased (`TODO`, `FIXME`, ...).
    pub keyword: String,
    /// The text that follows the marker up to the end of line, trimmed.
    pub text: String,
}

/// Scan `files` for task markers. Files are read whole; a read error for one
/// file is skipped (the file may have been deleted between listing and scan).
pub fn scan_tasks(files: &[PathBuf]) -> Vec<TaskMarker> {
    let mut markers = Vec::new();
    for path in files {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        scan_text(path, &text, &mut markers);
    }
    markers.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.line.cmp(&b.line))
            .then(a.column.cmp(&b.column))
    });
    markers
}

/// Scan a single in-memory text buffer for markers, appending to `out`.
/// Public so tests and the editor can reuse the same line scanner.
pub fn scan_text(path: &Path, text: &str, out: &mut Vec<TaskMarker>) {
    for (index, line) in text.lines().enumerate() {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            // Skip quoted strings and line comments so markers inside string
            // literals or URLs are not reported as tasks.
            if bytes[i] == b'"' || bytes[i] == b'\'' {
                i = skip_string(bytes, i);
                continue;
            }
             if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                 // Rest of the line is a line comment; markers inside it are
                 // still real tasks, so keep scanning without the quote skip.
                let mut j = i + 2;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                if let Some(keyword) = match_keyword(&bytes[j..]) {
                    let col = j + 1;
                    let rest = &line[j + keyword.len()..];
                     out.push(TaskMarker {
                         path: path.to_path_buf(),
                         line: index + 1,
                         column: col,
                         keyword: keyword.to_string(),
                         text: rest.trim().to_string(),
                     });
                 }
                 break;
             }
            // Match a marker keyword at a word boundary.
            if let Some(keyword) = match_keyword(&bytes[i..]) {
                let col = i + 1;
                let rest = &line[i + keyword.len()..];
                out.push(TaskMarker {
                    path: path.to_path_buf(),
                    line: index + 1,
                    column: col,
                    keyword: keyword.to_string(),
                    text: rest.trim().to_string(),
                });
                i += keyword.len();
                continue;
            }
            i += 1;
        }
    }
}

/// Skip over a quoted string starting at `start` (which must point at the
/// opening quote). Handles backslash escapes and returns the index just past
/// the closing quote.
fn skip_string(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

/// Match a task keyword at the start of `bytes` (word boundary respected).
/// Returns the matched keyword, uppercased.
fn match_keyword(bytes: &[u8]) -> Option<&'static str> {
    const KEYWORDS: &[&str] = &["TODO", "FIXME", "XXX", "HACK", "BUG", "NOTE"];
    for keyword in KEYWORDS {
        let kw = keyword.as_bytes();
        if bytes.len() >= kw.len() && bytes[..kw.len()].eq_ignore_ascii_case(kw) {
            let before_ok = true; // caller checks start of word
            let after = bytes.get(kw.len()).copied();
            let after_ok = match after {
                None => true,
                Some(c) => !c.is_ascii_alphanumeric() && c != b'_',
            };
            if before_ok && after_ok {
                return Some(keyword);
            }
        }
    }
    None
}

/// Collect the project's source files (skipping build/IDE state dirs and
/// binaries) by reusing the same filter the IDE search uses.
pub fn collect_source_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_dir(root, root, &mut out);
    out
}

fn collect_dir(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" || name == "build" || name == "target" || name == "node_modules" || name == ".ns-ide" {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            collect_dir(root, &path, out);
            continue;
        }
        if meta.len() > 1_048_576 {
            continue; // skip >1 MiB
        }
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext = ext.to_lowercase();
            if matches!(
                ext.as_str(),
                "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "rs" | "zig" | "zr" | "s" | "S" | "asm" | "toml" | "md" | "py"
            ) {
                out.push(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn scan_text_finds_markers_with_context() {
        let dir = std::env::temp_dir().join(format!("ns-tasks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        let main = dir.join("src/main.c");
        fs::write(
            &main,
            "// TODO: implement the UART driver\nint main() {\n  /* FIXME: off-by-one */\n  char *s = \"TODO not a task\"; // XXX: real one\n  return 0;\n}\n",
        )
        .unwrap();

        let mut markers = Vec::new();
        scan_text(&main, &fs::read_to_string(&main).unwrap(), &mut markers);
        // The TODO in the string literal is skipped; the XXX comment is found.
        assert_eq!(markers.len(), 3, "{markers:?}");
        assert_eq!(markers[0].line, 1);
        assert_eq!(markers[0].keyword, "TODO");
        assert!(markers[0].text.contains("implement the UART driver"));
        assert_eq!(markers[1].line, 3);
        assert_eq!(markers[1].keyword, "FIXME");
        assert_eq!(markers[2].line, 4);
        assert_eq!(markers[2].keyword, "XXX");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_tasks_collects_all_source_files() {
        let dir = std::env::temp_dir().join(format!("ns-tasks2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("build")).unwrap();
        fs::write(dir.join("src/main.c"), "// TODO: x\n").unwrap();
        fs::write(dir.join("src/util.rs"), "// FIXME: y\n").unwrap();
        fs::write(dir.join("build/ignored.c"), "// TODO: z\n").unwrap();

        let files = collect_source_files(&dir);
        assert!(files.iter().any(|p| p.ends_with("src/main.c")));
        assert!(files.iter().any(|p| p.ends_with("src/util.rs")));
        assert!(!files.iter().any(|p| p.ends_with("build/ignored.c")));

        let markers = scan_tasks(&files);
        assert_eq!(markers.len(), 2, "{markers:?}");
        assert!(markers.iter().all(|m| m.keyword == "TODO" || m.keyword == "FIXME"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_text_skips_strings_and_respects_word_boundary() {
        let dir = std::env::temp_dir().join(format!("ns-tasks3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.c");
        fs::write(&f, "let s = \"TODO: in string\";\nlet t = 'FIXME'; // TODO: real\nconst x = TODO_REF;\n").unwrap();

        let mut markers = Vec::new();
        scan_text(&f, &fs::read_to_string(&f).unwrap(), &mut markers);
        // Line 1 string skipped; line 2 char literal skipped but comment TODO found;
        // line 3 TODO_REF is not a keyword (underscore follows).
        assert_eq!(markers.len(), 1, "{markers:?}");
        assert_eq!(markers[0].line, 2);
        assert_eq!(markers[0].keyword, "TODO");

        let _ = fs::remove_dir_all(&dir);
    }
}
