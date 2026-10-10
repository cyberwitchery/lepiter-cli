use crate::highlight::{CodeToken, tokenize_code_block};
use crate::inline;
use lepiter_core::{Page, closes_fence, open_fence, render_page_to_text};

pub use crate::util::truncate_chars;

pub fn render_page_pretty(page: &Page, colored: bool) -> String {
    let mut out = String::new();
    if colored {
        out.push_str(&format!(
            "{}\n\n",
            ansi("1;36", &format!("# {}", page.title))
        ));
    } else {
        out.push_str(&format!("# {}\n\n", page.title));
    }
    if !page.tags.is_empty() {
        let line = format!("tags: {}\n", page.tags.join(", "));
        if colored {
            out.push_str(&ansi("2", line.trim_end()));
            out.push('\n');
        } else {
            out.push_str(&line);
        }
    }
    if let Some(updated_at) = page.updated_at {
        let line = format!("updated: {}\n", updated_at.to_rfc3339());
        if colored {
            out.push_str(&ansi("2", line.trim_end()));
            out.push('\n');
        } else {
            out.push_str(&line);
        }
    }
    if colored {
        out.push_str(&format!("{}\n\n", ansi("2", &format!("id: {}", page.id))));
        out.push_str(&format!("{}\n\n", ansi("2", "---")));
    } else {
        out.push_str(&format!("id: {}\n\n", page.id));
        out.push_str("---\n\n");
    }

    let body = render_page_to_text(page);
    if colored {
        let styled = render_markdown_with_ansi(body.trim());
        out.push_str(styled.strip_suffix('\n').unwrap_or(&styled));
    } else {
        out.push_str(body.trim());
    }
    out.push('\n');
    out
}

/// a fenced block being read, with the indent its lines carry in a list item.
struct CodeBlock<'a> {
    indent: String,
    fence_len: usize,
    language: Option<String>,
    lines: Vec<&'a str>,
}

fn render_markdown_with_ansi(markdown: &str) -> String {
    let mut out = String::new();
    let mut in_list = false;
    let mut code: Option<CodeBlock> = None;

    for line in markdown.lines() {
        if let Some(mut block) = code.take() {
            match line.strip_prefix(block.indent.as_str()) {
                Some(rest) if closes_fence(rest, block.fence_len) => {
                    push_code_block_ansi(&mut out, &block);
                    out.push_str(&block.indent);
                    out.push_str(&ansi("90", rest));
                    out.push('\n');
                    continue;
                }
                Some(rest) => {
                    block.lines.push(rest);
                    code = Some(block);
                    continue;
                }
                None => push_code_block_ansi(&mut out, &block),
            }
        }

        in_list = line.starts_with("- ") || (in_list && line.starts_with("  "));
        let (prefix, rest) = if in_list {
            split_list_prefix(line)
        } else {
            ("", line)
        };
        if let Some((fence_len, info)) = open_fence(rest) {
            out.push_str(prefix);
            out.push_str(&ansi("90", rest));
            out.push('\n');
            code = Some(CodeBlock {
                indent: " ".repeat(prefix.len()),
                fence_len,
                language: (!info.is_empty()).then(|| info.to_lowercase()),
                lines: Vec::new(),
            });
            continue;
        }

        if line.starts_with('#') {
            out.push_str(&ansi("1;36", line));
        } else if line.starts_with("> ") {
            out.push_str(&ansi("3;90", line));
        } else if let Some(stripped) = line.strip_prefix("- ") {
            out.push_str("- ");
            out.push_str(&style_inline_markdown_ansi(stripped));
        } else if line.starts_with("[[unknown: ") {
            out.push_str(&ansi("33", line));
        } else {
            out.push_str(&style_inline_markdown_ansi(line));
        }
        out.push('\n');
    }
    if let Some(block) = code {
        push_code_block_ansi(&mut out, &block);
    }

    out
}

/// splits the writer's list-item prefix, `- ` or two spaces per level, off `line`.
fn split_list_prefix(line: &str) -> (&str, &str) {
    let mut rest = line;
    while let Some(next) = rest.strip_prefix("- ").or_else(|| rest.strip_prefix("  ")) {
        rest = next;
    }
    line.split_at(line.len() - rest.len())
}

fn style_inline_markdown_ansi(text: &str) -> String {
    let elements = inline::parse_inline(text);
    let mut out = String::new();

    for elem in elements {
        match elem {
            inline::InlineElement::Styled {
                text,
                bold,
                italic,
                code,
            } => {
                if code {
                    out.push_str(&ansi("33", &text));
                } else {
                    let style = match (bold, italic) {
                        (true, true) => Some("1;3"),
                        (true, false) => Some("1"),
                        (false, true) => Some("3"),
                        (false, false) => None,
                    };
                    if let Some(style) = style {
                        out.push_str(&ansi(style, &text));
                    } else {
                        out.push_str(&text);
                    }
                }
            }
            inline::InlineElement::Link { label, target } => {
                out.push_str(&ansi("4;94", &label));
                out.push_str(&ansi("90", &format!(" ({target})")));
            }
            inline::InlineElement::Image { alt, target } => {
                out.push_str(&ansi("3;96", &alt));
                out.push_str(&ansi("90", &format!(" ({target})")));
            }
            inline::InlineElement::WikiLink { text } => {
                out.push_str(&ansi("4;94", &text));
            }
            inline::InlineElement::Annotation { text } => {
                out.push_str(&ansi("1;35", &text));
            }
        }
    }

    out
}

fn ansi(style: &str, text: &str) -> String {
    format!("\x1b[{style}m{text}\x1b[0m")
}

fn push_code_block_ansi(out: &mut String, block: &CodeBlock) {
    let lines = block.lines.iter().copied();
    for tokens in tokenize_code_block(lines, block.language.as_deref()) {
        out.push_str(&block.indent);
        for tok in tokens {
            match tok {
                CodeToken::Comment(s) => out.push_str(&ansi("90", s)),
                CodeToken::StringLit(s) => out.push_str(&ansi("32", s)),
                CodeToken::Number(s) => out.push_str(&ansi("33", s)),
                CodeToken::Keyword(s) => out.push_str(&ansi("1;35", s)),
                CodeToken::Ident(s) => out.push_str(s),
                CodeToken::Punct(c) => out.push(c),
            }
        }
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lepiter_core::{Node, render_nodes_to_text};

    fn colour(nodes: &[Node]) -> String {
        render_markdown_with_ansi(render_nodes_to_text(nodes).trim())
    }

    fn strip_ansi(text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(start) = rest.find("\x1b[") {
            out.push_str(&rest[..start]);
            let end = rest[start..].find('m').expect("unterminated escape");
            rest = &rest[start + end + 1..];
        }
        out.push_str(rest);
        out
    }

    #[test]
    fn a_widened_fence_holds_a_three_backtick_line() {
        let out = colour(&[
            Node::Code {
                language: Some("python".into()),
                code: "x = 1\n```\n# c".into(),
            },
            Node::Paragraph {
                text: "after *e*".into(),
            },
        ]);
        assert!(out.starts_with("\x1b[90m````python\x1b[0m\n"), "{out:?}");
        assert!(out.contains("\n```\n\x1b[90m# c\x1b[0m\n"), "{out:?}");
        assert!(out.contains("\n\x1b[90m````\x1b[0m\n"), "{out:?}");
        assert!(out.ends_with("\nafter \x1b[3me\x1b[0m\n"), "{out:?}");
    }

    #[test]
    fn code_in_a_list_item_is_highlighted() {
        let out = colour(&[Node::List {
            items: vec![
                vec![Node::Code {
                    language: Some("python".into()),
                    code: "x = 1 # c\n`a` *b*".into(),
                }],
                vec![
                    Node::Paragraph {
                        text: "intro".into(),
                    },
                    Node::List {
                        items: vec![vec![Node::Code {
                            language: None,
                            code: "[l](t)".into(),
                        }]],
                    },
                ],
            ],
        }]);
        let expected = "- \x1b[90m```python\x1b[0m\n\
                        \x20 x = \x1b[33m1\x1b[0m \x1b[90m# c\x1b[0m\n\
                        \x20 `a` *b*\n\
                        \x20 \x1b[90m```\x1b[0m\n\
                        - intro\n\
                        \x20 \n\
                        \x20 - \x1b[90m```\x1b[0m\n\
                        \x20   [l](t)\n\
                        \x20   \x1b[90m```\x1b[0m\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn a_rewrite_block_with_a_widened_fence_closes() {
        let out = colour(&[
            Node::Rewrite {
                language: Some("python".into()),
                search: "a ```".into(),
                replace: "b".into(),
                scope: None,
                is_method_pattern: None,
            },
            Node::Paragraph { text: "*e*".into() },
        ]);
        assert!(
            out.starts_with("\x1b[90m````diff python\x1b[0m\n-a ```\n+b\n"),
            "{out:?}"
        );
        assert!(
            out.ends_with("\x1b[90m````\x1b[0m\n\n\x1b[3me\x1b[0m\n"),
            "{out:?}"
        );
    }

    #[test]
    fn a_list_fence_ends_with_its_item() {
        let out = render_markdown_with_ansi("- ```\n  x 1\n\n*e*");
        assert!(
            out.ends_with("\n  x \x1b[33m1\x1b[0m\n\n\x1b[3me\x1b[0m\n"),
            "{out:?}"
        );
    }

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }

        fn lines(&mut self, words: &[&str]) -> String {
            let count = 1 + self.below(3);
            let lines: Vec<&str> = (0..count).map(|_| words[self.below(words.len())]).collect();
            lines.join("\n")
        }
    }

    // only the highlighter colours the digits in TEXT and the `"q"` in CODE
    const TEXT: &[&str] = &["alpha 1", "beta 22", "", "gamma 3 delta"];
    const CODE: &[&str] = &[
        "\"q\"",
        "` \"q\"",
        "``x`` \"q\"",
        "```",
        "````",
        "`````",
        "  ```",
        "- \"q\"",
        "> \"q\"",
        "# h \"q\"",
        "*e* [a](b) \"q\"",
        "",
    ];

    fn node(rng: &mut Rng, depth: usize) -> Node {
        match rng.below(if depth < 3 { 6 } else { 5 }) {
            0 => Node::Paragraph {
                text: rng.lines(TEXT),
            },
            1 => Node::Quote {
                text: rng.lines(TEXT),
            },
            2 => Node::Heading {
                level: 1 + rng.below(3) as u8,
                text: rng.lines(&TEXT[..2]),
            },
            3 => Node::Code {
                language: [None, Some("text".to_string())][rng.below(2)].clone(),
                code: rng.lines(CODE),
            },
            4 => Node::Rewrite {
                language: [None, Some("python".to_string())][rng.below(2)].clone(),
                search: rng.lines(&CODE[..CODE.len() - 1]),
                replace: rng.lines(&CODE[..CODE.len() - 1]),
                scope: [None, Some("\"q\"".to_string())][rng.below(2)].clone(),
                is_method_pattern: None,
            },
            _ => Node::List {
                items: (0..1 + rng.below(3))
                    .map(|_| {
                        (0..1 + rng.below(3))
                            .map(|_| node(rng, depth + 1))
                            .collect()
                    })
                    .collect(),
            },
        }
    }

    #[test]
    fn colour_output_keeps_the_plain_bytes_and_highlights_only_code() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..500 {
            let page = Page {
                id: "id".into(),
                title: "t".into(),
                updated_at: None,
                tags: Vec::new(),
                content: (0..1 + rng.below(5)).map(|_| node(&mut rng, 0)).collect(),
            };
            let plain = render_page_pretty(&page, false);
            let coloured = render_page_pretty(&page, true);
            assert_eq!(strip_ansi(&coloured), plain, "{:?}", page.content);
            for (line, plain_line) in coloured.lines().zip(plain.lines()) {
                if plain_line.contains("\"q\"") {
                    assert!(line.contains("\x1b[32m\"q\""), "{line:?} in {plain}");
                } else {
                    assert!(!line.contains("\x1b[33m"), "{line:?} in {plain}");
                }
            }
        }
    }

    #[test]
    fn code_block_comment_spans_lines() {
        let out = render_markdown_with_ansi("```javascript\n/*\nit's \"ok\"\n*/ x\n```");
        assert!(out.contains("\n\x1b[90mit's \"ok\"\x1b[0m\n"), "{out:?}");
    }

    #[test]
    fn code_block_state_does_not_leak_into_the_next_block() {
        let out = render_markdown_with_ansi("```javascript\n/* open\n```\n```javascript\nx\n```");
        assert!(out.contains("\nx\n"), "{out:?}");
    }

    #[test]
    fn unclosed_code_fence_is_still_highlighted() {
        let out = render_markdown_with_ansi("```python\nx = 1 # c");
        assert!(out.contains("\x1b[90m# c\x1b[0m"), "{out:?}");
    }
}
