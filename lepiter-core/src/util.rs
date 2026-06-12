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
/// Works directly with byte offsets instead of collecting into `Vec<char>`.
/// All delimiters (`[`, `]`, `(`, `)`) are ASCII, so no UTF-8 continuation
/// byte can produce a false match.
fn extract_inline_link_targets(text: &str, out: &mut Vec<String>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // [[wikilink]]
        if i + 1 < bytes.len() && bytes[i] == b'[' && bytes[i + 1] == b'[' {
            let start = i + 2;
            if let Some(end) = find_closing_double_bracket_bytes(bytes, start) {
                let target = text[start..end].trim();
                if !target.is_empty() {
                    out.push(target.to_string());
                }
                i = end + 2;
                continue;
            }
        }
        // [label](target)
        if bytes[i] == b'[' {
            let label_start = i + 1;
            if let Some(label_end) = find_byte(bytes, b']', label_start)
                && label_end + 1 < bytes.len()
                && bytes[label_end + 1] == b'('
            {
                let target_start = label_end + 2;
                if let Some(target_end) = find_byte(bytes, b')', target_start) {
                    let target = text[target_start..target_end].trim();
                    if !target.is_empty() {
                        out.push(target.to_string());
                    }
                    i = target_end + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
}

fn find_closing_double_bracket_bytes(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b']' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_byte(bytes: &[u8], target: u8, start: usize) -> Option<usize> {
    (start..bytes.len()).find(|&i| bytes[i] == target)
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
        let cand = &input[i..i + 36];
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
        // Direct "attachments/" prefix
        assert_eq!(
            extract_attachment_relative("attachments/image.png"),
            Some("attachments/image.png")
        );
        // Path containing "/attachments/"
        assert_eq!(
            extract_attachment_relative("abc/attachments/image.png"),
            Some("attachments/image.png")
        );
        // Bare "attachments/" found mid-string (no leading slash)
        assert_eq!(
            extract_attachment_relative("data_attachments/image.png"),
            Some("attachments/image.png")
        );
        // No attachment path at all
        assert_eq!(extract_attachment_relative("image.png"), None);
        assert_eq!(extract_attachment_relative(""), None);
    }

    #[test]
    fn extract_attachment_relative_nested_paths() {
        // Deeply nested path before attachments/
        assert_eq!(
            extract_attachment_relative("a/b/c/attachments/deep.png"),
            Some("attachments/deep.png")
        );
        // Multiple attachments/ segments — should match first /attachments/
        assert_eq!(
            extract_attachment_relative("x/attachments/attachments/file.txt"),
            Some("attachments/attachments/file.txt")
        );
    }

    #[test]
    fn extract_attachment_relative_no_filename() {
        // Trailing slash, no filename
        assert_eq!(
            extract_attachment_relative("attachments/"),
            Some("attachments/")
        );
        assert_eq!(
            extract_attachment_relative("foo/attachments/"),
            Some("attachments/")
        );
    }

    #[test]
    fn extract_attachment_relative_only_keyword() {
        // "attachments" without trailing slash
        assert_eq!(extract_attachment_relative("attachments"), None);
    }

    #[test]
    fn is_external_target_common_schemes() {
        assert!(is_external_target("http://example.com"));
        assert!(is_external_target("https://example.com"));
        assert!(is_external_target("mailto:user@example.com"));
        assert!(is_external_target("file:///home/user/doc.txt"));
        assert!(is_external_target("ftp://files.example.com"));
        assert!(is_external_target("ssh://host.example.com"));
    }

    #[test]
    fn is_external_target_mixed_case() {
        assert!(is_external_target("HTTP://EXAMPLE.COM"));
        assert!(is_external_target("Https://Example.Com"));
        assert!(is_external_target("MAILTO:USER@EXAMPLE.COM"));
        assert!(is_external_target("File://localhost/path"));
        assert!(is_external_target("hTtPs://mixed.case"));
        assert!(is_external_target("FTP://FILES.EXAMPLE.COM"));
    }

    #[test]
    fn is_external_target_rejects_non_urls() {
        assert!(!is_external_target("attachments/image.png"));
        assert!(!is_external_target("page:some-id"));
        assert!(!is_external_target("title:My Page"));
        assert!(!is_external_target("just some text"));
        assert!(!is_external_target(""));
        assert!(!is_external_target("   "));
        assert!(!is_external_target("not-a-scheme"));
    }

    #[test]
    fn is_external_target_boundary_inputs() {
        // scheme separator at the very start
        assert!(is_external_target("://bare"));
        // single character scheme
        assert!(is_external_target("x://y"));
        // scheme with numbers
        assert!(is_external_target("h323://voip.example.com"));
        // just the scheme prefix, no content after
        assert!(is_external_target("http://"));
        assert!(is_external_target("https://"));
        assert!(is_external_target("mailto:"));
    }
}
