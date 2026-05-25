use crate::model::{Node, Page};

/// Renders a parsed page to plain text.
pub fn render_page_to_text(page: &Page) -> String {
    render_nodes_to_text(&page.content)
}

/// Renders normalized nodes to plain text.
pub fn render_nodes_to_text(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Heading { level, text } => {
                out.push_str(&"#".repeat((*level).max(1) as usize));
                out.push(' ');
                out.push_str(text);
                out.push_str("\n\n");
            }
            Node::Paragraph { text } => {
                out.push_str(text);
                out.push_str("\n\n");
            }
            Node::Text { text } => {
                out.push_str(text);
                out.push('\n');
            }
            Node::List { items } => {
                for item in items {
                    let rendered = render_nodes_to_text(item);
                    let mut lines = rendered.trim().lines();
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
                out.push_str(&format!("[{text}]({url})\n\n"));
            }
            Node::Quote { text } => {
                out.push_str(&format!("> {text}\n\n"));
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
    out
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
}
