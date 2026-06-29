//! Shared scanner for inline `[[wikilink]]` and `[label](target)` syntax.
//!
//! A single byte-level walk underlies both link-target extraction (in
//! [`crate::util`]) and the export/import link rewriters in the tui crate, so
//! the grammar's edge cases live in one place instead of being re-implemented
//! per caller.
//!
//! This is distinct from the char-based styling parser in the tui crate's
//! `inline` module, which exists to render display spans and is intentionally
//! left separate.

use std::ops::Range;

/// Which inline-link syntax produced an [`InlineLink`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// `[[target]]`
    Wiki,
    /// `[label](target)`
    Markdown,
}

/// A single inline link located within a text run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineLink<'a> {
    pub kind: LinkKind,
    /// The display label. For [`LinkKind::Markdown`] this is the raw slice
    /// between `[` and `]`; for [`LinkKind::Wiki`] it equals `target`.
    pub label: &'a str,
    /// The link target, trimmed of surrounding whitespace. Never empty.
    pub target: &'a str,
    /// Byte range of the whole construct within the source text.
    pub range: Range<usize>,
}

/// Walks `text` and yields each `[[wikilink]]` / `[label](target)` it contains,
/// in source order.
///
/// Links whose target is empty (or whitespace-only) are skipped, and `[[wiki]]`
/// is matched before `[label](target)` at each position — matching the
/// behaviour every caller historically hand-rolled. All delimiters are ASCII,
/// so multi-byte UTF-8 content in labels and targets is handled correctly.
pub fn scan_inline_links(text: &str) -> InlineLinks<'_> {
    InlineLinks { text, i: 0 }
}

/// Iterator returned by [`scan_inline_links`].
pub struct InlineLinks<'a> {
    text: &'a str,
    i: usize,
}

impl<'a> Iterator for InlineLinks<'a> {
    type Item = InlineLink<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.text.as_bytes();
        while self.i < bytes.len() {
            let i = self.i;
            // [[wikilink]] — tried first so a `[[…]]` is never mistaken for a
            // markdown link opened by its first bracket.
            if i + 1 < bytes.len()
                && bytes[i] == b'['
                && bytes[i + 1] == b'['
                && let Some(end) = find_closing_double_bracket(bytes, i + 2)
            {
                let target = self.text[i + 2..end].trim();
                if !target.is_empty() {
                    self.i = end + 2;
                    return Some(InlineLink {
                        kind: LinkKind::Wiki,
                        label: target,
                        target,
                        range: i..end + 2,
                    });
                }
            }
            // [label](target)
            if bytes[i] == b'['
                && let Some(label_end) = find_byte(bytes, b']', i + 1)
                && label_end + 1 < bytes.len()
                && bytes[label_end + 1] == b'('
                && let Some(target_end) = find_byte(bytes, b')', label_end + 2)
            {
                let target = self.text[label_end + 2..target_end].trim();
                if !target.is_empty() {
                    self.i = target_end + 1;
                    return Some(InlineLink {
                        kind: LinkKind::Markdown,
                        label: &self.text[i + 1..label_end],
                        target,
                        range: i..target_end + 1,
                    });
                }
            }
            self.i += 1;
        }
        None
    }
}

/// Rewrites the inline links in `text`, leaving everything else byte-for-byte
/// intact.
///
/// For each link, `rewrite(kind, target)` may return `Some(new_target)` to
/// replace it — rendered as `[label](new_target)`, where wiki links use their
/// target as the label — or `None` to leave the original span verbatim
/// (preserving any internal whitespace).
pub fn rewrite_inline_links(
    text: &str,
    mut rewrite: impl FnMut(LinkKind, &str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for link in scan_inline_links(text) {
        out.push_str(&text[cursor..link.range.start]);
        match rewrite(link.kind, link.target) {
            Some(new_target) => {
                out.push('[');
                out.push_str(link.label);
                out.push_str("](");
                out.push_str(&new_target);
                out.push(')');
            }
            None => out.push_str(&text[link.range.clone()]),
        }
        cursor = link.range.end;
    }
    out.push_str(&text[cursor..]);
    out
}

fn find_closing_double_bracket(bytes: &[u8], start: usize) -> Option<usize> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(text: &str) -> Vec<InlineLink<'_>> {
        scan_inline_links(text).collect()
    }

    #[test]
    fn scans_wiki_kind_label_target_range() {
        let links = scan("see [[My Page]] here");
        assert_eq!(links.len(), 1);
        let link = &links[0];
        assert_eq!(link.kind, LinkKind::Wiki);
        assert_eq!(link.label, "My Page");
        assert_eq!(link.target, "My Page");
        // "see " is 4 bytes; "[[My Page]]" is 11 bytes
        assert_eq!(link.range, 4..15);
        assert_eq!(&"see [[My Page]] here"[link.range.clone()], "[[My Page]]");
    }

    #[test]
    fn scans_markdown_kind_label_target_range() {
        let links = scan("a [label](page:abc) b");
        assert_eq!(links.len(), 1);
        let link = &links[0];
        assert_eq!(link.kind, LinkKind::Markdown);
        assert_eq!(link.label, "label");
        assert_eq!(link.target, "page:abc");
        assert_eq!(link.range, 2..19);
        assert_eq!(
            &"a [label](page:abc) b"[link.range.clone()],
            "[label](page:abc)"
        );
    }

    #[test]
    fn scans_mixed_in_source_order() {
        let links = scan("see [[wiki]] and [md](target) done");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].kind, LinkKind::Wiki);
        assert_eq!(links[0].target, "wiki");
        assert_eq!(links[1].kind, LinkKind::Markdown);
        assert_eq!(links[1].label, "md");
        assert_eq!(links[1].target, "target");
    }

    #[test]
    fn target_is_trimmed_but_range_covers_whitespace() {
        let links = scan("[label](  url  )");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "url");
        assert_eq!(links[0].range, 0..16);
    }

    #[test]
    fn empty_targets_are_skipped() {
        assert!(scan("[]()").is_empty());
        assert!(scan("[[]]").is_empty());
        assert!(scan("[label](   )").is_empty());
        assert!(scan("[[   ]]").is_empty());
    }

    #[test]
    fn wiki_takes_precedence_over_markdown() {
        // A complete [[…]] is reported as Wiki even when it also contains `](`.
        let links = scan("[[x](y)]]");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Wiki);
        assert_eq!(links[0].target, "x](y)");
    }

    #[test]
    fn unclosed_wiki_falls_through_to_markdown() {
        // No closing `]]`, so the leading `[` opens a markdown link instead.
        let links = scan("[[text](url)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Markdown);
        assert_eq!(links[0].label, "[text");
        assert_eq!(links[0].target, "url");
    }

    #[test]
    fn unicode_content_in_labels_and_targets() {
        let links = scan("[名前](ページ) [[日本語]]");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "ページ");
        assert_eq!(links[1].target, "日本語");
    }

    #[test]
    fn rewrite_replaces_with_callback_target() {
        let out = rewrite_inline_links("see [a](b) and [[c]]", |kind, target| match kind {
            LinkKind::Markdown => Some(format!("md:{target}")),
            LinkKind::Wiki => Some(format!("wiki:{target}")),
        });
        assert_eq!(out, "see [a](md:b) and [c](wiki:c)");
    }

    #[test]
    fn rewrite_none_leaves_span_verbatim() {
        // Whitespace inside an untouched span is preserved exactly.
        let out = rewrite_inline_links("[a](  keep  ) and [[ keep ]]", |_, _| None);
        assert_eq!(out, "[a](  keep  ) and [[ keep ]]");
    }

    #[test]
    fn rewrite_callback_sees_kind_and_trimmed_target() {
        let mut seen = Vec::new();
        let _ = rewrite_inline_links("[a]( b ) [[ c ]]", |kind, target| {
            seen.push((kind, target.to_string()));
            None
        });
        assert_eq!(
            seen,
            vec![
                (LinkKind::Markdown, "b".to_string()),
                (LinkKind::Wiki, "c".to_string()),
            ]
        );
    }

    #[test]
    fn rewrite_is_identity_on_plain_text() {
        let out = rewrite_inline_links("no links here at all", |_, _| Some("x".to_string()));
        assert_eq!(out, "no links here at all");
    }
}
