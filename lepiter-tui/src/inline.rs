//! shared inline markdown parser.
//!
//! both the ansi cli renderer and the ratatui tui renderer need to parse
//! inline markdown (bold, italic, code, links, wiki-links, annotations).
//! this module provides a single parser that produces [`InlineElement`]s,
//! which each renderer converts to its own output format.
//!
//! links are located by [`lepiter_core::scan_inline_links`], the scanner every
//! other caller resolves link targets through; this module parses only the
//! styling that scanner does not describe.

use std::ops::Range;

use lepiter_core::{LinkKind, scan_inline_links};

/// a parsed fragment of inline markdown.
#[derive(Debug, Clone, PartialEq)]
pub enum InlineElement {
    /// plain or styled text segment.
    Styled {
        text: String,
        bold: bool,
        italic: bool,
        code: bool,
    },
    /// `[label](target)` url link.
    Link { label: String, target: String },
    /// `[[text]]` wiki-style link.
    WikiLink { text: String },
    /// `{{annotation}}` — includes the surrounding braces.
    Annotation { text: String },
}

/// parse inline markdown into a sequence of elements.
///
/// handles `**bold**`, `*italic*`, `` `code` ``, `[text](url)`,
/// `[[wiki-link]]`, and `{{annotation}}` syntax.
///
/// link syntax is whatever [`scan_inline_links`] accepts, and targets are
/// reported as it reports them: trimmed, never empty. a link inside a
/// `{{annotation}}` stays part of the annotation. emphasis markers inside a
/// code span are literal text; links and annotations are still recognised
/// there.
///
/// a code span opens on a run of n backticks and closes at the next run of
/// exactly n, so a span can contain a shorter run. an opening run with no such
/// closer is literal text and opens nothing. content is taken verbatim: no
/// leading or trailing space is stripped.
pub fn parse_inline(text: &str) -> Vec<InlineElement> {
    let chars = text.chars().collect::<Vec<_>>();
    let byte_at = byte_offsets(&chars);
    let scanned = scan_inline_links(text).collect::<Vec<_>>();
    let link_ranges = scanned
        .iter()
        .map(|link| link.range.clone())
        .collect::<Vec<_>>();
    let mut links = scanned.into_iter().peekable();
    let mut i = 0usize;
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut code: Option<CodeSpan> = None;

    let flush = |out: &mut Vec<InlineElement>, buf: &mut String, bold, italic, code| {
        if !buf.is_empty() {
            out.push(InlineElement::Styled {
                text: std::mem::take(buf),
                bold,
                italic,
                code,
            });
        }
    };

    while i < chars.len() {
        // annotations: {{...}}
        if i + 1 < chars.len() && chars[i] == '{' && chars[i + 1] == '{' {
            let limit = code.map_or(chars.len(), |span| span.close_at);
            let mut j = i + 2;
            while j + 1 < limit {
                if chars[j] == '}' && chars[j + 1] == '}' {
                    break;
                }
                j += 1;
            }
            if j + 1 < limit && chars[j] == '}' && chars[j + 1] == '}' {
                flush(&mut out, &mut buf, bold, italic, code.is_some());
                let annotation = chars[i..=j + 1].iter().collect::<String>();
                out.push(InlineElement::Annotation { text: annotation });
                i = j + 2;
                continue;
            }
        }

        // bold toggle: **
        if code.is_none() && i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '*' {
            flush(&mut out, &mut buf, bold, italic, false);
            bold = !bold;
            i += 2;
            continue;
        }

        // italic toggle: *
        if code.is_none() && chars[i] == '*' {
            flush(&mut out, &mut buf, bold, italic, false);
            italic = !italic;
            i += 1;
            continue;
        }

        // code spans: a run of n backticks closed by the next run of exactly n
        if chars[i] == '`' {
            if let Some(span) = code {
                if i == span.close_at {
                    flush(&mut out, &mut buf, bold, italic, true);
                    code = None;
                    i += span.close_len;
                    continue;
                }
            } else {
                let run = backtick_run(&chars, i);
                match closing_run(&chars, &byte_at, &link_ranges, i + run, run) {
                    Some(close_at) => {
                        flush(&mut out, &mut buf, bold, italic, false);
                        code = Some(CodeSpan {
                            close_at,
                            close_len: run,
                        });
                    }
                    None => buf.extend(&chars[i..i + run]),
                }
                i += run;
                continue;
            }
        }

        // links: [[wiki]] or [text](url)
        if chars[i] == '[' {
            // an annotation can consume a link, so drop any the walk has passed.
            while links
                .peek()
                .is_some_and(|link| link.range.start < byte_at[i])
            {
                links.next();
            }
            if let Some(link) = links.next_if(|link| link.range.start == byte_at[i]) {
                flush(&mut out, &mut buf, bold, italic, code.is_some());
                out.push(match link.kind {
                    LinkKind::Wiki => InlineElement::WikiLink {
                        text: link.target.to_string(),
                    },
                    LinkKind::Markdown => InlineElement::Link {
                        label: link.label.to_string(),
                        target: link.target.to_string(),
                    },
                });
                i += text[link.range].chars().count();
                continue;
            }
        }

        buf.push(chars[i]);
        i += 1;
    }

    flush(&mut out, &mut buf, bold, italic, code.is_some());
    out
}

/// where an open code span ends, in char indices.
#[derive(Clone, Copy)]
struct CodeSpan {
    close_at: usize,
    close_len: usize,
}

/// length of the backtick run starting at `start`.
fn backtick_run(chars: &[char], start: usize) -> usize {
    chars[start..].iter().take_while(|c| **c == '`').count()
}

/// start of the first run of exactly `len` backticks at or after `from`, taking
/// no run a scanner link covers.
fn closing_run(
    chars: &[char],
    byte_at: &[usize],
    link_ranges: &[Range<usize>],
    from: usize,
    len: usize,
) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] != '`' {
            i += 1;
            continue;
        }
        let run = backtick_run(chars, i);
        if run == len && !link_ranges.iter().any(|range| range.contains(&byte_at[i])) {
            return Some(i);
        }
        i += run;
    }
    None
}

/// byte offset of each char, plus the end of the text.
fn byte_offsets(chars: &[char]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(chars.len() + 1);
    let mut offset = 0usize;
    for c in chars {
        offsets.push(offset);
        offset += c.len_utf8();
    }
    offsets.push(offset);
    offsets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text() {
        assert_eq!(
            parse_inline("hello world"),
            vec![InlineElement::Styled {
                text: "hello world".into(),
                bold: false,
                italic: false,
                code: false,
            }]
        );
    }

    #[test]
    fn empty_string() {
        assert_eq!(parse_inline(""), vec![]);
    }

    #[test]
    fn bold() {
        let elems = parse_inline("before **bold** after");
        assert_eq!(elems.len(), 3);
        assert_eq!(
            elems[1],
            InlineElement::Styled {
                text: "bold".into(),
                bold: true,
                italic: false,
                code: false,
            }
        );
    }

    #[test]
    fn italic() {
        let elems = parse_inline("some *italic* text");
        assert_eq!(
            elems[1],
            InlineElement::Styled {
                text: "italic".into(),
                bold: false,
                italic: true,
                code: false,
            }
        );
    }

    #[test]
    fn code() {
        let elems = parse_inline("run `cargo test` now");
        assert_eq!(
            elems[1],
            InlineElement::Styled {
                text: "cargo test".into(),
                bold: false,
                italic: false,
                code: true,
            }
        );
    }

    #[test]
    fn code_span_keeps_double_asterisks() {
        assert_eq!(
            parse_inline("call `f(**kwargs)` now"),
            vec![
                InlineElement::Styled {
                    text: "call ".into(),
                    bold: false,
                    italic: false,
                    code: false,
                },
                InlineElement::Styled {
                    text: "f(**kwargs)".into(),
                    bold: false,
                    italic: false,
                    code: true,
                },
                InlineElement::Styled {
                    text: " now".into(),
                    bold: false,
                    italic: false,
                    code: false,
                },
            ]
        );
    }

    #[test]
    fn code_span_keeps_single_asterisk() {
        assert_eq!(
            parse_inline("`a * b` and *italic*"),
            vec![
                InlineElement::Styled {
                    text: "a * b".into(),
                    bold: false,
                    italic: false,
                    code: true,
                },
                InlineElement::Styled {
                    text: " and ".into(),
                    bold: false,
                    italic: false,
                    code: false,
                },
                InlineElement::Styled {
                    text: "italic".into(),
                    bold: false,
                    italic: true,
                    code: false,
                },
            ]
        );
    }

    #[test]
    fn bold_and_italic() {
        let elems = parse_inline("**bold *and italic* still bold**");
        assert_eq!(
            elems[1],
            InlineElement::Styled {
                text: "and italic".into(),
                bold: true,
                italic: true,
                code: false,
            }
        );
    }

    #[test]
    fn wiki_link() {
        let elems = parse_inline("see [[My Page]] here");
        assert_eq!(
            elems[1],
            InlineElement::WikiLink {
                text: "My Page".into()
            }
        );
    }

    #[test]
    fn url_link() {
        let elems = parse_inline("click [here](https://example.com) done");
        assert_eq!(
            elems[1],
            InlineElement::Link {
                label: "here".into(),
                target: "https://example.com".into(),
            }
        );
    }

    #[test]
    fn url_link_keeps_balanced_parens() {
        let elems = parse_inline(
            "see [Ruby](https://en.wikipedia.org/wiki/Ruby_(programming_language)) here",
        );
        assert_eq!(
            elems[1],
            InlineElement::Link {
                label: "Ruby".into(),
                target: "https://en.wikipedia.org/wiki/Ruby_(programming_language)".into(),
            }
        );
        assert_eq!(
            elems[2],
            InlineElement::Styled {
                text: " here".into(),
                bold: false,
                italic: false,
                code: false,
            }
        );
    }

    #[test]
    fn url_link_keeps_nested_parens() {
        assert_eq!(
            parse_inline("[x](a(b(c)d)e)"),
            vec![InlineElement::Link {
                label: "x".into(),
                target: "a(b(c)d)e".into(),
            }]
        );
    }

    #[test]
    fn unbalanced_target_is_not_a_link() {
        assert_eq!(
            parse_inline("[x](a(b)"),
            vec![InlineElement::Styled {
                text: "[x](a(b)".into(),
                bold: false,
                italic: false,
                code: false,
            }]
        );
    }

    #[test]
    fn unbalanced_target_does_not_swallow_a_later_link() {
        let elems = parse_inline("[a](un(closed and [b](ok)");
        assert_eq!(elems.len(), 2);
        assert_eq!(
            elems[1],
            InlineElement::Link {
                label: "b".into(),
                target: "ok".into(),
            }
        );
    }

    #[test]
    fn markdown_targets_match_the_core_scanner() {
        let cases = [
            "see [Ruby](https://en.wikipedia.org/wiki/Ruby_(programming_language)) here",
            "[x](a(b(c)d)e)",
            "[x](a(b)",
            "[a](un(closed and [b](ok)",
            "(see [a](b) too)",
            "[[wiki]] and [md](target)",
            "`[a](b)` inside a code span",
            "click [here](https://example.com) done",
        ];
        for text in cases {
            let core = scan_inline_links(text)
                .filter(|link| link.kind == LinkKind::Markdown)
                .map(|link| link.target.to_string())
                .collect::<Vec<_>>();
            let tui = parse_inline(text)
                .into_iter()
                .filter_map(|element| match element {
                    InlineElement::Link { target, .. } => Some(target),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(core, tui, "{text}");
        }
    }

    fn scanner_targets(text: &str) -> Vec<(LinkKind, String)> {
        scan_inline_links(text)
            .map(|link| (link.kind, link.target.to_string()))
            .collect()
    }

    fn reader_targets(text: &str) -> Vec<(LinkKind, String)> {
        reader_targets_of(parse_inline(text))
    }

    fn reader_targets_of(elements: Vec<InlineElement>) -> Vec<(LinkKind, String)> {
        elements
            .into_iter()
            .filter_map(|element| match element {
                InlineElement::Link { target, .. } => Some((LinkKind::Markdown, target)),
                InlineElement::WikiLink { text } => Some((LinkKind::Wiki, text)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn issue_153_table_rows_open_the_scanner_target() {
        for text in [
            "[a [b] c](t)",
            "[text with [inner] brackets](url)",
            "[![alt](img.png)](href)",
            "[![[x](y)](z)](t)",
        ] {
            assert_eq!(reader_targets(text), scanner_targets(text), "{text}");
        }
    }

    #[test]
    fn balanced_bracket_label_reaches_the_reader() {
        assert_eq!(
            parse_inline("[a [b] c](t)"),
            vec![InlineElement::Link {
                label: "a [b] c".into(),
                target: "t".into(),
            }]
        );
    }

    #[test]
    fn link_target_is_trimmed() {
        assert_eq!(
            parse_inline("[a]( t )"),
            vec![InlineElement::Link {
                label: "a".into(),
                target: "t".into(),
            }]
        );
    }

    #[test]
    fn wiki_target_is_trimmed() {
        assert_eq!(
            parse_inline("[[ a ]]"),
            vec![InlineElement::WikiLink { text: "a".into() }]
        );
    }

    #[test]
    fn empty_target_is_not_a_link() {
        assert_eq!(
            parse_inline("[a]() and [[ ]]"),
            vec![InlineElement::Styled {
                text: "[a]() and [[ ]]".into(),
                bold: false,
                italic: false,
                code: false,
            }]
        );
    }

    #[test]
    fn a_link_inside_an_annotation_stays_annotation_text() {
        assert_eq!(
            parse_inline("{{see [a](b)}}"),
            vec![InlineElement::Annotation {
                text: "{{see [a](b)}}".into(),
            }]
        );
        assert_eq!(scanner_targets("{{see [a](b)}}").len(), 1);
    }

    #[test]
    fn a_link_after_an_annotation_containing_a_link_is_still_a_link() {
        assert_eq!(
            parse_inline("{{see [a](b)}} and [c](d)"),
            vec![
                InlineElement::Annotation {
                    text: "{{see [a](b)}}".into(),
                },
                InlineElement::Styled {
                    text: " and ".into(),
                    bold: false,
                    italic: false,
                    code: false,
                },
                InlineElement::Link {
                    label: "c".into(),
                    target: "d".into(),
                },
            ]
        );
    }

    #[test]
    fn corpus_targets_match_the_core_scanner() {
        let alphabet = ['[', ']', '(', ')', '!', 'a', ' ', '*', '`', 'b', '\u{e9}'];
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut link_bearing = 0usize;
        let mut span_bearing = 0usize;
        for _ in 0..20_000 {
            let len = 3 + (next() % 16) as usize;
            let text = (0..len)
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect::<String>();
            let scanner = scanner_targets(&text);
            if !scanner.is_empty() {
                link_bearing += 1;
            }
            let elements = parse_inline(&text);
            if elements
                .iter()
                .any(|element| matches!(element, InlineElement::Styled { code: true, .. }))
            {
                span_bearing += 1;
            }
            assert_eq!(reader_targets_of(elements), scanner, "{text:?}");
        }
        assert!(
            link_bearing > 100,
            "corpus grew too few links: {link_bearing}"
        );
        assert!(
            span_bearing > 100,
            "corpus grew too few code spans: {span_bearing}"
        );
    }

    #[test]
    fn annotation() {
        let elems = parse_inline("text {{gtView}} more");
        assert_eq!(
            elems[1],
            InlineElement::Annotation {
                text: "{{gtView}}".into(),
            }
        );
    }

    #[test]
    fn code_span_still_yields_links_and_annotations() {
        assert_eq!(
            parse_inline("`[a](b) {{note}}`"),
            vec![
                InlineElement::Link {
                    label: "a".into(),
                    target: "b".into(),
                },
                InlineElement::Styled {
                    text: " ".into(),
                    bold: false,
                    italic: false,
                    code: true,
                },
                InlineElement::Annotation {
                    text: "{{note}}".into(),
                },
            ]
        );
    }

    #[test]
    fn code_span_keeps_emphasis_literal_around_a_scanner_link() {
        assert_eq!(
            parse_inline("`a * [b [c] d](t)`"),
            vec![
                InlineElement::Styled {
                    text: "a * ".into(),
                    bold: false,
                    italic: false,
                    code: true,
                },
                InlineElement::Link {
                    label: "b [c] d".into(),
                    target: "t".into(),
                },
            ]
        );
    }

    #[test]
    fn multiple_links() {
        let elems = parse_inline("[[First]] and [second](url2)");
        assert_eq!(
            elems[0],
            InlineElement::WikiLink {
                text: "First".into()
            }
        );
        assert_eq!(
            elems[2],
            InlineElement::Link {
                label: "second".into(),
                target: "url2".into(),
            }
        );
    }

    #[test]
    fn unclosed_bold() {
        let elems = parse_inline("before **unclosed");
        assert_eq!(
            elems[1],
            InlineElement::Styled {
                text: "unclosed".into(),
                bold: true,
                italic: false,
                code: false,
            }
        );
    }

    #[test]
    fn bracket_not_a_link() {
        let elems = parse_inline("array[0] done");
        assert_eq!(
            elems,
            vec![InlineElement::Styled {
                text: "array[0] done".into(),
                bold: false,
                italic: false,
                code: false,
            }]
        );
    }

    fn code_spans(text: &str) -> Vec<String> {
        parse_inline(text)
            .into_iter()
            .filter_map(|element| match element {
                InlineElement::Styled {
                    text, code: true, ..
                } => Some(text),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn unmatched_backtick_is_literal_text() {
        assert_eq!(
            parse_inline("a lone ` in prose"),
            vec![InlineElement::Styled {
                text: "a lone ` in prose".into(),
                bold: false,
                italic: false,
                code: false,
            }]
        );
    }

    #[test]
    fn unmatched_backtick_leaves_the_rest_of_the_line_alone() {
        assert_eq!(
            parse_inline("a ` then *italic*"),
            vec![
                InlineElement::Styled {
                    text: "a ` then ".into(),
                    bold: false,
                    italic: false,
                    code: false,
                },
                InlineElement::Styled {
                    text: "italic".into(),
                    bold: false,
                    italic: true,
                    code: false,
                },
            ]
        );
    }

    #[test]
    fn code_span_can_contain_a_shorter_run() {
        assert_eq!(code_spans("``a ` b``"), vec!["a ` b"]);
    }

    #[test]
    fn a_run_closes_only_at_a_run_of_the_same_length() {
        assert_eq!(code_spans("a `b`` c` d"), vec!["b`` c"]);
        assert_eq!(code_spans("a ``b` c"), Vec::<String>::new());
    }

    #[test]
    fn a_double_run_opens_one_span_not_two() {
        assert_eq!(
            parse_inline("``x`` y"),
            vec![
                InlineElement::Styled {
                    text: "x".into(),
                    bold: false,
                    italic: false,
                    code: true,
                },
                InlineElement::Styled {
                    text: " y".into(),
                    bold: false,
                    italic: false,
                    code: false,
                },
            ]
        );
    }

    #[test]
    fn code_span_extents_agree_with_commonmark() {
        // expected values from markdown-it-py 4.2.0, commonmark preset
        let cases: [(&str, &[&str]); 13] = [
            ("a ` b", &[]),
            ("unmatched ` tail is prose", &[]),
            ("a ``b` c", &[]),
            ("``a ` b``", &["a ` b"]),
            ("``x``", &["x"]),
            ("```x```", &["x"]),
            ("``a`b``", &["a`b"]),
            ("`a``b`", &["a``b"]),
            ("a `b`` c` d", &["b`` c"]),
            ("`a` and `b`", &["a", "b"]),
            ("`a\u{e9}b`", &["a\u{e9}b"]),
            ("`a * b` and *italic*", &["a * b"]),
            ("call `f(**kwargs)` now", &["f(**kwargs)"]),
        ];
        for (text, spans) in cases {
            assert_eq!(code_spans(text), spans, "{text:?}");
        }
    }

    #[test]
    fn code_span_content_is_not_space_stripped() {
        assert_eq!(code_spans("` a `"), vec![" a "]);
        assert_eq!(code_spans("`` ` ``"), vec![" ` "]);
    }

    #[test]
    fn a_backtick_inside_a_link_is_not_a_delimiter() {
        assert_eq!(
            parse_inline("`[a`](b)"),
            vec![
                InlineElement::Styled {
                    text: "`".into(),
                    bold: false,
                    italic: false,
                    code: false,
                },
                InlineElement::Link {
                    label: "a`".into(),
                    target: "b".into(),
                },
            ]
        );
        assert_eq!(reader_targets("`[a`](b)"), scanner_targets("`[a`](b)"));
    }

    #[test]
    fn a_span_holding_a_link_still_ends_at_its_closing_run() {
        for text in ["`[a](b) {{note}}` tail", "`a * [b [c] d](t)` tail"] {
            assert_eq!(
                parse_inline(text).last(),
                Some(&InlineElement::Styled {
                    text: " tail".into(),
                    bold: false,
                    italic: false,
                    code: false,
                }),
                "{text:?}"
            );
        }
    }

    #[test]
    fn an_annotation_does_not_reach_past_the_closing_run() {
        assert_eq!(
            parse_inline("`a {{b`c}} d`"),
            vec![
                InlineElement::Styled {
                    text: "a {{b".into(),
                    bold: false,
                    italic: false,
                    code: true,
                },
                InlineElement::Styled {
                    text: "c}} d`".into(),
                    bold: false,
                    italic: false,
                    code: false,
                },
            ]
        );
    }

    fn run_len(chars: &[char], start: usize) -> usize {
        let mut n = 0;
        while start + n < chars.len() && chars[start + n] == '`' {
            n += 1;
        }
        n
    }

    /// per char: `None` where a delimiter is consumed, `Some(code)` otherwise.
    fn expected_flags(chars: &[char]) -> Vec<Option<bool>> {
        let mut flags = vec![Some(false); chars.len()];
        let mut i = 0;
        while i < chars.len() {
            if chars[i] != '`' {
                i += 1;
                continue;
            }
            let run = run_len(chars, i);
            let mut j = i + run;
            let mut close = None;
            while j < chars.len() {
                if chars[j] != '`' {
                    j += 1;
                    continue;
                }
                let candidate = run_len(chars, j);
                if candidate == run {
                    close = Some(j);
                    break;
                }
                j += candidate;
            }
            let Some(close) = close else {
                i += run;
                continue;
            };
            flags[i..i + run].fill(None);
            flags[close..close + run].fill(None);
            flags[i + run..close].fill(Some(true));
            i = close + run;
        }
        flags
    }

    #[test]
    fn corpus_keeps_every_character_a_delimiter_run_does_not_consume() {
        let alphabet = ['`', 'a', 'b', ' ', '\u{e9}'];
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut span_bearing = 0usize;
        for _ in 0..20_000 {
            let len = 3 + (next() % 16) as usize;
            let source = (0..len)
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect::<String>();
            let chars = source.chars().collect::<Vec<_>>();
            let flags = expected_flags(&chars);
            if flags.contains(&Some(true)) {
                span_bearing += 1;
            }
            let expected = chars
                .iter()
                .zip(&flags)
                .filter_map(|(c, flag)| flag.map(|code| (*c, code)))
                .collect::<Vec<_>>();
            let actual = parse_inline(&source)
                .into_iter()
                .flat_map(|element| match element {
                    InlineElement::Styled { text, code, .. } => {
                        text.chars().map(|c| (c, code)).collect::<Vec<_>>()
                    }
                    other => panic!("{source:?} yielded {other:?}"),
                })
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "{source:?}");
        }
        assert!(
            span_bearing > 100,
            "corpus grew too few code spans: {span_bearing}"
        );
    }
}
