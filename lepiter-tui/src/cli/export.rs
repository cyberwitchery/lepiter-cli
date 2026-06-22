use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use lepiter_core::{KnowledgeBase, KnowledgeBaseIndex, LinkTargetKind, Node, Page, normalize_text};

pub fn run_export(args: Vec<String>) -> Result<()> {
    let mut positional = Vec::new();

    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => {
                eprintln!(
                    "usage: lepiter-cli export <output-dir> [kb-path]\n\n\
                     bulk-exports all pages to a directory of markdown files with\n\
                     yaml frontmatter and rewritten internal links."
                );
                return Ok(());
            }
            _ => positional.push(arg),
        }
    }

    if positional.is_empty() {
        bail!("missing required argument: <output-dir>");
    }

    let out_dir = PathBuf::from(&positional[0]);
    let kb_path = positional
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./lepiter"));

    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;

    let slug_map = build_slug_map(&index);

    fs::create_dir_all(&out_dir)
        .with_context(|| format!("failed to create output directory {}", out_dir.display()))?;

    let mut exported = 0usize;
    let mut errors = 0usize;

    for meta in index.sorted_pages() {
        let page = match index.load_page(&meta.id) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("warning: skipping page {}: {e:#}", meta.id);
                errors += 1;
                continue;
            }
        };

        let slug = &slug_map[&meta.id];
        let path = out_dir.join(format!("{slug}.md"));

        let frontmatter = build_frontmatter(&page);
        let body = render_with_rewritten_links(&page.content, &index, &slug_map);

        fs::write(&path, format!("{frontmatter}{body}"))
            .with_context(|| format!("failed to write {}", path.display()))?;
        exported += 1;
    }

    eprintln!("exported {exported} pages to {}", out_dir.display());
    if errors > 0 {
        eprintln!("{errors} pages skipped due to errors");
    }

    Ok(())
}

fn build_slug_map(index: &KnowledgeBaseIndex) -> HashMap<String, String> {
    let mut slugs: HashMap<String, String> = HashMap::new();
    let mut used: HashMap<String, usize> = HashMap::new();

    for meta in index.sorted_pages() {
        let base = slugify(&meta.title);
        let count = used.entry(base.clone()).or_insert(0);
        *count += 1;
        let slug = if *count == 1 {
            base
        } else {
            format!("{base}-{count}")
        };
        slugs.insert(meta.id.clone(), slug);
    }

    slugs
}

fn slugify(title: &str) -> String {
    let mut result = String::with_capacity(title.len());
    let mut prev_hyphen = true; // treat start as hyphen to trim leading

    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            result.push(c.to_ascii_lowercase());
            prev_hyphen = false;
        } else if !prev_hyphen {
            result.push('-');
            prev_hyphen = true;
        }
    }

    let trimmed = result.trim_end_matches('-');
    if trimmed.is_empty() {
        "untitled".to_string()
    } else {
        trimmed.to_string()
    }
}

fn build_frontmatter(page: &Page) -> String {
    let mut out = String::from("---\n");

    // quote the title for yaml safety
    let escaped_title = page.title.replace('\\', "\\\\").replace('"', "\\\"");
    out.push_str(&format!("title: \"{escaped_title}\"\n"));
    out.push_str(&format!("id: {}\n", page.id));

    if !page.tags.is_empty() {
        let quoted: Vec<String> = page
            .tags
            .iter()
            .map(|t| format!("\"{}\"", t.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        out.push_str(&format!("tags: [{}]\n", quoted.join(", ")));
    }

    if let Some(updated) = &page.updated_at {
        out.push_str(&format!("updated_at: {}\n", updated.to_rfc3339()));
    }

    out.push_str("---\n\n");
    out
}

fn render_with_rewritten_links(
    nodes: &[Node],
    index: &KnowledgeBaseIndex,
    slug_map: &HashMap<String, String>,
) -> String {
    let rewrite = |target: &str| -> Option<String> {
        match index.classify_link_target(target) {
            LinkTargetKind::InternalPage(id) => slug_map.get(&id).map(|slug| format!("{slug}.md")),
            _ => None,
        }
    };

    let mut out = String::new();
    render_nodes_rewritten(nodes, &rewrite, &mut out);
    out
}

fn render_nodes_rewritten(
    nodes: &[Node],
    rewrite: &impl Fn(&str) -> Option<String>,
    out: &mut String,
) {
    for node in nodes {
        match node {
            Node::Heading { level, text } => {
                out.push_str(&"#".repeat((*level).max(1) as usize));
                out.push(' ');
                out.push_str(&rewrite_inline_links(text, rewrite));
                out.push_str("\n\n");
            }
            Node::Paragraph { text } => {
                out.push_str(&rewrite_inline_links(text, rewrite));
                out.push_str("\n\n");
            }
            Node::Text { text } => {
                out.push_str(&rewrite_inline_links(text, rewrite));
                out.push('\n');
            }
            Node::List { items } => {
                for item in items {
                    let mut item_out = String::new();
                    render_nodes_rewritten(item, rewrite, &mut item_out);
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
                let rewritten = rewrite(url).unwrap_or_else(|| url.clone());
                out.push_str(&format!("[{text}]({rewritten})\n\n"));
            }
            Node::Quote { text } => {
                out.push_str(&format!("> {}\n\n", rewrite_inline_links(text, rewrite)));
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
                for line in normalize_text(search).lines() {
                    out.push('-');
                    out.push_str(line);
                    out.push('\n');
                }
                for line in normalize_text(replace).lines() {
                    out.push('+');
                    out.push_str(line);
                    out.push('\n');
                }
                out.push_str("```\n\n");
            }
            Node::Unknown { typ, .. } => {
                out.push_str(&format!("[[unknown: {typ}]]\n\n"));
            }
        }
    }
}

/// Rewrites `[[wikilink]]` and `[label](target)` inline links using the
/// provided rewriter. Wikilinks that resolve to an internal page are
/// converted to standard markdown links; unresolvable links are left as-is.
fn rewrite_inline_links(text: &str, rewrite: &impl Fn(&str) -> Option<String>) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;

    while i < bytes.len() {
        // [[wikilink]]
        if i + 1 < bytes.len() && bytes[i] == b'[' && bytes[i + 1] == b'[' {
            let start = i + 2;
            if let Some(end) = find_closing_double_bracket(bytes, start) {
                let target = text[start..end].trim();
                if !target.is_empty() {
                    if let Some(slug) = rewrite(target) {
                        out.push_str(&format!("[{target}]({slug})"));
                    } else {
                        // keep original [[...]] syntax
                        out.push_str(&text[i..end + 2]);
                    }
                    i = end + 2;
                    continue;
                }
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
                    let label = &text[label_start..label_end];
                    let target = text[target_start..target_end].trim();
                    if !target.is_empty() {
                        let rewritten = rewrite(target).unwrap_or_else(|| target.to_string());
                        out.push_str(&format!("[{label}]({rewritten})"));
                        i = target_end + 1;
                        continue;
                    }
                }
            }
        }

        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

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

    #[test]
    fn slugify_basic_title() {
        assert_eq!(slugify("My Cool Page"), "my-cool-page");
    }

    #[test]
    fn slugify_collapses_special_chars() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("foo---bar"), "foo-bar");
    }

    #[test]
    fn slugify_trims_hyphens() {
        assert_eq!(slugify("--hello--"), "hello");
        assert_eq!(slugify("  spaces  "), "spaces");
    }

    #[test]
    fn slugify_empty_falls_back() {
        assert_eq!(slugify(""), "untitled");
        assert_eq!(slugify("---"), "untitled");
        assert_eq!(slugify("!@#"), "untitled");
    }

    #[test]
    fn slugify_preserves_numbers() {
        assert_eq!(slugify("Page 42"), "page-42");
    }

    #[test]
    fn rewrite_wikilinks() {
        let rewrite = |target: &str| -> Option<String> {
            if target == "Other Page" {
                Some("other-page.md".to_string())
            } else {
                None
            }
        };
        assert_eq!(
            rewrite_inline_links("see [[Other Page]] here", &rewrite),
            "see [Other Page](other-page.md) here"
        );
    }

    #[test]
    fn rewrite_wikilinks_unresolved() {
        let rewrite = |_: &str| -> Option<String> { None };
        assert_eq!(
            rewrite_inline_links("see [[Unknown]] here", &rewrite),
            "see [[Unknown]] here"
        );
    }

    #[test]
    fn rewrite_markdown_links() {
        let rewrite = |target: &str| -> Option<String> {
            if target == "page:abc" {
                Some("alpha.md".to_string())
            } else {
                None
            }
        };
        assert_eq!(
            rewrite_inline_links("see [link](page:abc) done", &rewrite),
            "see [link](alpha.md) done"
        );
    }

    #[test]
    fn rewrite_preserves_external_links() {
        let rewrite = |_: &str| -> Option<String> { None };
        assert_eq!(
            rewrite_inline_links("[docs](https://example.com)", &rewrite),
            "[docs](https://example.com)"
        );
    }

    #[test]
    fn rewrite_mixed_links() {
        let rewrite = |target: &str| -> Option<String> {
            match target {
                "Other" => Some("other.md".to_string()),
                "page:p1" => Some("alpha.md".to_string()),
                _ => None,
            }
        };
        assert_eq!(
            rewrite_inline_links(
                "see [[Other]] and [link](page:p1) and [[Missing]]",
                &rewrite
            ),
            "see [Other](other.md) and [link](alpha.md) and [[Missing]]"
        );
    }

    #[test]
    fn rewrite_unicode_text() {
        let rewrite = |_: &str| -> Option<String> { None };
        assert_eq!(
            rewrite_inline_links("日本語テキスト [[リンク]]", &rewrite),
            "日本語テキスト [[リンク]]"
        );
    }

    #[test]
    fn build_frontmatter_basic() {
        let page = Page {
            id: "abc-123".to_string(),
            title: "Test Page".to_string(),
            updated_at: None,
            tags: vec!["rust".to_string(), "cli".to_string()],
            content: Vec::new(),
        };
        let fm = build_frontmatter(&page);
        assert!(fm.starts_with("---\n"));
        assert!(fm.ends_with("---\n\n"));
        assert!(fm.contains("title: \"Test Page\""));
        assert!(fm.contains("id: abc-123"));
        assert!(fm.contains("tags: [\"rust\", \"cli\"]"));
    }

    #[test]
    fn build_frontmatter_escapes_quotes() {
        let page = Page {
            id: "p1".to_string(),
            title: "Page with \"quotes\"".to_string(),
            updated_at: None,
            tags: Vec::new(),
            content: Vec::new(),
        };
        let fm = build_frontmatter(&page);
        assert!(fm.contains("title: \"Page with \\\"quotes\\\"\""));
    }

    #[test]
    fn build_frontmatter_omits_empty_tags() {
        let page = Page {
            id: "p1".to_string(),
            title: "No Tags".to_string(),
            updated_at: None,
            tags: Vec::new(),
            content: Vec::new(),
        };
        let fm = build_frontmatter(&page);
        assert!(!fm.contains("tags:"));
    }

    #[test]
    fn build_slug_map_handles_duplicates() {
        use lepiter_core::PageMeta;

        let dir = std::env::temp_dir().join(format!(
            "lepiter-export-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        for (id, title) in &[("p1", "Alpha"), ("p2", "Alpha"), ("p3", "Beta")] {
            let content = serde_json::json!({
                "uid": {"uuid": id},
                "pageType": {"title": title},
                "children": {"items": []}
            });
            std::fs::write(
                dir.join(format!("{id}.lepiter")),
                serde_json::to_vec(&content).unwrap(),
            )
            .unwrap();
        }

        let index = KnowledgeBase::open(&dir).unwrap();
        let slugs = build_slug_map(&index);

        assert_eq!(slugs["p1"], "alpha");
        assert_eq!(slugs["p2"], "alpha-2");
        assert_eq!(slugs["p3"], "beta");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
