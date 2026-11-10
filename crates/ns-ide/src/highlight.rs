use ns_project::Language;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Plain,
    Keyword,
    Type,
    Function,
    String,
    Number,
    Comment,
    Preprocessor,
    Punctuation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighlightSpan {
    pub len: usize,
    pub kind: TokenKind,
}

/// Highlight one line. Empty lines return an empty span list.
///
/// `block_comment` carries multi-line comment state between lines.
pub fn highlight_line(
    line: &str,
    language: Language,
    block_comment: &mut bool,
) -> Vec<HighlightSpan> {
    let mut spans = Vec::new();
    if line.is_empty() {
        return spans;
    }

    let bytes = line.as_bytes();
    let mut i = 0;
    let mut has_code = false;

    while i < bytes.len() {
        if *block_comment {
            if let Some(end) = line[i..].find("*/") {
                let end = i + end + 2;
                push(&mut spans, end - i, TokenKind::Comment);
                i = end;
                *block_comment = false;
            } else {
                push(&mut spans, bytes.len() - i, TokenKind::Comment);
                i = bytes.len();
            }
            continue;
        }

        let rest = &line[i..];

        if rest.starts_with("/*") {
            *block_comment = true;
            // The opening marker is consumed by the block-comment branch next loop.
            push(&mut spans, 2, TokenKind::Comment);
            i += 2;
            continue;
        }
        if rest.starts_with("//") {
            push(&mut spans, bytes.len() - i, TokenKind::Comment);
            break;
        }
        // Assembly line comments: `;` (GNU as / most targets) and `@` (ARM
        // syntax). Only for assembly; C/C++ keep `//`/`/* */` semantics.
        // Detected at the current scan position so identifiers/registers
        // before the comment are still classified normally.
        if language == Language::Asm
            && (rest.starts_with(';') || rest.starts_with('@'))
        {
            push(&mut spans, bytes.len() - i, TokenKind::Comment);
            break;
        }

        let ch = rest.chars().next().unwrap();
        let ch_len = ch.len_utf8();

        if ch.is_whitespace() {
            let mut len = ch_len;
            while i + len < bytes.len() {
                let next = line[i + len..].chars().next().unwrap();
                if !next.is_whitespace() {
                    break;
                }
                len += next.len_utf8();
            }
            push(&mut spans, len, TokenKind::Plain);
            i += len;
            continue;
        }

        if ch == '#' && !has_code && matches!(language, Language::C | Language::Cpp) {
            push(&mut spans, bytes.len() - i, TokenKind::Preprocessor);
            break;
        }

        if ch == '"' {
            let len = string_len(line, i, '"');
            push(&mut spans, len, TokenKind::String);
            i += len;
            has_code = true;
            continue;
        }

        if ch == '\'' {
            let len = char_literal_len(line, i);
            if let Some(len) = len {
                push(&mut spans, len, TokenKind::String);
                i += len;
                has_code = true;
                continue;
            }
        }

        if ch.is_ascii_digit() {
            let mut len = ch_len;
            while i + len < bytes.len() {
                let next = line[i + len..].chars().next().unwrap();
                if next.is_ascii_alphanumeric() || next == '_' || next == '.' {
                    len += next.len_utf8();
                } else {
                    break;
                }
            }
            push(&mut spans, len, TokenKind::Number);
            i += len;
            has_code = true;
            continue;
        }

        if is_ident_start(ch) {
            let mut len = ch_len;
            while i + len < bytes.len() {
                let next = line[i + len..].chars().next().unwrap();
                if is_ident_continue(next) {
                    len += next.len_utf8();
                } else {
                    break;
                }
            }
            let word = &line[i..i + len];
            let kind = classify_identifier(word, line, i + len, language);
            push(&mut spans, len, kind);
            i += len;
            has_code = true;
            continue;
        }

        // Assembly directive: `.section`, `.text`, `.word` ... read the dot
        // plus the following directive word as one keyword token.
        if language == Language::Asm && ch == '.' {
            let mut len = ch_len;
            while i + len < bytes.len() {
                let next = line[i + len..].chars().next().unwrap();
                if is_ident_continue(next) {
                    len += next.len_utf8();
                } else {
                    break;
                }
            }
            let word = &line[i..i + len];
            let kind = if keywords(language).contains(&word) {
                TokenKind::Keyword
            } else {
                TokenKind::Punctuation
            };
            push(&mut spans, len, kind);
            i += len;
            has_code = true;
            continue;
        }

        push(&mut spans, ch_len, TokenKind::Punctuation);
        i += ch_len;
        has_code = true;
    }

    spans
}

fn push(spans: &mut Vec<HighlightSpan>, len: usize, kind: TokenKind) {
    if len == 0 {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.kind == kind {
            last.len += len;
            return;
        }
    }
    spans.push(HighlightSpan { len, kind });
}

fn string_len(line: &str, start: usize, quote: char) -> usize {
    let mut escaped = false;
    for (offset, ch) in line[start + quote.len_utf8()..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
        } else if ch == quote {
            return start + quote.len_utf8() + offset + ch.len_utf8() - start;
        }
    }
    line.len() - start
}

fn char_literal_len(line: &str, start: usize) -> Option<usize> {
    let mut index = start + 1;
    let first = line[index..].chars().next()?;
    if first == '\\' {
        index += 1;
        let escaped = line[index..].chars().next()?;
        index += escaped.len_utf8();
    } else {
        index += first.len_utf8();
    }
    line[index..].starts_with('\'').then_some(index + 1 - start)
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

fn classify_identifier(
    word: &str,
    line: &str,
    end: usize,
    language: Language,
) -> TokenKind {
    if keywords(language).contains(&word) {
        return TokenKind::Keyword;
    }
    if types(language).contains(&word) {
        return TokenKind::Type;
    }
    // Assembly label: `name:` at the end of an identifier (with only a label
    // suffix following) marks a branch target / subroutine entry.
    if language == Language::Asm && line[end..].starts_with(':') {
        return TokenKind::Function;
    }
    let rest = line[end..].trim_start();
    if rest.starts_with('(') || rest.starts_with("!(") || (rest.starts_with('!') && !rest.starts_with("!=")) {
        return TokenKind::Function;
    }
    TokenKind::Plain
}

fn keywords(language: Language) -> &'static [&'static str] {
    match language {
        Language::C => &[
            "auto", "break", "case", "const", "continue", "default", "do", "else", "enum",
            "extern", "for", "goto", "if", "inline", "register", "restrict", "return", "sizeof",
            "static", "struct", "switch", "typedef", "union", "volatile", "while", "asm",
            "_Atomic", "_Generic", "_Noreturn", "_Static_assert", "_Thread_local",
        ],
        Language::Cpp => &[
            "auto", "break", "case", "const", "constexpr", "continue", "default", "delete", "do",
            "else", "enum", "explicit", "export", "extern", "for", "friend", "goto", "if",
            "inline", "namespace", "new", "noexcept", "operator", "private", "protected",
            "public", "register", "return", "sizeof", "static", "struct", "switch", "template",
            "this", "throw", "try", "typedef", "typename", "union", "using", "virtual",
            "volatile", "while", "class", "concept", "requires", "co_await", "co_return",
            "co_yield", "nullptr", "true", "false",
        ],
        Language::Rust => &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
            "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
            "move", "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait",
            "true", "type", "unsafe", "use", "where", "while", "yield", "macro_rules",
        ],
        Language::Zig => &[
            "const", "var", "fn", "pub", "export", "extern", "return", "if", "else", "while",
            "for", "switch", "break", "continue", "defer", "errdefer", "struct", "enum",
            "union", "error", "test", "comptime", "inline", "noinline", "asm", "try", "catch",
            "orelse", "unreachable", "and", "or",
        ],
        Language::ZR => &[
            "fn", "let", "mut", "if", "else", "loop", "while", "for", "in", "return", "break",
            "continue", "struct", "enum", "trait", "impl", "pub", "use", "mod", "match",
            "unsafe", "extern", "asm", "true", "false", "const",
        ],
        // Assembly: ARM / RISC-V mnemonics + directives common to GCC/as and
        // the vendor assemblers (arm-none-eabi-as, riscv-none-elf-as, sdcc).
        Language::Asm => &[
            // ARM Thumb/ARM mnemonics
            "mov", "movs", "movw", "movt", "add", "adds", "adc", "adcs", "sub", "subs", "sbc",
            "sbcs", "and", "ands", "eor", "eors", "orr", "orrs", "bic", "bics", "mvn", "mvns",
            "cmp", "cmn", "tst", "teq", "lsl", "lsr", "asr", "ror", "rrx", "mul", "mla", "mls",
            "umull", "smull", "ldr", "ldrb", "ldrh", "ldrsb", "ldrsh", "str", "strb", "strh",
            "ldm", "stm", "push", "pop", "bx", "blx", "b", "bl", "bx", "cbz", "cbnz", "it",
            "itt", "ite", "itee", "svc", "bkpt", "nop", "dmb", "dsb", "isb", "wfi", "wfe", "sev",
            "mrs", "msr", "cpsid", "cpsie", "rev", "rev16", "revsh", "rbit", "clz", "sxtb",
            "sxth", "uxtb", "uxth", "udiv", "sdiv",
            // RISC-V
            "addi", "slti", "sltiu", "xori", "ori", "andi", "slli", "srli", "srai", "jal",
            "jalr", "beq", "bne", "blt", "bge", "bltu", "bgeu", "lw", "lh", "lhu", "lb", "lbu",
            "sw", "sh", "sb", "fence", "fence.i", "ecall", "ebreak", "mret", "wfi", "csrrw",
            "csrrs", "csrrc", "csrrwi", "csrrsi", "csrrci", "auipc", "lui",
            // Directives & pseudo-ops
            ".section", ".text", ".data", ".bss", ".rodata", ".global", ".globl", ".extern",
            ".type", ".size", ".align", ".balign", ".p2align", ".word", ".hword", ".short",
            ".byte", ".long", ".ascii", ".asciz", ".string", ".space", ".fill", ".zero",
            ".equ", ".set", ".include", ".macro", ".endm", ".if", ".else", ".endif", ".org",
            ".thumb", ".arm", ".thumb_func", ".syntax", ".cpu", ".fpu", ".arch", ".code",
            ".weak", ".comm", ".lcomm", ".option", ".ltorg", ".pool", ".req", ".unreq", ".irq",
            ".fnstart", ".fnend", ".cantunwind",
        ],
    }
}

fn types(language: Language) -> &'static [&'static str] {
    match language {
        Language::C => &[
            "void", "char", "short", "int", "long", "float", "double", "signed", "unsigned",
            "size_t", "ssize_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "int8_t",
            "int16_t", "int32_t", "int64_t", "bool", "ptrdiff_t",
        ],
        Language::Cpp => &[
            "void", "char", "short", "int", "long", "float", "double", "signed", "unsigned",
            "size_t", "ssize_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "int8_t",
            "int16_t", "int32_t", "int64_t", "bool", "string", "vector", "array", "byte",
            "intptr_t", "uintptr_t",
        ],
        Language::Rust => &[
            "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128",
            "usize", "f32", "f64", "bool", "char", "str", "String", "Vec", "Option", "Result",
            "Self", "Box", "Rc", "Arc",
        ],
        Language::Zig => &[
            "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128",
            "isize", "f16", "f32", "f64", "f128", "bool", "void", "type", "anytype", "noreturn",
        ],
        Language::ZR => &[
            "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128",
            "isize", "f32", "f64", "bool", "void", "Self",
        ],
        // Assembly register names.
        Language::Asm => &[
            "r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "r12",
            "r13", "r14", "r15", "sp", "lr", "pc", "fp", "ip", "x0", "x1", "x2", "x3", "x4",
            "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13", "x14", "x15", "x16",
            "x17", "x18", "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27",
            "x28", "x29", "x30", "x31", "zero", "ra", "gp", "tp", "t0", "t1", "t2", "s0",
            "s1", "a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7", "s2", "s3", "s4", "s5",
            "s6", "s7", "s8", "s9", "s10", "s11", "t3", "t4", "t5", "t6", "s0", "s1",
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total(spans: &[HighlightSpan]) -> usize {
        spans.iter().map(|span| span.len).sum()
    }

    #[test]
    fn spans_cover_full_line() {
        let mut block = false;
        let line = "static int count = 0; // 计数";
        let spans = highlight_line(line, Language::C, &mut block);
        assert_eq!(total(&spans), line.len());
        assert!(spans.iter().any(|span| span.kind == TokenKind::Keyword));
        assert!(spans.iter().any(|span| span.kind == TokenKind::Type));
        assert!(spans.iter().any(|span| span.kind == TokenKind::Comment));
    }

    #[test]
    fn block_comment_carries_over() {
        let mut block = false;
        assert_eq!(
            highlight_line("/* start", Language::C, &mut block)
                .first()
                .unwrap()
                .kind,
            TokenKind::Comment
        );
        assert!(block);
        assert_eq!(
            highlight_line("still comment", Language::C, &mut block)
                .first()
                .unwrap()
                .kind,
            TokenKind::Comment
        );
        assert!(block);
        let spans = highlight_line("end */ int x;", Language::C, &mut block);
        assert!(!block);
        assert_eq!(spans[0].kind, TokenKind::Comment);
        assert!(spans.iter().any(|span| span.kind == TokenKind::Type));
    }

    #[test]
    fn preprocessor_and_strings() {
        let mut block = false;
        let spans = highlight_line("#include <stdint.h>", Language::C, &mut block);
        assert_eq!(spans[0].kind, TokenKind::Preprocessor);

        let spans = highlight_line("let s = \"hello\";", Language::Rust, &mut block);
        assert!(spans.iter().any(|span| span.kind == TokenKind::String));
    }

    #[test]
    fn rust_lifetime_is_not_mistaken_for_char() {
        let mut block = false;
        let line = "fn f<'a>(x: &'a str) {}";
        let spans = highlight_line(line, Language::Rust, &mut block);
        assert_eq!(total(&spans), line.len());
    }

    #[test]
    fn assembly_highlights_mnemonics_labels_comments() {
        let mut block = false;
        // ARM reset handler line: label + mnemonic + comment.
        let spans = highlight_line(
            "Reset_Handler: ldr r0, =_estack @ init sp",
            Language::Asm,
            &mut block,
        );
        assert_eq!(total(&spans), "Reset_Handler: ldr r0, =_estack @ init sp".len());
        // The label `Reset_Handler` reads as a function (branch target).
        assert!(spans.iter().any(|s| s.kind == TokenKind::Function));
        // `ldr` is a mnemonic.
        assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword));
        // `@` comment tail is a comment.
        assert!(spans.iter().any(|s| s.kind == TokenKind::Comment));
    }

    #[test]
    fn assembly_semicolon_comment_and_directive() {
        let mut block = false;
        let spans = highlight_line(
            ".section .text, \"ax\" ; code section",
            Language::Asm,
            &mut block,
        );
        assert_eq!(total(&spans), ".section .text, \"ax\" ; code section".len());
        // `.section` directive is a keyword.
        assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Comment));
    }
}
