use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::DateTime;
use lepiter_core::{
    LinkKind, closes_fence, language_to_snippet_type, open_fence, parse_standalone_link,
    rewrite_inline_links, unescape_block_start,
};
use serde_json::json;

use super::{ArgSpec, parse_args};

const SPEC: ArgSpec<'static> = ArgSpec {
    usage: "usage: lepiter-cli import <input-dir> [kb-path]\n\n\
            converts exported markdown files (with yaml frontmatter) back\n\
            into lepiter page json files. reverses the `export` subcommand.\n\n\
            page content survives the round trip, with these exceptions:\n  \
            - binary attachments (images, etc.) are not copied. picture\n    \
            snippet references are preserved but the files must be\n    \
            restored separately\n  \
            - an unknown snippet keeps its type but not its other fields\n  \
            - picture, youtube and word snippets come back as the link or\n    \
            text snippet their markdown form implies\n  \
            - a carriage return in a text snippet is dropped; a crlf line\n    \
            break comes back as a plain newline",
    toggles: &[],
    valued: &[],
};

pub fn run_import(args: Vec<String>) -> Result<()> {
    let Some(args) = parse_args(args, &SPEC)? else {
        return Ok(());
    };

    let Some(input_dir) = args.positional(0) else {
        bail!("missing required argument: <input-dir>");
    };
    let input_dir = PathBuf::from(input_dir);
    let kb_path = args.kb_path(1);

    if !input_dir.is_dir() {
        bail!("input directory does not exist: {}", input_dir.display());
    }

    // collect all .md files and parse their frontmatter to build a slug→id map
    let mut md_files: Vec<(String, Frontmatter)> = Vec::new();
    let mut slug_to_id: HashMap<String, String> = HashMap::new();
    let mut claimed_ids: HashSet<String> = HashSet::new();

    let mut entries: Vec<_> = fs::read_dir(&input_dir)
        .with_context(|| format!("failed to read directory {}", input_dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        let mut warnings = Vec::new();
        let fm = parse_frontmatter(&content, &mut warnings);
        for warning in warnings {
            eprintln!("warning: {}: {warning}", path.display());
        }
        let Some(fm) = fm else {
            eprintln!(
                "warning: skipping {} (no valid frontmatter)",
                path.display()
            );
            continue;
        };

        if fm.id.is_empty() {
            eprintln!(
                "warning: skipping {} (no id in frontmatter)",
                path.display()
            );
            continue;
        }

        if !id_is_safe(&fm.id) {
            eprintln!(
                "warning: skipping {} (unsafe id would escape output directory: {})",
                path.display(),
                fm.id
            );
            continue;
        }

        // first-seen id wins; entries are sorted by filename, so the survivor is
        // deterministic rather than dependent on directory iteration order.
        if !claimed_ids.insert(fm.id.clone()) {
            eprintln!(
                "warning: skipping {} (duplicate id already used by an earlier file: {})",
                path.display(),
                fm.id
            );
            continue;
        }

        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            let slug_file = format!("{stem}.md");
            slug_to_id.insert(slug_file, fm.id.clone());
        }

        md_files.push((content, fm));
    }

    fs::create_dir_all(&kb_path)
        .with_context(|| format!("failed to create output directory {}", kb_path.display()))?;

    let mut imported = 0usize;

    for (content, fm) in &md_files {
        let body = strip_frontmatter(content);
        let snippets = parse_markdown_body(body, &slug_to_id);

        let page_json = build_page_json(fm, &snippets);
        let out_path = kb_path.join(format!("{}.lepiter", fm.id));

        let bytes = serde_json::to_vec_pretty(&page_json)
            .with_context(|| format!("failed to serialize page {}", fm.id))?;
        fs::write(&out_path, bytes)
            .with_context(|| format!("failed to write {}", out_path.display()))?;

        imported += 1;

        if imported.is_multiple_of(50) {
            eprint!(".");
        }
    }

    if imported >= 50 {
        eprintln!();
    }

    eprintln!("imported {imported} pages to {}", kb_path.display());

    Ok(())
}

#[derive(Debug, Clone, Default)]
struct Frontmatter {
    title: String,
    id: String,
    tags: Vec<String>,
    updated_at: Option<String>,
}

/// a value outside the yaml subset `Yaml` reads is skipped and reported in `warnings`.
fn parse_frontmatter(content: &str, warnings: &mut Vec<String>) -> Option<Frontmatter> {
    let (block, _) = split_frontmatter(content)?;
    let block = block.replace("\r\n", "\n").replace('\r', "\n");
    let mut yaml = Yaml { s: &block, pos: 0 };
    let mut fields: BTreeMap<&str, Result<Value, &str>> = BTreeMap::new();
    loop {
        yaml.skip_empty_lines();
        if yaml.pos == yaml.s.len() {
            break;
        }
        let Some(key) = yaml.key() else {
            warnings.push(format!("ignoring frontmatter line {:?}", yaml.line()));
            yaml.skip_node();
            continue;
        };
        let value = yaml.value();
        if value.is_err() {
            yaml.skip_node();
        }
        if matches!(key, "title" | "id" | "tags" | "updated_at") {
            let value = if fields.contains_key(key) {
                Err("repeated key")
            } else {
                value
            };
            fields.insert(key, value);
        }
    }

    let mut fm = Frontmatter::default();
    for (key, value) in fields {
        match (key, value) {
            (_, Ok(Value::Null)) => {}
            ("title", Ok(Value::Scalar(s))) => fm.title = s,
            ("id", Ok(Value::Scalar(s))) => fm.id = s,
            ("updated_at", Ok(Value::Scalar(s))) => fm.updated_at = Some(s),
            ("tags", Ok(Value::Seq(tags))) => fm.tags = tags,
            (_, Ok(_)) => warnings.push(format!("ignoring frontmatter key {key:?}: wrong type")),
            (_, Err(why)) => warnings.push(format!("ignoring frontmatter key {key:?}: {why}")),
        }
    }
    Some(fm)
}

/// splits `content` into its frontmatter and the body after the closing `---` line.
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;
    let mut start = 0;
    loop {
        let end = rest[start..].find('\n').map(|i| start + i);
        let line = &rest[start..end.unwrap_or(rest.len())];
        if line.strip_suffix('\r').unwrap_or(line) == "---" {
            return Some((&rest[..start], end.map_or("", |e| &rest[e + 1..])));
        }
        start = end? + 1;
    }
}

enum Value {
    Null,
    Scalar(String),
    Seq(Vec<String>),
}

/// the yaml subset of frontmatter: plain and quoted scalars, and flow or block sequences of them.
struct Yaml<'a> {
    s: &'a str,
    pos: usize,
}

impl<'a> Yaml<'a> {
    fn rest(&self) -> &'a str {
        &self.s[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn line(&self) -> &'a str {
        self.rest().split('\n').next().unwrap_or("")
    }

    fn indent(&self) -> usize {
        let line = self.line();
        line.len() - line.trim_start_matches(' ').len()
    }

    fn blank_after(&self) -> bool {
        matches!(self.rest().chars().nth(1), None | Some(' ' | '\t' | '\n'))
    }

    /// at a line break, the end of input, or a comment
    fn at_line_end(&self) -> bool {
        match self.peek() {
            None | Some('\n') => true,
            Some('#') => {
                self.pos == 0 || matches!(self.s.as_bytes()[self.pos - 1], b' ' | b'\t' | b'\n')
            }
            _ => false,
        }
    }

    fn skip_blanks(&mut self) {
        let rest = self.rest();
        self.pos += rest.len() - rest.trim_start_matches([' ', '\t']).len();
    }

    fn skip_line(&mut self) {
        self.pos = (self.pos + self.line().len() + 1).min(self.s.len());
    }

    fn skip_empty_lines(&mut self) {
        while self.pos < self.s.len() {
            let line = self.line().trim_start_matches([' ', '\t']);
            if !line.is_empty() && !line.starts_with('#') {
                break;
            }
            self.skip_line();
        }
    }

    /// skips a node that failed to read: the rest of its line and the indented lines after it.
    fn skip_node(&mut self) {
        self.skip_line();
        while self.pos < self.s.len()
            && (self.line().is_empty()
                || self.line().starts_with([' ', '\t', '#'])
                || self.is_item(0))
        {
            self.skip_line();
        }
    }

    fn is_item(&self, indent: usize) -> bool {
        self.indent() == indent
            && self.line()[indent..].starts_with('-')
            && matches!(
                self.line().as_bytes().get(indent + 1),
                None | Some(b' ' | b'\t')
            )
    }

    /// a column-0 `key:` line, leaving the cursor after the colon.
    fn key(&mut self) -> Option<&'a str> {
        let line = self.line();
        if line.starts_with([
            ' ', '\t', '-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '\'',
            '"', '%', '@', '`',
        ]) {
            return None;
        }
        let colon = line
            .match_indices(':')
            .map(|(i, _)| i)
            .find(|&i| matches!(line.as_bytes().get(i + 1), None | Some(b' ' | b'\t')))?;
        let key = line[..colon].trim_end_matches([' ', '\t']);
        if key.contains(" #") || key.contains("\t#") {
            return None;
        }
        self.pos += colon + 1;
        Some(key)
    }

    fn value(&mut self) -> Result<Value, &'static str> {
        self.skip_blanks();
        if self.at_line_end() {
            self.skip_line();
            self.skip_empty_lines();
            let indent = self.indent();
            if self.is_item(indent) {
                return self.block_seq(indent);
            }
            if indent == 0 {
                return Ok(Value::Null);
            }
            self.skip_blanks();
        }
        let value = if self.peek() == Some('[') {
            Value::Seq(self.flow_seq()?)
        } else {
            self.scalar(Some(1))?
        };
        self.end_node(0)?;
        Ok(value)
    }

    /// only a comment may follow a node, and no later line may be indented past `indent`.
    fn end_node(&mut self, indent: usize) -> Result<(), &'static str> {
        self.skip_blanks();
        if !self.at_line_end() {
            return Err("unexpected text after the value");
        }
        self.skip_line();
        self.skip_empty_lines();
        if self.pos < self.s.len() && self.indent() > indent {
            return Err("unexpected indented text after the value");
        }
        Ok(())
    }

    fn block_seq(&mut self, indent: usize) -> Result<Value, &'static str> {
        let mut items = Vec::new();
        while self.pos < self.s.len() && self.is_item(indent) {
            self.pos += indent + 1;
            self.skip_blanks();
            if self.at_line_end() {
                return Err("empty sequence items are not supported");
            }
            if let Value::Scalar(s) = self.scalar(Some(indent + 1))? {
                items.push(s);
            }
            self.end_node(indent)?;
        }
        Ok(Value::Seq(items))
    }

    fn flow_seq(&mut self) -> Result<Vec<String>, &'static str> {
        let start = self.pos;
        let items = self.flow_items();
        if items.is_err() {
            self.pos = start;
            self.skip_flow();
        }
        items
    }

    fn flow_items(&mut self) -> Result<Vec<String>, &'static str> {
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            self.skip_flow_space();
            match self.peek() {
                None => return Err("unterminated flow sequence"),
                Some(']') => {
                    self.pos += 1;
                    return Ok(items);
                }
                _ => {}
            }
            if let Value::Scalar(s) = self.scalar(None)? {
                items.push(s);
            }
            self.skip_flow_space();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some(']') => {}
                _ => return Err("expected `,` or `]` in a flow sequence"),
            }
        }
    }

    fn skip_flow_space(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\n') => self.pos += 1,
                Some('#') if self.at_line_end() => self.pos += self.line().len(),
                _ => return,
            }
        }
    }

    /// skips a flow collection, with the collections and quoted scalars in it.
    fn skip_flow(&mut self) {
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            let node_start = self.pos == 0
                || matches!(
                    self.s.as_bytes()[self.pos - 1],
                    b' ' | b'\t' | b'\n' | b'[' | b'{' | b',' | b':'
                );
            match c {
                '"' | '\'' if node_start => {
                    let _ = self.quoted(c);
                    continue;
                }
                '#' if self.at_line_end() => {
                    self.pos += self.line().len();
                    continue;
                }
                '[' | '{' => depth += 1,
                ']' | '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        self.pos += 1;
                        return;
                    }
                }
                _ => {}
            }
            self.pos += c.len_utf8();
        }
    }

    /// `indent` is the least indentation of a plain scalar's continuation lines; `None` in a flow.
    fn scalar(&mut self, indent: Option<usize>) -> Result<Value, &'static str> {
        match self.peek() {
            Some(q @ ('"' | '\'')) => self.quoted(q).map(Value::Scalar),
            Some('[' | '{') => {
                self.skip_flow();
                Err("nested collections are not supported")
            }
            Some('|' | '>') => Err("block scalars are not supported"),
            Some('&' | '*') => Err("anchors and aliases are not supported"),
            Some('!') => Err("tags are not supported"),
            Some('-' | '?' | ':') if self.blank_after() => Err("unexpected indicator"),
            Some(',' | ']' | '}' | '#' | '%' | '@' | '`') => Err("unexpected indicator"),
            _ => self.plain(indent),
        }
    }

    fn quoted(&mut self, quote: char) -> Result<String, &'static str> {
        self.pos += 1;
        let mut out = String::new();
        let mut error = None;
        loop {
            let Some(c) = self.peek() else {
                return Err("unterminated quoted scalar");
            };
            self.pos += c.len_utf8();
            match c {
                '\'' if quote == '\'' && self.peek() == Some('\'') => {
                    self.pos += 1;
                    out.push('\'');
                }
                _ if c == quote => break,
                '\\' if quote == '"' && self.peek() == Some('\n') => {
                    self.pos += 1;
                    self.fold(&mut out, true)?;
                }
                '\\' if quote == '"' => match self.escape() {
                    Some(c) => out.push(c),
                    None => error = Some("invalid escape"),
                },
                ' ' | '\t' | '\n' => {
                    let start = self.pos - 1;
                    self.pos = start;
                    self.skip_blanks();
                    if self.peek() == Some('\n') {
                        self.pos += 1;
                        self.fold(&mut out, false)?;
                    } else {
                        out.push_str(&self.s[start..self.pos]);
                    }
                }
                _ => out.push(c),
            }
        }
        error.map_or(Ok(out), Err)
    }

    fn escape(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        let len = match c {
            'x' => 2,
            'u' => 4,
            'U' => 8,
            _ => {
                return Some(match c {
                    '0' => '\0',
                    'a' => '\u{7}',
                    'b' => '\u{8}',
                    't' => '\t',
                    'n' => '\n',
                    'v' => '\u{b}',
                    'f' => '\u{c}',
                    'r' => '\r',
                    'e' => '\u{1b}',
                    'N' => '\u{85}',
                    '_' => '\u{a0}',
                    'L' => '\u{2028}',
                    'P' => '\u{2029}',
                    ' ' | '\t' | '"' | '/' | '\\' => c,
                    _ => return None,
                });
            }
        };
        let hex = self.rest().get(..len)?;
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        self.pos += len;
        char::from_u32(u32::from_str_radix(hex, 16).ok()?)
    }

    /// folds the line break just read with the empty lines after it.
    fn fold(&mut self, out: &mut String, escaped: bool) -> Result<(), &'static str> {
        let mut breaks = 0;
        loop {
            let line = self.line();
            if (line.starts_with("---") || line.starts_with("..."))
                && matches!(line.as_bytes().get(3), None | Some(b' ' | b'\t'))
            {
                return Err("document marker inside a quoted scalar");
            }
            self.skip_blanks();
            if self.peek() != Some('\n') {
                break;
            }
            self.pos += 1;
            breaks += 1;
        }
        match breaks {
            0 if !escaped => out.push(' '),
            n => out.extend(std::iter::repeat_n('\n', n)),
        }
        Ok(())
    }

    fn plain(&mut self, indent: Option<usize>) -> Result<Value, &'static str> {
        let flow = indent.is_none();
        let mut out = String::new();
        let mut pending = String::new();
        loop {
            if self.peek() == Some('#') && self.at_line_end() {
                break;
            }
            let start = self.pos;
            while let Some(c) = self.peek() {
                let next = self.rest()[c.len_utf8()..].chars().next();
                let stop = match c {
                    ' ' | '\t' | '\n' => true,
                    ':' => {
                        matches!(next, None | Some(' ' | '\t' | '\n'))
                            || flow && matches!(next, Some(',' | '[' | ']' | '{' | '}'))
                    }
                    ',' | '[' | ']' | '{' | '}' => flow,
                    _ => false,
                };
                if stop {
                    break;
                }
                self.pos += c.len_utf8();
            }
            if self.pos == start {
                break;
            }
            out.push_str(&pending);
            out.push_str(&self.s[start..self.pos]);
            let end = self.pos;
            self.skip_blanks();
            if self.peek() != Some('\n') {
                pending = self.s[end..self.pos].to_string();
                continue;
            }
            self.pos += 1;
            let mut breaks = 0;
            while self.pos < self.s.len() && self.line().trim_start_matches([' ', '\t']).is_empty()
            {
                self.skip_line();
                breaks += 1;
            }
            let line = self.line();
            let marker = (line.starts_with("---") || line.starts_with("..."))
                && matches!(line.as_bytes().get(3), None | Some(b' ' | b'\t'));
            if self.pos == self.s.len() || marker || indent.is_some_and(|n| self.indent() < n) {
                self.pos = end;
                break;
            }
            self.skip_blanks();
            if self.peek() == Some('#') {
                self.pos = end;
                break;
            }
            pending = if breaks == 0 {
                " ".to_string()
            } else {
                "\n".repeat(breaks)
            };
        }
        if out.is_empty() {
            return Err("unexpected indicator");
        }
        Ok(match out.as_str() {
            "~" | "null" | "Null" | "NULL" => Value::Null,
            _ => Value::Scalar(out),
        })
    }
}

fn strip_frontmatter(content: &str) -> &str {
    split_frontmatter(content).map_or(content, |(_, body)| body)
}

/// Rejects frontmatter ids that would escape the output directory when joined as
/// a filename. Real Lepiter ids are UUIDs, so this never rejects legitimate input.
fn id_is_safe(id: &str) -> bool {
    !id.contains('/')
        && !id.contains('\\')
        && !id.contains("..")
        && !PathBuf::from(id).is_absolute()
}

#[derive(Debug, Clone)]
enum Snippet {
    Text(String),
    Code {
        language: Option<String>,
        code: String,
    },
    List(Vec<Vec<Snippet>>),
    Link {
        text: String,
        url: String,
    },
    Rewrite {
        language: Option<String>,
        search: String,
        replace: String,
        scope: Option<String>,
        is_method_pattern: Option<bool>,
    },
    Unknown {
        typ: String,
    },
}

/// deeper list nesting is read as text, keeping the page under `serde_json`'s nesting limit.
const MAX_LIST_DEPTH: usize = 16;

fn parse_markdown_body(body: &str, slug_to_id: &HashMap<String, String>) -> Vec<Snippet> {
    parse_blocks(body, slug_to_id, 0)
}

fn parse_blocks(body: &str, slug_to_id: &HashMap<String, String>, depth: usize) -> Vec<Snippet> {
    let mut snippets = Vec::new();
    let mut lines = body.lines().peekable();
    // no fence this long or longer closes anywhere in the rest of `body`
    let mut unclosable_fence = usize::MAX;

    while let Some(line) = lines.next() {
        // blank lines between blocks
        if line.trim().is_empty() {
            continue;
        }

        // code block
        if let Some((fence_len, info)) = open_fence(line) {
            let (snipps, consumed) = if fence_len < unclosable_fence {
                parse_code_block(fence_len, info, &mut lines)
            } else {
                (Vec::new(), false)
            };
            snippets.extend(snipps);
            if !consumed {
                unclosable_fence = unclosable_fence.min(fence_len);
                // unterminated code block — treat opening as text
                snippets.push(Snippet::Text(rewrite_links_to_internal(line, slug_to_id)));
            }
            continue;
        }

        // blockquote
        if is_quote_line(line) {
            let mut quoted = vec![quote_body(line)];
            while let Some(&next) = lines.peek() {
                if !is_quote_line(next) {
                    break;
                }
                quoted.push(quote_body(next));
                lines.next();
            }
            let text = format!("> {}", quoted.join("\n"));
            snippets.push(Snippet::Text(rewrite_links_to_internal(&text, slug_to_id)));
            continue;
        }

        // list
        if let Some(first_item) = line.strip_prefix("- ") {
            let mut items = vec![first_item.to_string()];
            while let Some(&next) = lines.peek() {
                if let Some(item) = next.strip_prefix("- ") {
                    items.push(item.to_string());
                    lines.next();
                } else if let Some(continuation) = next.strip_prefix("  ") {
                    // continuation of previous item
                    if let Some(last) = items.last_mut() {
                        last.push('\n');
                        last.push_str(continuation);
                    }
                    lines.next();
                } else {
                    break;
                }
            }
            let items = items
                .iter()
                .map(|item| {
                    if depth < MAX_LIST_DEPTH {
                        parse_blocks(item, slug_to_id, depth + 1)
                    } else {
                        let text = rewrite_links_to_internal(&unescape_lines(item), slug_to_id);
                        vec![Snippet::Text(text)]
                    }
                })
                .collect();
            snippets.push(Snippet::List(items));
            continue;
        }

        // paragraph
        let mut run = vec![line];
        while let Some(&next) = lines.peek() {
            if next.trim().is_empty()
                || open_fence(next).is_some()
                || is_quote_line(next)
                || next.starts_with("- ")
            {
                break;
            }
            run.push(next);
            lines.next();
        }

        // a lone line may be a whole-snippet form instead
        if let [only] = run[..] {
            // unknown snippet type marker from export: [[unknown: TYPE]]
            if let Some(typ) = only
                .trim()
                .strip_prefix("[[unknown: ")
                .and_then(|rest| rest.strip_suffix("]]"))
            {
                snippets.push(Snippet::Unknown {
                    typ: typ.to_string(),
                });
                continue;
            }

            // standalone link line: [text](url) with nothing else on the line
            if let Some((text, url)) = parse_standalone_link(only) {
                let url = rewrite_link_target_to_internal(&url, slug_to_id);
                snippets.push(Snippet::Link { text, url });
                continue;
            }
        }

        let text = unescape_lines(&run.join("\n"));
        snippets.push(Snippet::Text(rewrite_links_to_internal(&text, slug_to_id)));
    }

    snippets
}

fn unescape_lines(text: &str) -> String {
    text.lines()
        .map(unescape_block_start)
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_quote_line(line: &str) -> bool {
    line.starts_with("> ") || line == ">"
}

fn quote_body(line: &str) -> &str {
    line.strip_prefix("> ").unwrap_or("")
}

fn parse_code_block(
    fence_len: usize,
    info: &str,
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
) -> (Vec<Snippet>, bool) {
    let mut body = Vec::new();
    let mut found_end = false;
    let mut rest = lines.clone();

    for line in rest.by_ref() {
        if closes_fence(line, fence_len) {
            found_end = true;
            break;
        }
        body.push(line);
    }

    if !found_end {
        return (Vec::new(), false);
    }
    *lines = rest;

    // diff rewrite block: ```diff <lang>
    if let Some(rewrite_lang) = info.strip_prefix("diff ")
        && let Some(snippet) = try_parse_rewrite_block(rewrite_lang.trim(), &body)
    {
        return (vec![snippet], true);
    }

    let language = if info.is_empty() {
        None
    } else {
        Some(info.to_string())
    };

    (
        vec![Snippet::Code {
            language,
            code: body.join("\n"),
        }],
        true,
    )
}

fn try_parse_rewrite_block(lang: &str, all_lines: &[&str]) -> Option<Snippet> {
    let mut scope = None;
    let mut is_method_pattern = None;
    let mut search_lines = Vec::new();
    let mut replace_lines = Vec::new();

    for line in all_lines {
        if let Some(rest) = line.strip_prefix("# scope: ") {
            scope = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("# method_pattern: ") {
            is_method_pattern = rest.parse::<bool>().ok();
        } else if let Some(rest) = line.strip_prefix('-') {
            search_lines.push(rest.to_string());
        } else if let Some(rest) = line.strip_prefix('+') {
            replace_lines.push(rest.to_string());
        }
    }

    // if no search/replace lines found, this isn't a rewrite block
    if search_lines.is_empty() && replace_lines.is_empty() {
        return None;
    }

    let language = if lang.is_empty() {
        None
    } else {
        Some(lang.to_string())
    };

    Some(Snippet::Rewrite {
        language,
        search: search_lines.join("\n"),
        replace: replace_lines.join("\n"),
        scope,
        is_method_pattern,
    })
}

fn rewrite_link_target_to_internal(target: &str, slug_to_id: &HashMap<String, String>) -> String {
    if target.ends_with(".md")
        && let Some(id) = slug_to_id.get(target)
    {
        return format!("page:{id}");
    }
    target.to_string()
}

/// Rewrites `[label](slug.md)` markdown links whose target is an exported page
/// back to `[label](page:id)`. `[[wikilinks]]` and any other links are left
/// untouched, mirroring the export direction's asymmetry.
fn rewrite_links_to_internal(text: &str, slug_to_id: &HashMap<String, String>) -> String {
    rewrite_inline_links(text, |kind, target| match kind {
        LinkKind::Markdown if target.ends_with(".md") => {
            slug_to_id.get(target).map(|id| format!("page:{id}"))
        }
        _ => None,
    })
}

fn snippet_type_for_language(lang: &str) -> &str {
    // elementSnippet renders as an `element` fence through its own parse path
    // (it is not a code snippet), so map it back explicitly to keep it
    // round-tripping. every other code language comes from the shared table.
    if lang == "element" {
        return "elementSnippet";
    }
    language_to_snippet_type(lang).unwrap_or("textSnippet")
}

fn build_code_snippet_json(language: &Option<String>, code: &str) -> serde_json::Value {
    let typ = language
        .as_deref()
        .map(snippet_type_for_language)
        .unwrap_or("textSnippet");

    if typ == "textSnippet" {
        // for unrecognized languages, store as text with code fence markers
        let lang_str = language.as_deref().unwrap_or("");
        let text = format!("```{lang_str}\n{code}\n```");
        json!({ "__type": "textSnippet", "string": text })
    } else {
        json!({ "__type": typ, "code": code })
    }
}

fn snippet_json(snippet: &Snippet) -> serde_json::Value {
    match snippet {
        Snippet::Text(text) => json!({ "__type": "textSnippet", "string": text }),
        Snippet::Code { language, code } => build_code_snippet_json(language, code),
        Snippet::List(list_items) => {
            let children: Vec<serde_json::Value> =
                list_items.iter().map(|item| list_item_json(item)).collect();
            json!({
                "__type": "listSnippet",
                "children": { "items": children }
            })
        }
        Snippet::Link { text, url } => json!({
            "__type": "linkSnippet",
            "string": text,
            "url": url
        }),
        Snippet::Rewrite {
            language,
            search,
            replace,
            scope,
            is_method_pattern,
        } => {
            let mut obj = json!({
                "__type": "pharoRewrite",
                "search": search,
                "replace": replace,
            });
            if let Some(scope) = scope {
                obj["scope"] = json!(scope);
            }
            if let Some(is_method) = is_method_pattern {
                obj["isMethodPattern"] = json!(is_method);
            }
            // language is implicit in the __type
            let _ = language;
            obj
        }
        Snippet::Unknown { typ } => json!({ "__type": typ }),
    }
}

/// the list child for one item: its first block, holding the item's other blocks as children.
fn list_item_json(blocks: &[Snippet]) -> serde_json::Value {
    let holder = Snippet::Text(String::new());
    let (first, rest) = match blocks {
        // a list's children are its items, so blocks after a leading list need a holder
        [] | [Snippet::List(_), _, ..] => (&holder, blocks),
        [first, rest @ ..] => (first, rest),
    };
    let mut item = snippet_json(first);
    if !rest.is_empty() {
        let children: Vec<serde_json::Value> = rest.iter().map(snippet_json).collect();
        item["children"] = json!({ "items": children });
    }
    item
}

fn build_page_json(fm: &Frontmatter, snippets: &[Snippet]) -> serde_json::Value {
    let mut items: Vec<serde_json::Value> = snippets.iter().map(snippet_json).collect();

    if items.is_empty() {
        items.push(json!({ "__type": "textSnippet", "string": "" }));
    }

    let mut page = json!({
        "uid": { "uuid": &fm.id },
        "pageType": { "title": &fm.title },
        "children": { "items": items }
    });

    if !fm.tags.is_empty() {
        page["tags"] = json!(fm.tags);
    }

    if let Some(updated) = &fm.updated_at {
        match DateTime::parse_from_rfc3339(updated) {
            Ok(_) => {
                page["editTime"] = json!({
                    "time": { "dateAndTimeString": updated }
                });
            }
            Err(_) => {
                eprintln!(
                    "warning: ignoring unparseable updated_at {updated:?} for page {} (expected RFC 3339)",
                    fm.id
                );
            }
        }
    }

    page
}

#[cfg(test)]
mod tests {
    use super::super::export::build_frontmatter;
    use super::*;
    use lepiter_core::{BlockEscaping, Node, Page, is_standalone_link, render_nodes_to_text_with};

    const NASTY_LINKS: &[(&str, &str)] = &[
        ("plain", "https://example.com/path?q=1#frag"),
        ("Foo", "https://en.wikipedia.org/wiki/Foo_(bar)"),
        ("nested", "a(b(c)d)e"),
        ("spaces", "my notes/first draft.md"),
        ("open", "https://example.com/a(b"),
        ("close", "https://example.com/a)b"),
        ("both", "a b)c(d"),
        ("angles", "https://example.com/?q=<a>&r=b>c"),
        ("leading angle", "<https://example.com>"),
        ("windows", r"C:\Users\foo"),
        ("escapes", r"a\*b\(c\"),
        ("tab", "a\tb"),
        ("carriage return", "a\rb"),
        ("carriage return and space", "a b\rc"),
        ("carriage return and paren", "a(\rb"),
        ("carriage return and leading angle", "<a\rb"),
        ("blank", ""),
        ("padded", " x "),
        ("nbsp before", "\u{a0}x"),
        ("nbsp after", "x\u{a0}"),
        ("[draft] notes", "u"),
        ("a ] b [ c", "u"),
        ("x](y", "u"),
        ("[[wiki]]", "u"),
        (r"back\slash\", "u"),
        (r"\[not\]", "u"),
        ("日本語 ✓", "https://例え.jp/パス (1)"),
    ];

    fn export(node: Node) -> String {
        render_nodes_to_text_with(&[node], &mut |_, _| None, BlockEscaping::Escape)
    }

    #[test]
    fn a_link_snippet_survives_export_and_import() {
        for &(text, url) in NASTY_LINKS {
            let markdown = export(Node::Link {
                text: text.to_string(),
                url: url.to_string(),
            });
            let snippets = parse_markdown_body(&markdown, &HashMap::new());
            assert!(
                matches!(&snippets[..], [Snippet::Link { text: t, url: u }] if t == text && u == url),
                "{text:?} {url:?} exported as {markdown:?} imported as {snippets:?}"
            );
        }
    }

    #[test]
    fn a_text_snippet_spelling_a_link_survives_export_and_import() {
        for &(text, url) in NASTY_LINKS {
            let line = export(Node::Link {
                text: text.to_string(),
                url: url.to_string(),
            });
            let line = line.trim_end();
            let markdown = export(Node::Text {
                text: line.to_string(),
            });
            let snippets = parse_markdown_body(&markdown, &HashMap::new());
            assert!(
                matches!(&snippets[..], [Snippet::Text(t)] if t == line),
                "{line:?} exported as {markdown:?} imported as {snippets:?}"
            );
        }
    }

    #[test]
    fn a_url_with_a_line_ending_and_whitespace_at_an_edge_comes_back_as_text() {
        for (url, line) in [
            (" a\rb", "[x](< a\rb>)"),
            ("a\rb ", "[x](<a\rb >)"),
            ("\rab", "[x](<\rab>)"),
            ("ab\r", "[x](<ab\r>)"),
            ("a\rb\u{a0}", "[x](<a\rb\u{a0}>)"),
        ] {
            let markdown = export(Node::Link {
                text: "x".to_string(),
                url: url.to_string(),
            });
            let snippets = parse_markdown_body(&markdown, &HashMap::new());
            assert!(
                matches!(&snippets[..], [Snippet::Text(t)] if t == line),
                "{url:?} exported as {markdown:?} imported as {snippets:?}"
            );
        }
    }

    #[test]
    fn parse_frontmatter_basic() {
        let content = "---\ntitle: \"Test Page\"\nid: abc-123\ntags: [\"rust\", \"cli\"]\nupdated_at: 2024-01-01T00:00:00+00:00\n---\n\nBody text.\n";
        let fm = parse_frontmatter(content, &mut Vec::new()).unwrap();
        assert_eq!(fm.title, "Test Page");
        assert_eq!(fm.id, "abc-123");
        assert_eq!(fm.tags, vec!["rust", "cli"]);
        assert_eq!(fm.updated_at.as_deref(), Some("2024-01-01T00:00:00+00:00"));
    }

    #[test]
    fn parse_frontmatter_no_tags() {
        let content = "---\ntitle: \"Simple\"\nid: p2\n---\n\n";
        let fm = parse_frontmatter(content, &mut Vec::new()).unwrap();
        assert!(fm.tags.is_empty());
        assert!(fm.updated_at.is_none());
    }

    #[test]
    fn parse_frontmatter_missing_returns_none() {
        assert!(parse_frontmatter("no frontmatter here", &mut Vec::new()).is_none());
        assert!(parse_frontmatter("---\nunclosed", &mut Vec::new()).is_none());
    }

    #[test]
    fn parse_frontmatter_bom() {
        let content = "\u{feff}---\ntitle: \"BOM\"\nid: bom-1\n---\n\n";
        let fm = parse_frontmatter(content, &mut Vec::new()).unwrap();
        assert_eq!(fm.title, "BOM");
        assert_eq!(fm.id, "bom-1");
    }

    #[test]
    fn parse_frontmatter_reads_yaml_scalars_and_sequences() {
        let cases: &[(&str, &str, &str, &[&str])] = &[
            ("title: 'it''s'\nid: p1", "it's", "p1", &[]),
            (
                "title: \"caf\\u00e9\\tx\\x41\\U0001F600\"\nid: p1",
                "café\txA😀",
                "p1",
                &[],
            ),
            (
                "title: \"Folded\n  continued\"\nid: p1",
                "Folded continued",
                "p1",
                &[],
            ),
            (
                "title: \"Injected\ntags: [evil]\"\nid: p1",
                "Injected tags: [evil]",
                "p1",
                &[],
            ),
            ("title: \"a\\\n  b\n\n  c\"\nid: p1", "ab\nc", "p1", &[]),
            ("title: \"cr\rchar\"\nid: p1", "cr char", "p1", &[]),
            ("title: plain # comment\nid: p1", "plain", "p1", &[]),
            (
                "title: plain\n  continued\nid: p1",
                "plain continued",
                "p1",
                &[],
            ),
            ("title: ~\nid: \"p1\"", "", "p1", &[]),
            (
                "title: Real\nid: p1\nextra:\n  title: Fake",
                "Real",
                "p1",
                &[],
            ),
            (
                "title: Real\nid: p1\nx: {a: \"}\",\ntitle: Fake}",
                "Real",
                "p1",
                &[],
            ),
            (
                "title: t\nid: p1\ntags: [a, 'b, c', \"d, e\",\n  f]",
                "t",
                "p1",
                &["a", "b, c", "d, e", "f"],
            ),
            (
                "title: t\nid: p1\ntags:\n  - alpha\n  - 'beta'",
                "t",
                "p1",
                &["alpha", "beta"],
            ),
            (
                "title: t\nid: p1\ntags:\n- alpha # c\n- \"\"",
                "t",
                "p1",
                &["alpha", ""],
            ),
        ];
        for &(yaml, title, id, tags) in cases {
            let mut warnings = Vec::new();
            let fm = parse_frontmatter(&format!("---\n{yaml}\n---\n"), &mut warnings).unwrap();
            assert_eq!(fm.title, title, "{yaml:?}");
            assert_eq!(fm.id, id, "{yaml:?}");
            assert_eq!(fm.tags, tags, "{yaml:?}");
            assert!(warnings.is_empty(), "{yaml:?}: {warnings:?}");
        }
        let quoted = "---\nid: p1\nupdated_at: \"2024-01-01T00:00:00+02:00\"\n---\n";
        let fm = parse_frontmatter(quoted, &mut Vec::new()).unwrap();
        assert_eq!(fm.updated_at.as_deref(), Some("2024-01-01T00:00:00+02:00"));
    }

    #[test]
    fn parse_frontmatter_skips_and_reports_unsupported_values() {
        let cases = [
            ("title: |\n  block", "title"),
            ("title: &a anchored", "title"),
            ("title: !!str tagged", "title"),
            ("title: a\ntitle: b", "title"),
            ("title: \"bad \\q escape\"", "title"),
            ("title: nested: mapping", "title"),
            ("title: \"x\"\n  junk", "title"),
            ("tags: [a, [b]]", "tags"),
            ("tags: {a: b}", "tags"),
            ("tags: [a\n  # unterminated", "tags"),
            ("title: \"", "title"),
        ];
        for (yaml, key) in cases {
            let mut warnings = Vec::new();
            let fm =
                parse_frontmatter(&format!("---\nid: p1\n{yaml}\n---\n"), &mut warnings).unwrap();
            assert_eq!(fm.id, "p1", "{yaml:?}");
            assert!(
                fm.title.is_empty() && fm.tags.is_empty(),
                "{yaml:?}: {fm:?}"
            );
            assert!(
                warnings.len() == 1 && warnings[0].contains(&format!("{key:?}")),
                "{yaml:?}: {warnings:?}"
            );
        }
    }

    #[test]
    fn frontmatter_round_trips_hostile_titles_and_tags() {
        let hostile = [
            "Line1\nLine2",
            "Injected\ntags: [evil]",
            "x\n---\nbody-break",
            "cr\rcrlf\r\n",
            "bell\u{7}\u{0}\u{1b}\u{7f}",
            "\u{85}\u{9f}\u{a0}\u{2028}\u{2029}",
            "\u{feff}\u{fffe}\u{ffff}",
            "key: value # not a comment",
            "[a, b], c",
            "'it''s' \"q\" \\ \\n",
            "  padded\t ",
            "",
            "😀 日本",
            "- dash",
            "~",
        ];
        for title in hostile {
            let page = Page {
                id: "p1".to_string(),
                title: title.to_string(),
                updated_at: None,
                tags: hostile.iter().map(|t| t.to_string()).collect(),
                content: Vec::new(),
            };
            let mut warnings = Vec::new();
            let fm = parse_frontmatter(&build_frontmatter(&page), &mut warnings).unwrap();
            assert_eq!(fm.title, page.title);
            assert_eq!(fm.tags, page.tags);
            assert!(warnings.is_empty(), "{title:?}: {warnings:?}");
        }
    }

    #[test]
    fn id_is_safe_rejects_traversal_and_absolute() {
        // real UUID-style ids pass
        assert!(id_is_safe("550e8400-e29b-41d4-a716-446655440000"));
        assert!(id_is_safe("simple-id-123"));
        // path-escaping ids are rejected
        assert!(!id_is_safe("../../tmp/evil"));
        assert!(!id_is_safe("/tmp/evil"));
        assert!(!id_is_safe("foo/bar"));
        assert!(!id_is_safe("foo\\bar"));
        assert!(!id_is_safe(".."));
    }

    #[test]
    fn import_skips_traversal_id_without_writing_outside_kb() {
        // `tempfile` picks the name: a timestamp cannot, because `SystemTime::now`
        // ticks at microsecond granularity here, so two tests in the same process
        // stamp the same value and share a directory.
        let dir = tempfile::Builder::new()
            .prefix("lepiter-import-traversal-")
            .tempdir()
            .expect("temp dir")
            .keep();
        let input_dir = dir.join("input");
        let out_dir = dir.join("output");
        std::fs::create_dir_all(&input_dir).unwrap();

        // `id: ../escaped` would resolve to `<dir>/escaped.lepiter`, outside kb
        std::fs::write(
            input_dir.join("evil.md"),
            "---\ntitle: \"Evil\"\nid: ../escaped\n---\n\nbody\n",
        )
        .unwrap();
        // a well-formed sibling confirms the run continues after skipping
        std::fs::write(
            input_dir.join("good.md"),
            "---\ntitle: \"Good\"\nid: good-id\n---\n\nbody\n",
        )
        .unwrap();

        run_import(vec![
            input_dir.to_str().unwrap().to_string(),
            out_dir.to_str().unwrap().to_string(),
        ])
        .unwrap();

        assert!(!dir.join("escaped.lepiter").exists());
        assert!(!out_dir.join("../escaped.lepiter").exists());
        assert!(out_dir.join("good-id.lepiter").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn import_skips_duplicate_id_keeping_first_file() {
        // `tempfile` picks the name: a timestamp cannot, because `SystemTime::now`
        // ticks at microsecond granularity here, so two tests in the same process
        // stamp the same value and share a directory.
        let dir = tempfile::Builder::new()
            .prefix("lepiter-import-dup-")
            .tempdir()
            .expect("temp dir")
            .keep();
        let input_dir = dir.join("input");
        let out_dir = dir.join("output");
        std::fs::create_dir_all(&input_dir).unwrap();

        // two files share `id: shared`; the alphabetically-first filename wins
        std::fs::write(
            input_dir.join("a-first.md"),
            "---\ntitle: \"First\"\nid: shared\n---\n\nfirst body\n",
        )
        .unwrap();
        std::fs::write(
            input_dir.join("b-second.md"),
            "---\ntitle: \"Second\"\nid: shared\n---\n\nsecond body\n",
        )
        .unwrap();
        // a sibling with a distinct id must still import
        std::fs::write(
            input_dir.join("c-sibling.md"),
            "---\ntitle: \"Sibling\"\nid: sibling\n---\n\nsibling body\n",
        )
        .unwrap();

        run_import(vec![
            input_dir.to_str().unwrap().to_string(),
            out_dir.to_str().unwrap().to_string(),
        ])
        .unwrap();

        // exactly one output for the shared id, and it is the first file's content
        let shared_path = out_dir.join("shared.lepiter");
        assert!(shared_path.exists());
        let shared: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&shared_path).unwrap()).unwrap();
        assert_eq!(shared["pageType"]["title"], "First");

        // the sibling still imports
        assert!(out_dir.join("sibling.lepiter").exists());

        // only two files total: the survivor and the sibling
        let count = std::fs::read_dir(&out_dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "lepiter")
            })
            .count();
        assert_eq!(count, 2);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn strip_frontmatter_returns_body() {
        let content = "---\ntitle: \"X\"\nid: x\n---\n\nBody here.\n";
        assert_eq!(strip_frontmatter(content), "\nBody here.\n");
    }

    #[test]
    fn strip_frontmatter_no_frontmatter() {
        let content = "Just text.";
        assert_eq!(strip_frontmatter(content), content);
    }

    #[test]
    fn parse_code_block_no_language() {
        let slug_map = HashMap::new();
        let input = "```\nsome code\n```\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        match &snippets[0] {
            Snippet::Code { language, code } => {
                assert!(language.is_none());
                assert_eq!(code, "some code");
            }
            other => panic!("expected Code, got {other:?}"),
        }
    }

    #[test]
    fn parse_code_block_with_a_longer_fence() {
        let slug_map = HashMap::new();
        let input = "````python\ndoc = '''\n```\nnested\n```\n'''\n````\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        match &snippets[0] {
            Snippet::Code { language, code } => {
                assert_eq!(language.as_deref(), Some("python"));
                assert_eq!(code, "doc = '''\n```\nnested\n```\n'''");
            }
            other => panic!("expected Code, got {other:?}"),
        }
    }

    #[test]
    fn closing_fence_may_be_indented_by_up_to_three_spaces() {
        let slug_map = HashMap::new();
        let snippets = parse_markdown_body("```\ncode\n   ```\n\n", &slug_map);
        assert_eq!(snippets.len(), 1);
        assert!(matches!(&snippets[0], Snippet::Code { code, .. } if code == "code"));

        // four spaces is an indented code line, not a fence
        let snippets = parse_markdown_body("```\ncode\n    ```\n```\n\n", &slug_map);
        assert!(matches!(&snippets[0], Snippet::Code { code, .. } if code == "code\n    ```"));
    }

    #[test]
    fn a_shorter_backtick_run_does_not_close_a_longer_fence() {
        assert!(!closes_fence("```", 4));
        assert!(closes_fence("````", 4));
        assert!(closes_fence("`````", 4));
        assert!(!closes_fence("``` text", 3));
    }

    #[test]
    fn open_fence_rejects_a_backtick_in_the_info_string() {
        assert_eq!(open_fence("```python"), Some((3, "python")));
        assert_eq!(open_fence("````python"), Some((4, "python")));
        assert_eq!(open_fence("``"), None);
        assert_eq!(open_fence("```a`b"), None);
    }

    #[test]
    fn an_unterminated_fence_keeps_the_lines_after_it() {
        let input = "before\n\n```python\ncode line\n\n- ```python\n  item code\n- next\n";
        let snippets = parse_markdown_body(input, &HashMap::new());
        let [
            Snippet::Text(before),
            Snippet::Text(fence),
            Snippet::Text(code),
            Snippet::List(items),
        ] = &snippets[..]
        else {
            panic!("expected three texts and a list, got {snippets:?}");
        };
        assert_eq!([before, fence, code], ["before", "```python", "code line"]);
        assert!(matches!(
            &items[0][..],
            [Snippet::Text(f), Snippet::Text(c)] if f == "```python" && c == "item code"
        ));
        assert!(matches!(&items[1][..], [Snippet::Text(t)] if t == "next"));

        let snippets = parse_markdown_body("````a\n```b\nx\n```\n", &HashMap::new());
        assert!(matches!(
            &snippets[..],
            [Snippet::Text(t), Snippet::Code { language: Some(l), code }]
                if t == "````a" && l == "b" && code == "x"
        ));
    }

    #[test]
    fn diff_block_without_change_lines_stays_text() {
        let slug_map = HashMap::new();
        let input = "```diff pharo\njust a note\n```\n\nafter\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 2);
        assert!(matches!(
            &snippets[0],
            Snippet::Code { language: Some(l), code } if l == "diff pharo" && code == "just a note"
        ));
        assert!(matches!(&snippets[1], Snippet::Text(t) if t == "after"));
        assert_eq!(
            snippet_json(&snippets[0]),
            json!({"__type": "textSnippet", "string": "```diff pharo\njust a note\n```"})
        );
    }

    #[test]
    fn consecutive_lines_become_one_text_snippet() {
        let slug_map = HashMap::new();
        let input = "para line one\npara line two\n\nsecond snippet\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 2);
        assert!(matches!(&snippets[0], Snippet::Text(t) if t == "para line one\npara line two"));
        assert!(matches!(&snippets[1], Snippet::Text(t) if t == "second snippet"));
    }

    #[test]
    fn consecutive_quote_lines_become_one_snippet() {
        let slug_map = HashMap::new();
        for (input, text) in [
            ("> quoted text\n\n", "> quoted text"),
            ("> first\n>\n> third\n\n", "> first\n\nthird"),
        ] {
            let snippets = parse_markdown_body(input, &slug_map);
            assert!(
                matches!(&snippets[..], [Snippet::Text(t)] if t == text),
                "{input:?} read as {snippets:?}"
            );
        }
    }

    #[test]
    fn escaped_block_starts_come_back_as_prose() {
        let slug_map = HashMap::new();
        let input = "\\- not a list\n\n\\[[unknown: x]]\n\n\\```rust\n\n\\\\already\n";
        let snippets = parse_markdown_body(input, &slug_map);
        let texts: Vec<&str> = snippets
            .iter()
            .map(|s| match s {
                Snippet::Text(t) => t.as_str(),
                other => panic!("expected Text, got {other:?}"),
            })
            .collect();
        assert_eq!(
            texts,
            ["- not a list", "[[unknown: x]]", "```rust", "\\already"]
        );
    }

    #[test]
    fn an_escaped_blank_line_stays_inside_the_paragraph() {
        let slug_map = HashMap::new();
        let input = "para one\n\\\npara two\n\nsecond snippet\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 2);
        assert!(matches!(&snippets[0], Snippet::Text(t) if t == "para one\n\npara two"));
        assert!(matches!(&snippets[1], Snippet::Text(t) if t == "second snippet"));
    }

    #[test]
    fn escaped_blank_snippets_stay_separate_snippets() {
        let slug_map = HashMap::new();
        let input = "before\n\n\\\n\n\\  \n\nafter\n";
        let snippets = parse_markdown_body(input, &slug_map);
        let texts: Vec<&str> = snippets
            .iter()
            .map(|s| match s {
                Snippet::Text(t) => t.as_str(),
                other => panic!("expected Text, got {other:?}"),
            })
            .collect();
        assert_eq!(texts, ["before", "", "  ", "after"]);
    }

    #[test]
    fn a_link_line_inside_a_paragraph_stays_paragraph_text() {
        let slug_map = HashMap::new();
        for text in [
            "intro\n[click](https://example.com)",
            "see [docs](https://example.com) here",
        ] {
            let snippets = parse_markdown_body(&format!("{text}\n\n"), &slug_map);
            assert!(
                matches!(&snippets[..], [Snippet::Text(t)] if t == text),
                "{text:?} read as {snippets:?}"
            );
        }
    }

    #[test]
    fn parse_list() {
        let slug_map = HashMap::new();
        let input = "- first\n- second\n- third\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        match &snippets[0] {
            Snippet::List(items) => {
                let texts: Vec<_> = items
                    .iter()
                    .map(|item| match &item[..] {
                        [Snippet::Text(t)] => t.as_str(),
                        other => panic!("expected one Text, got {other:?}"),
                    })
                    .collect();
                assert_eq!(texts, ["first", "second", "third"]);
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn parse_list_continuation() {
        let slug_map = HashMap::new();
        let input = "- first\n  continued\n- second\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        match &snippets[0] {
            Snippet::List(items) => {
                assert!(matches!(&items[0][..], [Snippet::Text(t)] if t == "first\ncontinued"));
                assert!(matches!(&items[1][..], [Snippet::Text(t)] if t == "second"));
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn list_item_blocks_are_read_like_page_blocks() {
        let mut slug_map = HashMap::new();
        slug_map.insert("two.md".to_string(), "id-two".to_string());
        let input = "- intro [two](two.md)\n  \n  ```python\n  \\d [x](two.md)\n  ```\n- [two](two.md)\n- \\[two](two.md)\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        let [Snippet::List(items)] = &snippets[..] else {
            panic!("expected one List, got {snippets:?}");
        };
        assert!(matches!(
            &items[0][..],
            [Snippet::Text(t), Snippet::Code { language: Some(l), code }]
                if t == "intro [two](page:id-two)" && l == "python" && code == "\\d [x](two.md)"
        ));
        assert!(matches!(
            &items[1][..],
            [Snippet::Link { text, url }] if text == "two" && url == "page:id-two"
        ));
        assert!(matches!(&items[2][..], [Snippet::Text(t)] if t == "[two](page:id-two)"));
    }

    #[test]
    fn list_nesting_past_the_depth_limit_is_read_back_as_text() {
        let fm = Frontmatter {
            title: "Deep".to_string(),
            id: "deep-1".to_string(),
            tags: Vec::new(),
            updated_at: None,
        };
        let lists_in_items = format!("{}x\n", "- ".repeat(100_000));
        let lists_under_text: String = (0..100)
            .map(|level| format!("{}- level {level}\n", "  ".repeat(level)))
            .collect();
        // the list at depth MAX_LIST_DEPTH keeps the rest of its item as text.
        let snippets = parse_markdown_body(&lists_in_items, &HashMap::new());
        let mut level = &snippets[..];
        for _ in 0..MAX_LIST_DEPTH {
            let [Snippet::List(items)] = level else {
                panic!("expected one nested list, got {level:?}");
            };
            level = &items[0][..];
        }
        let rest = format!("{}x", "- ".repeat(100_000 - MAX_LIST_DEPTH - 1));
        assert!(
            matches!(level, [Snippet::List(items)] if matches!(&items[0][..], [Snippet::Text(t)] if *t == rest))
        );
        for input in [lists_in_items, lists_under_text] {
            let snippets = parse_markdown_body(&input, &HashMap::new());
            let bytes = serde_json::to_vec(&build_page_json(&fm, &snippets)).unwrap();
            serde_json::from_slice::<serde_json::Value>(&bytes).expect("page reads back");
        }
    }

    #[test]
    fn an_indented_list_inside_an_item_is_a_nested_list() {
        let snippets = parse_markdown_body("- outer\n  - inner\n- next\n", &HashMap::new());
        let [Snippet::List(items)] = &snippets[..] else {
            panic!("expected one List, got {snippets:?}");
        };
        let [Snippet::Text(outer), Snippet::List(inner)] = &items[0][..] else {
            panic!("expected text and a nested list, got {:?}", items[0]);
        };
        assert_eq!(outer, "outer");
        assert_eq!(inner.len(), 1);
        assert!(matches!(&inner[0][..], [Snippet::Text(t)] if t == "inner"));
        assert!(matches!(&items[1][..], [Snippet::Text(t)] if t == "next"));
    }

    #[test]
    fn parse_rewrite_block() {
        let slug_map = HashMap::new();
        let input = "```diff pharo\n# scope: MyClass\n# method_pattern: true\n-oldMethod\n+newMethod\n```\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        match &snippets[0] {
            Snippet::Rewrite {
                language,
                search,
                replace,
                scope,
                is_method_pattern,
            } => {
                assert_eq!(language.as_deref(), Some("pharo"));
                assert_eq!(search, "oldMethod");
                assert_eq!(replace, "newMethod");
                assert_eq!(scope.as_deref(), Some("MyClass"));
                assert_eq!(*is_method_pattern, Some(true));
            }
            other => panic!("expected Rewrite, got {other:?}"),
        }
    }

    #[test]
    fn build_page_json_basic() {
        let fm = Frontmatter {
            title: "Test".to_string(),
            id: "abc-123".to_string(),
            tags: vec!["rust".to_string()],
            updated_at: Some("2024-01-01T00:00:00+00:00".to_string()),
        };
        let snippets = vec![
            Snippet::Text("Hello".to_string()),
            Snippet::Code {
                language: Some("python".to_string()),
                code: "print(1)".to_string(),
            },
        ];
        let json = build_page_json(&fm, &snippets);

        assert_eq!(json["uid"]["uuid"], "abc-123");
        assert_eq!(json["pageType"]["title"], "Test");
        assert_eq!(json["tags"], serde_json::json!(["rust"]));
        assert_eq!(
            json["editTime"]["time"]["dateAndTimeString"],
            "2024-01-01T00:00:00+00:00"
        );

        let items = json["children"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["__type"], "textSnippet");
        assert_eq!(items[0]["string"], "Hello");
        assert_eq!(items[1]["__type"], "pythonSnippet");
        assert_eq!(items[1]["code"], "print(1)");
    }

    #[test]
    fn build_page_json_updated_at_parse() {
        let valid = Frontmatter {
            title: "Valid".to_string(),
            id: "valid-1".to_string(),
            tags: Vec::new(),
            updated_at: Some("2024-01-01T00:00:00+00:00".to_string()),
        };
        let json = build_page_json(&valid, &[]);
        assert_eq!(
            json["editTime"]["time"]["dateAndTimeString"],
            "2024-01-01T00:00:00+00:00"
        );

        let invalid = Frontmatter {
            title: "Invalid".to_string(),
            id: "invalid-1".to_string(),
            tags: Vec::new(),
            updated_at: Some("not-a-date".to_string()),
        };
        let json = build_page_json(&invalid, &[]);
        assert!(json.get("editTime").is_none());
    }

    #[test]
    fn build_page_json_empty_content() {
        let fm = Frontmatter {
            title: "Empty".to_string(),
            id: "empty-1".to_string(),
            tags: Vec::new(),
            updated_at: None,
        };
        let json = build_page_json(&fm, &[]);
        let items = json["children"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["__type"], "textSnippet");
        assert_eq!(items[0]["string"], "");
    }

    #[test]
    fn build_page_json_list() {
        let fm = Frontmatter {
            title: "List".to_string(),
            id: "list-1".to_string(),
            tags: Vec::new(),
            updated_at: None,
        };
        let snippets = vec![Snippet::List(vec![
            vec![Snippet::Text("item one".to_string())],
            vec![Snippet::Text("item two".to_string())],
        ])];
        let json = build_page_json(&fm, &snippets);
        assert_eq!(
            json["children"]["items"][0],
            json!({"__type": "listSnippet", "children": {"items": [
                {"__type": "textSnippet", "string": "item one"},
                {"__type": "textSnippet", "string": "item two"},
            ]}})
        );
    }

    #[test]
    fn list_item_json_nests_later_blocks_under_the_first() {
        let item = list_item_json(&[
            Snippet::Text("intro".to_string()),
            Snippet::Code {
                language: Some("python".to_string()),
                code: "x".to_string(),
            },
            Snippet::Unknown {
                typ: "wardleyMap".to_string(),
            },
        ]);
        assert_eq!(
            item,
            json!({
                "__type": "textSnippet",
                "string": "intro",
                "children": { "items": [
                    { "__type": "pythonSnippet", "code": "x" },
                    { "__type": "wardleyMap" },
                ] }
            })
        );
    }

    #[test]
    fn list_item_json_holds_blocks_after_a_leading_list_in_an_empty_text() {
        let nested = Snippet::List(vec![vec![Snippet::Text("a".to_string())]]);
        let item = list_item_json(&[nested.clone(), Snippet::Text("after".to_string())]);
        assert_eq!(
            item,
            json!({
                "__type": "textSnippet",
                "string": "",
                "children": { "items": [snippet_json(&nested), { "__type": "textSnippet", "string": "after" }] }
            })
        );
        assert_eq!(
            list_item_json(std::slice::from_ref(&nested)),
            snippet_json(&nested)
        );
        assert_eq!(
            list_item_json(&[]),
            json!({ "__type": "textSnippet", "string": "" })
        );
    }

    #[test]
    fn build_page_json_link() {
        let fm = Frontmatter {
            title: "Links".to_string(),
            id: "link-1".to_string(),
            tags: Vec::new(),
            updated_at: None,
        };
        let snippets = vec![Snippet::Link {
            text: "example".to_string(),
            url: "https://example.com".to_string(),
        }];
        let json = build_page_json(&fm, &snippets);
        let items = json["children"]["items"].as_array().unwrap();
        assert_eq!(items[0]["__type"], "linkSnippet");
        assert_eq!(items[0]["url"], "https://example.com");
        assert_eq!(items[0]["string"], "example");
    }

    #[test]
    fn build_page_json_rewrite() {
        let fm = Frontmatter {
            title: "Rewrite".to_string(),
            id: "rw-1".to_string(),
            tags: Vec::new(),
            updated_at: None,
        };
        let snippets = vec![Snippet::Rewrite {
            language: Some("pharo".to_string()),
            search: "old".to_string(),
            replace: "new".to_string(),
            scope: Some("MyClass".to_string()),
            is_method_pattern: Some(true),
        }];
        let json = build_page_json(&fm, &snippets);
        let items = json["children"]["items"].as_array().unwrap();
        assert_eq!(items[0]["__type"], "pharoRewrite");
        assert_eq!(items[0]["search"], "old");
        assert_eq!(items[0]["replace"], "new");
        assert_eq!(items[0]["scope"], "MyClass");
        assert_eq!(items[0]["isMethodPattern"], true);
    }

    #[test]
    fn snippet_type_mapping() {
        assert_eq!(snippet_type_for_language("python"), "pythonSnippet");
        assert_eq!(snippet_type_for_language("pharo"), "pharoSnippet");
        assert_eq!(snippet_type_for_language("javascript"), "javascriptSnippet");
        assert_eq!(snippet_type_for_language("json"), "jsonSnippet");
        assert_eq!(snippet_type_for_language("yaml"), "yamlSnippet");
        assert_eq!(
            snippet_type_for_language("shellcommand"),
            "shellCommandSnippet"
        );
        assert_eq!(
            snippet_type_for_language("robocodermetamodel"),
            "robocoderMetamodelSnippet"
        );
        // elementSnippet round-trips through the `element` fence.
        assert_eq!(snippet_type_for_language("element"), "elementSnippet");
        assert_eq!(snippet_type_for_language("unknown"), "textSnippet");
    }

    #[test]
    fn shell_fences_map_to_text_snippet() {
        assert_eq!(snippet_type_for_language("bash"), "textSnippet");
        assert_eq!(snippet_type_for_language("sh"), "textSnippet");
        assert_eq!(snippet_type_for_language("shell"), "textSnippet");
    }

    #[test]
    fn parse_unknown_marker() {
        let slug_map = HashMap::new();
        let input = "[[unknown: wardleyMap]]\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        match &snippets[0] {
            Snippet::Unknown { typ } => assert_eq!(typ, "wardleyMap"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn bash_fence_roundtrips_as_text_snippet() {
        let slug_map = HashMap::new();
        let input = "```bash\necho hello\n```\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        let fm = Frontmatter {
            title: "Shell".to_string(),
            id: "sh-1".to_string(),
            tags: Vec::new(),
            updated_at: None,
        };
        let json = build_page_json(&fm, &snippets);
        let items = json["children"]["items"].as_array().unwrap();
        assert_eq!(items[0]["__type"], "textSnippet");
        assert_eq!(items[0]["string"], "```bash\necho hello\n```");
    }

    #[test]
    fn is_standalone_link_detection() {
        assert!(is_standalone_link("[text](url)"));
        assert!(is_standalone_link("[text](https://example.com)"));
        assert!(!is_standalone_link("text [link](url) more"));
        assert!(!is_standalone_link("no link here"));
        assert!(!is_standalone_link("[unclosed]("));
    }

    #[test]
    fn end_to_end_import() {
        // `tempfile` picks the name: a timestamp cannot, because `SystemTime::now`
        // ticks at microsecond granularity here, so two tests in the same process
        // stamp the same value and share a directory.
        let dir = tempfile::Builder::new()
            .prefix("lepiter-import-test-")
            .tempdir()
            .expect("temp dir")
            .keep();
        let out_dir = dir.join("output");
        let input_dir = dir.join("input");
        std::fs::create_dir_all(&input_dir).unwrap();

        // write two markdown files
        std::fs::write(
            input_dir.join("alpha.md"),
            "---\ntitle: \"Alpha\"\nid: id-alpha\n---\n\n## heading\n\nsome text\n\n[Beta](beta.md)\n",
        ).unwrap();
        std::fs::write(
            input_dir.join("beta.md"),
            "---\ntitle: \"Beta\"\nid: id-beta\n---\n\nsee [link](alpha.md) here\n",
        )
        .unwrap();

        run_import(vec![
            input_dir.to_str().unwrap().to_string(),
            out_dir.to_str().unwrap().to_string(),
        ])
        .unwrap();

        // verify output files exist and parse
        let alpha_path = out_dir.join("id-alpha.lepiter");
        assert!(alpha_path.exists());
        let alpha: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&alpha_path).unwrap()).unwrap();
        assert_eq!(alpha["uid"]["uuid"], "id-alpha");
        assert_eq!(alpha["pageType"]["title"], "Alpha");
        let items = alpha["children"]["items"].as_array().unwrap();
        assert_eq!(items[0]["string"], "## heading");

        // check link was rewritten
        let link_item = items.iter().find(|i| i["__type"] == "linkSnippet").unwrap();
        assert_eq!(link_item["url"], "page:id-beta");

        let beta_path = out_dir.join("id-beta.lepiter");
        assert!(beta_path.exists());
        let beta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&beta_path).unwrap()).unwrap();
        let beta_items = beta["children"]["items"].as_array().unwrap();
        let text_item = &beta_items[0];
        assert_eq!(text_item["string"], "see [link](page:id-alpha) here");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
