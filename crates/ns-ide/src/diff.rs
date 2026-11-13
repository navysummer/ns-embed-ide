//! Line-based text diff for the "compare with saved" view.
//!
//! Uses the classic longest-common-subsequence (LCS) dynamic program over
//! lines. For editor-sized inputs this is fast enough (files are at most a few
//! thousand lines); the O(n·m) table is bounded by the product of the two line
//! counts, so an unusually large pair falls back to a coarse prefix/suffix
//! alignment instead of allocating a pathological table.

/// One line of the diff output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOp {
    /// Unchanged context line.
    Context(String),
    /// Removed line (only in the left / saved version).
    Removed(String),
    /// Added line (only in the right / buffer version).
    Added(String),
}

/// Compute a line diff between `left` and `right`, returning the ops in
/// presentation order. Lines are normalized to `\n` first; a trailing newline
/// does not create a spurious empty line, so `"a\n"` and `"a"` are equal.
pub fn diff_lines(left: &str, right: &str) -> Vec<DiffOp> {
    let left_norm = normalize(left);
    let right_norm = normalize(right);
    let left: Vec<&str> = left_norm.lines().collect();
    let right: Vec<&str> = right_norm.lines().collect();
    if left == right {
        return Vec::new();
    }
    let n = left.len();
    let m = right.len();
    if n == 0 && m == 0 {
        return Vec::new();
    }
    // Full LCS length table. Editor-sized inputs are a few thousand lines, so
    // the table stays small; bail to a coarse diff for pathological pairs.
    if n.saturating_mul(m) > 4_000_000 {
        return coarse_diff(&left, &right);
    }
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in 1..=n {
        for j in 1..=m {
            lcs[i][j] = if left[i - 1] == right[j - 1] {
                lcs[i - 1][j - 1] + 1
            } else {
                lcs[i - 1][j].max(lcs[i][j - 1])
            };
        }
    }
    // Backtrack from the bottom-right, building the ops in reverse.
    let mut i = n;
    let mut j = m;
    let mut reversed: Vec<DiffOp> = Vec::new();
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && left[i - 1] == right[j - 1] {
            reversed.push(DiffOp::Context(left[i - 1].to_string()));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || lcs[i][j - 1] >= lcs[i - 1][j]) {
            reversed.push(DiffOp::Added(right[j - 1].to_string()));
            j -= 1;
        } else if i > 0 {
            reversed.push(DiffOp::Removed(left[i - 1].to_string()));
            i -= 1;
        } else {
            break;
        }
    }
    reversed.reverse();
    reversed
}

/// Fallback for pathological inputs: align by the longest common prefix and
/// suffix and treat everything between as removed+added. Memory stays bounded
/// while the diff remains correct (if coarser than the LCS path).
fn coarse_diff(left: &[&str], right: &[&str]) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    let prefix = left
        .iter()
        .zip(right)
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = left[prefix..]
        .iter()
        .rev()
        .zip(right[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    for line in &left[..prefix] {
        ops.push(DiffOp::Context((*line).to_string()));
    }
    for line in &left[prefix..left.len() - suffix] {
        ops.push(DiffOp::Removed((*line).to_string()));
    }
    for line in &right[prefix..right.len() - suffix] {
        ops.push(DiffOp::Added((*line).to_string()));
    }
    for line in &left[left.len() - suffix..] {
        ops.push(DiffOp::Context((*line).to_string()));
    }
    ops
}

/// Normalize line endings so `\r\n` and `\n` compare equal.
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Render ops as a compact unified-diff-style string for the output panel.
pub fn render_diff(ops: &[DiffOp]) -> String {
    let mut out = String::new();
    for op in ops {
        let (prefix, line) = match op {
            DiffOp::Context(line) => ("  ", line),
            DiffOp::Removed(line) => ("- ", line),
            DiffOp::Added(line) => ("+ ", line),
        };
        out.push_str(prefix);
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_inputs_produce_no_ops() {
        let ops = diff_lines("a\nb\nc\n", "a\nb\nc\n");
        assert!(ops.is_empty());
    }

    #[test]
    fn trailing_newline_is_not_a_change() {
        let ops = diff_lines("a\nb\n", "a\nb");
        assert!(ops.is_empty());
    }

    #[test]
    fn single_line_insert_is_an_add() {
        let ops = diff_lines("a\nc\n", "a\nb\nc\n");
        assert_eq!(
            ops,
            vec![
                DiffOp::Context("a".into()),
                DiffOp::Added("b".into()),
                DiffOp::Context("c".into()),
            ]
        );
    }

    #[test]
    fn single_line_delete_is_a_remove() {
        let ops = diff_lines("a\nb\nc\n", "a\nc\n");
        assert_eq!(
            ops,
            vec![
                DiffOp::Context("a".into()),
                DiffOp::Removed("b".into()),
                DiffOp::Context("c".into()),
            ]
        );
    }

    #[test]
    fn replace_shows_remove_then_add() {
        let ops = diff_lines("a\nold\nc\n", "a\nnew\nc\n");
        assert!(ops.contains(&DiffOp::Removed("old".into())));
        assert!(ops.contains(&DiffOp::Added("new".into())));
        assert_eq!(ops.len(), 4);
    }

    #[test]
    fn render_marks_removed_and_added() {
        let ops = diff_lines("x\n", "x\ny\n");
        let text = render_diff(&ops);
        assert!(text.contains("  x\n"));
        assert!(text.contains("+ y\n"));
    }

    #[test]
    fn crlf_normalized_before_compare() {
        let ops = diff_lines("a\r\nb\r\n", "a\nb\n");
        assert!(ops.is_empty());
    }
}
