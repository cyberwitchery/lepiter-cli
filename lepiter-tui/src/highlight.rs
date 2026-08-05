//! shared code-highlighting tokenizer used by both the tui renderer
//! (ratatui spans) and the cli pretty-printer (ansi escape codes).

/// a single token produced by the code-line tokenizer.
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

/// per-language lexing rules that drive [`tokenize_code_line`].
///
/// single source of truth for comment/string/keyword syntax.
struct LanguageSyntax {
    /// rest-of-line comment markers, e.g. `#` or `//`.
    line_comments: &'static [&'static str],
    /// paired block-comment delimiters, e.g. `("/*", "*/")`.
    block_comment: Option<(&'static str, &'static str)>,
    /// characters that open and close a string literal.
    string_delims: &'static [char],
    /// paired single-character comment delimiters, e.g. smalltalk `"…"`.
    comment_delims: &'static [(char, char)],
    /// language keywords rendered distinctly.
    keywords: &'static [&'static str],
}

/// languages with no known syntax: both quote styles are strings.
static DEFAULT_SYNTAX: LanguageSyntax = LanguageSyntax {
    line_comments: &[],
    block_comment: None,
    string_delims: &['"', '\''],
    comment_delims: &[],
    keywords: &[],
};

/// smalltalk family (pharo, gemstone): `"…"` is a comment and `'…'` a string —
/// the inverse of most languages.
static SMALLTALK_SYNTAX: LanguageSyntax = LanguageSyntax {
    line_comments: &[],
    block_comment: None,
    string_delims: &['\''],
    comment_delims: &[('"', '"')],
    keywords: &["self", "super", "true", "false", "nil", "thisContext"],
};

static PYTHON_SYNTAX: LanguageSyntax = LanguageSyntax {
    line_comments: &["#"],
    block_comment: None,
    string_delims: &['"', '\''],
    comment_delims: &[],
    keywords: &[
        "def", "class", "return", "if", "elif", "else", "for", "while", "in", "try", "except",
        "with", "as", "import", "from", "pass", "break", "continue", "True", "False", "None",
    ],
};

static JAVASCRIPT_SYNTAX: LanguageSyntax = LanguageSyntax {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    string_delims: &['"', '\''],
    comment_delims: &[],
    keywords: &[
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
};

static SHELL_SYNTAX: LanguageSyntax = LanguageSyntax {
    line_comments: &["#"],
    block_comment: None,
    string_delims: &['"', '\''],
    comment_delims: &[],
    keywords: &[
        "if", "then", "fi", "for", "in", "do", "done", "case", "esac", "while", "function", "echo",
        "exit",
    ],
};

static JSON_SYNTAX: LanguageSyntax = LanguageSyntax {
    line_comments: &[],
    block_comment: None,
    string_delims: &['"'],
    comment_delims: &[],
    keywords: &["true", "false", "null"],
};

static YAML_SYNTAX: LanguageSyntax = LanguageSyntax {
    line_comments: &["#"],
    block_comment: None,
    string_delims: &['"', '\''],
    comment_delims: &[],
    keywords: &["true", "false", "null"],
};

/// selects the [`LanguageSyntax`] for a code-fence language. accepts both the
/// canonical snippet languages from `lepiter-core` (e.g. `shellcommand`) and
/// the common markdown-fence aliases (`shell`, `bash`).
fn syntax_for_language(language: Option<&str>) -> &'static LanguageSyntax {
    match language {
        Some("pharo") | Some("gemstone") => &SMALLTALK_SYNTAX,
        Some("python") => &PYTHON_SYNTAX,
        Some("javascript") => &JAVASCRIPT_SYNTAX,
        Some("shell") | Some("bash") | Some("shellcommand") => &SHELL_SYNTAX,
        Some("json") => &JSON_SYNTAX,
        Some("yaml") => &YAML_SYNTAX,
        _ => &DEFAULT_SYNTAX,
    }
}

/// tokenise a single source line into [`CodeToken`]s.
///
/// `language` selects a [`LanguageSyntax`] table that controls comment, string
/// and keyword lexing.
///
/// tokens borrow directly from `line` — no per-token `String` is allocated.
/// every delimiter in the syntax table is ascii, which is single-byte in
/// utf-8, so byte-level scanning is safe and only ever slices `line` at char
/// boundaries.  only the `Punct` fallback decodes a full `char` (for
/// multi-byte non-ascii punctuation).
pub fn tokenize_code_line<'a>(line: &'a str, language: Option<&str>) -> Vec<CodeToken<'a>> {
    let syntax = syntax_for_language(language);
    let bytes = line.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        // rest-of-line comments (e.g. `#`, `//`)
        if syntax
            .line_comments
            .iter()
            .any(|m| bytes[i..].starts_with(m.as_bytes()))
        {
            tokens.push(CodeToken::Comment(&line[i..]));
            return tokens;
        }

        // block comments (e.g. `/* … */`), scoped to this line
        if let Some((open, close)) = syntax.block_comment
            && bytes[i..].starts_with(open.as_bytes())
        {
            let start = i;
            i += open.len();
            while i < bytes.len() && !bytes[i..].starts_with(close.as_bytes()) {
                i += 1;
            }
            if i < bytes.len() {
                i += close.len();
            }
            tokens.push(CodeToken::Comment(&line[start..i]));
            continue;
        }

        // character-delimited comments and strings share an ascii-only guard so
        // multi-byte lead bytes never match a delimiter.
        if b.is_ascii() {
            let cur = b as char;

            // paired single-char comments (e.g. smalltalk `"…"`)
            if let Some(&(_, close)) = syntax.comment_delims.iter().find(|(open, _)| *open == cur) {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != close as u8 {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
                tokens.push(CodeToken::Comment(&line[start..i]));
                continue;
            }

            // string literals
            if syntax.string_delims.contains(&cur) {
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
            if syntax.keywords.contains(&word) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backslash_escape_in_string() {
        // a trailing escaped backslash must not swallow the closing quote
        let tokens = tokenize_code_line(r#"x = "hello\\""#, Some("python"));
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

    fn comments<'a>(tokens: &[CodeToken<'a>]) -> Vec<&'a str> {
        tokens
            .iter()
            .filter_map(|t| match t {
                CodeToken::Comment(s) => Some(*s),
                _ => None,
            })
            .collect()
    }

    fn strings<'a>(tokens: &[CodeToken<'a>]) -> Vec<&'a str> {
        tokens
            .iter()
            .filter_map(|t| match t {
                CodeToken::StringLit(s) => Some(*s),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn smalltalk_double_quote_is_comment_not_string() {
        let tokens = tokenize_code_line(r#"foo "a comment" bar"#, Some("pharo"));
        assert_eq!(comments(&tokens), vec![r#""a comment""#]);
        assert!(strings(&tokens).is_empty());
    }

    #[test]
    fn smalltalk_single_quote_is_string() {
        let tokens = tokenize_code_line("x := 'a string'", Some("pharo"));
        assert_eq!(strings(&tokens), vec!["'a string'"]);
        assert!(comments(&tokens).is_empty());
    }

    #[test]
    fn smalltalk_single_quote_inside_comment_does_not_desync() {
        // the apostrophe inside the comment must not open a string literal and
        // leave the rest of the line mis-tokenised.
        let tokens = tokenize_code_line(r#"foo "it's a comment" 42"#, Some("pharo"));
        assert_eq!(comments(&tokens), vec![r#""it's a comment""#]);
        assert!(strings(&tokens).is_empty());
        // the trailing number still tokenises correctly.
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Number("42"))));
    }

    #[test]
    fn gemstone_uses_smalltalk_syntax() {
        let tokens = tokenize_code_line(r#""doc" self"#, Some("gemstone"));
        assert_eq!(comments(&tokens), vec![r#""doc""#]);
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t, CodeToken::Keyword("self")))
        );
    }

    #[test]
    fn smalltalk_keyword_detection() {
        let tokens = tokenize_code_line("super foo", Some("pharo"));
        assert_eq!(tokens[0], CodeToken::Keyword("super"));
    }

    #[test]
    fn block_comment_spans_within_line() {
        let tokens = tokenize_code_line("a /* mid */ b", Some("javascript"));
        assert_eq!(comments(&tokens), vec!["/* mid */"]);
        // code on both sides is preserved as identifiers.
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Ident("a"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Ident("b"))));
    }

    #[test]
    fn unterminated_block_comment_runs_to_end_of_line() {
        let tokens = tokenize_code_line("x /* oops", Some("javascript"));
        assert_eq!(comments(&tokens), vec!["/* oops"]);
    }

    #[test]
    fn shellcommand_hash_is_comment() {
        // `shellCommandSnippet` infers the language `shellcommand`; its `#`
        // comments must highlight like `shell`/`bash`.
        let tokens = tokenize_code_line("ls -la # list", Some("shellcommand"));
        assert_eq!(comments(&tokens), vec!["# list"]);
    }

    #[test]
    fn non_smalltalk_double_quote_is_still_a_string() {
        let tokens = tokenize_code_line(r#"x = "hi""#, Some("python"));
        assert_eq!(strings(&tokens), vec![r#""hi""#]);
        assert!(comments(&tokens).is_empty());
    }
}
