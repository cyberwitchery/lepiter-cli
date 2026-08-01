use crate::inline_link::{LinkKind, rewrite_inline_links};
use crate::model::{Node, Page};

/// Renders a parsed page to plain text.
pub fn render_page_to_text(page: &Page) -> String {
    render_nodes_to_text(&page.content)
}

/// Renders normalized nodes to plain text.
pub fn render_nodes_to_text(nodes: &[Node]) -> String {
    render_nodes_to_text_with(nodes, &mut |_, _| None)
}

/// Renders normalized nodes to plain text, rewriting inline and explicit links
/// through `rewrite`.
///
/// `rewrite(kind, target)` is invoked for every `[[wikilink]]` and
/// `[label](target)` found in text-bearing nodes, and for every
/// [`Node::Link`] url (as [`LinkKind::Markdown`]). Returning `Some(new_target)`
/// substitutes the target; returning `None` leaves the link verbatim. A no-op
/// rewriter reproduces [`render_nodes_to_text`] exactly.
pub fn render_nodes_to_text_with(
    nodes: &[Node],
    rewrite: &mut impl FnMut(LinkKind, &str) -> Option<String>,
) -> String {
    let mut out = String::new();
    render_nodes_into(nodes, rewrite, &mut out);
    out
}

fn render_nodes_into(
    nodes: &[Node],
    rewrite: &mut impl FnMut(LinkKind, &str) -> Option<String>,
    out: &mut String,
) {
    for node in nodes {
        match node {
            Node::Heading { level, text } => {
                out.push_str(&"#".repeat((*level).max(1) as usize));
                out.push(' ');
                out.push_str(&rewrite_inline_links(text, &mut *rewrite));
                out.push_str("\n\n");
            }
            Node::Paragraph { text } => {
                out.push_str(&rewrite_inline_links(text, &mut *rewrite));
                out.push_str("\n\n");
            }
            Node::Text { text } => {
                out.push_str(&rewrite_inline_links(text, &mut *rewrite));
                out.push('\n');
            }
            Node::List { items } => {
                for item in items {
                    let mut item_out = String::new();
                    render_nodes_into(item, rewrite, &mut item_out);
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
                out.push_str("```");
                if let Some(lang) = language {
                    out.push_str(lang);
                }
                out.push('\n');
                out.push_str(code);
                out.push_str("\n```\n\n");
            }
            Node::Link { text, url } => {
                let rewritten = rewrite(LinkKind::Markdown, url).unwrap_or_else(|| url.clone());
                out.push_str(&format!("[{text}]({rewritten})\n\n"));
            }
            Node::Quote { text } => {
                out.push_str(&format!(
                    "> {}\n\n",
                    rewrite_inline_links(text, &mut *rewrite)
                ));
            }
            Node::Rewrite {
                language,
                search,
                replace,
                scope,
                is_method_pattern,
            } => {
                let lang = language.clone().unwrap_or_else(|| "rewrite".to_string());
                out.push_str(&format!("```diff {lang}\n"));
                if let Some(scope) = scope {
                    out.push_str(&format!("# scope: {scope}\n"));
                }
                if let Some(is_method_pattern) = is_method_pattern {
                    out.push_str(&format!("# method_pattern: {is_method_pattern}\n"));
                }
                for line in normalize_text(search).lines() {
                    out.push('-');
                    out.push_str(line);
                    out.push('\n');
                }
                for line in normalize_text(replace).lines() {
                    out.push('+');
                    out.push_str(line);
                    out.push('\n');
                }
                out.push_str("```\n\n");
            }
            Node::Unknown { typ, .. } => {
                out.push_str(&format!("[[unknown: {typ}]]\n\n"));
            }
        }
    }
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
