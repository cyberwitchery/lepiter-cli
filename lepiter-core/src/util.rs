use std::path::{Component, Path, PathBuf};

use crate::inline_link::LinkKind;
use crate::model::{AttachmentError, AttachmentResult, Node};

/// Extracts raw link target strings from a node tree.
///
/// Finds explicit `Node::Link` urls plus inline `[label](target)` and `[[wikilink]]`
/// syntax in text-bearing nodes.
pub fn extract_link_targets(nodes: &[Node]) -> Vec<String> {
    extract_links(nodes)
        .into_iter()
        .map(|(kind, name)| match kind {
            LinkKind::Wiki => name.trim().to_string(),
            LinkKind::Markdown => name,
        })
        .collect()
}

/// each link in a node tree as its kind and [`crate::InlineLink::name`], in
/// source order; a `Node::Link` url is a markdown link.
pub(crate) fn extract_links(nodes: &[Node]) -> Vec<(LinkKind, String)> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            Node::Link { url, .. } => out.push((LinkKind::Markdown, url.clone())),
            Node::Paragraph { text }
            | Node::Text { text }
            | Node::Quote { text }
            | Node::Heading { text, .. } => out.extend(
                crate::inline_link::scan_inline_links(text)
                    .map(|link| (link.kind, link.name.to_string())),
            ),
            Node::List { items } => {
                for item in items {
                    out.extend(extract_links(item));
                }
            }
            _ => {}
        }
    }
    out
}

/// the page a `{{gtPage:…}}` annotation names.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PageAnnotation {
    /// the page title, as written.
    pub(crate) name: Option<String>,
    /// `id=`: the page's uid in base36.
    pub(crate) id: Option<String>,
    /// `db=`: the knowledge base's uid in base36.
    pub(crate) db: Option<String>,
}

/// the `{{gtPage:…}}` annotations in the text [`extract_link_targets`] reads,
/// outside inline code and links.
pub(crate) fn extract_page_annotations(nodes: &[Node]) -> Vec<PageAnnotation> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            Node::Paragraph { text }
            | Node::Text { text }
            | Node::Quote { text }
            | Node::Heading { text, .. } => page_annotations_in(text, &mut out),
            Node::List { items } => {
                for item in items {
                    out.extend(extract_page_annotations(item));
                }
            }
            _ => {}
        }
    }
    out
}

fn page_annotations_in(text: &str, out: &mut Vec<PageAnnotation>) {
    let bytes = text.as_bytes();
    let mut links = crate::inline_link::scan_inline_links(text).peekable();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(next) = crate::inline_link::past_backtick_run(text, i) {
            i = next;
            continue;
        }
        while links.next_if(|link| link.range.start < i).is_some() {}
        if let Some(link) = links.next_if(|link| link.range.start == i) {
            i = link.range.end;
            continue;
        }
        if bytes[i..].starts_with(b"{{") && !crate::inline_link::is_escaped(bytes, i) {
            let Some(len) = text[i + 2..].find("}}") else {
                break;
            };
            if let Some(body) = text[i + 2..i + 2 + len].strip_prefix("gtPage:") {
                out.push(parse_page_annotation(body));
            }
            i += len + 4;
            continue;
        }
        i += 1;
    }
}

/// reads `|`-separated items; the first `db=`, `id=` and bare title win.
fn parse_page_annotation(body: &str) -> PageAnnotation {
    let mut annotation = PageAnnotation::default();
    for item in body.split('|') {
        match item.split_once('=') {
            Some(("db", db)) if annotation.db.is_none() => annotation.db = Some(db.to_string()),
            Some(("id", id)) if annotation.id.is_none() => annotation.id = Some(id.to_string()),
            None if annotation.name.is_none() && !item.is_empty() => {
                annotation.name = Some(item.to_string());
            }
            _ => {}
        }
    }
    annotation
}

/// the uid GT spells in base36 for a page or knowledge base.
pub(crate) fn uid_from_base36(text: &str) -> Option<u128> {
    u128::from_str_radix(text, 36).ok()
}

/// the uid GT reads from `uuid`: its bytes, little-endian.
pub(crate) fn uid_from_uuid(uuid: &str) -> Option<u128> {
    u128::from_str_radix(&uuid.replace('-', ""), 16)
        .ok()
        .map(u128::swap_bytes)
}

/// the lowercase uuid string of a GT uid.
pub(crate) fn uuid_from_uid(uid: u128) -> String {
    let hex = format!("{:032x}", uid.swap_bytes());
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
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
    fn extract_page_annotations_skips_code_links_and_escapes() {
        let nodes = vec![Node::Paragraph {
            text: concat!(
                r"`{{gtPage:A}}` [{{gtPage:B}}](u) \{{gtPage:C}} {{see [a](b)}} [{{gtPage:F}}](u) ",
                "{{gtPage:D |label=d|db=x|id=y|db=z|id=w|E}} {{gtPage:|id=v}}",
            )
            .into(),
        }];
        assert_eq!(
            extract_page_annotations(&nodes),
            vec![
                PageAnnotation {
                    name: Some("D ".into()),
                    id: Some("y".into()),
                    db: Some("x".into()),
                },
                PageAnnotation {
                    name: None,
                    id: Some("v".into()),
                    db: None,
                },
            ]
        );
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

    // -- inline link target edge cases --

    fn inline_targets(text: &str) -> Vec<String> {
        extract_link_targets(&[Node::Paragraph { text: text.into() }])
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
