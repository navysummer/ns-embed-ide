//! Command palette (F13/F14 命令面板).
//!
//! Static list of IDE commands + a small subsequence fuzzy matcher.
//! Matching is case-insensitive; consecutive matches and prefix matches are
//! scored higher.

/// One entry shown in the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandEntry {
    /// Stable identifier used to dispatch the command.
    pub id: &'static str,
    /// Title shown in the list.
    pub title: &'static str,
    /// Shortcut hint shown on the right.
    pub keys: &'static str,
    /// Extra terms the query may match against.
    pub keywords: &'static str,
}

pub const COMMANDS: &[CommandEntry] = &[
    CommandEntry { id: "new-project", title: "新建项目", keys: "cmd/ctrl+n", keywords: "new create project 新建 创建" },
    CommandEntry { id: "open-project", title: "打开项目…", keys: "cmd/ctrl+o", keywords: "open project 打开 项目" },
    CommandEntry { id: "save", title: "保存当前文件", keys: "cmd/ctrl+s", keywords: "save 保存" },
    CommandEntry { id: "build", title: "构建项目", keys: "cmd/ctrl+b", keywords: "build compile 构建 编译 b" },
    CommandEntry { id: "run", title: "构建并运行", keys: "cmd/ctrl+r", keywords: "run 构建 运行 r" },
    CommandEntry { id: "debug", title: "开始调试（构建并启动）", keys: "f5", keywords: "debug f5 调试 启动" },
    CommandEntry { id: "stop-debug", title: "停止调试", keys: "shift+f5", keywords: "stop debug 停止 调试" },
    CommandEntry { id: "find", title: "全局文件搜索…", keys: "cmd/ctrl+shift+f", keywords: "find search 搜索 查找" },
    CommandEntry { id: "toggle-output", title: "显示/隐藏输出面板", keys: "cmd/ctrl+j", keywords: "output 输出 面板 panel" },
    CommandEntry { id: "breakpoints", title: "显示断点面板", keys: "", keywords: "breakpoints 断点 面板" },
    CommandEntry { id: "clear-breakpoints", title: "清除全部断点", keys: "cmd/ctrl+shift+f9", keywords: "clear breakpoints 清除 断点" },
    CommandEntry { id: "add-watchpoint", title: "添加观察点（数据断点）…", keys: "", keywords: "watchpoint data breakpoint 观察点 数据断点 监视" },
    CommandEntry { id: "close-tab", title: "关闭当前标签页", keys: "cmd/ctrl+w", keywords: "close tab 关闭 标签" },
    CommandEntry { id: "toggle-diagnostics", title: "显示问题面板", keys: "", keywords: "problems diagnostics errors 问题 错误" },
    CommandEntry { id: "focus-next-tab", title: "切换到下一个标签页", keys: "ctrl+tab", keywords: "next tab 下一个 标签" },
    CommandEntry { id: "focus-prev-tab", title: "切换到上一个标签页", keys: "ctrl+shift+tab", keywords: "prev 上一个 标签" },
];

/// Rank `COMMANDS` for the given query.
///
/// Returns indices into `COMMANDS`, best first. An empty query returns all
/// commands in declaration order.
pub fn rank(query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return (0..COMMANDS.len()).collect();
    }
    let mut scored: Vec<(usize, i64)> = COMMANDS
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            fuzzy_score(&format!("{} {}", entry.title, entry.keywords), &query).map(|score| (index, score))
        })
        .collect();
    scored.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    scored.into_iter().map(|(index, _)| index).collect()
}

/// Score a candidate against a (already lowercased, trimmed) query.
///
/// Subsequence match: all query chars must appear in order. Consecutive runs
/// and an early/first match are worth more; big gaps between matches are
/// penalized.
fn fuzzy_score(candidate: &str, query: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let candidate = candidate.to_lowercase();
    let candidate_chars: Vec<char> = candidate.chars().collect();
    let query_chars: Vec<char> = query.chars().collect();

    let mut score: i64 = 0;
    let mut q = 0usize;
    let mut previous_match: Option<usize> = None;
    let mut run = 0i64;

    for (index, &character) in candidate_chars.iter().enumerate() {
        if q < query_chars.len() && character == query_chars[q] {
            score += 10;
            if let Some(previous) = previous_match {
                if index == previous + 1 {
                    run += 1;
                    score += 15 + run * 5;
                } else {
                    run = 0;
                    score -= 2;
                }
            } else {
                // First matched char: earlier is better, first char is ideal.
                score += (64 - index.min(64)) as i64;
                if index == 0 {
                    score += 40;
                }
            }
            previous_match = Some(index);
            q += 1;
        }
    }

    if q == query_chars.len() {
        Some(score)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_returns_all_commands() {
        assert_eq!(rank(""), (0..COMMANDS.len()).collect::<Vec<_>>());
        assert_eq!(rank("   "), (0..COMMANDS.len()).collect::<Vec<_>>());
    }

    #[test]
    fn exact_title_wins() {
        let ranked = rank("构建项目");
        assert_eq!(ranked.first().copied(), Some(3)); // build entry
    }

    #[test]
    fn subsequence_matches_multiple() {
        let ranked = rank("断点");
        assert!(ranked.contains(&9)); // 显示断点面板
        assert!(ranked.contains(&10)); // 清除全部断点
    }

    #[test]
    fn keywords_match() {
        let ranked = rank("f5");
        assert!(ranked.contains(&5)); // debug
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(rank("SAVE"), rank("save"));
    }

    #[test]
    fn no_match_returns_empty() {
        assert!(rank("zzzqqq无此命令").is_empty());
    }

    #[test]
    fn prefix_beats_scattered() {
        let ranked = rank("run");
        assert_eq!(ranked[0], 4); // 构建并运行 — "run" hits title
    }
}
