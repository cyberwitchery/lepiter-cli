use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::DateTime;
use lepiter_core::{
    LinkKind, language_to_snippet_type, rewrite_inline_links, unescape_block_start,
};
use serde_json::json;

use super::{ArgSpec, parse_args};

const SPEC: ArgSpec<'static> = ArgSpec {
    usage: "usage: lepiter-cli import <input-dir> [kb-path]\n\n\
            converts exported markdown files (with yaml frontmatter) back\n\
            into lepiter page json files. reverses the `export` subcommand.\n\n\
            page content survives the round trip, with these exceptions:\n\
            \x20 - binary attachments (images, etc.) are not copied. picture\n\
            \x20   snippet references are preserved but the files must be\n\
            \x20   restored separately\n\
            \x20 - an unknown snippet keeps its type but not its other fields\n\
            \x20 - picture, youtube and word snippets come back as the link or\n\
            \x20   text snippet their markdown form implies\n\
            \x20 - a text snippet that is exactly [label](url) comes back as a\n\
            \x20   link snippet\n\
            \x20 - an empty text snippet is dropped\n\
            \x20 - a snippet nested inside a list item is flattened to text",
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
    let mut md_files: Vec<(PathBuf, String)> = Vec::new();
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

        let Some(fm) = parse_frontmatter(&content) else {
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

        md_files.push((path, content));
    }

    fs::create_dir_all(&kb_path)
        .with_context(|| format!("failed to create output directory {}", kb_path.display()))?;

    let mut imported = 0usize;
    let mut errors = 0usize;

    for (_path, content) in &md_files {
        let Some(fm) = parse_frontmatter(content) else {
            errors += 1;
            continue;
        };

        let body = strip_frontmatter(content);
        let snippets = parse_markdown_body(body, &slug_to_id);

        let page_json = build_page_json(&fm, &snippets);
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
    if errors > 0 {
        eprintln!("{errors} files skipped due to errors");
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct Frontmatter {
    title: String,
    id: String,
    tags: Vec<String>,
    updated_at: Option<String>,
}

fn parse_frontmatter(content: &str) -> Option<Frontmatter> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    if !content.starts_with("---\n") && !content.starts_with("---\r\n") {
        return None;
    }
    let after_first = &content[4..];
    let end = after_first
        .find("\n---\n")
        .or_else(|| after_first.find("\n---\r\n"))
        .or_else(|| {
            // handle case where --- is at end of file
            if after_first.ends_with("\n---") {
                Some(after_first.len() - 3)
            } else {
                None
            }
        })?;
    let yaml_block = &after_first[..end];

    let mut title = String::new();
    let mut id = String::new();
    let mut tags = Vec::new();
    let mut updated_at = None;

    for line in yaml_block.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("title:") {
            title = parse_yaml_string(rest.trim());
        } else if let Some(rest) = line.strip_prefix("id:") {
            id = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("updated_at:") {
            let val = rest.trim().to_string();
            if !val.is_empty() {
                updated_at = Some(val);
            }
        } else if let Some(rest) = line.strip_prefix("tags:") {
            tags = parse_yaml_tags(rest.trim());
        }
    }

    Some(Frontmatter {
        title,
        id,
        tags,
        updated_at,
    })
}

/// Rejects frontmatter ids that would escape the output directory when joined as
/// a filename. Real Lepiter ids are UUIDs, so this never rejects legitimate input.
fn id_is_safe(id: &str) -> bool {
    !id.contains('/')
        && !id.contains('\\')
        && !id.contains("..")
        && !PathBuf::from(id).is_absolute()
}

fn parse_yaml_string(s: &str) -> String {
    let s = s.trim();
    // the len check keeps a lone quote (start == end) from slicing `1..0` and panicking
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        let inner = &s[1..s.len() - 1];
        inner.replace("\\\"", "\"").replace("\\\\", "\\")
    } else {
        s.to_string()
    }
}

fn parse_yaml_tags(s: &str) -> Vec<String> {
    let s = s.trim();
    if !s.starts_with('[') || !s.ends_with(']') {
        return Vec::new();
    }
    let inner = &s[1..s.len() - 1];
    inner
        .split(',')
        .map(|t| parse_yaml_string(t.trim()))
        .filter(|t| !t.is_empty())
        .collect()
}

fn strip_frontmatter(content: &str) -> &str {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    if !content.starts_with("---\n") && !content.starts_with("---\r\n") {
        return content;
    }
    let after_first = &content[4..];
    if let Some(pos) = after_first.find("\n---\n") {
        &after_first[pos + 5..]
    } else if let Some(pos) = after_first.find("\n---\r\n") {
        &after_first[pos + 6..]
    } else if after_first.ends_with("\n---") {
        ""
    } else {
        content
    }
}

#[derive(Debug, Clone)]
enum Snippet {
    Text(String),
    Code {
        language: Option<String>,
        code: String,
    },
    List(Vec<String>),
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

fn parse_markdown_body(body: &str, slug_to_id: &HashMap<String, String>) -> Vec<Snippet> {
    let mut snippets = Vec::new();
    let mut lines = body.lines().peekable();

    while let Some(line) = lines.next() {
        // blank lines between blocks
        if line.trim().is_empty() {
            continue;
        }

        // code block
        if let Some((fence_len, info)) = open_fence(line) {
            let (snipps, consumed) = parse_code_block(fence_len, info, &mut lines);
            snippets.extend(snipps);
            if !consumed {
                // unterminated code block — treat opening as text
                snippets.push(Snippet::Text(rewrite_links_to_internal(line, slug_to_id)));
            }
            continue;
        }

        // blockquote, one snippet for the whole run of `>` lines
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
            let items: Vec<String> = items
                .into_iter()
                .map(|item| rewrite_links_to_internal(&unescape_lines(&item), slug_to_id))
                .collect();
            snippets.push(Snippet::List(items));
            continue;
        }

        // paragraph: consecutive lines up to a blank line or another block
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

        // a lone line may instead be a whole-snippet form export writes by itself
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
            if is_standalone_link(only) {
                let (text, url) = extract_standalone_link(only);
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

/// Reads an opening code fence — at least three backticks and an info string
/// with no backtick in it — into its backtick count and info string.
fn open_fence(line: &str) -> Option<(usize, &str)> {
    let ticks = line.chars().take_while(|c| *c == '`').count();
    if ticks < 3 {
        return None;
    }
    let info = line[ticks..].trim();
    if info.contains('`') {
        return None;
    }
    Some((ticks, info))
}

/// Whether `line` closes a fence opened with `open_len` backticks: a run of at
/// least that many, indented by at most three spaces, and nothing else.
fn closes_fence(line: &str, open_len: usize) -> bool {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return false;
    }
    let ticks = rest.chars().take_while(|c| *c == '`').count();
    ticks >= open_len && rest[ticks..].trim().is_empty()
}

fn parse_code_block(
    fence_len: usize,
    info: &str,
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
) -> (Vec<Snippet>, bool) {
    let mut body = Vec::new();
    let mut found_end = false;

    for line in lines.by_ref() {
        if closes_fence(line, fence_len) {
            found_end = true;
            break;
        }
        body.push(line);
    }

    if !found_end {
        return (Vec::new(), false);
    }

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

fn is_standalone_link(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') {
        return false;
    }
    let Some(bracket_end) = trimmed.find("](") else {
        return false;
    };
    let after = &trimmed[bracket_end + 2..];
    after.ends_with(')') && !after[..after.len() - 1].contains(')')
}

fn extract_standalone_link(line: &str) -> (String, String) {
    let trimmed = line.trim();
    let bracket_end = trimmed.find("](").unwrap();
    let text = trimmed[1..bracket_end].to_string();
    let url = trimmed[bracket_end + 2..trimmed.len() - 1].to_string();
    (text, url)
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

fn build_page_json(fm: &Frontmatter, snippets: &[Snippet]) -> serde_json::Value {
    let mut items: Vec<serde_json::Value> = Vec::new();

    for snippet in snippets {
        match snippet {
            Snippet::Text(text) => {
                items.push(json!({ "__type": "textSnippet", "string": text }));
            }
            Snippet::Code { language, code } => {
                items.push(build_code_snippet_json(language, code));
            }
            Snippet::List(list_items) => {
                let children: Vec<serde_json::Value> = list_items
                    .iter()
                    .map(|item| json!({ "__type": "textSnippet", "string": item }))
                    .collect();
                items.push(json!({
                    "__type": "listSnippet",
                    "children": { "items": children }
                }));
            }
            Snippet::Link { text, url } => {
                items.push(json!({
                    "__type": "linkSnippet",
                    "string": text,
                    "url": url
                }));
            }
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
                // language is implicit in the __type for now
                let _ = language;
                items.push(obj);
            }
            Snippet::Unknown { typ } => {
                items.push(json!({ "__type": typ }));
            }
        }
    }

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
    use super::*;

    #[test]
    fn parse_frontmatter_basic() {
        let content = "---\ntitle: \"Test Page\"\nid: abc-123\ntags: [\"rust\", \"cli\"]\nupdated_at: 2024-01-01T00:00:00+00:00\n---\n\nBody text.\n";
        let fm = parse_frontmatter(content).unwrap();
        assert_eq!(fm.title, "Test Page");
        assert_eq!(fm.id, "abc-123");
        assert_eq!(fm.tags, vec!["rust", "cli"]);
        assert_eq!(fm.updated_at.as_deref(), Some("2024-01-01T00:00:00+00:00"));
    }

    #[test]
    fn parse_frontmatter_escaped_title() {
        let content = "---\ntitle: \"Page with \\\"quotes\\\"\"\nid: p1\n---\n\n";
        let fm = parse_frontmatter(content).unwrap();
        assert_eq!(fm.title, "Page with \"quotes\"");
    }

    #[test]
    fn parse_frontmatter_no_tags() {
        let content = "---\ntitle: \"Simple\"\nid: p2\n---\n\n";
        let fm = parse_frontmatter(content).unwrap();
        assert!(fm.tags.is_empty());
        assert!(fm.updated_at.is_none());
    }

    #[test]
    fn parse_frontmatter_missing_returns_none() {
        assert!(parse_frontmatter("no frontmatter here").is_none());
        assert!(parse_frontmatter("---\nunclosed").is_none());
    }

    #[test]
    fn parse_frontmatter_bom() {
        let content = "\u{feff}---\ntitle: \"BOM\"\nid: bom-1\n---\n\n";
        let fm = parse_frontmatter(content).unwrap();
        assert_eq!(fm.title, "BOM");
        assert_eq!(fm.id, "bom-1");
    }

    #[test]
    fn parse_yaml_string_lone_quote_does_not_panic() {
        // a value that is a single quote char used to slice `1..0` and panic
        assert_eq!(parse_yaml_string("\""), "\"");
        assert_eq!(parse_yaml_string("'"), "'");
        // normal quoting still works
        assert_eq!(parse_yaml_string("\"\""), "");
        assert_eq!(parse_yaml_string("\"hello\""), "hello");
        assert_eq!(parse_yaml_string("'hello'"), "hello");
        assert_eq!(parse_yaml_string("plain"), "plain");
    }

    #[test]
    fn parse_yaml_tags_lone_quote_does_not_panic() {
        // `tags: ["]` — the bracket-stripped inner is a lone quote
        assert_eq!(parse_yaml_tags("[\"]"), vec!["\""]);
    }

    #[test]
    fn parse_frontmatter_lone_quote_title_does_not_panic() {
        // a whole import run used to abort on a single stray quote in any file
        let content = "---\ntitle: \"\nid: lone-1\n---\n\nbody\n";
        let fm = parse_frontmatter(content).unwrap();
        assert_eq!(fm.title, "\"");
        assert_eq!(fm.id, "lone-1");
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
        let dir = std::env::temp_dir().join(format!(
            "lepiter-import-traversal-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
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
        let dir = std::env::temp_dir().join(format!(
            "lepiter-import-dup-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
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
    fn parse_heading() {
        let slug_map = HashMap::new();
        let snippets = parse_markdown_body("## My Heading\n\n", &slug_map);
        assert_eq!(snippets.len(), 1);
        match &snippets[0] {
            Snippet::Text(text) => assert_eq!(text, "## My Heading"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn parse_paragraph() {
        let slug_map = HashMap::new();
        let snippets = parse_markdown_body("Hello world.\n\n", &slug_map);
        assert_eq!(snippets.len(), 1);
        match &snippets[0] {
            Snippet::Text(text) => assert_eq!(text, "Hello world."),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn parse_code_block_with_language() {
        let slug_map = HashMap::new();
        let input = "```python\nprint('hi')\n```\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        match &snippets[0] {
            Snippet::Code { language, code } => {
                assert_eq!(language.as_deref(), Some("python"));
                assert_eq!(code, "print('hi')");
            }
            other => panic!("expected Code, got {other:?}"),
        }
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
    fn diff_block_without_change_lines_stays_text() {
        let slug_map = HashMap::new();
        let input = "```diff pharo\njust a note\n```\n\nafter\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 2);
        assert!(matches!(&snippets[0], Snippet::Code { .. }));
        assert!(matches!(&snippets[1], Snippet::Text(t) if t == "after"));
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
        let input = "> first\n>\n> third\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        assert!(matches!(&snippets[0], Snippet::Text(t) if t == "> first\n\nthird"));
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
    fn a_link_line_inside_a_paragraph_stays_paragraph_text() {
        let slug_map = HashMap::new();
        let snippets = parse_markdown_body("intro\n[click](https://example.com)\n", &slug_map);
        assert_eq!(snippets.len(), 1);
        assert!(
            matches!(&snippets[0], Snippet::Text(t) if t == "intro\n[click](https://example.com)")
        );
    }

    #[test]
    fn parse_list() {
        let slug_map = HashMap::new();
        let input = "- first\n- second\n- third\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        match &snippets[0] {
            Snippet::List(items) => {
                assert_eq!(items, &["first", "second", "third"]);
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
                assert_eq!(items[0], "first\ncontinued");
                assert_eq!(items[1], "second");
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn parse_blockquote() {
        let slug_map = HashMap::new();
        let input = "> quoted text\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        match &snippets[0] {
            Snippet::Text(text) => assert_eq!(text, "> quoted text"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn parse_standalone_link() {
        let slug_map = HashMap::new();
        let input = "[Click here](https://example.com)\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        match &snippets[0] {
            Snippet::Link { text, url } => {
                assert_eq!(text, "Click here");
                assert_eq!(url, "https://example.com");
            }
            other => panic!("expected Link, got {other:?}"),
        }
    }

    #[test]
    fn parse_link_rewrites_slug() {
        let mut slug_map = HashMap::new();
        slug_map.insert("other-page.md".to_string(), "uuid-123".to_string());

        let input = "[Other Page](other-page.md)\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        match &snippets[0] {
            Snippet::Link { text, url } => {
                assert_eq!(text, "Other Page");
                assert_eq!(url, "page:uuid-123");
            }
            other => panic!("expected Link, got {other:?}"),
        }
    }

    #[test]
    fn parse_inline_link_rewriting() {
        let mut slug_map = HashMap::new();
        slug_map.insert("target.md".to_string(), "uuid-456".to_string());

        let input = "see [link](target.md) here\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        match &snippets[0] {
            Snippet::Text(text) => {
                assert_eq!(text, "see [link](page:uuid-456) here");
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn parse_inline_link_external_unchanged() {
        let slug_map = HashMap::new();
        let input = "see [docs](https://example.com) here\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        match &snippets[0] {
            Snippet::Text(text) => {
                assert_eq!(text, "see [docs](https://example.com) here");
            }
            other => panic!("expected Text, got {other:?}"),
        }
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
            "item one".to_string(),
            "item two".to_string(),
        ])];
        let json = build_page_json(&fm, &snippets);
        let items = json["children"]["items"].as_array().unwrap();
        assert_eq!(items[0]["__type"], "listSnippet");
        let children = items[0]["children"]["items"].as_array().unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0]["string"], "item one");
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
        // regression: this arm was missing and degraded the snippet to text.
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
    fn build_page_json_unknown() {
        let fm = Frontmatter {
            title: "Unknown".to_string(),
            id: "unk-1".to_string(),
            tags: Vec::new(),
            updated_at: None,
        };
        let snippets = vec![Snippet::Unknown {
            typ: "wardleyMap".to_string(),
        }];
        let json = build_page_json(&fm, &snippets);
        let items = json["children"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["__type"], "wardleyMap");
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
    fn robocoder_metamodel_fence_roundtrips_to_its_snippet_type() {
        // regression: a `robocoderMetamodelSnippet` exports as a
        // `robocodermetamodel` fence and used to re-import as a plain
        // textSnippet, silently losing its type.
        let slug_map = HashMap::new();
        let input = "```robocodermetamodel\nMetamodel new\n```\n\n";
        let snippets = parse_markdown_body(input, &slug_map);
        assert_eq!(snippets.len(), 1);
        let fm = Frontmatter {
            title: "Robocoder".to_string(),
            id: "robo-1".to_string(),
            tags: Vec::new(),
            updated_at: None,
        };
        let json = build_page_json(&fm, &snippets);
        let items = json["children"]["items"].as_array().unwrap();
        assert_eq!(items[0]["__type"], "robocoderMetamodelSnippet");
        assert_eq!(items[0]["code"], "Metamodel new");
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
    fn roundtrip_frontmatter() {
        // build frontmatter the same way export does, then parse it back
        let original_title = "Page with \"quotes\" and \\backslashes\\";
        let escaped_title = original_title.replace('\\', "\\\\").replace('"', "\\\"");
        let fm_str = format!(
            "---\ntitle: \"{escaped_title}\"\nid: test-id\ntags: [\"a\", \"b\"]\nupdated_at: 2024-06-01T12:00:00+00:00\n---\n\nbody"
        );
        let fm = parse_frontmatter(&fm_str).unwrap();
        assert_eq!(fm.title, original_title);
        assert_eq!(fm.id, "test-id");
        assert_eq!(fm.tags, vec!["a", "b"]);
    }

    #[test]
    fn end_to_end_import() {
        let dir = std::env::temp_dir().join(format!(
            "lepiter-import-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
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
