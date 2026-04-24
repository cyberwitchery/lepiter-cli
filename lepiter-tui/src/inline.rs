//! shared inline markdown parser.
//!
//! both the ansi cli renderer and the ratatui tui renderer need to parse
//! inline markdown (bold, italic, code, links, wiki-links, annotations).
//! this module provides a single parser that produces [`InlineElement`]s,
//! which each renderer converts to its own output format.

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
pub fn parse_inline(text: &str) -> Vec<InlineElement> {
    let chars = text.chars().collect::<Vec<_>>();
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
        if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '*' {
            flush(&mut out, &mut buf, bold, italic, code);
            bold = !bold;
            i += 2;
            continue;
        }

        // italic toggle: *
        if chars[i] == '*' {
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
            // wiki-link: [[text]]
            if i + 1 < chars.len() && chars[i + 1] == '[' {
                let mut j = i + 2;
                while j + 1 < chars.len() {
                    if chars[j] == ']' && chars[j + 1] == ']' {
                        break;
                    }
                    j += 1;
                }
                if j + 1 < chars.len() && chars[j] == ']' && chars[j + 1] == ']' {
                    flush(&mut out, &mut buf, bold, italic, code);
                    let link_text = chars[i + 2..j].iter().collect::<String>();
                    out.push(InlineElement::WikiLink { text: link_text });
                    i = j + 2;
                    continue;
                }
            }

            // url link: [text](url)
            let mut j = i + 1;
            while j < chars.len() && chars[j] != ']' {
                j += 1;
            }
            if j + 1 < chars.len() && chars[j] == ']' && chars[j + 1] == '(' {
                let mut k = j + 2;
                while k < chars.len() && chars[k] != ')' {
                    k += 1;
                }
                if k < chars.len() {
                    flush(&mut out, &mut buf, bold, italic, code);
                    let label = chars[i + 1..j].iter().collect::<String>();
                    let target = chars[j + 2..k].iter().collect::<String>();
                    out.push(InlineElement::Link { label, target });
                    i = k + 1;
                    continue;
                }
            }
        }

        buf.push(chars[i]);
        i += 1;
    }

    flush(&mut out, &mut buf, bold, italic, code);
    out
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
