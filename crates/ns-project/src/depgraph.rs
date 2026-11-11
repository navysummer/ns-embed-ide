//! Project-level include dependency graph.
//!
//! Scans a project's source files for `#include` directives, resolves
//! project-local includes (quoted, or angle includes that exist under the
//! project root) to project files, and exposes the resulting directed graph so
//! the IDE can render a dependency view.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The kind of an include directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncludeKind {
    /// `#include "local.h"` — resolved against the including file's directory
    /// and the project's include paths.
    Quotes,
    /// `#include <system.h>` — resolved against the project root / include dirs.
    Angle,
}

/// One node in the dependency graph (a project file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepNode {
    /// Project-relative path (forward slashes).
    pub path: String,
    /// Direct dependencies (files this file includes).
    pub deps: Vec<String>,
    /// Files that include this file.
    pub dependents: Vec<String>,
}

/// The whole include dependency graph.
#[derive(Debug, Clone, Default)]
pub struct IncludeGraph {
    /// node path -> node, keyed by project-relative path.
    pub nodes: BTreeMap<String, DepNode>,
}

impl IncludeGraph {
    /// Whether the graph has any edges.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Files that include `path` transitively (breadth-first), excluding itself.
    pub fn transitive_dependents(&self, path: &str) -> Vec<String> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut queue: Vec<String> = self
            .nodes
            .get(path)
            .map(|node| node.dependents.clone())
            .unwrap_or_default();
        let mut order: Vec<String> = Vec::new();
        while let Some(current) = queue.pop() {
            if seen.insert(current.clone()) {
                order.push(current.clone());
                if let Some(node) = self.nodes.get(&current) {
                    for dependent in &node.dependents {
                        if !seen.contains(dependent) {
                            queue.push(dependent.clone());
                        }
                    }
                }
            }
        }
        order
    }
 }
 
/// A positioned node for the graphical DAG rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagNode {
    /// Project-relative path (forward slashes).
    pub path: String,
    /// Layered column index (0 = leftmost/root layer).
    pub column: usize,
    /// Row index within its column (0 = top).
    pub row: usize,
}

/// An edge between two positioned nodes (from source to target).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagEdge {
    pub from: String,
    pub to: String,
}

/// A layered DAG layout: nodes grouped into columns by their longest path
/// from a source (a file with no dependencies), so edges flow left→right.
/// This is the layout used by the graphical dependency view.
pub fn dag_layout(graph: &IncludeGraph) -> (Vec<DagNode>, Vec<DagEdge>, Vec<Vec<String>>) {
    // Column of each node = longest path from any root (node with no deps),
    // computed with memoized DFS. Nodes with no dependents sit leftmost.
    let mut column_of: BTreeMap<String, usize> = BTreeMap::new();
    fn column(
        path: &str,
        graph: &IncludeGraph,
        column_of: &mut BTreeMap<String, usize>,
        visiting: &mut BTreeSet<String>,
    ) -> usize {
        if let Some(&c) = column_of.get(path) {
            return c;
        }
        if !visiting.insert(path.to_string()) {
            return 0; // cycle guard
        }
        let deps = graph
            .nodes
            .get(path)
            .map(|node| node.deps.clone())
            .unwrap_or_default();
        let depth = deps
            .iter()
            .map(|dep| column(dep, graph, column_of, visiting) + 1)
            .max()
            .unwrap_or(0);
        visiting.remove(path);
        column_of.insert(path.to_string(), depth);
        depth
    }

    let mut visiting = BTreeSet::new();
    let paths: Vec<String> = graph.nodes.keys().cloned().collect();
    for path in &paths {
        column(path, graph, &mut column_of, &mut visiting);
    }

    // Group into columns, preserving a stable order by path.
    let mut columns: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (path, depth) in &column_of {
        columns.entry(*depth).or_default().push(path.clone());
    }
    let max_column = columns.keys().next_back().copied().unwrap_or(0);
    let mut ordered_columns: Vec<Vec<String>> = Vec::new();
    for column_index in 0..=max_column {
        let mut members = columns.remove(&column_index).unwrap_or_default();
        members.sort();
        ordered_columns.push(members);
    }

    // Build positioned nodes (row = index within column) and all edges.
    let mut nodes = Vec::new();
    for (column_index, members) in ordered_columns.iter().enumerate() {
        for (row, path) in members.iter().enumerate() {
            nodes.push(DagNode {
                path: path.clone(),
                column: column_index,
                row,
            });
        }
    }
    let mut edges = Vec::new();
    for node in graph.nodes.values() {
        for dep in &node.deps {
            edges.push(DagEdge {
                from: node.path.clone(),
                to: dep.clone(),
            });
        }
    }
    (nodes, edges, ordered_columns)
}

 /// Parse a single `#include` line. Returns `(kind, path)` for a valid directive.
pub fn parse_include(line: &str) -> Option<(IncludeKind, String)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let after = trimmed[1..].trim_start();
    let rest = after.strip_prefix("include")?;
    let rest = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('\t'))?;
    let rest = rest.trim_start();
    if let Some(path) = rest.strip_prefix('"') {
        let end = path.find('"')?;
        return Some((IncludeKind::Quotes, path[..end].to_string()));
    }
    if let Some(path) = rest.strip_prefix('<') {
        let end = path.find('>')?;
        return Some((IncludeKind::Angle, path[..end].to_string()));
    }
    None
}

/// Resolve an include target to a project-relative path if it exists in the
/// project. `base_dir` is the directory of the including file (project-relative
/// or absolute); `include_dirs` are the project's configured include paths.
fn resolve_include(
    project_root: &Path,
    base_dir: &Path,
    include_dirs: &[String],
    include: &str,
) -> Option<String> {
     let mut candidates: Vec<PathBuf> = Vec::new();
     // Quoted includes resolve relative to the including file first.
    candidates.push(project_root.join(base_dir).join(include));
     // Then against the project root and each configured include dir.
     candidates.push(project_root.join(include));
    for dir in include_dirs {
        candidates.push(project_root.join(dir).join(include));
    }
     for candidate in candidates {
         if candidate.is_file() {
             if let Ok(relative) = candidate.strip_prefix(project_root) {
                 return Some(normalize_path(relative));
             }
         }
     }
    // An include that cannot be resolved to a project file is still recorded as
    // a named node (system header or generated file) so the graph shows it.
    if include.contains('.') {
        Some(include.to_string())
    } else {
        None
    }
}

/// Normalize a path to forward slashes.
fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Build the include dependency graph for a project's source files.
///
/// Scans every C/C++ source under `src/` (and any file passed in `extra`),
/// parses `#include` directives, resolves project-local includes, and records
/// both directions. `include_dirs` are the project's configured include paths
/// (from `[compiler] includes`).
pub fn build_include_graph(
    project_root: &Path,
    sources: &[PathBuf],
    include_dirs: &[String],
) -> IncludeGraph {
    let mut graph = IncludeGraph::default();
    // First pass: ensure every source is a node.
    for source in sources {
        let relative = source
            .strip_prefix(project_root)
            .unwrap_or(source)
            .to_path_buf();
        let key = normalize_path(&relative);
        graph.nodes.entry(key).or_insert_with(|| DepNode {
            path: String::new(),
            deps: Vec::new(),
            dependents: Vec::new(),
        });
    }
    // Second pass: parse includes and wire edges.
    for source in sources {
        let relative = source.strip_prefix(project_root).unwrap_or(source);
        let key = normalize_path(relative);
        let base_dir = relative.parent().unwrap_or(Path::new("")).to_path_buf();
        let text = match std::fs::read_to_string(source) {
            Ok(text) => text,
            Err(_) => continue,
        };
        let mut deps: BTreeSet<String> = BTreeSet::new();
        for line in text.lines() {
            let Some((_kind, include)) = parse_include(line) else {
                continue;
            };
            if let Some(resolved) =
                resolve_include(project_root, &base_dir, include_dirs, &include)
            {
                if resolved != key {
                    deps.insert(resolved);
                }
            }
        }
        let node = graph
            .nodes
            .entry(key.clone())
            .or_insert_with(|| DepNode {
                path: String::new(),
                deps: Vec::new(),
                dependents: Vec::new(),
            });
        node.path = key.clone();
        node.deps = deps.iter().cloned().collect();
        node.deps.sort();
    }
    // Third pass: wire dependents.
    let keys: Vec<String> = graph.nodes.keys().cloned().collect();
    for key in &keys {
        let deps = graph.nodes[key].deps.clone();
        for dep in deps {
            let dependent_node = graph
                .nodes
                .entry(dep.clone())
                .or_insert_with(|| DepNode {
                    path: dep.clone(),
                    deps: Vec::new(),
                    dependents: Vec::new(),
                });
            dependent_node.path = dep.clone();
            if !dependent_node.dependents.contains(key) {
                dependent_node.dependents.push(key.clone());
            }
            dependent_node.dependents.sort();
        }
    }
    graph
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, contents: &str) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn parses_include_directives() {
        assert_eq!(
            parse_include(r#"#include "foo.h""#),
            Some((IncludeKind::Quotes, "foo.h".to_string()))
        );
        assert_eq!(
            parse_include(r#"#  include <sys/types.h>"#),
            Some((IncludeKind::Angle, "sys/types.h".to_string()))
        );
        assert_eq!(parse_include("int main() {}"), None);
        assert_eq!(parse_include("#includex \"a.h\""), None);
        assert_eq!(parse_include(r#"#include "unterminated"#), None);
    }

    #[test]
    fn builds_graph_and_resolves_local_includes() {
        let dir = std::env::temp_dir().join(format!("ns-dep-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("include")).unwrap();
        write(&dir, "src/main.c", "#include \"app.h\"\n#include <board.h>\n");
        write(&dir, "src/app.h", "#pragma once\n");
        write(&dir, "include/board.h", "#pragma once\n");
        let sources = vec![dir.join("src/main.c"), dir.join("src/app.h")];
        let include_dirs = vec!["include".to_string()];
        let graph = build_include_graph(&dir, &sources, &include_dirs);
         let main = graph.nodes.get("src/main.c").unwrap();
         assert!(main.deps.contains(&"src/app.h".to_string()));
         assert!(main.deps.contains(&"include/board.h".to_string()));
        // app.h has main.c as a dependent.
        let app = graph.nodes.get("src/app.h").unwrap();
        assert!(app.dependents.contains(&"src/main.c".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transitive_dependents_walks_the_chain() {
        let dir = std::env::temp_dir().join(format!("ns-dep-trans-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        write(&dir, "src/base.h", "#pragma once\n");
        write(&dir, "src/mid.h", "#include \"base.h\"\n");
        write(&dir, "src/main.c", "#include \"mid.h\"\n");
        let sources = vec![
            dir.join("src/base.h"),
            dir.join("src/mid.h"),
            dir.join("src/main.c"),
        ];
        let graph = build_include_graph(&dir, &sources, &[]);
        let dependents = graph.transitive_dependents("src/base.h");
        assert!(dependents.contains(&"src/mid.h".to_string()));
        assert!(dependents.contains(&"src/main.c".to_string()));
         let _ = std::fs::remove_dir_all(&dir);
     }

    #[test]
    fn dag_layout_layers_by_longest_dependency_path() {
        // main.c -> mid.h -> base.h  (a 2-edge chain, so base.h is leftmost).
        let dir = std::env::temp_dir().join(format!("ns-dep-dag-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        write(&dir, "src/base.h", "#pragma once\n");
        write(&dir, "src/mid.h", "#include \"base.h\"\n");
        write(&dir, "src/main.c", "#include \"mid.h\"\n");
        let sources = vec![
            dir.join("src/base.h"),
            dir.join("src/mid.h"),
            dir.join("src/main.c"),
        ];
        let graph = build_include_graph(&dir, &sources, &[]);
        let (nodes, edges, columns) = dag_layout(&graph);

        // Three nodes in three columns (base.h col 0, mid.h col 1, main.c col 2).
        assert_eq!(nodes.len(), 3);
        assert_eq!(columns.len(), 3);
        assert_eq!(columns[0], vec!["src/base.h".to_string()]);
        assert_eq!(columns[1], vec!["src/mid.h".to_string()]);
        assert_eq!(columns[2], vec!["src/main.c".to_string()]);

        // Edges flow from a node to its includes (main->mid, mid->base).
        assert!(edges.contains(&DagEdge {
            from: "src/main.c".to_string(),
            to: "src/mid.h".to_string(),
        }));
        assert!(edges.contains(&DagEdge {
            from: "src/mid.h".to_string(),
            to: "src/base.h".to_string(),
        }));
        let _ = std::fs::remove_dir_all(&dir);
    }
 }
