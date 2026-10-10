use crate::highlight::{CodeToken, tokenize_code_block};
use crate::inline;
use lepiter_core::{Page, render_page_to_text};

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
        out.push_str(&render_markdown_with_ansi(body.trim()));
        out.push('\n');
    } else {
        out.push_str(body.trim());
        out.push('\n');
    }
    out
}

fn render_markdown_with_ansi(markdown: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    let mut language: Option<String> = None;
    let mut code_lines = Vec::new();

    for line in markdown.lines() {
        if let Some(rest) = line.strip_prefix("```") {
            if in_code {
                push_code_block_ansi(&mut out, &mut code_lines, language.as_deref());
                out.push_str(&ansi("90", "```"));
                out.push('\n');
                in_code = false;
                language = None;
            } else {
                language = if rest.trim().is_empty() {
                    None
                } else {
                    Some(rest.trim().to_lowercase())
                };
                out.push_str(&ansi("90", line));
                out.push('\n');
                in_code = true;
            }
            continue;
        }

        if in_code {
            code_lines.push(line);
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
    push_code_block_ansi(&mut out, &mut code_lines, language.as_deref());

    out
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

fn push_code_block_ansi(out: &mut String, lines: &mut Vec<&str>, language: Option<&str>) {
    for tokens in tokenize_code_block(lines.drain(..), language) {
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
