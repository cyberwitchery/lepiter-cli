use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use lepiter_core::{
    BlockEscaping, KnowledgeBaseIndex, LinkKind, LinkTargetKind, Node, Page,
    render_nodes_to_text_with,
};

use super::{ArgSpec, open_kb, parse_args};

const SPEC: ArgSpec<'static> = ArgSpec {
    usage: "usage: lepiter-cli export <output-dir> [kb-path]\n\n\
            bulk-exports all pages to a directory of markdown files with\n\
            yaml frontmatter and rewritten internal links.",
    toggles: &[],
    valued: &[],
};

pub fn run_export(args: Vec<String>) -> Result<()> {
    let Some(args) = parse_args(args, &SPEC)? else {
        return Ok(());
    };

    let Some(out_dir) = args.positional(0) else {
        bail!("missing required argument: <output-dir>");
    };
    let out_dir = PathBuf::from(out_dir);
    let index = open_kb(&args.kb_path(1))?;

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
    let mut assigned: HashSet<String> = HashSet::new();
    let mut base_counts: HashMap<String, usize> = HashMap::new();

    for meta in index.sorted_pages() {
        let base = slugify(&meta.title);
        let count = base_counts.entry(base.clone()).or_insert(0);
        *count += 1;
        let mut slug = if *count == 1 {
            base.clone()
        } else {
            format!("{base}-{count}")
        };
        while !assigned.insert(slug.clone()) {
            *count += 1;
            slug = format!("{base}-{count}");
        }
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
    let resolve = |target: &str| -> Option<String> {
        match index.classify_link_target(target) {
            LinkTargetKind::InternalPage(id) => slug_map.get(&id).map(|slug| format!("{slug}.md")),
            _ => None,
        }
    };
    render_nodes_to_text_with(
        nodes,
        &mut export_link_rewriter(resolve),
        BlockEscaping::Escape,
    )
}

/// The export link policy, layered over the shared [`lepiter_core`] scanner:
/// `[[wikilink]]` becomes a markdown link only when its target resolves to an
/// exported page (otherwise left verbatim), while `[label](target)` always
/// keeps markdown syntax with the target resolved when possible.
fn export_link_rewriter(
    resolve: impl Fn(&str) -> Option<String>,
) -> impl FnMut(LinkKind, &str) -> Option<String> {
    move |kind, target| match kind {
        LinkKind::Wiki => resolve(target),
        LinkKind::Markdown => Some(resolve(target).unwrap_or_else(|| target.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lepiter_core::{KnowledgeBase, rewrite_inline_links};

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
        let resolve = |target: &str| -> Option<String> {
            if target == "Other Page" {
                Some("other-page.md".to_string())
            } else {
                None
            }
        };
        assert_eq!(
            rewrite_inline_links("see [[Other Page]] here", export_link_rewriter(resolve)),
            "see [Other Page](other-page.md) here"
        );
    }

    #[test]
    fn rewrite_wikilinks_unresolved() {
        let resolve = |_: &str| -> Option<String> { None };
        assert_eq!(
            rewrite_inline_links("see [[Unknown]] here", export_link_rewriter(resolve)),
            "see [[Unknown]] here"
        );
    }

    #[test]
    fn rewrite_markdown_links() {
        let resolve = |target: &str| -> Option<String> {
            if target == "page:abc" {
                Some("alpha.md".to_string())
            } else {
                None
            }
        };
        assert_eq!(
            rewrite_inline_links("see [link](page:abc) done", export_link_rewriter(resolve)),
            "see [link](alpha.md) done"
        );
    }

    #[test]
    fn rewrite_preserves_external_links() {
        let resolve = |_: &str| -> Option<String> { None };
        assert_eq!(
            rewrite_inline_links("[docs](https://example.com)", export_link_rewriter(resolve)),
            "[docs](https://example.com)"
        );
    }

    #[test]
    fn rewrite_mixed_links() {
        let resolve = |target: &str| -> Option<String> {
            match target {
                "Other" => Some("other.md".to_string()),
                "page:p1" => Some("alpha.md".to_string()),
                _ => None,
            }
        };
        assert_eq!(
            rewrite_inline_links(
                "see [[Other]] and [link](page:p1) and [[Missing]]",
                export_link_rewriter(resolve)
            ),
            "see [Other](other.md) and [link](alpha.md) and [[Missing]]"
        );
    }

    #[test]
    fn rewrite_unicode_text() {
        let resolve = |_: &str| -> Option<String> { None };
        assert_eq!(
            rewrite_inline_links("日本語テキスト [[リンク]]", export_link_rewriter(resolve)),
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

    fn make_test_kb(pages: &[(&str, &str)]) -> (std::path::PathBuf, KnowledgeBaseIndex) {
        let dir = std::env::temp_dir().join(format!(
            "lepiter-export-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        for (id, title) in pages {
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
        (dir, index)
    }

    #[test]
    fn build_slug_map_handles_duplicates() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha"), ("p2", "Alpha"), ("p3", "Beta")]);
        let slugs = build_slug_map(&index);

        let mut values: Vec<&str> = slugs.values().map(|s| s.as_str()).collect();
        values.sort();
        assert_eq!(values, vec!["alpha", "alpha-2", "beta"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_slug_map_cross_slug_collision() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha"), ("p2", "Alpha"), ("p3", "Alpha-2")]);
        let slugs = build_slug_map(&index);

        let mut values: Vec<&str> = slugs.values().map(|s| s.as_str()).collect();
        values.sort();
        // all three must get distinct slugs — no silent overwrites
        assert_eq!(values.len(), 3);
        assert!(
            values[0] != values[1] && values[1] != values[2],
            "duplicate slug in {:?}",
            values
        );
        assert!(values.contains(&"alpha"));
        assert!(values.contains(&"alpha-2"));
        // "Alpha-2" base collides with the suffix-assigned "alpha-2", so it
        // gets bumped to "alpha-2-2"
        assert!(values.contains(&"alpha-2-2"));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
