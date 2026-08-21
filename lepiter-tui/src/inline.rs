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
pub fn parse_inline(text: &str) -> Vec<InlineElement> {
    let chars = text.chars().collect::<Vec<_>>();
    let byte_at = byte_offsets(&chars);
    let mut links = scan_inline_links(text).peekable();
    let mut i = 0usize;
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut code = false;

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
            let mut j = i + 2;
            while j + 1 < chars.len() {
                if chars[j] == '}' && chars[j + 1] == '}' {
                    break;
                }
                j += 1;
            }
            if j + 1 < chars.len() && chars[j] == '}' && chars[j + 1] == '}' {
                flush(&mut out, &mut buf, bold, italic, code);
                let annotation = chars[i..=j + 1].iter().collect::<String>();
                out.push(InlineElement::Annotation { text: annotation });
                i = j + 2;
                continue;
            }
        }

        // bold toggle: **
        if !code && i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '*' {
            flush(&mut out, &mut buf, bold, italic, code);
            bold = !bold;
            i += 2;
            continue;
        }

        // italic toggle: *
        if !code && chars[i] == '*' {
            flush(&mut out, &mut buf, bold, italic, code);
            italic = !italic;
            i += 1;
            continue;
        }

        // code toggle: `
        if chars[i] == '`' {
            flush(&mut out, &mut buf, bold, italic, code);
            code = !code;
            i += 1;
            continue;
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
                flush(&mut out, &mut buf, bold, italic, code);
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

    flush(&mut out, &mut buf, bold, italic, code);
    out
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
        parse_inline(text)
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
        for _ in 0..20_000 {
            let len = 3 + (next() % 16) as usize;
            let text = (0..len)
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect::<String>();
            let scanner = scanner_targets(&text);
            if !scanner.is_empty() {
                link_bearing += 1;
            }
            assert_eq!(reader_targets(&text), scanner, "{text:?}");
        }
        assert!(
            link_bearing > 100,
            "corpus grew too few links: {link_bearing}"
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
}
