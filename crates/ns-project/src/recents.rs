use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

const RECENTS_FILE: &str = "recent-projects.json";
const MAX_RECENTS: usize = 12;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentProject {
    pub path: PathBuf,
    pub name: String,
    /// Unix timestamp in seconds.
    pub opened_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RecentFile {
    #[serde(default)]
    projects: Vec<RecentProject>,
}

pub fn config_dir() -> Option<PathBuf> {
    ProjectDirs::from("dev", "nsummer", "ns-embed-ide")
        .map(|dirs| dirs.config_dir().to_path_buf())
}

pub fn load_recents() -> Vec<RecentProject> {
    let Some(path) = recents_path() else {
        return Vec::new();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut recents: Vec<RecentProject> = serde_json::from_str::<RecentFile>(&text)
        .map(|file| file.projects)
        .unwrap_or_default();
    recents.retain(|recent| recent.path.is_dir());
    recents.sort_by(|a, b| b.opened_at.cmp(&a.opened_at));
    recents
}

pub fn remember_project(root: &Path, name: &str) -> Result<()> {
    let Some(path) = recents_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建配置目录 {} 失败", parent.display()))?;
    }
    let mut projects = load_recents();
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    projects.retain(|recent| {
        recent.path.canonicalize().unwrap_or_else(|_| recent.path.clone()) != canonical
    });
    projects.insert(
        0,
        RecentProject {
            path: canonical,
            name: name.to_string(),
            opened_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0),
        },
    );
    projects.truncate(MAX_RECENTS);
    let contents = serde_json::to_string_pretty(&RecentFile { projects })
        .context("序列化最近项目失败")?;
    fs::write(&path, contents).with_context(|| format!("写入 {} 失败", path.display()))
}

/// Remove `root` from the recent-projects list (project deleted or renamed).
pub fn forget_project(root: &Path) -> Result<()> {
    let Some(path) = recents_path() else {
        return Ok(());
    };
    let projects = remove_from_recents(load_recents(), root);
    let contents = serde_json::to_string_pretty(&RecentFile { projects })
        .context("序列化最近项目失败")?;
    fs::write(&path, contents).with_context(|| format!("写入 {} 失败", path.display()))
}

/// Drop every recent entry whose canonical path equals `root`.
fn remove_from_recents(projects: Vec<RecentProject>, root: &Path) -> Vec<RecentProject> {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    projects
        .into_iter()
        .filter(|recent| {
            recent.path.canonicalize().unwrap_or_else(|_| recent.path.clone()) != canonical
        })
        .collect()
}

fn recents_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(RECENTS_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ns-recents-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let project = dir.join("project");
        fs::create_dir_all(&project).unwrap();

        let file = RecentFile {
            projects: vec![RecentProject {
                path: project,
                name: "demo".into(),
                opened_at: 42,
            }],
        };
        let text = serde_json::to_string(&file).unwrap();
        let decoded: RecentFile = serde_json::from_str(&text).unwrap();
        assert_eq!(decoded.projects.len(), 1);
        assert_eq!(decoded.projects[0].name, "demo");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn remove_from_recents_drops_matching_paths() {
        let dir = std::env::temp_dir().join(format!("ns-recents-rm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let keep = dir.join("keep");
        let drop = dir.join("drop");
        fs::create_dir_all(&keep).unwrap();
        fs::create_dir_all(&drop).unwrap();
        let projects = vec![
            RecentProject { path: keep.clone(), name: "keep".into(), opened_at: 1 },
            RecentProject { path: drop.clone(), name: "drop".into(), opened_at: 2 },
        ];
        let remaining = remove_from_recents(projects, &drop);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].name, "keep");
        // Removing a path not present is a no-op (still keeps everything).
        let again = remove_from_recents(remaining, &dir.join("none"));
        assert_eq!(again.len(), 1);
        let _ = fs::remove_dir_all(dir);
    }
}
