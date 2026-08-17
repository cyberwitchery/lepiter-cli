use crate::inline_link::{LinkKind, rewrite_inline_links};
use crate::model::{Node, Page};

/// whether a render escapes block-looking line starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockEscaping {
    /// for markdown `import` reads back; see [`escape_block_start`].
    Escape,
    /// for display; every line reaches the reader as the content has it.
    Verbatim,
}

/// Renders a parsed page to plain text for display.
pub fn render_page_to_text(page: &Page) -> String {
    render_nodes_to_text(&page.content)
}

/// Renders normalized nodes to plain text for display.
pub fn render_nodes_to_text(nodes: &[Node]) -> String {
    render_nodes_to_text_with(nodes, &mut |_, _| None, BlockEscaping::Verbatim)
}

/// Renders normalized nodes to plain text, rewriting inline and explicit links
/// through `rewrite`.
///
/// `rewrite(kind, target)` is invoked for every `[[wikilink]]` and
/// `[label](target)` found in text-bearing nodes, and for every
/// [`Node::Link`] url (as [`LinkKind::Markdown`]). Returning `Some(new_target)`
/// substitutes the target; returning `None` leaves the link verbatim. A no-op
/// rewriter with [`BlockEscaping::Verbatim`] reproduces
/// [`render_nodes_to_text`] exactly.
pub fn render_nodes_to_text_with(
    nodes: &[Node],
    rewrite: &mut impl FnMut(LinkKind, &str) -> Option<String>,
    escaping: BlockEscaping,
) -> String {
    let mut out = String::new();
    render_nodes_into(nodes, rewrite, escaping, Position::Snippet, &mut out);
    out
}

/// whether a node is a snippet of its own or sits inside a list item.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Position {
    Snippet,
    ListItem,
}

fn render_nodes_into(
    nodes: &[Node],
    rewrite: &mut impl FnMut(LinkKind, &str) -> Option<String>,
    escaping: BlockEscaping,
    position: Position,
    out: &mut String,
) {
    for node in nodes {
        match node {
            Node::Heading { level, text } => {
                let text = rewrite_inline_links(text, &mut *rewrite);
                let mut lines = block_lines(&text).into_iter();
                out.push_str(&"#".repeat((*level).max(1) as usize));
                out.push(' ');
                out.push_str(lines.next().unwrap_or(""));
                out.push('\n');
                for line in lines {
                    push_block_line(line, escaping, out);
                }
                out.push('\n');
            }
            Node::Paragraph { text } | Node::Text { text } => {
                let text = rewrite_inline_links(text, &mut *rewrite);
                if escaping == BlockEscaping::Escape
                    && position == Position::Snippet
                    && !text.contains('\n')
                    && is_standalone_link(&text)
                {
                    out.push('\\');
                }
                push_block_lines(&text, escaping, out);
                out.push('\n');
            }
            Node::List { items } => {
                for item in items {
                    let mut item_out = String::new();
                    render_nodes_into(item, rewrite, escaping, Position::ListItem, &mut item_out);
                    let mut lines = item_out.trim().lines();
                    if let Some(first) = lines.next() {
                        out.push_str("- ");
                        out.push_str(first);
                        out.push('\n');
                        for line in lines {
                            out.push_str("  ");
                            out.push_str(line);
                            out.push('\n');
                        }
                    }
                }
                out.push('\n');
            }
            Node::Code { language, code } => {
                let fence = fence_for(code);
                out.push_str(&fence);
                if let Some(lang) = language {
                    out.push_str(lang);
                }
                out.push('\n');
                out.push_str(code);
                out.push('\n');
                out.push_str(&fence);
                out.push_str("\n\n");
            }
            Node::Link { text, url } => {
                let rewritten = rewrite(LinkKind::Markdown, url).unwrap_or_else(|| url.clone());
                out.push_str(&format!("[{text}]({rewritten})\n\n"));
            }
            Node::Quote { text } => {
                let text = rewrite_inline_links(text, &mut *rewrite);
                for line in block_lines(&text) {
                    if line.is_empty() {
                        out.push_str(">\n");
                    } else {
                        out.push_str("> ");
                        out.push_str(line);
                        out.push('\n');
                    }
                }
                out.push('\n');
            }
            Node::Rewrite {
                language,
                search,
                replace,
                scope,
                is_method_pattern,
            } => {
                let lang = language.clone().unwrap_or_else(|| "rewrite".to_string());
                let mut body = String::new();
                if let Some(scope) = scope {
                    body.push_str(&format!("# scope: {scope}\n"));
                }
                if let Some(is_method_pattern) = is_method_pattern {
                    body.push_str(&format!("# method_pattern: {is_method_pattern}\n"));
                }
                for line in normalize_text(search).lines() {
                    body.push('-');
                    body.push_str(line);
                    body.push('\n');
                }
                for line in normalize_text(replace).lines() {
                    body.push('+');
                    body.push_str(line);
                    body.push('\n');
                }
                let fence = fence_for(&body);
                out.push_str(&format!("{fence}diff {lang}\n{body}{fence}\n\n"));
            }
            Node::Unknown { typ, .. } => {
                out.push_str(&format!("[[unknown: {typ}]]\n\n"));
            }
        }
    }
}

fn block_lines(text: &str) -> Vec<&str> {
    text.split('\n').collect()
}

fn push_block_lines(text: &str, escaping: BlockEscaping, out: &mut String) {
    for line in block_lines(text) {
        push_block_line(line, escaping, out);
    }
}

fn push_block_line(line: &str, escaping: BlockEscaping, out: &mut String) {
    match escaping {
        BlockEscaping::Escape => out.push_str(&escape_block_start(line)),
        BlockEscaping::Verbatim => out.push_str(line),
    }
    out.push('\n');
}

/// a backtick fence long enough that no line of `content` can close it.
fn fence_for(content: &str) -> String {
    let mut longest = 0usize;
    let mut run = 0usize;
    for c in content.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

/// prefixes `line` with a backslash if a line-oriented markdown reader would
/// take it for the start of a block.
pub fn escape_block_start(line: &str) -> String {
    if starts_block(line) {
        format!("\\{line}")
    } else {
        line.to_string()
    }
}

/// inverse of [`escape_block_start`].
pub fn unescape_block_start(line: &str) -> String {
    line.strip_prefix('\\').unwrap_or(line).to_string()
}

fn starts_block(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty()
        || line.starts_with('\\')
        || line.starts_with("- ")
        || line.starts_with("> ")
        || line == ">"
        || line.starts_with("```")
        || (trimmed.starts_with("[[unknown: ") && trimmed.ends_with("]]"))
}

/// whether `line`, ignoring surrounding whitespace, is one `[label](target)`
/// markdown link and nothing else.
///
/// the markdown `import` reads such a line back as a link snippet, but only
/// where it is a whole snippet on its own.
pub fn is_standalone_link(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') {
        return false;
    }
    let Some(bracket_end) = trimmed.find("](") else {
        return false;
    };
    let after = &trimmed[bracket_end + 2..];
    after.ends_with(')') && !after[..after.len() - 1].contains(')')
}

/// Checks whether the rendered text of a page contains `needle`
/// (case-insensitive) without allocating the full rendered string.
///
/// Walks nodes one at a time and returns `true` on the first match.
pub fn page_content_contains(page: &Page, needle: &str) -> bool {
    let needle: String = needle.chars().flat_map(char::to_lowercase).collect();
    let mut buf = String::new();
    nodes_contain(&page.content, &needle, &mut buf)
}

/// Substring check against the lowercased `text`, reusing `buf` to avoid
/// allocating a new lowercased string on every call. `needle` must already
/// be lowercased the same way by the caller.
fn node_text_contains(text: &str, needle: &str, buf: &mut String) -> bool {
    buf.clear();
    buf.extend(text.chars().flat_map(char::to_lowercase));
    buf.contains(needle)
}

fn nodes_contain(nodes: &[Node], needle: &str, buf: &mut String) -> bool {
    for node in nodes {
        match node {
            Node::Heading { text, .. }
            | Node::Paragraph { text }
            | Node::Text { text }
            | Node::Quote { text } => {
                if node_text_contains(text, needle, buf) {
                    return true;
                }
            }
            Node::Code { language, code } => {
                if node_text_contains(code, needle, buf) {
                    return true;
                }
                if let Some(lang) = language
                    && node_text_contains(lang, needle, buf)
                {
                    return true;
                }
            }
            Node::Link { text, url } => {
                if node_text_contains(text, needle, buf) || node_text_contains(url, needle, buf) {
                    return true;
                }
            }
            Node::List { items } => {
                for item in items {
                    if nodes_contain(item, needle, buf) {
                        return true;
                    }
                }
            }
            Node::Rewrite {
                language,
                search,
                replace,
                scope,
                ..
            } => {
                if node_text_contains(search, needle, buf)
                    || node_text_contains(replace, needle, buf)
                {
                    return true;
                }
                if let Some(lang) = language
                    && node_text_contains(lang, needle, buf)
                {
                    return true;
                }
                if let Some(s) = scope
                    && node_text_contains(s, needle, buf)
                {
                    return true;
                }
            }
            Node::Unknown { typ, .. } => {
                if node_text_contains(typ, needle, buf) {
                    return true;
                }
            }
        }
    }
    false
}

pub fn normalize_text(input: &str) -> String {
    input.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn render_escaped(nodes: &[Node]) -> String {
        render_nodes_to_text_with(nodes, &mut |_, _| None, BlockEscaping::Escape)
    }

    #[test]
    fn render_nodes_outputs_unknown_placeholder() {
        let text = render_nodes_to_text(&[
            Node::Paragraph {
                text: "para".to_string(),
            },
            Node::Rewrite {
                language: Some("pharo".to_string()),
                search: "a".to_string(),
                replace: "b".to_string(),
                scope: None,
                is_method_pattern: Some(true),
            },
            Node::Unknown {
                typ: "weird".to_string(),
                raw: json!({"a":1}),
            },
        ]);
        assert!(text.contains("para"));
        assert!(text.contains("```diff pharo"));
        assert!(text.contains("-a"));
        assert!(text.contains("+b"));
        assert!(text.contains("[[unknown: weird]]"));
    }

    #[test]
    fn render_nodes_to_text_with_rewrites_inline_and_explicit_links() {
        let mut rewrite = |kind: LinkKind, target: &str| match (kind, target) {
            (LinkKind::Wiki, "Topic") => Some("topic.md".to_string()),
            (LinkKind::Markdown, "page:abc") => Some("alpha.md".to_string()),
            _ => None,
        };
        let text = render_nodes_to_text_with(
            &[
                Node::Paragraph {
                    text: "see [[Topic]] and [x](page:abc)".to_string(),
                },
                Node::Link {
                    text: "go".to_string(),
                    url: "page:abc".to_string(),
                },
            ],
            &mut rewrite,
            BlockEscaping::Escape,
        );
        assert!(text.contains("see [Topic](topic.md) and [x](alpha.md)"));
        assert!(text.contains("[go](alpha.md)"));
    }

    #[test]
    fn render_nodes_to_text_noop_matches_plain_render() {
        let nodes = vec![
            Node::Heading {
                level: 1,
                text: "see [[Topic]]".to_string(),
            },
            Node::Paragraph {
                text: "a [x](page:abc) b".to_string(),
            },
            Node::Link {
                text: "go".to_string(),
                url: "page:abc".to_string(),
            },
        ];
        // The default renderer must leave every link untouched.
        let plain = render_nodes_to_text(&nodes);
        assert!(plain.contains("see [[Topic]]"));
        assert!(plain.contains("a [x](page:abc) b"));
        assert!(plain.contains("[go](page:abc)"));
    }

    #[test]
    fn fence_widens_past_the_longest_backtick_run() {
        assert_eq!(fence_for("plain code"), "```");
        assert_eq!(fence_for("a ` b"), "```");
        assert_eq!(fence_for("```"), "````");
        assert_eq!(fence_for("outer\n`````\ninner"), "``````");
    }

    #[test]
    fn code_block_fence_outgrows_a_nested_fence() {
        let text = render_nodes_to_text(&[Node::Code {
            language: Some("python".to_string()),
            code: "doc = '''\n```\nnested\n```\n'''".to_string(),
        }]);
        assert_eq!(
            text,
            "````python\ndoc = '''\n```\nnested\n```\n'''\n````\n\n"
        );
    }

    #[test]
    fn escape_block_start_covers_every_block_opener() {
        assert_eq!(escape_block_start("- item"), "\\- item");
        assert_eq!(escape_block_start("> quote"), "\\> quote");
        assert_eq!(escape_block_start(">"), "\\>");
        assert_eq!(escape_block_start("```rust"), "\\```rust");
        assert_eq!(escape_block_start("[[unknown: x]]"), "\\[[unknown: x]]");
        assert_eq!(escape_block_start("\\already"), "\\\\already");
        assert_eq!(escape_block_start(""), "\\");
        assert_eq!(escape_block_start("  "), "\\  ");
    }

    #[test]
    fn line_escaping_leaves_a_standalone_link_to_the_node_renderer() {
        assert_eq!(escape_block_start("[label](url)"), "[label](url)");
        assert_eq!(escape_block_start("  [pad](url)  "), "  [pad](url)  ");
    }

    #[test]
    fn escape_block_start_leaves_ordinary_prose_alone() {
        for line in [
            "plain",
            "-dash",
            "a - b",
            ">>chevron",
            "``inline``",
            "see [label](url) below",
            "[label](url) trails off",
            "[unclosed](url",
            "[no target]",
        ] {
            assert_eq!(escape_block_start(line), line);
        }
    }

    #[test]
    fn escape_block_start_matches_the_importer_on_a_hand_escaped_link() {
        assert_eq!(escape_block_start("\\[label](url)"), "\\\\[label](url)");
        assert!(!is_standalone_link("\\[label](url)"));
    }

    #[test]
    fn unescape_block_start_inverts_escape_block_start() {
        for line in [
            "plain",
            "",
            "- item",
            "> quote",
            ">",
            "```rust",
            "[[unknown: x]]",
            "\\already",
            "\\\\twice",
            "\\- hand-escaped",
            "  ",
            "\\ ",
            "[label](url)",
            "  [pad](url)  ",
            "\\[label](url)",
        ] {
            assert_eq!(unescape_block_start(&escape_block_start(line)), line);
        }
    }

    #[test]
    fn multi_line_nodes_escape_every_line() {
        let text = render_escaped(&[Node::Paragraph {
            text: "intro\n- not a list\n> not a quote".to_string(),
        }]);
        assert_eq!(text, "intro\n\\- not a list\n\\> not a quote\n\n");
    }

    #[test]
    fn heading_escapes_continuation_lines_only() {
        let text = render_escaped(&[Node::Heading {
            level: 2,
            text: "title\n- not a list".to_string(),
        }]);
        assert_eq!(text, "## title\n\\- not a list\n\n");
    }

    #[test]
    fn quote_marks_every_line() {
        let text = render_nodes_to_text(&[Node::Quote {
            text: "first\n\nthird".to_string(),
        }]);
        assert_eq!(text, "> first\n>\n> third\n\n");
    }

    #[test]
    fn blank_lines_inside_a_block_are_escaped() {
        let text = render_escaped(&[Node::Paragraph {
            text: "para one\n\npara two".to_string(),
        }]);
        assert_eq!(text, "para one\n\\\npara two\n\n");
    }

    #[test]
    fn trailing_and_leading_blank_lines_are_escaped() {
        let text = render_escaped(&[Node::Paragraph {
            text: "\nbody\n".to_string(),
        }]);
        assert_eq!(text, "\\\nbody\n\\\n\n");
    }

    #[test]
    fn whitespace_only_text_node_is_escaped_and_separated() {
        let text = render_escaped(&[
            Node::Text {
                text: "  ".to_string(),
            },
            Node::Paragraph {
                text: "after".to_string(),
            },
        ]);
        assert_eq!(text, "\\  \n\nafter\n\n");
    }

    #[test]
    fn text_node_that_is_only_a_link_is_escaped() {
        let text = render_escaped(&[Node::Text {
            text: "[label](https://example.com)".to_string(),
        }]);
        assert_eq!(text, "\\[label](https://example.com)\n\n");
        let padded = render_escaped(&[Node::Text {
            text: "  [pad](https://example.com)  ".to_string(),
        }]);
        assert_eq!(padded, "\\  [pad](https://example.com)  \n\n");
    }

    #[test]
    fn a_link_line_among_others_is_left_a_working_link() {
        let text = render_escaped(&[Node::Text {
            text: "intro\n[label](https://example.com)\noutro".to_string(),
        }]);
        assert_eq!(text, "intro\n[label](https://example.com)\noutro\n\n");
    }

    #[test]
    fn a_list_item_that_is_a_link_is_left_a_working_link() {
        let text = render_escaped(&[Node::List {
            items: vec![
                vec![Node::Text {
                    text: "[label](https://example.com)".to_string(),
                }],
                vec![Node::Text {
                    text: "plain".to_string(),
                }],
            ],
        }]);
        assert_eq!(text, "- [label](https://example.com)\n- plain\n\n");
    }

    #[test]
    fn a_heading_continuation_that_is_a_link_is_left_a_working_link() {
        let text = render_escaped(&[Node::Heading {
            level: 2,
            text: "title\n[label](https://example.com)".to_string(),
        }]);
        assert_eq!(text, "## title\n[label](https://example.com)\n\n");
    }

    #[test]
    fn link_node_renders_unescaped() {
        let text = render_escaped(&[Node::Link {
            text: "label".to_string(),
            url: "https://example.com".to_string(),
        }]);
        assert_eq!(text, "[label](https://example.com)\n\n");
    }

    #[test]
    fn render_emits_carriage_returns_verbatim_though_import_drops_them() {
        let text = render_escaped(&[Node::Paragraph {
            text: "one\r\ntwo".to_string(),
        }]);
        assert_eq!(text, "one\r\ntwo\n\n");
    }

    #[test]
    fn display_render_leaves_block_looking_prose_alone() {
        let text = render_nodes_to_text(&[Node::Paragraph {
            text: "intro\n- not a list\n> not a quote\n```not a fence\n[not](a-link-snippet)"
                .to_string(),
        }]);
        assert_eq!(
            text,
            "intro\n- not a list\n> not a quote\n```not a fence\n[not](a-link-snippet)\n\n"
        );
    }

    #[test]
    fn display_render_keeps_a_blank_line_blank() {
        let text = render_nodes_to_text(&[Node::Paragraph {
            text: "para one\n\npara two".to_string(),
        }]);
        assert_eq!(text, "para one\n\npara two\n\n");
    }

    #[test]
    fn display_render_does_not_double_a_leading_backslash() {
        let text = render_nodes_to_text(&[Node::Paragraph {
            text: "\\newcommand{\\foo}{bar}".to_string(),
        }]);
        assert_eq!(text, "\\newcommand{\\foo}{bar}\n\n");
    }

    #[test]
    fn display_render_leaves_heading_continuation_lines_alone() {
        let text = render_nodes_to_text(&[Node::Heading {
            level: 2,
            text: "title\n- not a list".to_string(),
        }]);
        assert_eq!(text, "## title\n- not a list\n\n");
    }

    #[test]
    fn render_list_single_line_items() {
        let text = render_nodes_to_text(&[Node::List {
            items: vec![
                vec![Node::Paragraph {
                    text: "first".to_string(),
                }],
                vec![Node::Paragraph {
                    text: "second".to_string(),
                }],
            ],
        }]);
        assert_eq!(text, "- first\n- second\n\n");
    }

    #[test]
    fn render_list_item_with_code_block() {
        let text = render_nodes_to_text(&[Node::List {
            items: vec![vec![Node::Code {
                language: Some("py".to_string()),
                code: "x = 1\ny = 2".to_string(),
            }]],
        }]);
        assert_eq!(text, "- ```py\n  x = 1\n  y = 2\n  ```\n\n");
    }

    #[test]
    fn render_list_item_with_multiple_nodes() {
        let text = render_nodes_to_text(&[Node::List {
            items: vec![vec![
                Node::Paragraph {
                    text: "intro".to_string(),
                },
                Node::Code {
                    language: None,
                    code: "code".to_string(),
                },
            ]],
        }]);
        // First line starts with "- ", continuation lines with "  "
        let lines: Vec<&str> = text.trim().lines().collect();
        assert_eq!(lines[0], "- intro");
        for line in &lines[1..] {
            assert!(
                line.starts_with("  "),
                "continuation line not indented: {line:?}"
            );
        }
    }

    fn make_page(nodes: Vec<Node>) -> Page {
        Page {
            id: "test".to_string(),
            title: "Test".to_string(),
            updated_at: None,
            tags: Vec::new(),
            content: nodes,
        }
    }

    #[test]
    fn page_content_contains_matches_paragraph() {
        let page = make_page(vec![Node::Paragraph {
            text: "the quick brown fox".to_string(),
        }]);
        assert!(page_content_contains(&page, "quick"));
        assert!(!page_content_contains(&page, "lazy"));
    }

    #[test]
    fn page_content_contains_case_insensitive() {
        let page = make_page(vec![Node::Paragraph {
            text: "Hello World".to_string(),
        }]);
        assert!(page_content_contains(&page, "hello world"));
        assert!(page_content_contains(&page, "hello"));
        assert!(page_content_contains(&page, "Hello"));
        assert!(page_content_contains(&page, "WORLD"));
        assert!(page_content_contains(&page, "HeLLo WoRLd"));
        assert!(!page_content_contains(&page, "GOODBYE"));
    }

    #[test]
    fn page_content_contains_case_insensitive_non_ascii() {
        let page = make_page(vec![Node::Paragraph {
            text: "Über Café".to_string(),
        }]);
        assert!(page_content_contains(&page, "über"));
        assert!(page_content_contains(&page, "ÜBER"));
        assert!(page_content_contains(&page, "CAFÉ"));
    }

    /// Haystack and needle must be lowercased identically. `str::to_lowercase`
    /// maps a word-final sigma to `ς` where `char::to_lowercase` yields `σ`, so
    /// using it for the needle here would break the match.
    #[test]
    fn page_content_contains_lowercases_needle_per_char() {
        let page = make_page(vec![Node::Paragraph {
            text: "ΟΔΟΣ".to_string(),
        }]);
        assert!(page_content_contains(&page, "ΟΔΟΣ"));
        assert!(page_content_contains(&page, "οδοσ"));
    }

    #[test]
    fn page_content_contains_matches_heading() {
        let page = make_page(vec![Node::Heading {
            level: 2,
            text: "Important Section".to_string(),
        }]);
        assert!(page_content_contains(&page, "important"));
    }

    #[test]
    fn page_content_contains_matches_code() {
        let page = make_page(vec![Node::Code {
            language: Some("rust".to_string()),
            code: "fn main() {}".to_string(),
        }]);
        assert!(page_content_contains(&page, "fn main"));
        assert!(page_content_contains(&page, "rust"));
    }

    #[test]
    fn page_content_contains_matches_link() {
        let page = make_page(vec![Node::Link {
            text: "click here".to_string(),
            url: "https://example.com".to_string(),
        }]);
        assert!(page_content_contains(&page, "click"));
        assert!(page_content_contains(&page, "example.com"));
    }

    #[test]
    fn page_content_contains_matches_quote() {
        let page = make_page(vec![Node::Quote {
            text: "to be or not to be".to_string(),
        }]);
        assert!(page_content_contains(&page, "not to be"));
    }

    #[test]
    fn page_content_contains_matches_list_items() {
        let page = make_page(vec![Node::List {
            items: vec![
                vec![Node::Paragraph {
                    text: "first item".to_string(),
                }],
                vec![Node::Paragraph {
                    text: "second item".to_string(),
                }],
            ],
        }]);
        assert!(page_content_contains(&page, "second"));
        assert!(!page_content_contains(&page, "third"));
    }

    #[test]
    fn page_content_contains_matches_rewrite() {
        let page = make_page(vec![Node::Rewrite {
            language: Some("pharo".to_string()),
            search: "oldMethod".to_string(),
            replace: "newMethod".to_string(),
            scope: Some("MyClass".to_string()),
            is_method_pattern: None,
        }]);
        assert!(page_content_contains(&page, "oldmethod"));
        assert!(page_content_contains(&page, "newmethod"));
        assert!(page_content_contains(&page, "pharo"));
        assert!(page_content_contains(&page, "myclass"));
    }

    #[test]
    fn page_content_contains_matches_unknown_type() {
        let page = make_page(vec![Node::Unknown {
            typ: "wardleyMap".to_string(),
            raw: json!({}),
        }]);
        assert!(page_content_contains(&page, "wardley"));
    }

    #[test]
    fn page_content_contains_early_termination() {
        let page = make_page(vec![
            Node::Paragraph {
                text: "match here".to_string(),
            },
            Node::Paragraph {
                text: "no match".to_string(),
            },
        ]);
        assert!(page_content_contains(&page, "match here"));
    }

    #[test]
    fn page_content_contains_empty_content() {
        let page = make_page(vec![]);
        assert!(!page_content_contains(&page, "anything"));
    }

    #[test]
    fn page_content_contains_matches_text_node() {
        let page = make_page(vec![Node::Text {
            text: "plain text line".to_string(),
        }]);
        assert!(page_content_contains(&page, "plain text"));
    }
}
