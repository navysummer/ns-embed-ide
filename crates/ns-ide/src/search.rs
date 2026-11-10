//! Project-wide search (F14 全局文件搜索).
//!
//! Searches file contents (case-insensitive substring) and file names under the
//! project root, skipping VCS/build directories and binary files.

use std::fs;
use std::path::{Path, PathBuf};

pub const MAX_MATCHES: usize = 200;
pub const MAX_FILE_SIZE: u64 = 1_048_576;
const SKIP_DIRS: &[&str] = &[".git", "build", "target", "node_modules", ".ns-ide"];
const BINARY_EXTENSIONS: &[&str] = &[
    "elf", "axf", "hex", "bin", "o", "a", "d", "rlib", "dylib", "so", "exe", "pdb",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchOrigin {
    /// A file whose relative path matches the query.
    FileName,
    /// A line inside a file whose contents match the query.
    Content,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    /// Path relative to the project root (forward slashes).
    pub path: String,
    /// 1-based line, 1 for file-name matches.
    pub line: usize,
    /// The matching line (trimmed, capped), empty for file-name matches.
    pub line_text: String,
    pub origin: MatchOrigin,
}

pub fn search_project(root: &Path, query: &str, limit: usize) -> Vec<SearchMatch> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut matches = Vec::new();
    walk(root, root, &needle, limit, &mut matches);
    matches
}

fn walk(
    dir: &Path,
    root: &Path,
    needle: &str,
    limit: usize,
    matches: &mut Vec<SearchMatch>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect();
    children.sort();

    for path in children {
        if matches.len() >= limit {
            return;
        }
        if path.is_dir() {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk(&path, root, needle, limit, matches);
            continue;
        }

        let relative = relative_path(root, &path);
        if relative.to_lowercase().contains(needle) {
            matches.push(SearchMatch {
                path: relative.clone(),
                line: 1,
                line_text: String::new(),
                origin: MatchOrigin::FileName,
            });
            if matches.len() >= limit {
                return;
            }
        }

        if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
            if BINARY_EXTENSIONS
                .iter()
                .any(|binary| extension.eq_ignore_ascii_case(binary))
            {
                continue;
            }
        }
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_FILE_SIZE {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            if matches.len() >= limit {
                return;
            }
            if line.to_lowercase().contains(needle) {
                let line_text: String = line.trim_end().chars().take(240).collect();
                matches.push(SearchMatch {
                    path: relative.clone(),
                    line: index + 1,
                    line_text,
                    origin: MatchOrigin::Content,
                });
            }
        }
    }
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Collect every source file under `root` (relative paths, forward slashes),
/// skipping VCS/build directories, hidden entries and binary files. Used by
/// Quick Open to enumerate the project's files for fuzzy filtering.
pub fn collect_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_walk(root, root, &mut files);
    files
}

fn collect_walk(dir: &Path, root: &Path, files: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect();
    children.sort();
    for path in children {
        if path.is_dir() {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            collect_walk(&path, root, files);
            continue;
        }
        if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
            if BINARY_EXTENSIONS
                .iter()
                .any(|binary| extension.eq_ignore_ascii_case(binary))
            {
                continue;
            }
        }
        files.push(relative_path(root, &path));
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn write_fixture(dir: &Path) {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("build")).unwrap();
        fs::write(dir.join("src/main.c"), "int main(void) {\n    ticks++;\n    return 0;\n}\n").unwrap();
        fs::write(dir.join("src/other.c"), "int other(void) { return 42; }\n").unwrap();
        fs::write(dir.join("build/generated.c"), "int generated_marker;\n").unwrap();
        fs::write(dir.join("data.bin"), [0xff, 0xd8, 0x00, 0x01]).unwrap();
        fs::write(dir.join("README.md"), "ticks and ticks again\n").unwrap();
    }

    fn fixture_root(test_name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ns-search-test-{}-{test_name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        write_fixture(&root);
        root
    }

    #[test]
    fn finds_content_matches_case_insensitively() {
        let root = fixture_root("finds_content_matches_case_insensitively");
        let matches = search_project(&root, "TICKS", 100);
        let contents: Vec<_> = matches
            .iter()
            .filter(|m| m.origin == MatchOrigin::Content)
            .collect();
        assert_eq!(contents.len(), 2, "main.c x1, README.md x1: {contents:?}");
        assert!(contents.iter().all(|m| m.path != "build/generated.c"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn skips_build_directories() {
        let root = fixture_root("skips_build_directories");
        let matches = search_project(&root, "generated_marker", 100);
        assert!(matches.iter().all(|m| !m.path.contains("build/")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn finds_file_name_matches() {
        let root = fixture_root("finds_file_name_matches");
        let matches = search_project(&root, "main.c", 100);
        assert!(matches.iter().any(|m| {
            m.origin == MatchOrigin::FileName && m.path == "src/main.c" && m.line == 1
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn respects_the_limit() {
        let root = fixture_root("respects_the_limit");
        let matches = search_project(&root, "ticks", 2);
        assert_eq!(matches.len(), 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn empty_query_returns_nothing() {
        let root = fixture_root("empty_query_returns_nothing");
        assert!(search_project(&root, "   ", 100).is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn binary_files_match_by_name_but_never_by_content() {
        let root = fixture_root("binary_files_match_by_name_but_never_by_content");
        let matches = search_project(&root, "data", 100);
        assert!(matches.iter().all(|m| !(m.path.ends_with("data.bin") && m.origin == MatchOrigin::Content)));
        let _ = fs::remove_dir_all(root);
    }

    /// Quick Open's flat file list includes source files, excludes build/VCS
    /// directories and binary files, and reports forward-slash relative paths.
    #[test]
    fn collect_files_lists_sources_only() {
        let root = fixture_root("collect_files_lists_sources_only");
        let files = collect_files(&root);
        assert!(files.contains(&"src/main.c".to_string()));
        assert!(files.contains(&"src/other.c".to_string()));
        assert!(files.contains(&"README.md".to_string()));
        assert!(
            !files.iter().any(|f| f.starts_with("build/")),
            "build dir excluded: {files:?}"
        );
        assert!(
            !files.iter().any(|f| f.ends_with("data.bin")),
            "binary files excluded: {files:?}"
        );
        let _ = fs::remove_dir_all(root);
    }
}
