//! Minimal Git integration (F14 版本控制).
//!
//! Wraps the `git` CLI (available via `git` on PATH) with typed results for
//! the repository status bar and the Git panel. Every command runs in the
//! project root and reports errors as strings so the UI can show them without
//! panicking.

use std::path::Path;
use std::process::Command;

/// One changed path, as reported by `git status --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitChange {
    /// Path relative to the repo root, forward slashes.
    pub path: String,
    /// Index status letter (space when not staged).
    pub staged: char,
    /// Work-tree status letter (`?` for untracked, `!` for ignored).
    pub worktree: char,
}

impl GitChange {
    pub fn is_untracked(&self) -> bool {
        self.worktree == '?'
    }
    pub fn is_deleted(&self) -> bool {
        self.staged == 'D' || self.worktree == 'D'
    }
    /// Short label used in the panel, e.g. "M" / "A" / "??".
    pub fn label(&self) -> String {
        if self.is_untracked() {
            "??".to_string()
        } else if self.staged != ' ' {
            format!("{}", self.staged)
        } else {
            format!("{}", self.worktree)
        }
    }
}

/// Snapshot of the repository at the project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoStatus {
    /// Whether the directory is inside a Git work tree.
    pub in_repo: bool,
    /// Current branch, or `HEAD` when detached.
    pub branch: String,
    /// Number of changed paths (staged + unstaged + untracked).
    pub changes: usize,
    /// Per-path change list.
    pub files: Vec<GitChange>,
}

/// Run `git` with `args` in `root`, returning stdout with a single trailing
/// newline removed. Leading whitespace is preserved because `status --porcelain`
/// uses the first column of the first line to encode the index status.
fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|error| format!("无法运行 git: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("git {} 失败", args.join(" "))
        } else {
            stderr
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(stdout.strip_suffix('\n').unwrap_or(&stdout).to_string())
}

/// Query the repository status under `root`.
pub fn repo_status(root: &Path) -> RepoStatus {
    let is_repo = git(root, &["rev-parse", "--is-inside-work-tree"])
        .map(|value| value == "true")
        .unwrap_or(false);
    if !is_repo {
        return RepoStatus {
            in_repo: false,
            branch: String::new(),
            changes: 0,
            files: Vec::new(),
        };
    }
    // `branch --show-current` is reliable even on an unborn branch (returns
    // "master" when HEAD points at refs/heads/master before the first commit),
    // whereas `rev-parse --abbrev-ref HEAD` fails on an unborn branch.
    let branch = git(root, &["branch", "--show-current"])
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "HEAD".to_string());
    let porcelain = git(root, &["status", "--porcelain"]).unwrap_or_default();
    let files: Vec<GitChange> = porcelain
        .lines()
        .filter_map(|line| {
            // XY<space>path (path may itself contain spaces; rename targets
            // use `->` after the first path).
            let bytes = line.as_bytes();
            if bytes.len() < 4 {
                return None;
            }
            let staged = bytes[0] as char;
            let worktree = bytes[1] as char;
            let path = line[3..].trim().to_string();
            Some(GitChange {
                path,
                staged,
                worktree,
            })
        })
        .collect();
    RepoStatus {
        in_repo: true,
        branch,
        changes: files.len(),
        files,
    }
}

/// One commit in the history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommit {
    pub hash: String,
    pub subject: String,
    pub author: String,
    pub date: String,
}

/// Recent commit history (default branch, latest first).
pub fn log(root: &Path, limit: usize) -> Vec<GitCommit> {
    let format = "%h%x1f%s%x1f%an%x1f%ad%x1e";
    let args = [
        "log",
        "-n",
        &limit.to_string(),
        "--date=short",
        "--pretty=format:",
        &format,
    ];
    let output = git(root, &args).unwrap_or_default();
    output
        .split('\x1e')
        .filter_map(|entry| {
            let entry = entry.trim();
            if entry.is_empty() {
                return None;
            }
            let mut parts = entry.split('\x1f');
            Some(GitCommit {
                hash: parts.next().unwrap_or("").to_string(),
                subject: parts.next().unwrap_or("").to_string(),
                author: parts.next().unwrap_or("").to_string(),
                date: parts.next().unwrap_or("").to_string(),
            })
        })
        .take(limit)
        .collect()
}

/// List local branches (current one marked with `*`).
pub fn branches(root: &Path) -> Vec<(String, bool)> {
    git(root, &["branch"])
        .map(|output| {
            output
                .lines()
                .map(|line| {
                    let current = line.starts_with('*');
                    let name = line.trim_start_matches("* ").trim().to_string();
                    (name, current)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Commits the current branch is ahead of / behind its upstream.
pub fn ahead_behind(root: &Path) -> (usize, usize) {
    let output = git(
        root,
        &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
    )
    .unwrap_or_default();
    let mut parts = output.split_whitespace();
    let behind = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let ahead = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    (ahead, behind)
}
/// Unified diff for one path (work-tree vs index).
pub fn diff(root: &Path, path: &str) -> Result<String, String> {
    git(root, &["diff", "--", path])
}

/// Stage a path (add a new/modified file or the deletion).
pub fn stage(root: &Path, path: &str) -> Result<(), String> {
    git(root, &["add", "--", path]).map(|_| ())
}

/// Unstage a path (reset it from the index back to the work tree).
pub fn unstage(root: &Path, path: &str) -> Result<(), String> {
    git(root, &["reset", "HEAD", "--", path]).map(|_| ())
}


/// Commit the staged changes with `message`.
pub fn commit(root: &Path, message: &str) -> Result<(), String> {
    if message.trim().is_empty() {
        return Err("提交信息不能为空".to_string());
    }
    git(root, &["commit", "-m", message.trim()]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_repo() -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let slot = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("ns-git-test-{}-{}", std::process::id(), slot));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        // A throwaway repository, independent of the real project repo. Use an
        // empty template (git's default template copy can race on macOS when
        // several tests create repos in the same process) and a fixed branch.
        let git = Command::new("git")
            .args(["init", "-q", "--template=", "-b", "master"])
            .current_dir(&root)
            .status()
            .expect("git init");
        assert!(git.success());
        let git = Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&root)
            .status()
            .expect("git config");
        assert!(git.success());
        let git = Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&root)
            .status()
            .expect("git config");
        assert!(git.success());
        root
    }

    #[test]
    fn reports_branch_and_untracked() {
        let root = temp_repo();
        fs::write(root.join("a.txt"), "hello").unwrap();
        let repo = repo_status(&root);
        assert!(repo.in_repo);
        assert_eq!(repo.branch, "master");
        assert_eq!(repo.changes, 1);
        assert_eq!(repo.files.len(), 1);
        assert!(repo.files[0].is_untracked());
        assert_eq!(repo.files[0].label(), "??");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn stage_commit_and_track_status() {
        let root = temp_repo();
        fs::write(root.join("a.txt"), "v1").unwrap();
        stage(&root, "a.txt").unwrap();
        commit(&root, "initial").unwrap();
        let repo = repo_status(&root);
        assert_eq!(repo.changes, 0);

        fs::write(root.join("a.txt"), "v2").unwrap();
        let repo = repo_status(&root);
        assert_eq!(repo.files[0].staged, ' ');
        assert_eq!(repo.files[0].worktree, 'M');
        let d = diff(&root, "a.txt").unwrap();
        assert!(d.contains("v1"), "diff should show old content: {d}");
        assert!(d.contains("v2"), "diff should show new content: {d}");

        stage(&root, "a.txt").unwrap();
        let repo = repo_status(&root);
        assert_eq!(repo.files[0].staged, 'M');
        assert_eq!(repo.files[0].worktree, ' ');
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_commit_is_rejected() {
        let root = temp_repo();
        assert!(commit(&root, "   ").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn non_repo_reports_in_repo_false() {
        let root = std::env::temp_dir().join(format!("ns-git-notrepo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let repo = repo_status(&root);
        assert!(!repo.in_repo);
        assert_eq!(repo.changes, 0);
        let _ = fs::remove_dir_all(&root);
    }
}
