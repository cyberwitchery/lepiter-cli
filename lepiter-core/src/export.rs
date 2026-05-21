//! markdown export for lepiter knowledge base pages.
//!
//! converts parsed pages to markdown files with yaml frontmatter and resolves
//! internal wikilinks to relative file paths.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{KnowledgeBaseIndex, LinkTargetKind, Node, Page, PageId};

/// builds a lookup from page id to the slug that will be used as the markdown filename.
pub fn build_slug_table(index: &KnowledgeBaseIndex) -> HashMap<PageId, String> {
    let mut table = HashMap::new();
    let mut seen = HashMap::<String, usize>::new();
    for meta in index.sorted_pages() {
        let mut slug = slug_from_title(&meta.title);
        let count = seen.entry(slug.clone()).or_insert(0);
        if *count > 0 {
            slug = format!("{slug}-{count}");
        }
        *seen.get_mut(&slug_from_title(&meta.title)).unwrap() += 1;
        table.insert(meta.id.clone(), slug);
    }
    table
}

/// builds a case-insensitive title-to-id lookup for wikilink resolution.
pub fn build_title_table(index: &KnowledgeBaseIndex) -> HashMap<String, PageId> {
    let mut table = HashMap::new();
    for meta in index.sorted_pages() {
        table
            .entry(meta.title_lower.clone())
            .or_insert_with(|| meta.id.clone());
    }
    table
}

/// converts a page title to a filesystem-safe slug.
///
/// lowercases, replaces non-alphanumeric characters with hyphens,
/// collapses runs of hyphens, and strips leading/trailing hyphens.
pub fn slug_from_title(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

/// renders a page to markdown with yaml frontmatter and resolved wikilinks.
pub fn render_page_to_markdown(
    page: &Page,
    index: &KnowledgeBaseIndex,
    slug_table: &HashMap<PageId, String>,
    title_table: &HashMap<String, PageId>,
) -> String {
    let mut out = String::new();

    // yaml frontmatter
    out.push_str("---\n");
    out.push_str(&format!("title: \"{}\"\n", escape_yaml(&page.title)));
    out.push_str(&format!("id: \"{}\"\n", page.id));
    if let Some(ts) = &page.updated_at {
        out.push_str(&format!("updated_at: \"{}\"\n", ts.to_rfc3339()));
    }
    if !page.tags.is_empty() {
        out.push_str("tags:\n");
        for tag in &page.tags {
            out.push_str(&format!("  - \"{}\"\n", escape_yaml(tag)));
        }
    }
    out.push_str("---\n\n");

    render_nodes_to_markdown(&page.content, index, slug_table, title_table, &mut out);
    out
}

fn render_nodes_to_markdown(
    nodes: &[Node],
    index: &KnowledgeBaseIndex,
    slug_table: &HashMap<PageId, String>,
    title_table: &HashMap<String, PageId>,
    out: &mut String,
) {
    for node in nodes {
        match node {
            Node::Heading { level, text } => {
                out.push_str(&"#".repeat((*level).max(1) as usize));
                out.push(' ');
                out.push_str(&resolve_inline_links(text, index, slug_table, title_table));
                out.push_str("\n\n");
            }
            Node::Paragraph { text } => {
                out.push_str(&resolve_inline_links(text, index, slug_table, title_table));
                out.push_str("\n\n");
            }
            Node::Text { text } => {
                out.push_str(&resolve_inline_links(text, index, slug_table, title_table));
                out.push('\n');
            }
            Node::List { items } => {
                for item in items {
                    let mut item_out = String::new();
                    render_nodes_to_markdown(item, index, slug_table, title_table, &mut item_out);
                    let mut lines = item_out.trim().lines();
                    if let Some(first) = lines.next() {
                        out.push_str("- ");
                        out.push_str(first);
                        out.push('\n');
                        for line in lines {
                            out.push_str("  ");
                            out.push_str(line);
                            out.push('\n');
                        }
                    }
                }
                out.push('\n');
            }
            Node::Code { language, code } => {
                out.push_str("```");
                if let Some(lang) = language {
                    out.push_str(lang);
                }
                out.push('\n');
                out.push_str(code);
                out.push_str("\n```\n\n");
            }
            Node::Link { text, url } => {
                let resolved_url = resolve_link_url(url, index, slug_table);
                out.push_str(&format!("[{text}]({resolved_url})\n\n"));
            }
            Node::Quote { text } => {
                let resolved = resolve_inline_links(text, index, slug_table, title_table);
                for line in resolved.lines() {
                    out.push_str("> ");
                    out.push_str(line);
                    out.push('\n');
                }
                out.push('\n');
            }
            Node::Rewrite {
                language,
                search,
                replace,
                scope,
                is_method_pattern,
            } => {
                let lang = language.clone().unwrap_or_else(|| "rewrite".to_string());
                out.push_str(&format!("```diff {lang}\n"));
                if let Some(scope) = scope {
                    out.push_str(&format!("# scope: {scope}\n"));
                }
                if let Some(is_method_pattern) = is_method_pattern {
                    out.push_str(&format!("# method_pattern: {is_method_pattern}\n"));
                }
                for line in crate::normalize_text(search).lines() {
                    out.push('-');
                    out.push_str(line);
                    out.push('\n');
                }
                for line in crate::normalize_text(replace).lines() {
                    out.push('+');
                    out.push_str(line);
                    out.push('\n');
                }
                out.push_str("```\n\n");
            }
            Node::Unknown { typ, .. } => {
                out.push_str(&format!("<!-- unknown snippet type: {typ} -->\n\n"));
            }
        }
    }
}

/// resolves a link url to a relative markdown path if it points to an internal page.
fn resolve_link_url(
    url: &str,
    index: &KnowledgeBaseIndex,
    slug_table: &HashMap<PageId, String>,
) -> String {
    match index.classify_link_target(url) {
        LinkTargetKind::InternalPage(id) => {
            if let Some(slug) = slug_table.get(&id) {
                format!("{slug}.md")
            } else {
                url.to_string()
            }
        }
        _ => url.to_string(),
    }
}

/// processes inline text, resolving `[[wikilinks]]` to `[title](slug.md)` and
/// internal `[label](target)` links to relative paths.
fn resolve_inline_links(
    text: &str,
    index: &KnowledgeBaseIndex,
    slug_table: &HashMap<PageId, String>,
    title_table: &HashMap<String, PageId>,
) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;

    while i < chars.len() {
        // [[wikilink]]
        if i + 1 < chars.len() && chars[i] == '[' && chars[i + 1] == '[' {
            let start = i + 2;
            if let Some(end) = find_closing_double_bracket(&chars, start) {
                let target: String = chars[start..end].iter().collect();
                let target_trimmed = target.trim();
                if !target_trimmed.is_empty() {
                    let resolved = resolve_wikilink(target_trimmed, index, slug_table, title_table);
                    out.push_str(&resolved);
                } else {
                    // empty wikilink, preserve as-is
                    out.push_str("[[]]");
                }
                i = end + 2;
                continue;
            }
        }

        // [label](target) - resolve internal links
        if chars[i] == '[' {
            let label_start = i + 1;
            if let Some(label_end) = find_char(&chars, ']', label_start)
                && label_end + 1 < chars.len()
                && chars[label_end + 1] == '('
            {
                let target_start = label_end + 2;
                if let Some(target_end) = find_char(&chars, ')', target_start) {
                    let label: String = chars[label_start..label_end].iter().collect();
                    let target: String = chars[target_start..target_end].iter().collect();
                    let target_trimmed = target.trim();
                    if !target_trimmed.is_empty() {
                        let resolved = resolve_link_url(target_trimmed, index, slug_table);
                        out.push_str(&format!("[{label}]({resolved})"));
                    } else {
                        out.push_str(&format!("[{label}]()"));
                    }
                    i = target_end + 1;
                    continue;
                }
            }
        }

        out.push(chars[i]);
        i += 1;
    }

    out
}

/// resolves a wikilink target to `[display](slug.md)` or leaves as `[[target]]` if unresolved.
fn resolve_wikilink(
    target: &str,
    index: &KnowledgeBaseIndex,
    slug_table: &HashMap<PageId, String>,
    title_table: &HashMap<String, PageId>,
) -> String {
    // try direct title lookup first (case-insensitive)
    let needle = target.to_lowercase();
    if let Some(id) = title_table.get(&needle)
        && let Some(slug) = slug_table.get(id)
    {
        return format!("[{target}]({slug}.md)");
    }

    // fall back to full link classification
    match index.classify_link_target(target) {
        LinkTargetKind::InternalPage(id) => {
            if let Some(slug) = slug_table.get(&id) {
                format!("[{target}]({slug}.md)")
            } else {
                format!("[[{target}]]")
            }
        }
        LinkTargetKind::ExternalUrl(url) => format!("[{target}]({url})"),
        _ => format!("[[{target}]]"),
    }
}

fn find_closing_double_bracket(chars: &[char], start: usize) -> Option<usize> {
    let mut i = start;
    while i + 1 < chars.len() {
        if chars[i] == ']' && chars[i + 1] == ']' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_char(chars: &[char], target: char, start: usize) -> Option<usize> {
    (start..chars.len()).find(|&i| chars[i] == target)
}

fn escape_yaml(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// exports a single page to a markdown file in the output directory.
///
/// returns the path to the written file.
pub fn export_page(
    index: &KnowledgeBaseIndex,
    id: &str,
    output_dir: &Path,
    slug_table: &HashMap<PageId, String>,
    title_table: &HashMap<String, PageId>,
) -> Result<PathBuf> {
    let page = index
        .load_page(id)
        .with_context(|| format!("failed to load page {id}"))?;

    let slug = slug_table
        .get(id)
        .cloned()
        .unwrap_or_else(|| slug_from_title(&page.title));

    let md = render_page_to_markdown(&page, index, slug_table, title_table);
    let file_path = output_dir.join(format!("{slug}.md"));
    fs::write(&file_path, md)
        .with_context(|| format!("failed to write {}", file_path.display()))?;
    Ok(file_path)
}

/// exports all pages in the knowledge base to markdown files.
///
/// creates the output directory if it does not exist. returns the list of
/// written file paths.
pub fn export_all(index: &KnowledgeBaseIndex, output_dir: &Path) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create output directory {}", output_dir.display()))?;

    let slug_table = build_slug_table(index);
    let title_table = build_title_table(index);

    let mut paths = Vec::new();
    for id in &index.sorted_ids {
        let path = export_page(index, id, output_dir, &slug_table, &title_table)?;
        paths.push(path);
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_from_title_basic() {
        assert_eq!(slug_from_title("Hello World"), "hello-world");
    }

    #[test]
    fn slug_from_title_special_chars() {
        assert_eq!(slug_from_title("Foo: Bar & Baz!"), "foo-bar-baz");
    }

    #[test]
    fn slug_from_title_leading_trailing() {
        assert_eq!(slug_from_title("  --hello-- "), "hello");
    }

    #[test]
    fn slug_from_title_collapses_hyphens() {
        assert_eq!(slug_from_title("a   b---c"), "a-b-c");
    }

    #[test]
    fn slug_from_title_empty() {
        assert_eq!(slug_from_title(""), "");
    }

    #[test]
    fn escape_yaml_quotes_and_backslash() {
        assert_eq!(escape_yaml(r#"say "hi" \ done"#), r#"say \"hi\" \\ done"#);
    }

    #[test]
    fn render_frontmatter_includes_title_and_id() {
        let page = Page {
            id: "test-id".to_string(),
            title: "Test Page".to_string(),
            updated_at: None,
            tags: vec!["alpha".to_string(), "beta".to_string()],
            content: vec![],
        };
        let index = make_empty_index();
        let slug_table = HashMap::new();
        let title_table = HashMap::new();
        let md = render_page_to_markdown(&page, &index, &slug_table, &title_table);
        assert!(md.starts_with("---\n"));
        assert!(md.contains("title: \"Test Page\""));
        assert!(md.contains("id: \"test-id\""));
        assert!(md.contains("  - \"alpha\""));
        assert!(md.contains("  - \"beta\""));
        assert!(md.contains("---\n\n"));
    }

    #[test]
    fn render_heading_and_paragraph() {
        let page = Page {
            id: "p1".to_string(),
            title: "T".to_string(),
            updated_at: None,
            tags: vec![],
            content: vec![
                Node::Heading {
                    level: 2,
                    text: "Hello".to_string(),
                },
                Node::Paragraph {
                    text: "World".to_string(),
                },
            ],
        };
        let index = make_empty_index();
        let slug_table = HashMap::new();
        let title_table = HashMap::new();
        let md = render_page_to_markdown(&page, &index, &slug_table, &title_table);
        assert!(md.contains("## Hello\n\n"));
        assert!(md.contains("World\n\n"));
    }

    #[test]
    fn render_code_block() {
        let page = Page {
            id: "p1".to_string(),
            title: "T".to_string(),
            updated_at: None,
            tags: vec![],
            content: vec![Node::Code {
                language: Some("python".to_string()),
                code: "print(1)".to_string(),
            }],
        };
        let index = make_empty_index();
        let slug_table = HashMap::new();
        let title_table = HashMap::new();
        let md = render_page_to_markdown(&page, &index, &slug_table, &title_table);
        assert!(md.contains("```python\nprint(1)\n```\n\n"));
    }

    #[test]
    fn render_unknown_as_comment() {
        let page = Page {
            id: "p1".to_string(),
            title: "T".to_string(),
            updated_at: None,
            tags: vec![],
            content: vec![Node::Unknown {
                typ: "wardleyMap".to_string(),
                raw: serde_json::Value::Null,
            }],
        };
        let index = make_empty_index();
        let slug_table = HashMap::new();
        let title_table = HashMap::new();
        let md = render_page_to_markdown(&page, &index, &slug_table, &title_table);
        assert!(md.contains("<!-- unknown snippet type: wardleyMap -->"));
    }

    #[test]
    fn wikilink_resolved_to_relative_path() {
        let mut slug_table = HashMap::new();
        slug_table.insert("page-1".to_string(), "target-page".to_string());
        let mut title_table = HashMap::new();
        title_table.insert("target page".to_string(), "page-1".to_string());

        let index = make_empty_index();
        let result = resolve_inline_links(
            "see [[Target Page]] for details",
            &index,
            &slug_table,
            &title_table,
        );
        assert_eq!(result, "see [Target Page](target-page.md) for details");
    }

    #[test]
    fn wikilink_unresolved_preserved() {
        let index = make_empty_index();
        let slug_table = HashMap::new();
        let title_table = HashMap::new();
        let result = resolve_inline_links(
            "see [[Missing Page]] here",
            &index,
            &slug_table,
            &title_table,
        );
        assert_eq!(result, "see [[Missing Page]] here");
    }

    #[test]
    fn inline_link_external_preserved() {
        let index = make_empty_index();
        let slug_table = HashMap::new();
        let title_table = HashMap::new();
        let result = resolve_inline_links(
            "click [here](https://example.com) now",
            &index,
            &slug_table,
            &title_table,
        );
        assert_eq!(result, "click [here](https://example.com) now");
    }

    #[test]
    fn build_slug_table_deduplicates() {
        let index = make_index_with_pages(&[("id-1", "Same Title"), ("id-2", "Same Title")]);
        let table = build_slug_table(&index);
        let slug1 = table.get("id-1").unwrap();
        let slug2 = table.get("id-2").unwrap();
        assert_ne!(slug1, slug2);
        let mut slugs = vec![slug1.as_str(), slug2.as_str()];
        slugs.sort();
        assert_eq!(slugs, vec!["same-title", "same-title-1"]);
    }

    #[test]
    fn export_all_creates_files() {
        let dir = tempfile::tempdir().unwrap();
        let kb_dir = dir.path().join("kb");
        fs::create_dir_all(&kb_dir).unwrap();

        let page_json = serde_json::json!({
            "uid": { "uuid": "page-abc" },
            "pageType": { "title": "My Page" },
            "children": {
                "items": [
                    { "__type": "textSnippet", "string": "hello" }
                ]
            }
        });
        fs::write(
            kb_dir.join("page-abc.lepiter"),
            serde_json::to_string_pretty(&page_json).unwrap(),
        )
        .unwrap();

        let index = crate::KnowledgeBase::open(&kb_dir).unwrap();
        let out_dir = dir.path().join("export");
        let paths = export_all(&index, &out_dir).unwrap();
        assert_eq!(paths.len(), 1);
        assert!(paths[0].exists());
        let content = fs::read_to_string(&paths[0]).unwrap();
        assert!(content.starts_with("---\n"));
        assert!(content.contains("title: \"My Page\""));
        assert!(content.contains("hello"));
    }

    #[test]
    fn export_all_resolves_cross_page_wikilinks() {
        let dir = tempfile::tempdir().unwrap();
        let kb_dir = dir.path().join("kb");
        fs::create_dir_all(&kb_dir).unwrap();

        let page_a = serde_json::json!({
            "uid": { "uuid": "page-a" },
            "pageType": { "title": "Alpha" },
            "children": {
                "items": [
                    { "__type": "textSnippet", "string": "see [[Beta]] for more" }
                ]
            }
        });
        let page_b = serde_json::json!({
            "uid": { "uuid": "page-b" },
            "pageType": { "title": "Beta" },
            "children": {
                "items": [
                    { "__type": "textSnippet", "string": "back to [[Alpha]]" }
                ]
            }
        });
        fs::write(
            kb_dir.join("page-a.lepiter"),
            serde_json::to_string_pretty(&page_a).unwrap(),
        )
        .unwrap();
        fs::write(
            kb_dir.join("page-b.lepiter"),
            serde_json::to_string_pretty(&page_b).unwrap(),
        )
        .unwrap();

        let index = crate::KnowledgeBase::open(&kb_dir).unwrap();
        let out_dir = dir.path().join("export");
        let paths = export_all(&index, &out_dir).unwrap();
        assert_eq!(paths.len(), 2);

        let alpha_content = fs::read_to_string(&out_dir.join("alpha.md")).unwrap();
        assert!(
            alpha_content.contains("[Beta](beta.md)"),
            "alpha should link to beta.md, got: {alpha_content}"
        );

        let beta_content = fs::read_to_string(&out_dir.join("beta.md")).unwrap();
        assert!(
            beta_content.contains("[Alpha](alpha.md)"),
            "beta should link to alpha.md, got: {beta_content}"
        );
    }

    fn make_empty_index() -> KnowledgeBaseIndex {
        KnowledgeBaseIndex {
            root: PathBuf::from("/tmp/empty-kb"),
            pages: HashMap::new(),
            sorted_ids: vec![],
            index_issues: vec![],
            backlinks: HashMap::new(),
        }
    }

    fn make_index_with_pages(pages: &[(&str, &str)]) -> KnowledgeBaseIndex {
        let mut page_map = HashMap::new();
        for (id, title) in pages {
            page_map.insert(
                id.to_string(),
                crate::PageMeta {
                    id: id.to_string(),
                    title: title.to_string(),
                    title_lower: title.to_lowercase(),
                    path: PathBuf::from(format!("/tmp/{id}.lepiter")),
                    updated_at: None,
                    tags: vec![],
                },
            );
        }
        let sorted_ids = crate::compute_sorted_ids(&page_map);
        KnowledgeBaseIndex {
            root: PathBuf::from("/tmp/test-kb"),
            pages: page_map,
            sorted_ids,
            index_issues: vec![],
            backlinks: HashMap::new(),
        }
    }
}
