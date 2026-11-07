use ns_zr::{Severity, analyze, lex, offset_at};

#[test]
fn lexes_keywords_identifiers_and_symbols() {
    let source = "fn main() -> void { let x: u32 = 1; }";
    let tokens = lex(source);
    let significant: Vec<&str> = tokens
        .iter()
        .filter(|t| !matches!(t.kind, ns_zr::TokenKind::Whitespace | ns_zr::TokenKind::Comment))
        .map(|t| t.text.as_str())
        .collect();
    assert_eq!(
        significant,
        vec!["fn", "main", "(", ")", "->", "void", "{", "let", "x", ":", "u32", "=", "1", ";", "}"]
    );
}

#[test]
fn flags_unclosed_delimiter() {
    let analysis = analyze("fn main() { let x = 1;");
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error && d.message.contains("未闭合")),
        "expected unclosed-brace error, got {:?}",
        analysis.diagnostics
    );
}

#[test]
fn flags_unknown_identifier() {
    let analysis = analyze("fn main() { let x = y; }");
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|d| d.message.contains("未知标识符") && d.message.contains('y')),
        "expected unknown-identifier warning, got {:?}",
        analysis.diagnostics
    );
}

#[test]
fn no_diagnostics_for_balanced_code() {
    let analysis = analyze(
        "fn main() -> void {\n    let counter: u32 = 0;\n    while counter < 10 {\n        counter = counter + 1;\n    }\n}\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        analysis.diagnostics
    );
}

#[test]
fn records_declared_symbols() {
    let analysis = analyze("fn helper() -> void { let temp: u32 = 3; }");
    assert!(analysis.symbols.contains("helper"));
}

#[test]
fn offset_at_line_column() {
    let source = "ab\ncd";
    assert_eq!(offset_at(source, 1, 1), 0);
    assert_eq!(offset_at(source, 2, 1), 3);
    assert_eq!(offset_at(source, 2, 2), 4);
}
