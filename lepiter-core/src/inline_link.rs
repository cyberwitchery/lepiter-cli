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
    /// what the link names: for [`LinkKind::Wiki`] the text between the brackets
    /// as written, untrimmed; `target` otherwise.
    pub name: &'a str,
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
///
/// No link starts inside an inline code span or a code block (see
/// [`code_span_at`] and [`code_block_at`]); a backtick preceded by an odd number
/// of backslashes opens neither.
pub fn scan_inline_links(text: &str) -> InlineLinks<'_> {
    InlineLinks {
        text,
        i: 0,
        keep_blank: false,
        delimiters: None,
    }
}

/// The one `[label](target)` link that is all of the trimmed `line`, blank target allowed.
pub(crate) fn whole_line_link(line: &str) -> Option<InlineLink<'_>> {
    let line = line.trim();
    let link = InlineLinks {
        text: line,
        i: 0,
        keep_blank: true,
        delimiters: None,
    }
    .next()?;
    (link.kind == LinkKind::Markdown && link.range == (0..line.len())).then_some(link)
}

/// Iterator returned by [`scan_inline_links`].
pub struct InlineLinks<'a> {
    text: &'a str,
    i: usize,
    keep_blank: bool,
    delimiters: Option<Delimiters<'a>>,
}

impl<'a> Iterator for InlineLinks<'a> {
    type Item = InlineLink<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let text = self.text;
        if self.i >= text.len() {
            return None;
        }
        if self.delimiters.is_none() && !text.contains('[') {
            self.i = text.len();
            return None;
        }
        let delimiters = self.delimiters.get_or_insert_with(|| Delimiters::new(text));
        while self.i < text.len() {
            let i = self.i;
            if let Some(next) = past_backtick_run(text, i) {
                self.i = next;
                continue;
            }
            if let Some(link) = delimiters.link_at(i, text.len(), self.keep_blank, true) {
                self.i = link.range.end;
                return Some(link);
            }
            self.i += 1;
        }
        None
    }
}

/// Where each delimiter of one text closes, so a link is found without re-scanning.
struct Delimiters<'a> {
    text: &'a str,
    escaped: Vec<bool>,
    /// the closing `]` or `)` of each unescaped `[` or `(`, or the text's length
    partner: Vec<usize>,
    /// the first unescaped `]]` at or after each byte, or the text's length
    wiki_close: Vec<usize>,
    /// the first line break, unescaped `<` or unescaped `>` at or after each byte, or the text's length
    angle_stop: Vec<usize>,
    /// the first non-whitespace char at or after each char boundary, or the text's length
    non_space: Vec<usize>,
}

impl<'a> Delimiters<'a> {
    fn new(text: &'a str) -> Self {
        let bytes = text.as_bytes();
        let n = bytes.len();
        let mut escaped = vec![false; n];
        let mut partner = vec![n; n];
        let (mut brackets, mut parens) = (Vec::new(), Vec::new());
        let mut backslashes = 0usize;
        for (i, &byte) in bytes.iter().enumerate() {
            escaped[i] = backslashes % 2 == 1;
            backslashes = if byte == b'\\' { backslashes + 1 } else { 0 };
            match byte {
                _ if escaped[i] => {}
                b'[' => brackets.push(i),
                b'(' => parens.push(i),
                b']' => {
                    if let Some(open) = brackets.pop() {
                        partner[open] = i;
                    }
                }
                b')' => {
                    if let Some(open) = parens.pop() {
                        partner[open] = i;
                    }
                }
                _ => {}
            }
        }
        let mut wiki_close = vec![n; n + 1];
        let mut angle_stop = vec![n; n + 1];
        for i in (0..n).rev() {
            let live = !escaped[i];
            wiki_close[i] = if live && bytes[i] == b']' && bytes.get(i + 1) == Some(&b']') {
                i
            } else {
                wiki_close[i + 1]
            };
            angle_stop[i] = match bytes[i] {
                b'\n' | b'\r' => i,
                b'<' | b'>' if live => i,
                _ => angle_stop[i + 1],
            };
        }
        let mut non_space = vec![n; n + 1];
        for (i, c) in text.char_indices().rev() {
            non_space[i] = if c.is_whitespace() {
                non_space[i + c.len_utf8()]
            } else {
                i
            };
        }
        Self {
            text,
            escaped,
            partner,
            wiki_close,
            angle_stop,
            non_space,
        }
    }

    /// The link starting at byte `i` that ends by byte `hi`, `[[wiki]]` before `[label](target)`.
    fn link_at(
        &self,
        i: usize,
        hi: usize,
        keep_blank: bool,
        guard_nesting: bool,
    ) -> Option<InlineLink<'a>> {
        let (text, bytes) = (self.text, self.text.as_bytes());
        if bytes[i] != b'[' || self.escaped[i] {
            return None;
        }
        if i + 1 < hi && bytes[i + 1] == b'[' {
            let end = self.wiki_close[i + 2];
            if end + 1 < hi && self.non_space[i + 2] < end {
                let name = &text[i + 2..end];
                let target = name.trim();
                return Some(InlineLink {
                    kind: LinkKind::Wiki,
                    label: target,
                    target,
                    name,
                    range: i..end + 2,
                });
            }
        }
        let label_end = self.partner[i];
        if label_end + 1 >= hi || bytes[label_end + 1] != b'(' {
            return None;
        }
        let (target, blank, target_end) = self.destination(label_end + 2, hi)?;
        if (blank && !keep_blank) || (guard_nesting && self.nests_a_link(i + 1..label_end)) {
            return None;
        }
        Some(InlineLink {
            kind: LinkKind::Markdown,
            label: &text[i + 1..label_end],
            target,
            name: target,
            range: i..target_end + 1,
        })
    }

    /// The target of the link whose `(` precedes `start`, whether it is blank, and its closing `)` before `hi`.
    fn destination(&self, start: usize, hi: usize) -> Option<(&'a str, bool, usize)> {
        let (text, bytes) = (self.text, self.text.as_bytes());
        let open = self.non_space[start];
        if open >= hi || bytes[open] != b'<' {
            let end = self.partner[start - 1];
            return (end < hi).then(|| (text[start..end].trim(), open >= end, end));
        }
        let close = self.angle_stop[open + 1];
        if close >= hi || bytes[close] != b'>' {
            return None;
        }
        let end = self.non_space[close + 1];
        (end < hi && bytes[end] == b')').then(|| {
            (
                &text[open + 1..close],
                self.non_space[open + 1] >= close,
                end,
            )
        })
    }

    /// Reports whether `label` holds a link other than an image, reading its outermost links only.
    fn nests_a_link(&self, label: Range<usize>) -> bool {
        let bytes = self.text.as_bytes();
        let mut i = label.start;
        while i < label.end {
            if let Some(next) = past_backtick_run(&self.text[..label.end], i) {
                i = next;
                continue;
            }
            match self.link_at(i, label.end, false, false) {
                Some(link)
                    if link.kind == LinkKind::Markdown
                        && i > label.start
                        && bytes[i - 1] == b'!'
                        && !self.escaped[i - 1] =>
                {
                    i = link.range.end;
                }
                Some(_) => return true,
                None => i += 1,
            }
        }
        false
    }
}

/// the byte range of the inline code span whose backtick run starts at byte
/// `start`, if it closes: a run of n backticks closes at the next run of exactly n
/// on the same line.
pub fn code_span_at(text: &str, start: usize) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    let run = backtick_run(bytes, start);
    let mut i = start + run;
    while run > 0 && i < bytes.len() && !matches!(bytes[i], b'\n' | b'\r') {
        let len = backtick_run(bytes, i);
        if len == run {
            return Some(start..i + len);
        }
        i += len.max(1);
    }
    None
}

/// the byte range of the code block whose ` ``` ` opens at byte `start`: through
/// the next ` ``` ` anywhere after it, or to the end of `text` when none follows.
/// a ` ``` ` followed by text and then ` `` ` on the same line opens inline code
/// instead.
pub fn code_block_at(text: &str, start: usize) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    let opens = bytes[start..].starts_with(b"```")
        && match bytes.get(start + 3) {
            None | Some(b'`' | b'\n' | b'\r') => true,
            Some(_) => !bytes[start + 4..]
                .windows(2)
                .take_while(|pair| !matches!(pair[0], b'\n' | b'\r'))
                .any(|pair| pair == b"``"),
        };
    if !opens {
        return None;
    }
    let body = start + 3;
    let end = text[body..]
        .find("```")
        .map_or(text.len(), |at| body + at + 3);
    Some(start..end)
}

/// where a walk resumes from an unescaped backtick run at byte `i`: past its
/// code block or code span, or past the run when it opens neither.
pub fn past_backtick_run(text: &str, i: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    (bytes.get(i) == Some(&b'`') && !is_escaped(bytes, i)).then(|| {
        code_block_at(text, i)
            .or_else(|| code_span_at(text, i))
            .map_or_else(|| i + backtick_run(bytes, i), |code| code.end)
    })
}

fn backtick_run(bytes: &[u8], start: usize) -> usize {
    bytes[start..].iter().take_while(|&&b| b == b'`').count()
}

/// Rewrites the inline links in `text`, leaving everything else byte-for-byte
/// intact.
///
/// For each link, `rewrite(kind, name)` may return `Some(new_target)` to
/// replace it — rendered as `[label](new_target)`, where wiki links use their
/// target as the label — or `None` to leave the original span verbatim
/// (preserving any internal whitespace). `name` is [`InlineLink::name`].
pub fn rewrite_inline_links(
    text: &str,
    mut rewrite: impl FnMut(LinkKind, &str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for link in scan_inline_links(text) {
        out.push_str(&text[cursor..link.range.start]);
        match rewrite(link.kind, link.name) {
            Some(new_target) => push_link(&mut out, link.label, &new_target),
            None => out.push_str(&text[link.range.clone()]),
        }
        cursor = link.range.end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Writes `[label](target)`, bracketing or escaping a target that would not read back bare.
pub(crate) fn push_link(out: &mut String, label: &str, target: &str) {
    out.push('[');
    out.push_str(label);
    out.push_str("](");
    if needs_angle_brackets(target) {
        out.push('<');
        push_escaped(out, target, |_, c| matches!(c, '<' | '>'));
        out.push('>');
    } else if target.contains(['\r', '\n']) {
        let balanced = parens_balance(target.as_bytes());
        push_escaped(out, target, |i, c| {
            (i == 0 && c == '<') || (!balanced && matches!(c, '(' | ')'))
        });
    } else {
        out.push_str(target);
    }
    out.push(')');
}

/// Pushes `text`, backslash-escaping each unescaped char at `i` for which `special(i, c)` holds.
fn push_escaped(out: &mut String, text: &str, special: impl Fn(usize, char) -> bool) {
    let bytes = text.as_bytes();
    for (i, c) in text.char_indices() {
        if special(i, c) && !is_escaped(bytes, i) {
            out.push('\\');
        }
        out.push(c);
    }
}

fn needs_angle_brackets(target: &str) -> bool {
    target.starts_with(char::is_whitespace)
        || target.ends_with(char::is_whitespace)
        // `<…>` can't hold a line ending
        || (!target.contains(['\r', '\n'])
            && (target.starts_with('<')
                || target.contains(|c: char| c == ' ' || c.is_ascii_control())
                || !parens_balance(target.as_bytes())))
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
pub(crate) fn is_escaped(bytes: &[u8], at: usize) -> bool {
    bytes[..at]
        .iter()
        .rev()
        .take_while(|&&b| b == b'\\')
        .count()
        % 2
        == 1
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
    fn a_construct_in_a_label_ends_inside_it() {
        for (text, targets) in [
            ("[ [ ](x)", vec!["x"]),
            ("[a[[x]y]](z)", vec!["z"]),
            ("[x `y [a](b)](c) `", vec![]),
        ] {
            let found: Vec<_> = scan(text).iter().map(|link| link.target).collect();
            assert_eq!(found, targets, "{text:?}");
        }
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
    fn rewrite_callback_sees_kind_and_name() {
        let mut seen = Vec::new();
        let _ = rewrite_inline_links("[a]( b ) [[ c ]]", |kind, name| {
            seen.push((kind, name.to_string()));
            None
        });
        assert_eq!(
            seen,
            vec![
                (LinkKind::Markdown, "b".to_string()),
                (LinkKind::Wiki, " c ".to_string()),
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
            "[a](<b\rc>)",
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
            ("x y\rz", "[a](x y\rz)"),
            ("x\ny", "[a](x\ny)"),
            ("x(\ry", "[a](x\\(\ry)"),
            ("<x\ry", "[a](\\<x\ry)"),
            ("(x)\r<y>", "[a]((x)\r<y>)"),
            ("x\\((\ry", "[a](x\\(\\(\ry)"),
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
    fn a_link_inside_inline_code_or_a_code_block_is_code() {
        for text in [
            "`[[a]]` ``[b](c)``",
            "```sh\nif [[ -f x ]]; then\n```",
            "````\n[[a]]\n````",
            "x ```\n[[a]]",
            "```x\r[[a]] ``",
        ] {
            assert!(scan(text).is_empty(), "{text:?}");
        }
        for text in [
            "`[[a]]` [[a]]",
            r"\`[[a]]` x",
            "``[[a]]`",
            "`\n[[a]]\n`",
            "`\r[[a]]\r`",
            "```\n[[b]] ``` [[a]]",
            "```x`` [[a]]\n```",
        ] {
            let targets: Vec<_> = scan(text).iter().map(|link| link.target).collect();
            assert_eq!(targets, ["a"], "{text:?}");
        }
    }

    #[test]
    fn rewrite_is_identity_on_plain_text() {
        let out = rewrite_inline_links("no links here at all", |_, _| Some("x".to_string()));
        assert_eq!(out, "no links here at all");
    }
}
