//! shared code-highlighting tokenizer used by both the tui renderer
//! (ratatui spans) and the cli pretty-printer (ansi escape codes).

/// a single token produced by the code-line tokenizer.
///
/// tokens borrow directly from the input line, avoiding per-token
/// `String` allocations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeToken<'a> {
    /// rest-of-line comment (includes the comment marker).
    Comment(&'a str),
    /// string literal including its quotes.
    StringLit(&'a str),
    /// numeric literal (digits and dots).
    Number(&'a str),
    /// a language keyword.
    Keyword(&'a str),
    /// a non-keyword identifier.
    Ident(&'a str),
    /// a single punctuation / operator / whitespace character.
    Punct(char),
}

/// tokenise a single source line into [`CodeToken`]s.
///
/// `language` controls which comment syntax and keyword list to use.
///
/// tokens borrow directly from `line` — no per-token `String` is allocated.
/// all branch conditions test ascii characters, which are single-byte in
/// utf-8, so byte-level indexing is safe.  only the `Punct` fallback needs
/// to decode a full `char` (for multi-byte non-ascii punctuation).
pub fn tokenize_code_line<'a>(line: &'a str, language: Option<&str>) -> Vec<CodeToken<'a>> {
    let keywords = keywords_for_language(language.unwrap_or_default());
    let bytes = line.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        // line comments: # for python / shell / bash / toml
        if matches!(
            language,
            Some("python") | Some("shell") | Some("bash") | Some("toml")
        ) && b == b'#'
        {
            tokens.push(CodeToken::Comment(&line[i..]));
            return tokens;
        }

        // line comments: // for javascript / rust
        if matches!(language, Some("javascript") | Some("rust"))
            && i + 1 < bytes.len()
            && b == b'/'
            && bytes[i + 1] == b'/'
        {
            tokens.push(CodeToken::Comment(&line[i..]));
            return tokens;
        }

        // rust: distinguish char literals ('a', '\n') from lifetimes ('a)
        if language == Some("rust") && b == b'\'' {
            // '\X' — escaped char literal
            if i + 3 < bytes.len() && bytes[i + 1] == b'\\' && bytes[i + 3] == b'\'' {
                tokens.push(CodeToken::StringLit(&line[i..i + 4]));
                i += 4;
                continue;
            }
            // 'X' — single-char literal
            if i + 2 < bytes.len() && bytes[i + 2] == b'\'' {
                tokens.push(CodeToken::StringLit(&line[i..i + 3]));
                i += 3;
                continue;
            }
            // otherwise it's a lifetime tick — emit as punctuation
            tokens.push(CodeToken::Punct('\''));
            i += 1;
            continue;
        }

        // string literals
        if b == b'"' || b == b'\'' {
            let quote = b;
            let start = i;
            i += 1;
            let mut escaped = false;
            while i < bytes.len() {
                if escaped {
                    escaped = false;
                    i += 1;
                    continue;
                }
                if bytes[i] == b'\\' {
                    escaped = true;
                    i += 1;
                    continue;
                }
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            tokens.push(CodeToken::StringLit(&line[start..i]));
            continue;
        }

        // numeric literals
        if b.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            tokens.push(CodeToken::Number(&line[start..i]));
            continue;
        }

        // identifiers and keywords
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &line[start..i];
            if keywords.contains(&word) {
                tokens.push(CodeToken::Keyword(word));
            } else {
                tokens.push(CodeToken::Ident(word));
            }
            continue;
        }

        // punctuation / whitespace / non-ascii: decode one full char
        let ch = line[i..].chars().next().unwrap();
        tokens.push(CodeToken::Punct(ch));
        i += ch.len_utf8();
    }

    tokens
}

pub fn keywords_for_language(language: &str) -> &'static [&'static str] {
    match language {
        "python" => &[
            "def", "class", "return", "if", "elif", "else", "for", "while", "in", "try", "except",
            "with", "as", "import", "from", "pass", "break", "continue", "True", "False", "None",
        ],
        "javascript" => &[
            "function",
            "return",
            "if",
            "else",
            "for",
            "while",
            "const",
            "let",
            "var",
            "class",
            "new",
            "import",
            "from",
            "export",
            "default",
            "try",
            "catch",
            "true",
            "false",
            "null",
            "undefined",
        ],
        "shell" | "bash" => &[
            "if", "then", "fi", "for", "in", "do", "done", "case", "esac", "while", "function",
            "echo", "exit",
        ],
        "pharo" => &["self", "super", "true", "false", "nil", "thisContext", "^"],
        "rust" => &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
            "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
            "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
            "trait", "true", "type", "unsafe", "use", "where", "while",
        ],
        "toml" => &["true", "false"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backslash_escape_in_string() {
        // the old code used chars[i.saturating_sub(1)] which fails on
        // trailing escaped backslashes like "hello\\"
        let tokens = tokenize_code_line(r#"x = "hello\\""#, Some("python"));
        // should produce: Ident(x), Punct( ), Punct(=), Punct( ), StringLit("hello\\"), Ident()...
        // the string literal should end after the second backslash + closing quote
        let strings: Vec<_> = tokens
            .iter()
            .filter_map(|t| match t {
                CodeToken::StringLit(s) => Some(*s),
                _ => None,
            })
            .collect();
        assert_eq!(strings, vec![r#""hello\\""#]);
    }

    #[test]
    fn simple_escaped_quote() {
        let tokens = tokenize_code_line(r#""say \"hi\"""#, None);
        let strings: Vec<_> = tokens
            .iter()
            .filter_map(|t| match t {
                CodeToken::StringLit(s) => Some(*s),
                _ => None,
            })
            .collect();
        assert_eq!(strings, vec![r#""say \"hi\"""#]);
    }

    #[test]
    fn python_comment() {
        let tokens = tokenize_code_line("x = 1 # comment", Some("python"));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Comment(_))));
        let comment = tokens.last().unwrap();
        assert_eq!(comment, &CodeToken::Comment("# comment"));
    }

    #[test]
    fn javascript_comment() {
        let tokens = tokenize_code_line("let x = 1 // comment", Some("javascript"));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Comment(_))));
    }

    #[test]
    fn keyword_detection() {
        let tokens = tokenize_code_line("def foo():", Some("python"));
        assert_eq!(tokens[0], CodeToken::Keyword("def"));
        assert_eq!(tokens[2], CodeToken::Ident("foo"));
    }

    #[test]
    fn number_literal() {
        let tokens = tokenize_code_line("x = 42.5", None);
        let nums: Vec<_> = tokens
            .iter()
            .filter_map(|t| match t {
                CodeToken::Number(s) => Some(*s),
                _ => None,
            })
            .collect();
        assert_eq!(nums, vec!["42.5"]);
    }

    // --- rust ---

    #[test]
    fn rust_comment() {
        let tokens = tokenize_code_line("let x = 1; // a comment", Some("rust"));
        let comment = tokens.last().unwrap();
        assert_eq!(comment, &CodeToken::Comment("// a comment"));
    }

    #[test]
    fn rust_keywords() {
        let tokens = tokenize_code_line("pub fn main() {}", Some("rust"));
        assert_eq!(tokens[0], CodeToken::Keyword("pub"));
        assert_eq!(tokens[2], CodeToken::Keyword("fn"));
        assert_eq!(tokens[4], CodeToken::Ident("main"));
    }

    #[test]
    fn rust_char_literal() {
        let tokens = tokenize_code_line("let c = 'a';", Some("rust"));
        let strings: Vec<_> = tokens
            .iter()
            .filter_map(|t| match t {
                CodeToken::StringLit(s) => Some(*s),
                _ => None,
            })
            .collect();
        assert_eq!(strings, vec!["'a'"]);
    }

    #[test]
    fn rust_escaped_char_literal() {
        let tokens = tokenize_code_line(r"let c = '\n';", Some("rust"));
        let strings: Vec<_> = tokens
            .iter()
            .filter_map(|t| match t {
                CodeToken::StringLit(s) => Some(*s),
                _ => None,
            })
            .collect();
        assert_eq!(strings, vec![r"'\n'"]);
    }

    #[test]
    fn rust_lifetime_not_string() {
        let tokens = tokenize_code_line("fn foo<'a>(x: &'a str) {}", Some("rust"));
        // lifetimes should produce Punct(') then Ident("a"), not StringLit
        let strings: Vec<_> = tokens
            .iter()
            .filter_map(|t| match t {
                CodeToken::StringLit(s) => Some(*s),
                _ => None,
            })
            .collect();
        assert!(
            strings.is_empty(),
            "lifetimes should not be string literals"
        );
        // the tick should be punctuation
        assert!(
            tokens.iter().any(|t| *t == CodeToken::Punct('\'')),
            "lifetime tick should be Punct"
        );
    }

    #[test]
    fn rust_lifetime_static() {
        let tokens = tokenize_code_line("&'static str", Some("rust"));
        // 'static is a lifetime — the tick is Punct, "static" is a Keyword
        assert!(tokens.contains(&CodeToken::Punct('\'')));
        assert!(tokens.contains(&CodeToken::Keyword("static")));
        assert!(
            !tokens.iter().any(|t| matches!(t, CodeToken::StringLit(_))),
            "'static should not produce a StringLit"
        );
    }

    #[test]
    fn rust_string_literal() {
        let tokens = tokenize_code_line(r#"let s = "hello";"#, Some("rust"));
        let strings: Vec<_> = tokens
            .iter()
            .filter_map(|t| match t {
                CodeToken::StringLit(s) => Some(*s),
                _ => None,
            })
            .collect();
        assert_eq!(strings, vec![r#""hello""#]);
    }

    // --- toml ---

    #[test]
    fn toml_comment() {
        let tokens = tokenize_code_line("key = 'value' # a comment", Some("toml"));
        let comment = tokens.last().unwrap();
        assert_eq!(comment, &CodeToken::Comment("# a comment"));
    }

    #[test]
    fn toml_keywords() {
        let tokens = tokenize_code_line("enabled = true", Some("toml"));
        assert_eq!(tokens[0], CodeToken::Ident("enabled"));
        assert_eq!(tokens[4], CodeToken::Keyword("true"));
    }

    #[test]
    fn toml_string_literal() {
        let tokens = tokenize_code_line(r#"name = "lepiter-cli""#, Some("toml"));
        let strings: Vec<_> = tokens
            .iter()
            .filter_map(|t| match t {
                CodeToken::StringLit(s) => Some(*s),
                _ => None,
            })
            .collect();
        assert_eq!(strings, vec![r#""lepiter-cli""#]);
    }
}
