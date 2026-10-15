use std::path::{Component, Path, PathBuf};

use crate::model::{AttachmentError, AttachmentResult, Node};

/// Extracts raw link target strings from a node tree.
///
/// Finds explicit `Node::Link` urls plus inline `[label](target)` and `[[wikilink]]`
/// syntax in text-bearing nodes.
pub fn extract_link_targets(nodes: &[Node]) -> Vec<String> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            Node::Link { url, .. } => out.push(url.clone()),
            Node::Paragraph { text }
            | Node::Text { text }
            | Node::Quote { text }
            | Node::Heading { text, .. } => {
                extract_inline_link_targets(text, &mut out);
            }
            Node::List { items } => {
                for item in items {
                    out.extend(extract_link_targets(item));
                }
            }
            _ => {}
        }
    }
    out
}

/// Extracts `[label](target)` and `[[wikilink]]` targets from inline text.
///
/// Delegates to the shared [`crate::inline_link`] scanner so the link grammar
/// lives in one place; collects each link's trimmed target in source order.
fn extract_inline_link_targets(text: &str, out: &mut Vec<String>) {
    for link in crate::inline_link::scan_inline_links(text) {
        out.push(link.target.to_string());
    }
}

fn starts_with_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len()
        && haystack[..needle.len()]
            .iter()
            .zip(needle)
            .all(|(h, n)| h.to_ascii_lowercase() == *n)
}

fn contains_scheme_separator(target: &[u8]) -> bool {
    target.windows(3).any(|w| w == b"://")
}

pub(crate) fn is_external_target(target: &str) -> bool {
    let bytes = target.as_bytes();
    starts_with_ignore_ascii_case(bytes, b"http://")
        || starts_with_ignore_ascii_case(bytes, b"https://")
        || starts_with_ignore_ascii_case(bytes, b"mailto:")
        || starts_with_ignore_ascii_case(bytes, b"file://")
        || contains_scheme_separator(bytes)
}

pub(crate) fn extract_attachment_relative(target: &str) -> Option<&str> {
    if target.starts_with("attachments/") {
        return Some(target);
    }
    if let Some(pos) = target.find("/attachments/") {
        let start = pos + 1;
        return target.get(start..);
    }
    if let Some(pos) = target.find("attachments/") {
        return target.get(pos..);
    }
    None
}

pub(crate) fn sanitize_relative_path(rel: &str) -> AttachmentResult<PathBuf> {
    let rel = rel.trim();
    if rel.is_empty() {
        return Err(AttachmentError::Empty);
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return Err(AttachmentError::EscapesRoot(rel.to_string()));
    }
    for comp in path.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err(AttachmentError::EscapesRoot(rel.to_string()));
            }
            _ => {}
        }
    }
    Ok(path.to_path_buf())
}

pub(crate) fn extract_uuid_like(input: &str) -> Option<&str> {
    let bytes = input.as_bytes();
    if bytes.len() < 36 {
        return None;
    }

    for i in 0..=bytes.len() - 36 {
        // `i` walks byte offsets, so it can land inside a multi-byte UTF-8
        // char; `get` yields None there instead of panicking. A UUID is pure
        // ASCII, so a non-boundary slice can never be a real candidate.
        let Some(cand) = input.get(i..i + 36) else {
            continue;
        };
        let ok = cand.chars().enumerate().all(|(idx, c)| match idx {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        });
        if ok {
            return Some(cand);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_link_targets_finds_link_nodes() {
        let nodes = vec![
            Node::Link {
                text: "page".into(),
                url: "page:abc".into(),
            },
            Node::Paragraph {
                text: "plain text".into(),
            },
        ];
        assert_eq!(extract_link_targets(&nodes), vec!["page:abc"]);
    }

    #[test]
    fn extract_link_targets_finds_inline_markdown_links() {
        let nodes = vec![Node::Paragraph {
            text: "see [docs](https://docs.rs) and [api](page:xyz)".into(),
        }];
        let targets = extract_link_targets(&nodes);
        assert_eq!(targets, vec!["https://docs.rs", "page:xyz"]);
    }

    #[test]
    fn extract_link_targets_finds_wiki_links() {
        let nodes = vec![Node::Text {
            text: "see also [[My Page]] and [[Other]]".into(),
        }];
        let targets = extract_link_targets(&nodes);
        assert_eq!(targets, vec!["My Page", "Other"]);
    }

    #[test]
    fn extract_link_targets_handles_headings_and_quotes() {
        let nodes = vec![
            Node::Heading {
                level: 2,
                text: "about [[Topic]]".into(),
            },
            Node::Quote {
                text: "see [ref](page:ref-id)".into(),
            },
        ];
        let targets = extract_link_targets(&nodes);
        assert_eq!(targets, vec!["Topic", "page:ref-id"]);
    }

    #[test]
    fn extract_link_targets_recurses_into_lists() {
        let nodes = vec![Node::List {
            items: vec![vec![Node::Paragraph {
                text: "item with [[link]]".into(),
            }]],
        }];
        let targets = extract_link_targets(&nodes);
        assert_eq!(targets, vec!["link"]);
    }

    #[test]
    fn extract_link_targets_ignores_code_and_unknown() {
        let nodes = vec![
            Node::Code {
                language: None,
                code: "[not](a-link)".into(),
            },
            Node::Unknown {
                typ: "x".into(),
                raw: json!({}),
            },
        ];
        assert!(extract_link_targets(&nodes).is_empty());
    }

    #[test]
    fn extract_attachment_relative_covers_all_branches() {
        let cases: &[(&str, Option<&str>)] = &[
            // direct "attachments/" prefix
            ("attachments/image.png", Some("attachments/image.png")),
            // path containing "/attachments/"
            ("abc/attachments/image.png", Some("attachments/image.png")),
            // bare "attachments/" found mid-string (no leading slash)
            ("data_attachments/image.png", Some("attachments/image.png")),
            // deeply nested path before attachments/
            ("a/b/c/attachments/deep.png", Some("attachments/deep.png")),
            // several attachments/ segments: the first one wins
            (
                "x/attachments/attachments/file.txt",
                Some("attachments/attachments/file.txt"),
            ),
            // trailing slash, no filename
            ("attachments/", Some("attachments/")),
            ("foo/attachments/", Some("attachments/")),
            // "attachments" without trailing slash
            ("attachments", None),
            // no attachment path at all
            ("image.png", None),
            ("", None),
        ];
        for (input, expected) in cases {
            assert_eq!(extract_attachment_relative(input), *expected, "{input:?}");
        }
    }

    #[test]
    fn is_external_target_classifies_inputs() {
        let cases: &[(&str, bool)] = &[
            // common schemes
            ("http://example.com", true),
            ("https://example.com", true),
            ("mailto:user@example.com", true),
            ("file:///home/user/doc.txt", true),
            ("ftp://files.example.com", true),
            ("ssh://host.example.com", true),
            // mixed case
            ("HTTP://EXAMPLE.COM", true),
            ("Https://Example.Com", true),
            ("MAILTO:USER@EXAMPLE.COM", true),
            ("File://localhost/path", true),
            ("hTtPs://mixed.case", true),
            ("FTP://FILES.EXAMPLE.COM", true),
            // scheme separator at the very start
            ("://bare", true),
            // single character scheme
            ("x://y", true),
            // scheme with numbers
            ("h323://voip.example.com", true),
            // just the scheme prefix, no content after
            ("http://", true),
            ("https://", true),
            ("mailto:", true),
            // not urls
            ("attachments/image.png", false),
            ("page:some-id", false),
            ("title:My Page", false),
            ("just some text", false),
            ("", false),
            ("   ", false),
            ("not-a-scheme", false),
        ];
        for (input, expected) in cases {
            assert_eq!(is_external_target(input), *expected, "{input:?}");
        }
    }

    // -- extract_inline_link_targets edge cases --

    /// Helper: call the private extract_inline_link_targets directly.
    fn inline_targets(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        extract_inline_link_targets(text, &mut out);
        out
    }

    #[test]
    fn inline_consecutive_links() {
        assert_eq!(inline_targets("[a](b)[c](d)"), vec!["b", "d"],);
    }

    #[test]
    fn inline_wikilink_unclosed_single_bracket() {
        // [[text — no closing ]], falls through to markdown link check which also fails
        assert!(inline_targets("[[text").is_empty());
    }

    #[test]
    fn inline_bracket_without_paren() {
        // [text] alone — no ( follows the ], so no link
        assert!(inline_targets("[text]").is_empty());
        // [text] followed by something other than (
        assert!(inline_targets("[text] rest").is_empty());
    }

    #[test]
    fn inline_empty_input() {
        assert!(inline_targets("").is_empty());
    }
}
