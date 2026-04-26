//! page rendering to tui lines, including inline markdown, annotations, and
//! code highlighting.

use lepiter_core::{Node, Page, PageId, normalize_text};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::highlight::{CodeToken, tokenize_code_line};
use crate::inline::{InlineElement, parse_inline};
use crate::plugins::{PluginManager, PluginRender};

#[derive(Debug, Clone)]
pub struct LinkTarget {
    pub label: String,
    pub target: String,
}

#[derive(Debug, Clone)]
pub struct RenderedPage {
    pub id: PageId,
    pub title: String,
    pub lines: Vec<Line<'static>>,
    pub links: Vec<LinkTarget>,
}

pub fn render_page(page: &Page, plugins: &mut PluginManager) -> RenderedPage {
    let mut lines = Vec::new();
    let mut links = Vec::new();

    for node in &page.content {
        render_node(node, &mut lines, &mut links, plugins);
    }

    RenderedPage {
        id: page.id.clone(),
        title: page.title.clone(),
        lines,
        links,
    }
}

pub fn render_node(
    node: &Node,
    out: &mut Vec<Line<'static>>,
    links: &mut Vec<LinkTarget>,
    plugins: &mut PluginManager,
) {
    match node {
        Node::Heading { level, text } => {
            let style = match *level {
                1 => Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                2 => Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                _ => Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            };
            out.push(Line::from(Span::styled(
                format!(
                    "{} {}",
                    "#".repeat((*level).max(1) as usize),
                    sanitize_for_terminal(text)
                ),
                style,
            )));
            out.push(Line::raw(""));
        }
        Node::Paragraph { text } | Node::Text { text } => {
            out.push(parse_inline_markdown(&sanitize_for_terminal(text), links));
            out.push(Line::raw(""));
        }
        Node::Quote { text } => {
            out.push(Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    sanitize_for_terminal(text),
                    Style::default()
                        .fg(Color::Gray)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
            out.push(Line::raw(""));
        }
        Node::Code { language, code } => {
            let title = language.clone().unwrap_or_else(|| "code".to_string());
            out.push(Line::from(Span::styled(
                format!("```{title}"),
                Style::default().fg(Color::DarkGray),
            )));
            for line in normalize_text(code).lines() {
                out.push(highlight_code_line(
                    &sanitize_for_terminal(line),
                    language.as_deref(),
                ));
            }
            out.push(Line::from(Span::styled(
                "```".to_string(),
                Style::default().fg(Color::DarkGray),
            )));
            out.push(Line::raw(""));
        }
        Node::Rewrite {
            language,
            search,
            replace,
            scope,
            is_method_pattern,
        } => {
            let lang = language.clone().unwrap_or_else(|| "rewrite".to_string());
            out.push(Line::from(Span::styled(
                format!("rewrite ({lang})"),
                Style::default()
                    .fg(Color::LightMagenta)
                    .add_modifier(Modifier::BOLD),
            )));
            if let Some(scope) = scope {
                out.push(Line::from(Span::styled(
                    format!("scope: {}", sanitize_for_terminal(scope)),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            if let Some(is_method_pattern) = is_method_pattern {
                out.push(Line::from(Span::styled(
                    format!("method_pattern: {is_method_pattern}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            for line in normalize_text(search).lines() {
                out.push(Line::from(vec![
                    Span::styled("- ", Style::default().fg(Color::Red)),
                    Span::styled(sanitize_for_terminal(line), Style::default().fg(Color::Red)),
                ]));
            }
            for line in normalize_text(replace).lines() {
                out.push(Line::from(vec![
                    Span::styled("+ ", Style::default().fg(Color::Green)),
                    Span::styled(
                        sanitize_for_terminal(line),
                        Style::default().fg(Color::Green),
                    ),
                ]));
            }
            out.push(Line::raw(""));
        }
        Node::List { items } => {
            for item in items {
                let mut rendered = Vec::new();
                for n in item {
                    render_node(n, &mut rendered, links, plugins);
                }
                if let Some(first) = rendered.first() {
                    let mut spans = vec![Span::styled(
                        "- ".to_string(),
                        Style::default().fg(Color::DarkGray),
                    )];
                    spans.extend(first.spans.iter().cloned());
                    out.push(Line::from(spans));
                } else {
                    out.push(Line::from(Span::raw("-")));
                }
            }
            out.push(Line::raw(""));
        }
        Node::Link { text, url } => {
            links.push(LinkTarget {
                label: sanitize_for_terminal(text),
                target: sanitize_for_terminal(url),
            });
            let idx = links.len();
            out.push(Line::from(vec![
                Span::styled(
                    format!("[{idx}] "),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    sanitize_for_terminal(text),
                    Style::default()
                        .fg(Color::LightBlue)
                        .add_modifier(Modifier::UNDERLINED),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("({})", sanitize_for_terminal(url)),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            out.push(Line::raw(""));
        }
        Node::Unknown { typ, raw } => {
            if let Some(rendered) = plugins.render(typ, raw) {
                match rendered {
                    PluginRender::Lines(lines) => {
                        for line in lines {
                            out.push(Line::from(Span::raw(line)));
                        }
                        out.push(Line::raw(""));
                        return;
                    }
                    PluginRender::Error(err) => {
                        out.push(Line::from(Span::styled(
                            format!("[[plugin error: {err}]]"),
                            Style::default().fg(Color::Red),
                        )));
                        out.push(Line::raw(""));
                        return;
                    }
                }
            }
            out.push(Line::from(Span::styled(
                format!("[[unknown: {}]]", sanitize_for_terminal(typ)),
                Style::default().fg(Color::Yellow),
            )));
            out.push(Line::raw(""));
        }
    }
}

/// Convert parsed inline elements to ratatui spans.
///
/// When `links` is `Some`, link targets are tracked and numbered markers
/// (`[1]`, `[2]`, …) are appended after each link.  When `None`, links are
/// rendered as styled text without markers or tracking.
fn render_inline_to_spans(
    elements: Vec<InlineElement>,
    mut links: Option<&mut Vec<LinkTarget>>,
) -> Vec<Span<'static>> {
    let annotation_style = Style::default()
        .fg(Color::LightMagenta)
        .add_modifier(Modifier::BOLD);
    let link_style = Style::default()
        .fg(Color::LightBlue)
        .add_modifier(Modifier::UNDERLINED);

    let mut spans = Vec::new();
    for elem in elements {
        match elem {
            InlineElement::Styled {
                text,
                bold,
                italic,
                code,
            } => {
                let mut style = Style::default();
                if bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if italic {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if code {
                    style = style.fg(Color::Yellow).bg(Color::Black);
                }
                spans.push(Span::styled(text, style));
            }
            InlineElement::Link { label, target } => {
                if let Some(links) = &mut links {
                    links.push(LinkTarget {
                        label: label.clone(),
                        target,
                    });
                    let idx = links.len();
                    spans.push(Span::styled(label, link_style));
                    spans.push(Span::styled(
                        format!("[{idx}]"),
                        Style::default().fg(Color::Yellow),
                    ));
                } else {
                    spans.push(Span::styled(label, link_style));
                }
            }
            InlineElement::WikiLink { text } => {
                if let Some(links) = &mut links {
                    links.push(LinkTarget {
                        label: text.clone(),
                        target: text.clone(),
                    });
                    let idx = links.len();
                    spans.push(Span::styled(text, link_style));
                    spans.push(Span::styled(
                        format!("[{idx}]"),
                        Style::default().fg(Color::Yellow),
                    ));
                } else {
                    spans.push(Span::styled(text, link_style));
                }
            }
            InlineElement::Annotation { text } => {
                spans.push(Span::styled(text, annotation_style));
            }
        }
    }

    spans
}

fn parse_inline_markdown(text: &str, links: &mut Vec<LinkTarget>) -> Line<'static> {
    Line::from(render_inline_to_spans(parse_inline(text), Some(links)))
}

pub fn parse_inline_annotations(text: &str) -> Line<'static> {
    Line::from(render_inline_to_spans(parse_inline(text), None))
}

pub fn highlight_code_line(line: &str, language: Option<&str>) -> Line<'static> {
    let tokens = tokenize_code_line(line, language);
    let spans: Vec<Span<'static>> = tokens
        .into_iter()
        .map(|tok| match tok {
            CodeToken::Comment(s) => Span::styled(s, Style::default().fg(Color::DarkGray)),
            CodeToken::StringLit(s) => Span::styled(s, Style::default().fg(Color::Green)),
            CodeToken::Number(s) => Span::styled(s, Style::default().fg(Color::Yellow)),
            CodeToken::Keyword(s) => Span::styled(
                s,
                Style::default()
                    .fg(Color::LightMagenta)
                    .add_modifier(Modifier::BOLD),
            ),
            CodeToken::Ident(s) => Span::raw(s),
            CodeToken::Punct(c) => Span::raw(c.to_string()),
        })
        .collect();
    Line::from(spans)
}

pub fn sanitize_for_terminal(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\t' => out.push_str("    "),
            '\u{001b}' => {}
            c if c.is_control() && c != '\n' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

pub fn highlight_selected_link_markers(
    lines: &[Line<'static>],
    selected_idx: usize,
) -> Vec<Line<'static>> {
    let marker = format!("[{selected_idx}]");
    let marker_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let mut spans = Vec::new();
        for span in &line.spans {
            let content = span.content.as_ref();
            if let Some(pos) = content.find(&marker) {
                let (before, after) = content.split_at(pos);
                if !before.is_empty() {
                    spans.push(Span::styled(before.to_string(), span.style));
                }
                spans.push(Span::styled(marker.clone(), marker_style));
                let rest = &after[marker.len()..];
                if !rest.is_empty() {
                    spans.push(Span::styled(rest.to_string(), span.style));
                }
            } else {
                spans.push(span.clone());
            }
        }
        out.push(Line::from(spans));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- helpers ---

    fn span_texts<'a>(line: &'a Line<'a>) -> Vec<&'a str> {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn has_modifier(line: &Line, idx: usize, m: Modifier) -> bool {
        line.spans[idx].style.add_modifier.contains(m)
    }

    fn has_fg(line: &Line, idx: usize, c: Color) -> bool {
        line.spans[idx].style.fg == Some(c)
    }

    // --- parse_inline_markdown ---

    #[test]
    fn inline_plain_text() {
        let mut links = Vec::new();
        let line = parse_inline_markdown("hello world", &mut links);
        assert_eq!(span_texts(&line), vec!["hello world"]);
        assert!(links.is_empty());
    }

    #[test]
    fn inline_empty_string() {
        let mut links = Vec::new();
        let line = parse_inline_markdown("", &mut links);
        assert!(line.spans.is_empty());
    }

    #[test]
    fn inline_bold() {
        let mut links = Vec::new();
        let line = parse_inline_markdown("before **bold** after", &mut links);
        assert_eq!(span_texts(&line), vec!["before ", "bold", " after"]);
        assert!(!has_modifier(&line, 0, Modifier::BOLD));
        assert!(has_modifier(&line, 1, Modifier::BOLD));
        assert!(!has_modifier(&line, 2, Modifier::BOLD));
    }

    #[test]
    fn inline_italic() {
        let mut links = Vec::new();
        let line = parse_inline_markdown("some *italic* text", &mut links);
        assert_eq!(span_texts(&line), vec!["some ", "italic", " text"]);
        assert!(has_modifier(&line, 1, Modifier::ITALIC));
    }

    #[test]
    fn inline_code() {
        let mut links = Vec::new();
        let line = parse_inline_markdown("run `cargo test` now", &mut links);
        assert_eq!(span_texts(&line), vec!["run ", "cargo test", " now"]);
        assert!(has_fg(&line, 1, Color::Yellow));
        assert_eq!(line.spans[1].style.bg, Some(Color::Black));
    }

    #[test]
    fn inline_bold_and_italic_combined() {
        let mut links = Vec::new();
        let line = parse_inline_markdown("**bold *and italic* still bold**", &mut links);
        // "bold " is bold, "and italic" is bold+italic, " still bold" is bold
        assert!(has_modifier(&line, 0, Modifier::BOLD));
        assert!(has_modifier(&line, 1, Modifier::BOLD));
        assert!(has_modifier(&line, 1, Modifier::ITALIC));
        assert!(has_modifier(&line, 2, Modifier::BOLD));
    }

    #[test]
    fn inline_wiki_link() {
        let mut links = Vec::new();
        let line = parse_inline_markdown("see [[My Page]] here", &mut links);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label, "My Page");
        assert_eq!(links[0].target, "My Page");
        // span 0 = "see ", span 1 = link text, span 2 = marker, span 3 = " here"
        assert_eq!(line.spans[0].content.as_ref(), "see ");
        assert!(has_fg(&line, 1, Color::LightBlue));
        assert!(has_modifier(&line, 1, Modifier::UNDERLINED));
        assert_eq!(line.spans[2].content.as_ref(), "[1]");
        assert!(has_fg(&line, 2, Color::Yellow));
        assert_eq!(line.spans[3].content.as_ref(), " here");
    }

    #[test]
    fn inline_url_link() {
        let mut links = Vec::new();
        let line = parse_inline_markdown("click [here](https://example.com) done", &mut links);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label, "here");
        assert_eq!(links[0].target, "https://example.com");
        // span 0 = "click ", span 1 = link text, span 2 = marker, span 3 = " done"
        assert_eq!(line.spans[0].content.as_ref(), "click ");
        assert!(has_fg(&line, 1, Color::LightBlue));
        assert_eq!(line.spans[2].content.as_ref(), "[1]");
    }

    #[test]
    fn inline_annotation() {
        let mut links = Vec::new();
        let line = parse_inline_markdown("text {{gtView}} more", &mut links);
        assert_eq!(span_texts(&line), vec!["text ", "{{gtView}}", " more"]);
        assert!(has_fg(&line, 1, Color::LightMagenta));
        assert!(has_modifier(&line, 1, Modifier::BOLD));
    }

    #[test]
    fn inline_multiple_links_indexed() {
        let mut links = Vec::new();
        let line = parse_inline_markdown("[[First]] and [second](url2)", &mut links);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].label, "First");
        assert_eq!(links[1].label, "second");
        // wiki link gets [1], url link gets [2]
        let texts = span_texts(&line);
        assert!(texts.contains(&"[1]"));
        assert!(texts.contains(&"[2]"));
    }

    #[test]
    fn inline_unclosed_bold_treated_as_text() {
        let mut links = Vec::new();
        // unclosed ** — the parser just toggles bold on, rest is bold-styled
        let line = parse_inline_markdown("before **unclosed", &mut links);
        assert_eq!(span_texts(&line), vec!["before ", "unclosed"]);
        assert!(has_modifier(&line, 1, Modifier::BOLD));
    }

    #[test]
    fn inline_bracket_not_a_link() {
        let mut links = Vec::new();
        // single [ without matching ](url) should be treated as text
        let line = parse_inline_markdown("array[0] done", &mut links);
        assert!(links.is_empty());
        assert_eq!(span_texts(&line), vec!["array[0] done"]);
    }

    // --- parse_inline_annotations ---

    #[test]
    fn annotations_plain_text() {
        let line = parse_inline_annotations("hello world");
        assert_eq!(span_texts(&line), vec!["hello world"]);
    }

    #[test]
    fn annotations_empty_string() {
        let line = parse_inline_annotations("");
        assert!(line.spans.is_empty());
    }

    #[test]
    fn annotations_highlight() {
        let line = parse_inline_annotations("text {{gtView}} more");
        assert_eq!(span_texts(&line), vec!["text ", "{{gtView}}", " more"]);
        assert!(has_fg(&line, 1, Color::LightMagenta));
        assert!(has_modifier(&line, 1, Modifier::BOLD));
    }

    #[test]
    fn annotations_multiple() {
        let line = parse_inline_annotations("{{a}} mid {{b}}");
        assert_eq!(span_texts(&line), vec!["{{a}}", " mid ", "{{b}}"]);
        assert!(has_fg(&line, 0, Color::LightMagenta));
        assert!(has_fg(&line, 2, Color::LightMagenta));
    }

    #[test]
    fn annotations_renders_bold() {
        let line = parse_inline_annotations("before **bold** after");
        assert_eq!(span_texts(&line), vec!["before ", "bold", " after"]);
        assert!(has_modifier(&line, 1, Modifier::BOLD));
    }

    #[test]
    fn annotations_renders_links_without_markers() {
        let line = parse_inline_annotations("see [[Page]] here");
        // links are rendered but without numbered markers
        assert_eq!(span_texts(&line), vec!["see ", "Page", " here"]);
        assert!(has_fg(&line, 1, Color::LightBlue));
        assert!(has_modifier(&line, 1, Modifier::UNDERLINED));
    }

    // --- render_inline_to_spans consistency ---

    #[test]
    fn both_paths_style_annotations_identically() {
        let mut links = Vec::new();
        let md_line = parse_inline_markdown("text {{note}} end", &mut links);
        let ann_line = parse_inline_annotations("text {{note}} end");
        // annotation span (index 1) should have the same style in both
        assert_eq!(md_line.spans[1].style, ann_line.spans[1].style);
        assert_eq!(md_line.spans[1].content, ann_line.spans[1].content);
    }

    #[test]
    fn both_paths_style_bold_identically() {
        let mut links = Vec::new();
        let md_line = parse_inline_markdown("**bold**", &mut links);
        let ann_line = parse_inline_annotations("**bold**");
        assert_eq!(md_line.spans[0].style, ann_line.spans[0].style);
    }

    // --- sanitize_for_terminal ---

    #[test]
    fn sanitize_tabs_to_spaces() {
        assert_eq!(sanitize_for_terminal("a\tb"), "a    b");
    }

    #[test]
    fn sanitize_strips_esc() {
        assert_eq!(sanitize_for_terminal("hi\x1b[31mred"), "hi[31mred");
    }

    #[test]
    fn sanitize_control_chars_to_space() {
        // \x01 (SOH) should become a space
        assert_eq!(sanitize_for_terminal("a\x01b"), "a b");
    }

    #[test]
    fn sanitize_preserves_newlines() {
        assert_eq!(sanitize_for_terminal("a\nb"), "a\nb");
    }

    #[test]
    fn sanitize_plain_text_unchanged() {
        let s = "hello world 123!";
        assert_eq!(sanitize_for_terminal(s), s);
    }

    #[test]
    fn sanitize_mixed() {
        assert_eq!(
            sanitize_for_terminal("\x1bhello\tworld\x02"),
            "hello    world "
        );
    }

    // --- highlight_selected_link_markers ---

    #[test]
    fn highlight_replaces_matching_marker() {
        let lines = vec![Line::from(vec![
            Span::raw("text "),
            Span::styled("[2]", Style::default().fg(Color::Yellow)),
            Span::raw(" more"),
        ])];
        let result = highlight_selected_link_markers(&lines, 2);
        // the [2] span should now have the highlight style
        assert_eq!(result[0].spans[1].content.as_ref(), "[2]");
        assert_eq!(result[0].spans[1].style.fg, Some(Color::Black));
        assert_eq!(result[0].spans[1].style.bg, Some(Color::Yellow));
        assert!(
            result[0].spans[1]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn highlight_leaves_non_matching_markers() {
        let lines = vec![Line::from(vec![
            Span::styled("[1]", Style::default().fg(Color::Yellow)),
            Span::styled("[3]", Style::default().fg(Color::Yellow)),
        ])];
        let result = highlight_selected_link_markers(&lines, 2);
        // neither [1] nor [3] should change
        assert_eq!(result[0].spans[0].style.fg, Some(Color::Yellow));
        assert_eq!(result[0].spans[0].style.bg, None);
        assert_eq!(result[0].spans[1].style.fg, Some(Color::Yellow));
        assert_eq!(result[0].spans[1].style.bg, None);
    }

    #[test]
    fn highlight_marker_embedded_in_text() {
        let lines = vec![Line::from(Span::raw("before [1] after"))];
        let result = highlight_selected_link_markers(&lines, 1);
        let texts: Vec<&str> = result[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, vec!["before ", "[1]", " after"]);
        assert_eq!(result[0].spans[1].style.bg, Some(Color::Yellow));
    }

    #[test]
    fn highlight_empty_lines() {
        let lines: Vec<Line<'static>> = vec![Line::raw("")];
        let result = highlight_selected_link_markers(&lines, 1);
        assert_eq!(result.len(), 1);
        // Line::raw("") produces a line with one empty span
        assert!(
            result[0]
                .spans
                .iter()
                .all(|s| s.content.is_empty() || s.style.bg.is_none())
        );
    }

    #[test]
    fn highlight_marker_at_span_start() {
        let lines = vec![Line::from(Span::raw("[5] trailing"))];
        let result = highlight_selected_link_markers(&lines, 5);
        assert_eq!(result[0].spans[0].content.as_ref(), "[5]");
        assert_eq!(result[0].spans[0].style.bg, Some(Color::Yellow));
        assert_eq!(result[0].spans[1].content.as_ref(), " trailing");
    }

    #[test]
    fn highlight_marker_at_span_end() {
        let lines = vec![Line::from(Span::raw("leading [3]"))];
        let result = highlight_selected_link_markers(&lines, 3);
        assert_eq!(result[0].spans[0].content.as_ref(), "leading ");
        assert_eq!(result[0].spans[1].content.as_ref(), "[3]");
        assert_eq!(result[0].spans[1].style.bg, Some(Color::Yellow));
    }
}
