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

    // -- extract_inline_link_targets edge cases --

    /// Helper: call the private extract_inline_link_targets directly.
    fn inline_targets(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        extract_inline_link_targets(text, &mut out);
        out
    }

    #[test]
    fn inline_unclosed_bracket() {
        // [unclosed( — no closing ] so no link is formed
        assert!(inline_targets("[unclosed(").is_empty());
    }

    #[test]
    fn inline_unclosed_paren() {
        // [text]( — ] found but no closing )
        assert!(inline_targets("[text](").is_empty());
    }

    #[test]
    fn inline_empty_target() {
        // []() — empty target string is skipped
        assert!(inline_targets("[]()").is_empty());
    }

    #[test]
    fn inline_nested_brackets() {
        assert_eq!(
            inline_targets("[text with [inner] brackets](url)"),
            vec!["url"]
        );
    }

    #[test]
    fn inline_linked_image_yields_the_link_not_the_image_source() {
        assert_eq!(
            inline_targets("[![img](a.png)](page:abc)"),
            vec!["page:abc"]
        );
    }

    #[test]
    fn inline_consecutive_links() {
        assert_eq!(inline_targets("[a](b)[c](d)"), vec!["b", "d"],);
    }

    #[test]
    fn inline_empty_wikilink() {
        // [[]] — empty target after trim is skipped
        assert!(inline_targets("[[]]").is_empty());
    }

    #[test]
    fn inline_unclosed_wikilink() {
        // [[ with no closing ]] — no target extracted
        assert!(inline_targets("[[").is_empty());
    }

    #[test]
    fn inline_wikilink_unclosed_single_bracket() {
        // [[text — no closing ]], falls through to markdown link check which also fails
        assert!(inline_targets("[[text").is_empty());
    }

    #[test]
    fn inline_unicode_link_target() {
        // Delimiters are ASCII, so multi-byte UTF-8 content is handled correctly
        assert_eq!(inline_targets("[名前](ページ)"), vec!["ページ"],);
    }

    #[test]
    fn inline_unicode_wikilink() {
        assert_eq!(inline_targets("[[日本語ページ]]"), vec!["日本語ページ"],);
    }

    #[test]
    fn inline_whitespace_only_target() {
        // [label](   ) — target is whitespace-only, trimmed to empty, skipped
        assert!(inline_targets("[label](   )").is_empty());
    }

    #[test]
    fn inline_whitespace_only_wikilink() {
        // [[   ]] — whitespace-only, trimmed to empty, skipped
        assert!(inline_targets("[[   ]]").is_empty());
    }

    #[test]
    fn inline_target_trimmed() {
        // Surrounding whitespace is trimmed from the target
        assert_eq!(inline_targets("[label](  url  )"), vec!["url"],);
    }

    #[test]
    fn inline_mixed_links_and_wikilinks() {
        assert_eq!(
            inline_targets("see [[wiki]] and [md](target) done"),
            vec!["wiki", "target"],
        );
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

    #[test]
    fn inline_no_links() {
        assert!(inline_targets("just plain text").is_empty());
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
