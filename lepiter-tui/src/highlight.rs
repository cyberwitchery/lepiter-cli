//! shared code-highlighting tokenizer used by both the tui renderer
//! (ratatui spans) and the cli pretty-printer (ansi escape codes).

/// a single token produced by the code tokenizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeToken<'a> {
    /// comment, or the part of one that falls on this line.
    Comment(&'a str),
    /// string literal, or the part of one that falls on this line.
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

/// per-language lexing rules that drive [`tokenize_code_block`].
///
/// single source of truth for comment/string/keyword syntax.
struct LanguageSyntax {
    /// rest-of-line comment markers, e.g. `#` or `//`.
    line_comments: &'static [&'static str],
    /// unquoted characters a line comment must follow unless it starts the line; `None` for any.
    comment_after: Option<&'static [char]>,
    /// paired comment delimiters, e.g. `("/*", "*/")` or smalltalk `("\"", "\"")`.
    block_comments: &'static [(&'static str, &'static str)],
    /// how a string opened by each quote character is lexed.
    strings: &'static [StringDelim],
    /// prefix that quotes the one character after it, e.g. smalltalk `$'`.
    char_escape: Option<u8>,
    /// `<<WORD` opens a heredoc: the lines up to the one holding `WORD` are a string.
    heredocs: bool,
    /// a `/` where an operand is expected opens a regex literal, e.g. javascript `/[/*]/g`.
    regex_literals: bool,
    /// `"…"`, `$(…)`, `${…}`, `$((…))` and backquotes nest inside one another, as in a posix shell.
    substitutions: bool,
    /// language keywords rendered distinctly.
    keywords: &'static [&'static str],
}

#[derive(Debug)]
struct StringDelim {
    quote: u8,
    /// a backslash escapes the next character, line break included.
    backslash: bool,
    /// a doubled quote is a literal quote, e.g. smalltalk `'it''s'`.
    doubled: bool,
    /// the string may run past the end of its line.
    multiline: bool,
    /// three quotes open a string that runs to the next three, across lines.
    triple: bool,
}

/// a single-line string in which a backslash escapes.
const fn escaped(quote: u8) -> StringDelim {
    StringDelim {
        quote,
        backslash: true,
        doubled: false,
        multiline: false,
        triple: false,
    }
}

/// a single-line string in which a backslash is an ordinary character.
const fn verbatim(quote: u8) -> StringDelim {
    StringDelim {
        backslash: false,
        ..escaped(quote)
    }
}

/// languages with no known syntax: both quote styles are strings.
static DEFAULT_SYNTAX: LanguageSyntax = LanguageSyntax {
    line_comments: &[],
    comment_after: None,
    block_comments: &[],
    strings: &[escaped(b'"'), escaped(b'\'')],
    char_escape: None,
    heredocs: false,
    regex_literals: false,
    substitutions: false,
    keywords: &[],
};

/// smalltalk family (pharo, gemstone): `"…"` is a comment and `'…'` a string —
/// the inverse of most languages.
static SMALLTALK_SYNTAX: LanguageSyntax = LanguageSyntax {
    line_comments: &[],
    comment_after: None,
    block_comments: &[("\"", "\"")],
    strings: &[StringDelim {
        doubled: true,
        multiline: true,
        ..verbatim(b'\'')
    }],
    char_escape: Some(b'$'),
    heredocs: false,
    regex_literals: false,
    substitutions: false,
    keywords: &["self", "super", "true", "false", "nil", "thisContext"],
};

static PYTHON_SYNTAX: LanguageSyntax = LanguageSyntax {
    line_comments: &["#"],
    comment_after: None,
    block_comments: &[],
    strings: &[
        StringDelim {
            triple: true,
            ..escaped(b'"')
        },
        StringDelim {
            triple: true,
            ..escaped(b'\'')
        },
    ],
    char_escape: None,
    heredocs: false,
    regex_literals: false,
    substitutions: false,
    keywords: &[
        "def", "class", "return", "if", "elif", "else", "for", "while", "in", "try", "except",
        "with", "as", "import", "from", "pass", "break", "continue", "True", "False", "None",
    ],
};

static JAVASCRIPT_SYNTAX: LanguageSyntax = LanguageSyntax {
    line_comments: &["//"],
    comment_after: None,
    block_comments: &[("/*", "*/")],
    strings: &[
        escaped(b'"'),
        escaped(b'\''),
        StringDelim {
            multiline: true,
            ..escaped(b'`')
        },
    ],
    char_escape: None,
    heredocs: false,
    regex_literals: true,
    substitutions: false,
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

/// the characters that end an unquoted shell word.
const SHELL_METACHARACTERS: &[char] = &[' ', '\t', '|', '&', ';', '(', ')', '<', '>'];

static SHELL_SYNTAX: LanguageSyntax = LanguageSyntax {
    line_comments: &["#"],
    comment_after: Some(SHELL_METACHARACTERS),
    block_comments: &[],
    strings: &[StringDelim {
        multiline: true,
        ..verbatim(b'\'')
    }],
    char_escape: Some(b'\\'),
    heredocs: true,
    regex_literals: false,
    substitutions: true,
    keywords: &[
        "if", "then", "fi", "for", "in", "do", "done", "case", "esac", "while", "function", "echo",
        "exit",
    ],
};

static JSON_SYNTAX: LanguageSyntax = LanguageSyntax {
    line_comments: &[],
    comment_after: None,
    block_comments: &[],
    strings: &[escaped(b'"')],
    char_escape: None,
    heredocs: false,
    regex_literals: false,
    substitutions: false,
    keywords: &["true", "false", "null"],
};

static YAML_SYNTAX: LanguageSyntax = LanguageSyntax {
    line_comments: &["#"],
    comment_after: Some(&[' ', '\t']),
    block_comments: &[],
    strings: &[
        escaped(b'"'),
        StringDelim {
            doubled: true,
            ..verbatim(b'\'')
        },
    ],
    char_escape: None,
    heredocs: false,
    regex_literals: false,
    substitutions: false,
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

/// what the lexer is inside of where one line ends and the next begins.
#[derive(Debug, Default)]
struct LexState<'a> {
    mode: Mode<'a>,
    /// shell quotes and substitutions still open, innermost last.
    shell: Vec<Shell>,
    /// an escaped line break carries the shell word in progress into the next line.
    word: bool,
}

#[derive(Debug, Clone, Copy, Default)]
enum Mode<'a> {
    #[default]
    Code,
    /// a block comment, ended by the delimiter held here.
    Comment(&'static str),
    String {
        delim: &'static StringDelim,
        triple: bool,
    },
    /// a heredoc body, ended by a line holding just this word.
    Heredoc(&'a str),
}

/// a shell quote or substitution the lexer is inside of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shell {
    /// `"…"`: everything in it, substitutions included, is one string.
    Quote,
    /// `'…'` inside a double-quoted word.
    Single,
    /// `$'…'` inside a double-quoted word.
    Ansi,
    /// `$(…)`, `<(…)`, `>(…)` or `name=(…)`: the word goes on after its `)`.
    Command,
    /// `(…)`: its `)` ends the word.
    Subshell,
    /// one open parenthesis of `$((…))` inside a double-quoted word.
    Arithmetic,
    /// `${…}`.
    Param,
    /// a backquoted command.
    Backtick,
}

/// tokenise the lines of one code block into [`CodeToken`]s, one `Vec` per
/// line. a comment or string still open at the end of a line carries on into
/// the next, and nothing carries past the last line.
///
/// `language` selects a [`LanguageSyntax`] table that controls comment, string
/// and keyword lexing.
///
/// tokens borrow directly from the lines — no per-token `String` is allocated.
/// every delimiter in the syntax table is ascii, which is single-byte in
/// utf-8, so byte-level scanning is safe and only ever slices a line at char
/// boundaries.
pub fn tokenize_code_block<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    language: Option<&str>,
) -> Vec<Vec<CodeToken<'a>>> {
    let syntax = syntax_for_language(language);
    let mut state = LexState::default();
    lines
        .into_iter()
        .map(|line| tokenize_line(line, syntax, &mut state))
        .collect()
}

fn tokenize_line<'a>(
    line: &'a str,
    syntax: &'static LanguageSyntax,
    state: &mut LexState<'a>,
) -> Vec<CodeToken<'a>> {
    let bytes = line.as_bytes();
    let mut tokens = Vec::new();
    let mut heredoc = None;
    let mut start = 0;
    let mut i = 0;
    // the token count at which a shell word goes on, so a `#` there opens no comment
    let mut glued = std::mem::take(&mut state.word).then_some(0);

    loop {
        if !matches!(state.mode, Mode::Code) {
            let (end, next) = scan(state.mode, bytes, i);
            if end > start {
                let text = &line[start..end];
                tokens.push(match state.mode {
                    Mode::String { .. } | Mode::Heredoc(_) => CodeToken::StringLit(text),
                    _ => CodeToken::Comment(text),
                });
            }
            state.mode = next;
            i = end;
        } else if state.shell.contains(&Shell::Quote) {
            let end = scan_quoted(line, i, glued != Some(tokens.len()), state, &mut heredoc);
            if end > start {
                tokens.push(CodeToken::StringLit(&line[start..end]));
            }
            i = end;
        }
        if i >= bytes.len() {
            break;
        }
        start = i;
        let rest = &bytes[i..];
        let b = bytes[i];
        let comment_ok = glued != Some(tokens.len())
            && comment_allowed(syntax.comment_after, &tokens)
            && state.shell.last() != Some(&Shell::Param);

        // rest-of-line comments (e.g. `#`, `//`)
        if comment_ok
            && syntax
                .line_comments
                .iter()
                .any(|m| rest.starts_with(m.as_bytes()))
        {
            if state.shell.last() == Some(&Shell::Backtick) {
                i = backtick_end(bytes, i);
                tokens.push(CodeToken::Comment(&line[start..i]));
                continue;
            }
            tokens.push(CodeToken::Comment(&line[i..]));
            break;
        }

        // block comments (e.g. `/* … */`)
        if let Some(&(open, close)) = syntax
            .block_comments
            .iter()
            .find(|(open, _)| rest.starts_with(open.as_bytes()))
        {
            state.mode = Mode::Comment(close);
            i += open.len();
            continue;
        }

        if syntax.regex_literals && b == b'/' && regex_allowed(&tokens) {
            i = regex_end(bytes, i);
            tokens.push(CodeToken::StringLit(&line[start..i]));
            continue;
        }

        if syntax.heredocs
            && (i == 0 || bytes[i - 1] != b'<')
            && let Some((end, word)) = heredoc_opener(&line[i..])
            && !in_arithmetic(&bytes[..i])
        {
            i += end;
            tokens.push(CodeToken::StringLit(&line[start..i]));
            heredoc = Some(word);
            continue;
        }

        if syntax.char_escape == Some(b) {
            if syntax.substitutions && i + 1 == bytes.len() {
                state.word = !comment_ok;
            }
            i += 1 + line[i + 1..].chars().next().map_or(0, char::len_utf8);
            tokens.push(CodeToken::StringLit(&line[start..i]));
            continue;
        }

        if syntax.substitutions && b == b'"' {
            state.shell.push(Shell::Quote);
            i += 1;
            continue;
        }

        // string literals
        if let Some(delim) = syntax.strings.iter().find(|d| d.quote == b) {
            let triple = delim.triple && rest.starts_with(&[b; 3]);
            state.mode = Mode::String { delim, triple };
            i += if triple { 3 } else { 1 };
            continue;
        }

        // numeric literals
        if b.is_ascii_digit() {
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            tokens.push(CodeToken::Number(&line[start..i]));
            continue;
        }

        // identifiers and keywords
        if b.is_ascii_alphabetic() || b == b'_' {
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
        if syntax.substitutions && nest(&mut state.shell, ch, tokens.last()) {
            glued = Some(tokens.len() + 1);
        }
        tokens.push(CodeToken::Punct(ch));
        i += ch.len_utf8();
    }

    if let Some(word) = heredoc {
        state.mode = Mode::Heredoc(word);
    }
    tokens
}

/// tracks the shell construct `ch` opens or closes, returning whether the word goes on after it.
fn nest(shell: &mut Vec<Shell>, ch: char, last: Option<&CodeToken<'_>>) -> bool {
    let top = shell.last().copied();
    let after = |chars: &[char]| matches!(last, Some(CodeToken::Punct(c)) if chars.contains(c));
    match ch {
        '(' if after(&['$', '<', '>', '=']) => shell.push(Shell::Command),
        '(' => shell.push(Shell::Subshell),
        ')' if matches!(top, Some(Shell::Command | Shell::Subshell)) => {
            return shell.pop() == Some(Shell::Command);
        }
        '{' if after(&['$']) => shell.push(Shell::Param),
        '}' if top == Some(Shell::Param) => {
            shell.pop();
        }
        '`' if top == Some(Shell::Backtick) => {
            shell.pop();
        }
        '`' => shell.push(Shell::Backtick),
        _ => {}
    }
    false
}

/// opens the substitution a `$` at the start of `rest` begins, returning its length.
fn open_dollar(shell: &mut Vec<Shell>, rest: &[u8]) -> usize {
    if rest.starts_with(b"$((") {
        shell.extend([Shell::Arithmetic; 2]);
        3
    } else if rest.starts_with(b"$(") {
        shell.push(Shell::Command);
        2
    } else if rest.starts_with(b"${") {
        shell.push(Shell::Param);
        2
    } else {
        1
    }
}

/// scans the double-quoted shell word `state` is inside, nesting included, from `i` to its
/// closing quote or the end of the line, returning where it stops.
fn scan_quoted<'a>(
    line: &'a str,
    mut i: usize,
    mut word_start: bool,
    state: &mut LexState<'a>,
    heredoc: &mut Option<&'a str>,
) -> usize {
    let bytes = line.as_bytes();
    let shell = &mut state.shell;
    let base = shell.iter().position(|s| *s == Shell::Quote).unwrap_or(0);
    while i < bytes.len() && shell.len() > base {
        let top = shell[shell.len() - 1];
        let b = bytes[i];
        let code = matches!(top, Shell::Command | Shell::Subshell);
        let mut step = 1;
        let mut glue = false;
        match top {
            Shell::Single | Shell::Ansi => match b {
                b'\\' if top == Shell::Ansi => step = 2,
                b'\'' => {
                    shell.pop();
                }
                _ => {}
            },
            Shell::Quote | Shell::Backtick => match b {
                b'\\' => step = 2,
                b'"' if top == Shell::Quote => {
                    shell.pop();
                }
                b'`' if top == Shell::Backtick => {
                    shell.pop();
                }
                b'`' => shell.push(Shell::Backtick),
                b'$' if top == Shell::Quote => step = open_dollar(shell, &bytes[i..]),
                _ => {}
            },
            _ => match b {
                b'\\' => {
                    if code && i + 1 == bytes.len() {
                        state.word = !word_start;
                    }
                    step = 2;
                }
                b'\'' => shell.push(Shell::Single),
                b'"' => shell.push(Shell::Quote),
                b'`' => shell.push(Shell::Backtick),
                b'$' if bytes.get(i + 1) == Some(&b'\'') => {
                    shell.push(Shell::Ansi);
                    step = 2;
                }
                b'$' => step = open_dollar(shell, &bytes[i..]),
                b'#' if code && word_start => step = bytes.len() - i,
                b'<' if code
                    && (i == 0 || bytes[i - 1] != b'<')
                    && let Some((len, word)) = heredoc_opener(&line[i..]) =>
                {
                    *heredoc = Some(word);
                    step = len;
                }
                b'(' => shell.push(if top == Shell::Arithmetic {
                    Shell::Arithmetic
                } else if i > 0 && matches!(bytes[i - 1], b'<' | b'>' | b'=') {
                    Shell::Command
                } else {
                    Shell::Subshell
                }),
                b')' if top != Shell::Param => glue = shell.pop() != Some(Shell::Subshell),
                b'}' if top == Shell::Param => {
                    shell.pop();
                }
                _ => {}
            },
        }
        word_start =
            !glue && b != b'\\' && SHELL_METACHARACTERS.contains(&char::from(bytes[i + step - 1]));
        i += step;
    }
    i.min(bytes.len())
}

/// the first unescaped backquote at or after `i`, else the end of the line.
fn backtick_end(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i] != b'`' {
        i += if bytes[i] == b'\\' { 2 } else { 1 };
    }
    i.min(bytes.len())
}

/// whether a line comment may open after `tokens`.
fn comment_allowed(after: Option<&[char]>, tokens: &[CodeToken<'_>]) -> bool {
    match (after, tokens.last()) {
        (None, _) | (_, None) => true,
        (Some(after), Some(CodeToken::Punct(c))) => after.contains(c),
        (Some(_), Some(_)) => false,
    }
}

/// words after which an expression starts, so a `/` opens a regex.
const REGEX_AFTER_WORDS: &[&str] = &[
    "return",
    "typeof",
    "instanceof",
    "in",
    "of",
    "new",
    "delete",
    "void",
    "throw",
    "case",
    "do",
    "else",
    "yield",
    "await",
];

/// whether a `/` after `tokens` opens a regex rather than dividing.
fn regex_allowed(tokens: &[CodeToken<'_>]) -> bool {
    match tokens
        .iter()
        .rev()
        .find(|t| !matches!(t, CodeToken::Punct(c) if c.is_whitespace()))
    {
        None | Some(CodeToken::Comment(_)) => true,
        Some(CodeToken::Punct(c)) => !(matches!(c, ')' | ']' | '$') || c.is_alphanumeric()),
        Some(CodeToken::Keyword(word) | CodeToken::Ident(word)) => REGEX_AFTER_WORDS.contains(word),
        Some(CodeToken::StringLit(_) | CodeToken::Number(_)) => false,
    }
}

/// the end of the regex opening at `i`: past its closing `/` and flags, else the end of the line.
fn regex_end(bytes: &[u8], i: usize) -> usize {
    let mut class = false;
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 1,
            b'[' => class = true,
            b']' => class = false,
            b'/' if !class => {
                j += 1;
                while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                    j += 1;
                }
                return j;
            }
            _ => {}
        }
        j += 1;
    }
    bytes.len()
}

/// whether `before` leaves a `((` arithmetic expression open, where `<<` is a shift.
fn in_arithmetic(before: &[u8]) -> bool {
    let pairs = |pair: &[u8]| before.windows(2).filter(|w| *w == pair).count();
    pairs(b"((") > pairs(b"))")
}

/// parses a heredoc opener such as `<<EOF`, `<<-'EOF'` or `<< "EOF"` at the
/// start of `text`, returning its length and terminating word.
fn heredoc_opener(text: &str) -> Option<(usize, &str)> {
    let rest = text.strip_prefix("<<")?;
    let rest = rest
        .strip_prefix('-')
        .unwrap_or(rest)
        .trim_start_matches(' ');
    let rest = rest.strip_prefix('\\').unwrap_or(rest);
    let quote = rest.chars().next().filter(|c| matches!(c, '\'' | '"'));
    let rest = &rest[quote.map_or(0, char::len_utf8)..];
    let len = match quote {
        Some(q) => rest.find(q)?,
        None => rest.find(SHELL_METACHARACTERS).unwrap_or(rest.len()),
    };
    let word = &rest[..len];
    if !word.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        return None;
    }
    let after = &rest[len + quote.map_or(0, char::len_utf8)..];
    Some((text.len() - after.len(), word))
}

/// scans from `i` to the end of the comment or string `state` is inside,
/// returning where it stops and the state after it.
fn scan<'a>(state: Mode<'a>, bytes: &[u8], mut i: usize) -> (usize, Mode<'a>) {
    match state {
        Mode::Code => (i, state),
        Mode::Heredoc(word) if bytes.trim_ascii() == word.as_bytes() => (bytes.len(), Mode::Code),
        Mode::Heredoc(_) => (bytes.len(), state),
        Mode::Comment(close) => match bytes[i..]
            .windows(close.len())
            .position(|w| w == close.as_bytes())
        {
            Some(at) => (i + at + close.len(), Mode::Code),
            None => (bytes.len(), state),
        },
        Mode::String { delim, triple } => {
            let q = delim.quote;
            while i < bytes.len() {
                if delim.backslash && bytes[i] == b'\\' {
                    if i + 1 == bytes.len() {
                        return (bytes.len(), state);
                    }
                    i += 2;
                } else if bytes[i] != q {
                    i += 1;
                } else if triple {
                    if bytes[i..].starts_with(&[q; 3]) {
                        return (i + 3, Mode::Code);
                    }
                    i += 1;
                } else if delim.doubled && bytes.get(i + 1) == Some(&q) {
                    i += 2;
                } else {
                    return (i + 1, Mode::Code);
                }
            }
            if delim.multiline || triple {
                (i, state)
            } else {
                (i, Mode::Code)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokenize_code_line<'a>(line: &'a str, language: Option<&str>) -> Vec<CodeToken<'a>> {
        tokenize_code_block([line], language).remove(0)
    }

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
    fn shell_hash_starts_a_comment_only_at_the_start_of_a_word() {
        for language in ["shell", "bash", "shellcommand"] {
            for (line, comment) in [
                ("echo $#", None),
                ("n=${#arr[@]}", None),
                ("base=${path##*/}", None),
                ("x=${v#pre}", None),
                ("echo a#b", None),
                ("echo \"a\"#b", None),
                ("echo 'a'#b", None),
                ("echo $((16#ff))", None),
                (r"echo \ #b", None),
                ("echo {#,x}", None),
                ("cat 2>&1#b", None),
                ("# c", Some("# c")),
                ("ls -la\t# list", Some("# list")),
                ("cmd;# c", Some("# c")),
                ("a|# c", Some("# c")),
                ("(echo a)# c", Some("# c")),
                ("[ $# -eq 0 ] && exit # c", Some("# c")),
                ("x=$(date)#tag", None),
                ("a=$((16#ff))#x", None),
                ("diff <(true)#x", None),
                ("arr=(a b)#what", None),
                ("echo $(echo $(date)#in)#out", None),
                ("echo ${x:-a #b} # c", Some("# c")),
                ("((i++))#c", Some("#c")),
                ("$( (cd /tmp; pwd)#c", Some("#c")),
                ("echo `echo a #c` b", Some("#c")),
            ] {
                let tokens = &block(&[line], language)[0];
                assert_eq!(
                    comments(tokens),
                    Vec::from_iter(comment),
                    "{language}: {line}"
                );
            }
        }
    }

    #[test]
    fn shell_escaped_line_break_carries_the_word_into_the_next_line() {
        for (first, comment) in [
            (r"echo a\", None),
            (r"x=$(date)\", None),
            (r"echo a \", Some("#b")),
            (r"\", Some("#b")),
        ] {
            let lines = block(&[first, "#b"], "shellcommand");
            assert_eq!(comments(&lines[1]), Vec::from_iter(comment), "{first}");
        }
    }

    #[test]
    fn shell_double_quoted_word_ends_at_its_own_closing_quote() {
        for line in [
            r#"y="$(echo "a b")""#,
            r#"u="$(printf '%s' "$(echo "deep")")""#,
            r#"echo "${x:-"d e"}""#,
            r#"echo "`echo "bq"`""#,
            r#"V="${V:-$(rg x | sed -E 's/.*"([^"]+)".*/\1/')}""#,
            r#"echo "${x//'"'/y}""#,
            r#"echo "$(echo $'a\'b')""#,
            r#"echo "$(echo a#b "c")""#,
            r#"echo "$(( $# > 0 ))""#,
            r#"echo "$(echo $(date)#in "x")""#,
            r#"echo "$(( n << bits ))""#,
            r#"echo "$(echo "1) it's")""#,
        ] {
            let lines = block(&[line, "ls"], "shellcommand");
            assert_eq!(
                strings(&lines[0]),
                vec![&line[line.find('"').unwrap()..]],
                "{line}"
            );
            assert_eq!(lines[1], vec![CodeToken::Ident("ls")], "{line}");
        }
    }

    #[test]
    fn shell_substitution_in_a_double_quoted_word_spans_lines() {
        let lines = block(
            &[
                r#"x="$( # 1) list the "pending" files"#,
                "  cat <<EOF",
                "it's",
                "EOF",
                r#")" # c"#,
                "ls",
            ],
            "shellcommand",
        );
        assert_eq!(
            lines[0][2],
            CodeToken::StringLit(r#""$( # 1) list the "pending" files"#)
        );
        assert_eq!(lines[1], vec![CodeToken::StringLit("  cat <<EOF")]);
        assert_eq!(lines[2], vec![CodeToken::StringLit("it's")]);
        assert_eq!(strings(&lines[4]), vec![r#")""#]);
        assert_eq!(comments(&lines[4]), vec!["# c"]);
        assert_eq!(lines[5], vec![CodeToken::Ident("ls")]);
    }

    #[test]
    fn shell_hash_after_a_multiline_string_continues_the_word() {
        let lines = block(&["x='a", "b'#c # d"], "shellcommand");
        assert_eq!(comments(&lines[1]), vec!["# d"]);
    }

    #[test]
    fn yaml_comment_needs_white_space_before_it() {
        for (line, comment) in [
            ("url: http://x/#frag", None),
            ("key: a#b", None),
            ("key:#c", None),
            ("key: [a#b, c] # d", Some("# d")),
            ("key: v # c", Some("# c")),
            ("key: v\t# c", Some("# c")),
            ("# c", Some("# c")),
            ("- # c", Some("# c")),
        ] {
            let tokens = &block(&[line], "yaml")[0];
            assert_eq!(comments(tokens), Vec::from_iter(comment), "{line}");
        }
    }

    #[test]
    fn python_hash_starts_a_comment_anywhere_outside_a_string() {
        let tokens = &block(&["x=1#c 'd'"], "python")[0];
        assert_eq!(comments(tokens), vec!["#c 'd'"]);
    }

    #[test]
    fn non_smalltalk_double_quote_is_still_a_string() {
        let tokens = tokenize_code_line(r#"x = "hi""#, Some("python"));
        assert_eq!(strings(&tokens), vec![r#""hi""#]);
        assert!(comments(&tokens).is_empty());
    }

    fn block<'a>(lines: &[&'a str], language: &str) -> Vec<Vec<CodeToken<'a>>> {
        tokenize_code_block(lines.iter().copied(), Some(language))
    }

    #[test]
    fn smalltalk_comment_spans_lines() {
        let lines = block(&["x := 1. \"a", "it's here", "done\" y"], "pharo");
        assert_eq!(comments(&lines[0]), vec!["\"a"]);
        assert_eq!(lines[1], vec![CodeToken::Comment("it's here")]);
        assert_eq!(comments(&lines[2]), vec!["done\""]);
        assert_eq!(lines[2].last(), Some(&CodeToken::Ident("y")));
    }

    #[test]
    fn javascript_block_comment_spans_lines() {
        let lines = block(&["/*", " * it's \"ok\"", " */ return x"], "javascript");
        assert_eq!(lines[0], vec![CodeToken::Comment("/*")]);
        assert_eq!(lines[1], vec![CodeToken::Comment(" * it's \"ok\"")]);
        assert_eq!(comments(&lines[2]), vec![" */"]);
        assert!(lines[2].contains(&CodeToken::Keyword("return")));
    }

    #[test]
    fn python_docstring_spans_lines() {
        for q in ["\"\"\"", "'''"] {
            let open = format!("    {q}Doc.");
            let close = format!("    {q}");
            let lines = block(
                &[&open, "    for x in it's", &close, "    return 1"],
                "python",
            );
            assert_eq!(strings(&lines[0]), vec![&open[4..]]);
            assert_eq!(lines[1], vec![CodeToken::StringLit("    for x in it's")]);
            assert_eq!(lines[2], vec![CodeToken::StringLit(&close[..])]);
            assert_eq!(lines[3][4], CodeToken::Keyword("return"));
        }
    }

    #[test]
    fn python_empty_string_is_not_a_docstring() {
        let lines = block(&["x = \"\" if y", "z"], "python");
        assert_eq!(strings(&lines[0]), vec!["\"\""]);
        assert!(lines[0].contains(&CodeToken::Keyword("if")));
        assert_eq!(lines[1], vec![CodeToken::Ident("z")]);
    }

    #[test]
    fn triple_quoted_string_needs_three_quotes_to_close() {
        let lines = block(&[r#""""a "" \""" b"#, r#"c""" d"#], "python");
        assert_eq!(lines[0], vec![CodeToken::StringLit(r#""""a "" \""" b"#)]);
        assert_eq!(strings(&lines[1]), vec![r#"c""""#]);
        assert_eq!(lines[1].last(), Some(&CodeToken::Ident("d")));
    }

    #[test]
    fn smalltalk_doubled_quote_is_an_escape() {
        let lines = block(&["x := 'it''s' , y"], "pharo");
        assert_eq!(strings(&lines[0]), vec!["'it''s'"]);
        assert_eq!(lines[0].last(), Some(&CodeToken::Ident("y")));
    }

    #[test]
    fn yaml_doubled_quote_is_an_escape() {
        let tokens = &block(&["a: 'it''s' # c"], "yaml")[0];
        assert_eq!(strings(tokens), vec!["'it''s'"]);
        assert_eq!(comments(tokens), vec!["# c"]);
    }

    #[test]
    fn backslash_is_literal_in_smalltalk_shell_and_yaml_single_quotes() {
        for (language, line) in [
            ("pharo", r"p := 'C:\' , name"),
            ("shellcommand", r"echo 'C:\' name"),
            ("yaml", r"p: 'C:\' # name"),
        ] {
            let tokens = &block(&[line], language)[0];
            assert_eq!(strings(tokens), vec![r"'C:\'"], "{language}");
            assert!(
                tokens.contains(&CodeToken::Ident("name")) || comments(tokens) == vec!["# name"],
                "{language}"
            );
        }
    }

    #[test]
    fn smalltalk_character_literals_do_not_open_a_comment_or_string() {
        let lines = block(&["a := $\". b := $'. c := $é.", "d"], "pharo");
        assert_eq!(strings(&lines[0]), vec!["$\"", "$'", "$é"]);
        assert!(comments(&lines[0]).is_empty());
        assert_eq!(lines[1], vec![CodeToken::Ident("d")]);
    }

    #[test]
    fn shell_backslash_quotes_the_next_character() {
        let lines = block(&[r"echo don\'t # c", "ls"], "shellcommand");
        assert_eq!(strings(&lines[0]), vec![r"\'"]);
        assert_eq!(comments(&lines[0]), vec!["# c"]);
        assert_eq!(lines[1], vec![CodeToken::Ident("ls")]);
    }

    #[test]
    fn multiline_strings_span_lines() {
        for (language, open, close) in [
            ("pharo", "x := 'a", "b' , c"),
            ("shellcommand", "x='a", "b' c"),
            ("shellcommand", "x=\"a", "b\" c"),
            ("javascript", "x = `a", "b` + c"),
        ] {
            let lines = block(&[open, "for if", close], language);
            assert_eq!(lines[1], vec![CodeToken::StringLit("for if")], "{language}");
            assert_eq!(lines[2].last(), Some(&CodeToken::Ident("c")), "{language}");
        }
    }

    #[test]
    fn single_line_string_ends_at_end_of_line() {
        for (language, open) in [
            ("python", "x = \"open"),
            ("python", "x = 'open"),
            ("javascript", "x = 'open"),
            ("json", "\"open"),
            ("yaml", "a: 'open"),
            ("markdown", "x = \"open"),
        ] {
            let lines = block(&[open, "y"], language);
            assert_eq!(lines[1], vec![CodeToken::Ident("y")], "{language}");
        }
    }

    #[test]
    fn escaped_line_break_continues_a_single_line_string() {
        let lines = block(&[r#"s = "a \"#, r#"if" + t"#], "python");
        assert_eq!(lines[1][0], CodeToken::StringLit(r#"if""#));
        assert_eq!(lines[1].last(), Some(&CodeToken::Ident("t")));
    }

    #[test]
    fn shell_heredoc_body_is_a_string() {
        let lines = block(
            &["cat <<-'EOF' > out # c", "it's $x", "  EOF", "ls"],
            "shellcommand",
        );
        assert_eq!(strings(&lines[0]), vec!["<<-'EOF'"]);
        assert_eq!(comments(&lines[0]), vec!["# c"]);
        assert_eq!(lines[1], vec![CodeToken::StringLit("it's $x")]);
        assert_eq!(lines[2], vec![CodeToken::StringLit("  EOF")]);
        assert_eq!(lines[3], vec![CodeToken::Ident("ls")]);
    }

    #[test]
    fn shell_here_string_and_shift_are_not_heredocs() {
        for line in [
            "cat <<<word",
            "y=$((1 << 2))",
            "mask=$(( (1 << bits) - 1 ))",
            "bit=$((1<<n))",
        ] {
            let lines = block(&[line, "ls"], "shellcommand");
            assert!(strings(&lines[0]).is_empty(), "{line}");
            assert_eq!(lines[1], vec![CodeToken::Ident("ls")], "{line}");
        }
    }

    #[test]
    fn shell_heredoc_opens_after_a_closed_arithmetic_expression() {
        let lines = block(&["(( n )) && cat <<EOF", "ls", "EOF"], "shellcommand");
        assert_eq!(lines[1], vec![CodeToken::StringLit("ls")]);
    }

    #[test]
    fn shell_heredoc_word_runs_to_a_blank_or_metacharacter() {
        for (open, word) in [
            ("cat <<END-OF-TEXT", "END-OF-TEXT"),
            ("cat <<EOF. | sort", "EOF."),
            ("cat <<'A B' > out", "A B"),
            ("x=$(cat <<EOF)", "EOF"),
            ("cat <<EOF|sort", "EOF"),
            ("cat <<EOF\t> out", "EOF"),
        ] {
            let lines = block(&[open, "it's", word, "ls"], "shellcommand");
            assert_eq!(lines[1], vec![CodeToken::StringLit("it's")], "{open}");
            assert_eq!(lines[3], vec![CodeToken::Ident("ls")], "{open}");
        }
    }

    #[test]
    fn javascript_regex_literal_does_not_leak_into_the_next_lines() {
        for (line, regex) in [
            (
                "const EMAIL = /^[a-z0-9!#$%&'*+/=?^_`{|}~-]+(?:\\.[a-z0-9!#$%&'*+/=?^_`{|}~-]+)*@example\\.com$/i;",
                "/^[a-z0-9!#$%&'*+/=?^_`{|}~-]+(?:\\.[a-z0-9!#$%&'*+/=?^_`{|}~-]+)*@example\\.com$/i",
            ),
            (
                r"allowed = !ch || /[[{(,;:?/*=+\-~!|&%^<>]/.test(ch);",
                r"/[[{(,;:?/*=+\-~!|&%^<>]/",
            ),
            (r#"s = s.replace(/\/*$/, "");"#, r"/\/*$/"),
        ] {
            let lines = block(
                &[line, "const retries = 3; // a comment", "return retries;"],
                "javascript",
            );
            assert_eq!(strings(&lines[0])[0], regex);
            assert!(comments(&lines[0]).is_empty(), "{line}");
            assert_eq!(comments(&lines[1]), vec!["// a comment"], "{line}");
            assert_eq!(lines[2][0], CodeToken::Keyword("return"), "{line}");
        }
    }

    #[test]
    fn javascript_regex_follows_an_operator_keyword_or_line_start() {
        for line in [
            "/a/.test(s)",
            "  /a/.test(s)",
            "x = /a/",
            "f(/a/, /a/)",
            "[/a/]",
            "{ k: /a/ }",
            "!/a/ && /a/ || /a/",
            "c ? /a/ : /a/",
            "} /a/; /a/",
            "/* c */ /a/",
            "return /a/",
            "typeof /a/",
            "x instanceof /a/",
            "x in /a/",
            "for (x of /a/)",
            "new /a/",
            "delete /a/",
            "void /a/",
            "throw /a/",
            "case /a/:",
            "do /a/",
            "else /a/",
            "yield /a/",
            "await /a/",
        ] {
            let tokens = &block(&[line], "javascript")[0];
            assert!(strings(tokens).iter().all(|s| *s == "/a/"), "{line}");
            assert_eq!(strings(tokens).len(), line.matches("/a/").count(), "{line}");
        }
    }

    #[test]
    fn javascript_division_is_not_a_regex() {
        for line in [
            "a / b / c",
            "x = y / 2 // c",
            "arr[i] / n / 2",
            "f(x) / g(y) / 2",
            "1 / 2 / 3",
            "$ / 2 / 3",
            "\u{3c0} / 2 / 3",
        ] {
            let tokens = &block(&[line], "javascript")[0];
            assert!(strings(tokens).is_empty(), "{line}");
        }
        let tokens = &block(&["x = y / 2 // c"], "javascript")[0];
        assert_eq!(comments(tokens), vec!["// c"]);
    }

    #[test]
    fn unterminated_javascript_regex_ends_at_end_of_line() {
        let lines = block(&["x = /a`b\\", "y"], "javascript");
        assert_eq!(strings(&lines[0]), vec!["/a`b\\"]);
        assert_eq!(lines[1], vec![CodeToken::Ident("y")]);
    }

    #[test]
    fn only_javascript_lexes_regex_literals() {
        for language in ["python", "shellcommand", "pharo", "yaml", "markdown"] {
            let tokens = &block(&["ls >/dev/null"], language)[0];
            assert!(strings(tokens).is_empty(), "{language}");
        }
    }

    #[test]
    fn open_comment_or_string_does_not_leak_into_the_next_block() {
        for (language, open) in [
            ("javascript", "/* open"),
            ("pharo", "\"open"),
            ("python", "\"\"\"open"),
            ("shellcommand", "cat <<EOF"),
        ] {
            assert_ne!(
                block(&[open, "x"], language)[1],
                vec![CodeToken::Ident("x")]
            );
            assert_eq!(block(&["x"], language)[0], vec![CodeToken::Ident("x")]);
        }
    }
}
