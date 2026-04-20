//! shared code-highlighting tokenizer used by both the tui renderer
//! (ratatui spans) and the cli pretty-printer (ansi escape codes).

/// a single token produced by the code-line tokenizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeToken {
    /// rest-of-line comment (includes the comment marker).
    Comment(String),
    /// string literal including its quotes.
    StringLit(String),
    /// numeric literal (digits and dots).
    Number(String),
    /// a language keyword.
    Keyword(String),
    /// a non-keyword identifier.
    Ident(String),
    /// a single punctuation / operator / whitespace character.
    Punct(char),
}

/// tokenise a single source line into [`CodeToken`]s.
///
/// `language` controls which comment syntax and keyword list to use.
pub fn tokenize_code_line(line: &str, language: Option<&str>) -> Vec<CodeToken> {
    let keywords = keywords_for_language(language.unwrap_or_default());
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        // line comments: # for python / shell / bash
        if matches!(language, Some("python") | Some("shell") | Some("bash")) && c == '#' {
            let rest: String = chars[i..].iter().collect();
            tokens.push(CodeToken::Comment(rest));
            return tokens;
        }

        // line comments: // for javascript
        if language == Some("javascript") && i + 1 < chars.len() && c == '/' && chars[i + 1] == '/'
        {
            let rest: String = chars[i..].iter().collect();
            tokens.push(CodeToken::Comment(rest));
            return tokens;
        }

        // string literals
        if c == '"' || c == '\'' {
            let quote = c;
            let start = i;
            i += 1;
            let mut escaped = false;
            while i < chars.len() {
                if escaped {
                    escaped = false;
                    i += 1;
                    continue;
                }
                if chars[i] == '\\' {
                    escaped = true;
                    i += 1;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            tokens.push(CodeToken::StringLit(s));
            continue;
        }

        // numeric literals
        if c.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            tokens.push(CodeToken::Number(s));
            continue;
        }

        // identifiers and keywords
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if keywords.contains(&word.as_str()) {
                tokens.push(CodeToken::Keyword(word));
            } else {
                tokens.push(CodeToken::Ident(word));
            }
            continue;
        }

        tokens.push(CodeToken::Punct(c));
        i += 1;
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
                CodeToken::StringLit(s) => Some(s.as_str()),
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
                CodeToken::StringLit(s) => Some(s.as_str()),
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
        assert_eq!(comment, &CodeToken::Comment("# comment".to_string()));
    }

    #[test]
    fn javascript_comment() {
        let tokens = tokenize_code_line("let x = 1 // comment", Some("javascript"));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Comment(_))));
    }

    #[test]
    fn keyword_detection() {
        let tokens = tokenize_code_line("def foo():", Some("python"));
        assert_eq!(tokens[0], CodeToken::Keyword("def".to_string()));
        assert_eq!(tokens[2], CodeToken::Ident("foo".to_string()));
    }

    #[test]
    fn number_literal() {
        let tokens = tokenize_code_line("x = 42.5", None);
        let nums: Vec<_> = tokens
            .iter()
            .filter_map(|t| match t {
                CodeToken::Number(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(nums, vec!["42.5"]);
    }
}
