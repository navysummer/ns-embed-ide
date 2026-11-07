//! ZR language analysis for ns-embed-ide.
//!
//! ZR is the IDE's in-house language for embedded targets. This crate provides
//! a self-contained lexer and diagnostic analyzer plus the `zr-lsp` binary that
//! speaks LSP over stdio (Content-Length framing), so the IDE's LSP client
//! (`ns_lsp`) can drive it exactly like `clangd` or `rust-analyzer`.

use ::std::collections::HashSet;

pub mod std;
/// ZR language keywords.
pub const KEYWORDS: &[&str] = &[
    "let", "var", "const", "fn", "struct", "enum", "trait", "impl", "pub", "mut", "static",
    "if", "else", "match", "loop", "while", "for", "in", "break", "continue", "return",
    "true", "false", "void", "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "f32",
    "f64", "bool", "char", "str", "as", "type", "mod", "use", "where", "async", "await",
    "unsafe", "extern", "self", "super", "crate", "ref", "move", "dyn", "impl_trait",
];

/// A lexed token with a source span (byte offsets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// Token classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    Ident,
    Number,
    String,
    Symbol,
    Comment,
    Whitespace,
    Unknown,
}

/// Lex `source` into tokens (comments and whitespace included).
pub fn lex(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            tokens.push(token(TokenKind::Whitespace, source, start, i));
        } else if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            tokens.push(token(TokenKind::Comment, source, start, i));
        } else if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            tokens.push(token(TokenKind::Comment, source, start, i));
        } else if b == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(bytes.len());
            tokens.push(token(TokenKind::String, source, start, i));
        } else if b.is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            tokens.push(token(TokenKind::Number, source, start, i));
        } else if b.is_ascii_alphabetic() || b == b'_' {
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
            {
                i += 1;
            }
            let text = &source[start..i];
            let kind = if KEYWORDS.contains(&text) {
                TokenKind::Keyword
            } else {
                TokenKind::Ident
            };
            tokens.push(Token {
                kind,
                text: text.to_string(),
                start,
                end: i,
            });
        } else if b == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'>' {
            i += 2;
            tokens.push(token(TokenKind::Symbol, source, start, i));
        } else {
            i += 1;
            tokens.push(token(TokenKind::Symbol, source, start, i));
        }
    }
    tokens
}

fn token(kind: TokenKind, source: &str, start: usize, end: usize) -> Token {
    Token {
        kind,
        text: source[start..end].to_string(),
        start,
        end,
    }
}

/// A diagnostic produced by the analyzer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// 1-based line number.
    pub line: usize,
    /// 1-based column number.
    pub column: usize,
    /// 0-based byte offset.
    pub offset: usize,
    pub severity: Severity,
    pub message: String,
}

/// Diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// An analyzer result for a ZR source file.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub diagnostics: Vec<Diagnostic>,
    /// Symbols declared at file scope or within the analyzed block.
    pub symbols: HashSet<String>,
}

/// Analyze `source`, reporting unbalanced delimiters and unknown identifiers.
pub fn analyze(source: &str) -> Analysis {
    let tokens = lex(source);
    let mut diagnostics = Vec::new();
    let mut symbols: HashSet<String> = HashSet::new();
    let mut scopes: Vec<HashSet<String>> = vec![HashSet::new()];
    let mut stack: Vec<(char, usize, usize)> = Vec::new(); // (open, line, col)
    let mut line = 1usize;
    let mut col = 1usize;

    for (idx, token) in tokens.iter().enumerate() {
        if token.kind == TokenKind::Comment || token.kind == TokenKind::Whitespace {
            update_pos(&mut line, &mut col, &token.text);
            continue;
        }
        match token.kind {
            TokenKind::Symbol => {
                let ch = token.text.chars().next().unwrap_or('\0');
                match ch {
                    '(' | '[' | '{' => {
                        stack.push((ch, line, col));
                        scopes.push(HashSet::new());
                    }
                    ')' | ']' | '}' => {
                        let expected = match ch {
                            ')' => '(',
                            ']' => '[',
                            '}' => '{',
                            _ => '\0',
                        };
                        if let Some((open, oline, ocol)) = stack.pop() {
                            scopes.pop();
                            if open != expected {
                                diagnostics.push(Diagnostic {
                                    line,
                                    column: col,
                                    offset: token.start,
                                    severity: Severity::Error,
                                    message: format!(
                                        "不匹配的定界符 `{ch}`，期望关闭 `{open}`（开启于 {oline}:{ocol}）"
                                    ),
                                });
                            }
                        } else {
                            diagnostics.push(Diagnostic {
                                line,
                                column: col,
                                offset: token.start,
                                severity: Severity::Error,
                                message: format!("意外的定界符 `{ch}`，没有匹配的开启符"),
                            });
                        }
                    }
                    ';' | ',' => {}
                    _ => {}
                }
            }
            TokenKind::Ident => {
                // Look ahead: if the next non-trivia token is a `:` (type decl)
                // or `(` (function decl), or followed by `=` as a `let`, record
                // it as a symbol. Otherwise, it's a use — check it resolves.
                let next = next_significant(&tokens, idx + 1);
                let looks_decl = matches!(next, Some(t) if t.text == ":" || t.text == "(")
                    || matches!(
                        next,
                        Some(t)
                            if t.text == "="
                                && previous_significant(&tokens, idx)
                                    .map(|p| p.text == "let" || p.text == "var")
                                    .unwrap_or(false)
                    );
                if looks_decl {
                    scopes.last_mut().unwrap().insert(token.text.clone());
                    symbols.insert(token.text.clone());
                } else {
                    let resolved = scopes.iter().rev().any(|scope| scope.contains(&token.text));
                    if !resolved && !KEYWORDS.contains(&token.text.as_str()) {
                        diagnostics.push(Diagnostic {
                            line,
                            column: col,
                            offset: token.start,
                            severity: Severity::Warning,
                            message: format!("未知标识符 `{}`", token.text),
                        });
                    }
                }
            }
            _ => {}
        }
        update_pos(&mut line, &mut col, &token.text);
    }

    for (open, oline, ocol) in stack {
        diagnostics.push(Diagnostic {
            line: oline,
            column: ocol,
            offset: tokens
                .iter()
                .position(|t| t.kind == TokenKind::Symbol && t.text.starts_with(open))
                .unwrap_or(0),
            severity: Severity::Error,
            message: format!("未闭合的定界符 `{open}`"),
        });
    }

    // Sort diagnostics by offset (stack pop order may differ).
    diagnostics.sort_by_key(|d| d.offset);
    Analysis { diagnostics, symbols }
}

fn update_pos(line: &mut usize, col: &mut usize, text: &str) {
    for ch in text.chars() {
        if ch == '\n' {
            *line += 1;
            *col = 1;
        } else {
            *col += 1;
        }
    }
}

fn next_significant(tokens: &[Token], from: usize) -> Option<&Token> {
    tokens[from..]
        .iter()
        .find(|t| t.kind != TokenKind::Whitespace && t.kind != TokenKind::Comment)
}

fn previous_significant(tokens: &[Token], from: usize) -> Option<&Token> {
    tokens[..from]
        .iter()
        .rev()
        .find(|t| t.kind != TokenKind::Whitespace && t.kind != TokenKind::Comment)
}

/// Byte offset of a 1-based line/column position (line and column start at 1).
pub fn offset_at(source: &str, line: usize, column: usize) -> usize {
    if line == 0 || column == 0 {
        return 0;
    }
    let mut current_line = 1usize;
    let mut current_col = 1usize;
    for (i, ch) in source.char_indices() {
        if current_line == line && current_col == column {
            return i;
        }
        if ch == '\n' {
            current_line += 1;
            current_col = 1;
        } else {
            current_col += 1;
        }
    }
    source.len()
}
