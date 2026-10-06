//! Shared scanner for inline `[[wikilink]]` and `[label](target)` syntax.
//!
//! A single byte-level walk underlies both link-target extraction (in
//! [`crate::util`]) and the export/import link rewriters in the tui crate, so
//! the grammar's edge cases live in one place instead of being re-implemented
//! per caller.
//!
//! The tui crate's `inline` module locates links with this scanner too, and
//! parses only the styling the scanner does not describe.

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
    /// between `[` and its matching `]`; for [`LinkKind::Wiki`] it equals
    /// `target`.
    pub label: &'a str,
    /// The link target, exactly as written between `<` and `>` when it is
    /// bracketed and trimmed of surrounding whitespace otherwise. Never blank.
    pub target: &'a str,
    /// Byte range of the whole construct within the source text.
    pub range: Range<usize>,
}

/// Walks `text` and yields each `[[wikilink]]` / `[label](target)` it contains,
/// in source order.
///
/// Links whose target is empty (or whitespace-only) are skipped, and `[[wiki]]`
/// is matched before `[label](target)` at each position. All delimiters are
/// ASCII, so multi-byte UTF-8 content in labels and targets is handled
/// correctly.
///
/// A `[label](target)` label may contain brackets and its target parentheses,
/// as long as each balances; an unbalanced `[` or `(` yields no link at that
/// position. A label may nest an image, and the outer target is the one
/// reported; a label nesting a link of its own is not a label at all, so the
/// nested link is reported instead — a link inside an image's alt text is part
/// of the image, so it does not count.
///
/// A `<target>` may hold spaces and any parentheses, but no line break or
/// unescaped `<` or `>`; a target opening with `<` that does not close so is no link.
///
/// A `[`, `]`, `(`, `)`, `<` or `>` preceded by an odd number of backslashes is
/// literal text and delimits nothing. Labels and targets are reported as raw
/// slices, so an escape inside one is still spelled with its backslash.
pub fn scan_inline_links(text: &str) -> InlineLinks<'_> {
    InlineLinks {
        text,
        i: 0,
        guard_nesting: true,
        keep_blank: false,
    }
}

/// Scans without the nested-link guard, yielding a label's outermost links only.
fn scan_shallow(text: &str) -> InlineLinks<'_> {
    InlineLinks {
        text,
        i: 0,
        guard_nesting: false,
        keep_blank: false,
    }
}

/// The one `[label](target)` link that is all of the trimmed `line`, blank target allowed.
pub(crate) fn whole_line_link(line: &str) -> Option<InlineLink<'_>> {
    let line = line.trim();
    let link = InlineLinks {
        text: line,
        i: 0,
        guard_nesting: true,
        keep_blank: true,
    }
    .next()?;
    (link.kind == LinkKind::Markdown && link.range == (0..line.len())).then_some(link)
}

/// Iterator returned by [`scan_inline_links`].
pub struct InlineLinks<'a> {
    text: &'a str,
    i: usize,
    guard_nesting: bool,
    keep_blank: bool,
}

impl<'a> Iterator for InlineLinks<'a> {
    type Item = InlineLink<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.text.as_bytes();
        while self.i < bytes.len() {
            let i = self.i;
            // [[wikilink]]
            if i + 1 < bytes.len()
                && bytes[i] == b'['
                && bytes[i + 1] == b'['
                && !is_escaped(bytes, i)
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
                && !is_escaped(bytes, i)
                && let Some(label_end) = find_balanced_close_bracket(bytes, i + 1)
                && label_end + 1 < bytes.len()
                && bytes[label_end + 1] == b'('
                && let Some((target, target_end)) = find_destination(self.text, label_end + 2)
                && (self.keep_blank || !target.trim().is_empty())
                && (!self.guard_nesting || !nests_a_link(&self.text[i + 1..label_end]))
            {
                self.i = target_end + 1;
                return Some(InlineLink {
                    kind: LinkKind::Markdown,
                    label: &self.text[i + 1..label_end],
                    target,
                    range: i..target_end + 1,
                });
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
            Some(new_target) => push_link(&mut out, link.label, &new_target),
            None => out.push_str(&text[link.range.clone()]),
        }
        cursor = link.range.end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Writes `[label](target)`, bracketing a target that would not read back bare.
pub(crate) fn push_link(out: &mut String, label: &str, target: &str) {
    out.push('[');
    out.push_str(label);
    out.push_str("](");
    if needs_angle_brackets(target) {
        let bytes = target.as_bytes();
        out.push('<');
        for (i, c) in target.char_indices() {
            if matches!(c, '<' | '>') && !is_escaped(bytes, i) {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('>');
    } else {
        out.push_str(target);
    }
    out.push(')');
}

fn needs_angle_brackets(target: &str) -> bool {
    target.starts_with('<')
        || target.starts_with(char::is_whitespace)
        || target.ends_with(char::is_whitespace)
        || target.contains(|c: char| c == ' ' || c.is_ascii_control())
        || !parens_balance(target.as_bytes())
}

fn parens_balance(bytes: &[u8]) -> bool {
    let mut depth = 0usize;
    for (i, &byte) in bytes.iter().enumerate() {
        if !matches!(byte, b'(' | b')') || is_escaped(bytes, i) {
            continue;
        }
        match byte {
            b'(' => depth += 1,
            _ if depth == 0 => return false,
            _ => depth -= 1,
        }
    }
    depth == 0
}

/// Backslash-escapes each of `special`, and each backslash that would escape what follows it.
pub(crate) fn escape(text: &str, special: &[char]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let escapes_next = c == '\\' && chars.peek().is_none_or(char::is_ascii_punctuation);
        if escapes_next || special.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Drops each backslash that escapes ASCII punctuation.
pub(crate) fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match chars.peek() {
            Some(&next) if c == '\\' && next.is_ascii_punctuation() => {
                out.push(next);
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

/// Whether the byte at `at` is preceded by an odd number of backslashes.
fn is_escaped(bytes: &[u8], at: usize) -> bool {
    bytes[..at]
        .iter()
        .rev()
        .take_while(|&&b| b == b'\\')
        .count()
        % 2
        == 1
}

fn find_closing_double_bracket(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b']' && !is_escaped(bytes, i) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_balanced_close_bracket(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &byte) in bytes.iter().enumerate().skip(start) {
        if !matches!(byte, b'[' | b']') || is_escaped(bytes, i) {
            continue;
        }
        match byte {
            b'[' => depth += 1,
            b']' if depth == 0 => return Some(i),
            _ => depth -= 1,
        }
    }
    None
}

/// Reports whether `label` contains a link other than an image.
fn nests_a_link(label: &str) -> bool {
    let bytes = label.as_bytes();
    label.contains('[')
        && scan_shallow(label).any(|link| {
            link.kind != LinkKind::Markdown
                || link.range.start == 0
                || bytes[link.range.start - 1] != b'!'
                || is_escaped(bytes, link.range.start - 1)
        })
}

/// The target of the link whose `(` precedes `start`, and the index of its closing `)`.
fn find_destination(text: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = text.as_bytes();
    let open = text.len() - text[start..].trim_start().len();
    if bytes.get(open) != Some(&b'<') {
        let end = find_balanced_close_paren(bytes, start)?;
        return Some((text[start..end].trim(), end));
    }
    let close = find_angle_close(bytes, open + 1)?;
    let end = text.len() - text[close + 1..].trim_start().len();
    (bytes.get(end) == Some(&b')')).then(|| (&text[open + 1..close], end))
}

fn find_angle_close(bytes: &[u8], start: usize) -> Option<usize> {
    for (i, &byte) in bytes.iter().enumerate().skip(start) {
        match byte {
            b'\n' | b'\r' => return None,
            b'<' | b'>' if is_escaped(bytes, i) => {}
            b'<' => return None,
            b'>' => return Some(i),
            _ => {}
        }
    }
    None
}

fn find_balanced_close_paren(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &byte) in bytes.iter().enumerate().skip(start) {
        if !matches!(byte, b'(' | b')') || is_escaped(bytes, i) {
            continue;
        }
        match byte {
            b'(' => depth += 1,
            b')' if depth == 0 => return Some(i),
            _ => depth -= 1,
        }
    }
    None
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
    fn an_escaped_bracket_opens_no_link() {
        assert!(scan(r"\[not a link](x)").is_empty());
        assert!(scan(r"\[\[not a wiki]]").is_empty());
        assert!(scan(r"\[[not a wiki]]").is_empty());
        assert!(scan(r"\!\[a](b)").is_empty());
    }

    #[test]
    fn an_escaped_delimiter_does_not_close_a_link() {
        let links = scan(r"[a\](b)](c)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label, r"a\](b)");
        assert_eq!(links[0].target, "c");

        let links = scan(r"[a](b\)c)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, r"b\)c");

        let links = scan(r"[a](b\(c\)d)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, r"b\(c\)d");
    }

    #[test]
    fn an_escaped_close_does_not_end_a_wiki_link() {
        let links = scan(r"[[a\]]b]]");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Wiki);
        assert_eq!(links[0].target, r"a\]]b");
    }

    #[test]
    fn a_doubled_backslash_leaves_the_delimiter_live() {
        let links = scan(r"\\[a](b)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "b");
        assert_eq!(links[0].range, 2..8);

        let links = scan(r"[a](b\\)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, r"b\\");
    }

    #[test]
    fn an_escaped_bang_leaves_a_nested_link_nesting() {
        let links = scan(r"[\![a](b)](c)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "b");

        let links = scan(r"[![a](b)](c)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "c");
    }

    #[test]
    fn a_backslash_before_ordinary_text_delimits_nothing() {
        let links = scan(r"C:\Users\foo [a](b)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "b");

        let links = scan("[a](b) tail\\");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "b");
    }

    #[test]
    fn rewriting_leaves_an_escaped_link_verbatim() {
        let out = rewrite_inline_links(r"\[a](b) and [c](d)", |_, target| {
            Some(format!("{target}!"))
        });
        assert_eq!(out, r"\[a](b) and [c](d!)");
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
        // the `[[…]]` body itself contains `](`
        let links = scan("[[x](y)]]");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Wiki);
        assert_eq!(links[0].target, "x](y)");
    }

    #[test]
    fn unclosed_wiki_falls_through_to_markdown() {
        let links = scan("[[text](url)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Markdown);
        assert_eq!(links[0].label, "text");
        assert_eq!(links[0].target, "url");
        assert_eq!(links[0].range, 1..12);
    }

    #[test]
    fn markdown_label_keeps_balanced_brackets() {
        let text = "[a [b] c](t)";
        let links = scan(text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label, "a [b] c");
        assert_eq!(links[0].target, "t");
        assert_eq!(links[0].range, 0..text.len());
    }

    #[test]
    fn markdown_label_keeps_nested_brackets() {
        let text = "[a [b [c] d] e](t)";
        let links = scan(text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label, "a [b [c] d] e");
        assert_eq!(links[0].target, "t");
        assert_eq!(links[0].range, 0..text.len());
    }

    #[test]
    fn linked_image_yields_the_outer_target() {
        let text = "[![img](a.png)](target)";
        let links = scan(text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Markdown);
        assert_eq!(links[0].label, "![img](a.png)");
        assert_eq!(links[0].target, "target");
        assert_eq!(links[0].range, 0..text.len());
    }

    #[test]
    fn unbalanced_open_bracket_yields_no_link() {
        assert!(scan("[a [b] c(t)").is_empty());
        assert!(scan("[a [b(t)").is_empty());
    }

    #[test]
    fn unbalanced_close_bracket_yields_no_link() {
        assert!(scan("[a]](t)").is_empty());
        assert!(scan("a] b](t)").is_empty());
    }

    #[test]
    fn unbalanced_label_does_not_swallow_a_later_link() {
        let links = scan("[a [unclosed and [b](ok)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label, "b");
        assert_eq!(links[0].target, "ok");
    }

    #[test]
    fn bracket_label_link_beside_a_plain_one() {
        let links = scan("[a [b] c](t) and [d](e)");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].label, "a [b] c");
        assert_eq!(links[0].target, "t");
        assert_eq!(links[1].label, "d");
        assert_eq!(links[1].target, "e");
    }

    #[test]
    fn wiki_nested_in_a_label_still_wins() {
        let links = scan("[label [[wiki]] more](t)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Wiki);
        assert_eq!(links[0].target, "wiki");
    }

    #[test]
    fn markdown_link_nested_in_a_label_wins_over_the_outer() {
        let links = scan("[a [b](ok) c](t)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label, "b");
        assert_eq!(links[0].target, "ok");
    }

    #[test]
    fn a_link_in_an_images_alt_text_leaves_the_outer_target() {
        let text = "[![[x](y)](z)](t)";
        let links = scan(text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label, "![[x](y)](z)");
        assert_eq!(links[0].target, "t");
        assert_eq!(links[0].range, 0..text.len());
    }

    #[test]
    fn deeply_nested_links_do_not_blow_up_the_scan() {
        for innermost in ["![a](p)", "[x](y)"] {
            let mut text = innermost.to_string();
            for _ in 0..24 {
                text = format!("[{text}](t)");
            }
            let started = std::time::Instant::now();
            let links = scan(&text);
            let elapsed = started.elapsed();
            assert_eq!(links.len(), 1);
            assert!(
                elapsed < std::time::Duration::from_secs(2),
                "scanning {} bytes of nesting took {elapsed:?}",
                text.len()
            );
        }
    }

    #[test]
    fn rewrite_a_linked_image_touches_only_the_outer_target() {
        let mut seen = Vec::new();
        let out = rewrite_inline_links("see [![img](a.png)](page:abc) here", |_, target| {
            seen.push(target.to_string());
            Some(format!("{target}.md"))
        });
        assert_eq!(seen, vec!["page:abc"]);
        assert_eq!(out, "see [![img](a.png)](page:abc.md) here");
    }

    #[test]
    fn rewrite_none_leaves_a_bracket_label_verbatim() {
        let text = "[a [b] c](  t  ) and [![img](a.png)](u)";
        assert_eq!(rewrite_inline_links(text, |_, _| None), text);
    }

    #[test]
    fn markdown_target_keeps_balanced_parens() {
        let text = "see [Ruby](https://en.wikipedia.org/wiki/Ruby_(programming_language)) here";
        let links = scan(text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label, "Ruby");
        assert_eq!(
            links[0].target,
            "https://en.wikipedia.org/wiki/Ruby_(programming_language)"
        );
        assert_eq!(
            &text[links[0].range.clone()],
            "[Ruby](https://en.wikipedia.org/wiki/Ruby_(programming_language))"
        );
    }

    #[test]
    fn markdown_target_keeps_nested_parens() {
        let text = "[x](a(b(c)d)e)";
        let links = scan(text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "a(b(c)d)e");
        assert_eq!(links[0].range, 0..text.len());
    }

    #[test]
    fn unbalanced_open_paren_yields_no_link() {
        assert!(scan("[x](a(b)").is_empty());
        assert!(scan("[x](a(b").is_empty());
    }

    #[test]
    fn unbalanced_target_does_not_swallow_a_later_link() {
        let links = scan("[a](un(closed and [b](ok)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label, "b");
        assert_eq!(links[0].target, "ok");
    }

    #[test]
    fn close_paren_after_a_link_is_not_part_of_the_target() {
        let links = scan("(see [a](b) too)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "b");
        assert_eq!(links[0].range, 5..11);
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
    fn rewrite_replaces_a_paren_url_without_orphaning_a_tail() {
        let text = "see [Ruby](https://en.wikipedia.org/wiki/Ruby_(programming_language)) here";
        let mut seen = Vec::new();
        let out = rewrite_inline_links(text, |_, target| {
            seen.push(target.to_string());
            Some("page:abc".to_string())
        });
        assert_eq!(
            seen,
            vec!["https://en.wikipedia.org/wiki/Ruby_(programming_language)"]
        );
        assert_eq!(out, "see [Ruby](page:abc) here");
    }

    #[test]
    fn an_angle_bracket_target_is_reported_without_its_brackets() {
        let text = "[a](<my file (1).md>) b";
        let links = scan(text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label, "a");
        assert_eq!(links[0].target, "my file (1).md");
        assert_eq!(&text[links[0].range.clone()], "[a](<my file (1).md>)");
    }

    #[test]
    fn an_angle_bracket_target_may_hold_unbalanced_parens() {
        assert_eq!(scan("[a](<x(y>)")[0].target, "x(y");
        assert_eq!(scan("[a](<x)y>)")[0].target, "x)y");
    }

    #[test]
    fn an_angle_bracket_target_keeps_inner_whitespace_and_escapes() {
        assert_eq!(scan("[a]( < x > )")[0].target, " x ");
        assert_eq!(scan(r"[a](<\<x\>>)")[0].target, r"\<x\>");
    }

    #[test]
    fn an_angle_bracket_target_that_does_not_close_cleanly_yields_no_link() {
        for text in [
            "[a](<b)",
            "[a](<b<c>)",
            "[a](<b\nc>)",
            "[a](<b> c)",
            "[a](<b>c)",
            "[a](<>)",
            "[a](< >)",
        ] {
            assert!(scan(text).is_empty(), "{text:?}");
        }
    }

    #[test]
    fn a_bare_target_may_hold_angle_brackets_after_its_start() {
        assert_eq!(scan("[a](x<y>)")[0].target, "x<y>");
    }

    #[test]
    fn rewrite_brackets_a_target_that_cannot_be_written_bare() {
        for (new_target, written) in [
            ("my file.md", "[a](<my file.md>)"),
            ("x(y", "[a](<x(y>)"),
            ("x)y", "[a](<x)y>)"),
            ("<x>", r"[a](<\<x\>>)"),
            ("Foo_(bar)", "[a](Foo_(bar))"),
            (r"x\(y", r"[a](x\(y)"),
            (r"x y\<z", r"[a](<x y\<z>)"),
        ] {
            let out = rewrite_inline_links("[a](b)", |_, _| Some(new_target.to_string()));
            assert_eq!(out, written);
            assert_eq!(unescape(scan(&out)[0].target), unescape(new_target));
        }
    }

    #[test]
    fn rewrite_sees_an_angle_bracket_target_unbracketed() {
        let mut seen = Vec::new();
        let out = rewrite_inline_links("[a](<b c>) [d](<e f>)", |_, target| {
            seen.push(target.to_string());
            (target == "b c").then(|| "page:x".to_string())
        });
        assert_eq!(seen, ["b c", "e f"]);
        assert_eq!(out, "[a](page:x) [d](<e f>)");
    }

    #[test]
    fn whole_line_link_is_one_markdown_link_spanning_the_trimmed_line() {
        assert_eq!(whole_line_link("  [a](b)  ").map(|l| l.target), Some("b"));
        assert_eq!(whole_line_link("[a]()").map(|l| l.target), Some(""));
        for line in [
            "[a](b) c",
            "c [a](b)",
            "[a](b)(c)",
            "[[a]]",
            "[a [b](c) d](e)",
        ] {
            assert!(whole_line_link(line).is_none(), "{line:?}");
        }
    }

    #[test]
    fn rewrite_is_identity_on_plain_text() {
        let out = rewrite_inline_links("no links here at all", |_, _| Some("x".to_string()));
        assert_eq!(out, "no links here at all");
    }
}
