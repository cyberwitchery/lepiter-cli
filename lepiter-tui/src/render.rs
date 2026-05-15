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
            render_code_block(language.as_deref(), code, out);
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
                // Strip trailing empty lines added by individual node renderers;
                // the list manages its own spacing.
                while rendered
                    .last()
                    .is_some_and(|l| l.spans.iter().all(|s| s.content.is_empty()))
                {
                    rendered.pop();
                }
                if let Some((first, rest)) = rendered.split_first() {
                    let mut spans = vec![Span::styled(
                        "- ".to_string(),
                        Style::default().fg(Color::DarkGray),
                    )];
                    spans.extend(first.spans.iter().cloned());
                    out.push(Line::from(spans));
                    for line in rest {
                        let mut spans = vec![Span::raw("  ".to_string())];
                        spans.extend(line.spans.iter().cloned());
                        out.push(Line::from(spans));
                    }
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

pub fn render_code_block(language: Option<&str>, code: &str, out: &mut Vec<Line<'static>>) {
    let title = language.unwrap_or("code");
    out.push(Line::from(Span::styled(
        format!("```{title}"),
        Style::default().fg(Color::DarkGray),
    )));
    for line in normalize_text(code).lines() {
        out.push(highlight_code_line(&sanitize_for_terminal(line), language));
    }
    out.push(Line::from(Span::styled(
        "```".to_string(),
        Style::default().fg(Color::DarkGray),
    )));
    out.push(Line::raw(""));
}

pub fn highlight_code_line(line: &str, language: Option<&str>) -> Line<'static> {
    let tokens = tokenize_code_line(line, language);
    let spans: Vec<Span<'static>> = tokens
        .into_iter()
        .map(|tok| match tok {
            CodeToken::Comment(s) => {
                Span::styled(s.to_owned(), Style::default().fg(Color::DarkGray))
            }
            CodeToken::StringLit(s) => {
                Span::styled(s.to_owned(), Style::default().fg(Color::Green))
            }
            CodeToken::Number(s) => Span::styled(s.to_owned(), Style::default().fg(Color::Yellow)),
            CodeToken::Keyword(s) => Span::styled(
                s.to_owned(),
                Style::default()
                    .fg(Color::LightMagenta)
                    .add_modifier(Modifier::BOLD),
            ),
            CodeToken::Ident(s) => Span::raw(s.to_owned()),
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

/// Highlight all occurrences of `needle` (case-insensitive) in the rendered
/// lines.  The match at `current_idx` gets a distinct "active" style while all
/// other matches share a dimmer highlight.
pub fn highlight_page_search(
    lines: &[Line<'static>],
    needle: &str,
    current_match_line: Option<usize>,
) -> Vec<Line<'static>> {
    if needle.is_empty() {
        return lines.to_vec();
    }
    let lower_needle = needle.to_lowercase();
    let active_style = Style::default()
        .bg(Color::Yellow)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);
    let passive_style = Style::default().bg(Color::DarkGray).fg(Color::White);

    let mut out = Vec::with_capacity(lines.len());
    for (line_idx, line) in lines.iter().enumerate() {
        let full_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let lower_text = full_text.to_lowercase();
        if !lower_text.contains(&lower_needle) {
            out.push(line.clone());
            continue;
        }
        let is_current = current_match_line == Some(line_idx);
        let style = if is_current {
            active_style
        } else {
            passive_style
        };
        out.push(highlight_all_in_line(line, &lower_needle, style));
    }
    out
}

/// Highlight every occurrence of `needle` (lowercase) within a single line.
fn highlight_all_in_line(
    line: &Line<'static>,
    lower_needle: &str,
    match_style: Style,
) -> Line<'static> {
    // Flatten spans into (char_offset, byte_range_in_full_text, original_style).
    // Then find all needle positions in the full text and split spans around them.
    let full_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let lower_text = full_text.to_lowercase();

    // Collect match byte ranges in the original text.
    let mut match_ranges: Vec<(usize, usize)> = Vec::new();
    let mut search_start = 0;
    while let Some(pos) = lower_text[search_start..].find(lower_needle) {
        let abs_start = search_start + pos;
        let abs_end = abs_start + lower_needle.len();
        // Map lowered byte offsets back to original text byte offsets.
        let raw_start = lower_byte_to_raw_byte(&full_text, abs_start);
        let raw_end = lower_byte_to_raw_byte(&full_text, abs_end);
        match_ranges.push((raw_start, raw_end));
        search_start = abs_end;
    }
    if match_ranges.is_empty() {
        return line.clone();
    }

    // Walk through original spans, splitting them wherever a match range
    // intersects.
    let mut result_spans: Vec<Span<'static>> = Vec::new();
    let mut global_byte = 0usize;
    let mut match_idx = 0;

    for span in &line.spans {
        let span_text = span.content.as_ref();
        let span_start = global_byte;
        let span_end = global_byte + span_text.len();
        let mut local_byte = 0usize;

        while local_byte < span_text.len() && match_idx < match_ranges.len() {
            let (m_start, m_end) = match_ranges[match_idx];

            if m_end <= span_start + local_byte {
                match_idx += 1;
                continue;
            }
            if m_start >= span_end {
                break;
            }

            // Portion before match
            let before_end = m_start.saturating_sub(span_start).max(local_byte);
            if before_end > local_byte {
                let s = &span_text[local_byte..before_end];
                if !s.is_empty() {
                    result_spans.push(Span::styled(s.to_string(), span.style));
                }
                local_byte = before_end;
            }

            // Highlighted portion within this span
            let hl_start = m_start.saturating_sub(span_start).max(local_byte);
            let hl_end = m_end.min(span_end) - span_start;
            if hl_end > hl_start {
                let s = &span_text[hl_start..hl_end];
                result_spans.push(Span::styled(s.to_string(), match_style));
                local_byte = hl_end;
            }

            if span_start + local_byte >= m_end {
                match_idx += 1;
            }
        }

        // Remainder of span after all matches
        if local_byte < span_text.len() {
            result_spans.push(Span::styled(
                span_text[local_byte..].to_string(),
                span.style,
            ));
        }
        global_byte = span_end;
    }

    Line::from(result_spans)
}

/// Map a byte offset in `raw.to_lowercase()` to the corresponding byte offset
/// in `raw`.
fn lower_byte_to_raw_byte(raw: &str, lower_pos: usize) -> usize {
    let mut raw_byte = 0usize;
    let mut lower_byte = 0usize;
    for ch in raw.chars() {
        if lower_byte >= lower_pos {
            return raw_byte;
        }
        raw_byte += ch.len_utf8();
        for lch in ch.to_lowercase() {
            lower_byte += lch.len_utf8();
        }
    }
    raw_byte
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

    // --- list rendering ---

    #[test]
    fn list_single_line_items() {
        use crate::plugins::PluginManager;
        let node = Node::List {
            items: vec![
                vec![Node::Paragraph {
                    text: "first".into(),
                }],
                vec![Node::Paragraph {
                    text: "second".into(),
                }],
            ],
        };
        let mut out = Vec::new();
        let mut links = Vec::new();
        let mut plugins = PluginManager::empty();
        render_node(&node, &mut out, &mut links, &mut plugins);
        // "- first", "- second", trailing blank
        assert_eq!(out.len(), 3);
        assert_eq!(span_texts(&out[0]), vec!["- ", "first"]);
        assert!(has_fg(&out[0], 0, Color::DarkGray));
        assert_eq!(span_texts(&out[1]), vec!["- ", "second"]);
        assert!(out[2].spans.iter().all(|s| s.content.is_empty()));
    }

    #[test]
    fn list_item_with_code_block() {
        use crate::plugins::PluginManager;
        let node = Node::List {
            items: vec![vec![Node::Code {
                language: Some("rust".into()),
                code: "let x = 1;\nlet y = 2;".into(),
            }]],
        };
        let mut out = Vec::new();
        let mut links = Vec::new();
        let mut plugins = PluginManager::empty();
        render_node(&node, &mut out, &mut links, &mut plugins);
        // "- ```rust", "  let x = 1;", "  let y = 2;", "  ```", trailing blank
        assert_eq!(out.len(), 5);
        assert_eq!(out[0].spans[0].content.as_ref(), "- ");
        // continuation lines have "  " indent
        assert_eq!(out[1].spans[0].content.as_ref(), "  ");
        assert_eq!(out[2].spans[0].content.as_ref(), "  ");
        assert_eq!(out[3].spans[0].content.as_ref(), "  ");
    }

    #[test]
    fn list_empty_item() {
        use crate::plugins::PluginManager;
        let node = Node::List {
            items: vec![vec![Node::Text { text: "".into() }]],
        };
        let mut out = Vec::new();
        let mut links = Vec::new();
        let mut plugins = PluginManager::empty();
        render_node(&node, &mut out, &mut links, &mut plugins);
        // Empty text produces no visible spans, so falls back to bare "-"
        // plus trailing blank
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].spans[0].content.as_ref(), "-");
    }

    #[test]
    fn list_item_with_multiple_nodes() {
        use crate::plugins::PluginManager;
        let node = Node::List {
            items: vec![vec![
                Node::Paragraph {
                    text: "intro text".into(),
                },
                Node::Code {
                    language: None,
                    code: "x = 1".into(),
                },
            ]],
        };
        let mut out = Vec::new();
        let mut links = Vec::new();
        let mut plugins = PluginManager::empty();
        render_node(&node, &mut out, &mut links, &mut plugins);
        // First line: "- intro text"
        assert_eq!(out[0].spans[0].content.as_ref(), "- ");
        // Continuation lines should all have "  " indent
        for line in &out[1..out.len() - 1] {
            assert_eq!(
                line.spans[0].content.as_ref(),
                "  ",
                "continuation line missing indent: {:?}",
                span_texts(line)
            );
        }
        // Last line is trailing blank
        assert!(
            out.last()
                .unwrap()
                .spans
                .iter()
                .all(|s| s.content.is_empty())
        );
    }

    // --- highlight_page_search ---

    #[test]
    fn page_search_empty_needle_returns_clone() {
        let lines = vec![Line::raw("hello world")];
        let result = highlight_page_search(&lines, "", None);
        assert_eq!(result.len(), 1);
        assert_eq!(span_texts(&result[0]), vec!["hello world"]);
    }

    #[test]
    fn page_search_highlights_matching_line() {
        let lines = vec![
            Line::raw("no match here"),
            Line::raw("find the needle in this line"),
        ];
        let result = highlight_page_search(&lines, "needle", Some(1));
        // First line: unchanged.
        assert_eq!(span_texts(&result[0]), vec!["no match here"]);
        // Second line: split around "needle".
        let texts = span_texts(&result[1]);
        assert_eq!(texts, vec!["find the ", "needle", " in this line"]);
        // Active match gets yellow bg.
        assert_eq!(result[1].spans[1].style.bg, Some(Color::Yellow));
        assert_eq!(result[1].spans[1].style.fg, Some(Color::Black));
    }

    #[test]
    fn page_search_passive_highlight_for_non_current() {
        let lines = vec![Line::raw("needle first"), Line::raw("needle second")];
        // Current match is line 0; line 1 should get passive style.
        let result = highlight_page_search(&lines, "needle", Some(0));
        assert_eq!(result[0].spans[0].style.bg, Some(Color::Yellow)); // active
        assert_eq!(result[1].spans[0].style.bg, Some(Color::DarkGray)); // passive
    }

    #[test]
    fn page_search_case_insensitive() {
        let lines = vec![Line::raw("Hello World")];
        let result = highlight_page_search(&lines, "hello", Some(0));
        let texts = span_texts(&result[0]);
        assert_eq!(texts, vec!["Hello", " World"]);
        assert_eq!(result[0].spans[0].style.bg, Some(Color::Yellow));
    }

    #[test]
    fn page_search_multiple_occurrences_in_line() {
        let lines = vec![Line::raw("aa bb aa")];
        let result = highlight_page_search(&lines, "aa", Some(0));
        let texts = span_texts(&result[0]);
        assert_eq!(texts, vec!["aa", " bb ", "aa"]);
        assert_eq!(result[0].spans[0].style.bg, Some(Color::Yellow));
        assert_eq!(result[0].spans[2].style.bg, Some(Color::Yellow));
    }

    #[test]
    fn page_search_across_styled_spans() {
        let lines = vec![Line::from(vec![
            Span::styled("hel", Style::default().fg(Color::Cyan)),
            Span::raw("lo world"),
        ])];
        let result = highlight_page_search(&lines, "hello", Some(0));
        // "hel" and "lo" should be highlighted, " world" plain.
        let texts = span_texts(&result[0]);
        assert_eq!(texts, vec!["hel", "lo", " world"]);
        assert_eq!(result[0].spans[0].style.bg, Some(Color::Yellow));
        assert_eq!(result[0].spans[1].style.bg, Some(Color::Yellow));
    }

    #[test]
    fn page_search_no_current_all_passive() {
        let lines = vec![Line::raw("needle here")];
        let result = highlight_page_search(&lines, "needle", None);
        assert_eq!(result[0].spans[0].style.bg, Some(Color::DarkGray));
    }
}
