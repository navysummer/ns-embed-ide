//! User-defined code snippets: trigger word + snippet body, persisted in the
//! user config directory (`snippets.json`), shared across projects.
//!
//! A snippet is expanded with the LSP snippet grammar (`${1:default}` for
//! placeholders, `$0`/`${0}` for the final cursor, `$TM_FILENAME` etc.), so
//! user snippets can use the same tab-stop variables as LSP snippets.

use std::fs;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::recents::config_dir;

const SNIPPETS_FILE: &str = "snippets.json";

/// One user-defined code snippet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    /// Trigger word, e.g. `for` (matched by prefix).
    pub prefix: String,
    /// The snippet body in LSP snippet grammar.
    pub body: String,
    /// Optional plain description shown in the completion popup.
    #[serde(default)]
    pub description: String,
}

impl Snippet {
    pub fn new(prefix: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            body: body.into(),
            description: String::new(),
        }
    }
}

/// Global user snippet store.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnippetStore {
    pub snippets: Vec<Snippet>,
}

impl SnippetStore {
    pub fn load() -> Self {
        let Some(dir) = config_dir() else {
            return Self::default();
        };
        let path = dir.join(SNIPPETS_FILE);
        let Ok(text) = fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let Some(dir) = config_dir() else {
            anyhow::bail!("无法定位用户配置目录");
        };
        fs::create_dir_all(&dir)
            .with_context(|| format!("创建配置目录 {} 失败", dir.display()))?;
        let text = serde_json::to_string_pretty(self).context("序列化代码片段失败")?;
        fs::write(dir.join(SNIPPETS_FILE), text)
            .with_context(|| format!("写入 {} 失败", dir.join(SNIPPETS_FILE).display()))
    }

    /// Snippets whose `prefix` starts with `word` (case-insensitive).
    pub fn matching(&self, word: &str) -> Vec<Snippet> {
        let word = word.to_ascii_lowercase();
        self.snippets
            .iter()
            .filter(|snippet| snippet.prefix.to_ascii_lowercase().starts_with(&word))
            .cloned()
            .collect()
    }

    /// Insert (or replace) a snippet with the same prefix.
    pub fn upsert(&mut self, snippet: Snippet) {
        if let Some(existing) = self
            .snippets
            .iter_mut()
            .find(|existing| existing.prefix.eq_ignore_ascii_case(&snippet.prefix))
        {
            *existing = snippet;
        } else {
            self.snippets.push(snippet);
        }
    }

    /// Remove the snippet with the given prefix; returns whether one was removed.
    pub fn remove(&mut self, prefix: &str) -> bool {
        let before = self.snippets.len();
        self.snippets
            .retain(|snippet| !snippet.prefix.eq_ignore_ascii_case(prefix));
        self.snippets.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_store_matches_upserts_and_removes() {
        let mut store = SnippetStore::default();
        store.upsert(Snippet::new("for", "for (int i = 0; i < $1; i++) { $0 }"));
        store.upsert(Snippet::new("while", "while ($1) { $0 }"));
        // Case-insensitive prefix matching.
        assert_eq!(store.matching("fo").len(), 1);
        assert_eq!(store.matching("FOR").len(), 1);
        assert_eq!(store.matching("wh").len(), 1);
        // Upsert replaces by prefix.
        store.upsert(Snippet::new("for", "for (;;) { $0 }"));
        assert_eq!(store.snippets.len(), 2);
        assert!(store.snippets.iter().any(|s| s.body == "for (;;) { $0 }"));
        // Remove.
        assert!(store.remove("FOR"));
        assert_eq!(store.snippets.len(), 1);
        assert!(!store.remove("missing"));
    }

    #[test]
    fn snippet_store_round_trips_json() {
        let mut store = SnippetStore::default();
        store.upsert(Snippet::new("main", "int main(void) { $0 }"));
        let dir = std::env::temp_dir().join("ns-snippets-test");
        let _ = fs::remove_dir_all(&dir);
        // Save uses the real config dir; to keep the test hermetic we serialize
        // and re-deserialize the JSON directly.
        let text = serde_json::to_string(&store).unwrap();
        let loaded: SnippetStore = serde_json::from_str(&text).unwrap();
        assert_eq!(loaded, store);
    }
}
