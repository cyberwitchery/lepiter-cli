use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use lepiter_core::{BrokenLink, KnowledgeBaseIndex, PageId, ParseIssue};

use super::{ArgSpec, open_kb, parse_args};

const SPEC: ArgSpec<'static> = ArgSpec {
    usage: "usage: lepiter-cli info [--detail] [--json] [kb-path]\n\n\
            prints a knowledge base metadata summary.\n\n\
            flags:\n  \
              --detail  show broken links, orphan pages, tag distribution, snippet type breakdown\n  \
              --json    output as json (combinable with --detail)",
    toggles: &["--detail", "--json"],
    valued: &[],
};

pub fn run_info(args: Vec<String>) -> Result<()> {
    let Some(args) = parse_args(args, &SPEC)? else {
        return Ok(());
    };

    print_kb_info(args.kb_path(0), args.has("--detail"), args.has("--json"))
}

pub fn print_kb_info(kb_path: PathBuf, detail: bool, json: bool) -> Result<()> {
    let index = open_kb(&kb_path)?;

    let props_path = kb_path.join("lepiter.properties");
    let props = if props_path.is_file() {
        let bytes = fs::read(&props_path)
            .with_context(|| format!("failed to read {}", props_path.display()))?;
        serde_json::from_slice::<serde_json::Value>(&bytes).ok()
    } else {
        None
    };

    let db_name = props
        .as_ref()
        .and_then(|v| v.get("databaseName"))
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");
    let db_uuid = props
        .as_ref()
        .and_then(|v| v.get("uuid"))
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");
    let schema = props
        .as_ref()
        .and_then(|v| v.get("schema"))
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");
    let table_of_contents = props
        .as_ref()
        .and_then(|v| v.get("tableOfContents"))
        .and_then(|v| v.as_str())
        .unwrap_or("<none>");

    let mut min_updated = None;
    let mut max_updated = None;
    let mut tag_counts: HashMap<String, usize> = HashMap::new();
    for page in index.pages.values() {
        if let Some(ts) = page.updated_at {
            min_updated = Some(min_updated.map_or(ts, |x| if ts < x { ts } else { x }));
            max_updated = Some(max_updated.map_or(ts, |x| if ts > x { ts } else { x }));
        }
        for tag in &page.tags {
            *tag_counts.entry(tag.clone()).or_insert(0) += 1;
        }
    }

    let detailed = if detail {
        Some(compute_detailed_info(&index, table_of_contents))
    } else {
        None
    };

    let info = KbInfo {
        path: kb_path.display().to_string(),
        name: db_name.to_string(),
        uuid: db_uuid.to_string(),
        schema: schema.to_string(),
        table_of_contents: table_of_contents.to_string(),
        page_count: index.pages.len(),
        index_issues: &index.index_issues,
        min_updated,
        max_updated,
        tag_counts: &tag_counts,
        detailed: detailed.as_ref(),
        index: &index,
    };

    if json {
        print_kb_info_json(&info);
    } else {
        print_kb_info_text(&info);
    }

    Ok(())
}

struct DetailedInfo {
    broken_links: Vec<BrokenLink>,
    orphan_ids: Vec<PageId>,
    snippet_types: Vec<(String, usize)>,
}

struct KbInfo<'a> {
    path: String,
    name: String,
    uuid: String,
    schema: String,
    table_of_contents: String,
    page_count: usize,
    index_issues: &'a [ParseIssue],
    min_updated: Option<chrono::DateTime<chrono::FixedOffset>>,
    max_updated: Option<chrono::DateTime<chrono::FixedOffset>>,
    tag_counts: &'a HashMap<String, usize>,
    detailed: Option<&'a DetailedInfo>,
    index: &'a KnowledgeBaseIndex,
}

fn compute_detailed_info(index: &KnowledgeBaseIndex, toc_page_id: &str) -> DetailedInfo {
    use lepiter_core::collect_node_types_in_file;

    let analysis = index.analyze_all();
    let orphan_ids = index.orphan_ids(&analysis.linked_pages, toc_page_id);

    // Log page-load errors.
    for err in &analysis.load_errors {
        eprintln!(
            "warning: failed to load page {} ({}): {}",
            err.page_id, err.title, err.error
        );
    }

    // Snippet type counting from raw JSON (independent of link analysis).
    let mut snippet_totals: HashMap<String, usize> = HashMap::new();
    for id in &index.sorted_ids {
        let meta = match index.pages.get(id) {
            Some(m) => m,
            None => continue,
        };
        if let Ok(types) = collect_node_types_in_file(&meta.path) {
            for (typ, count) in types {
                if is_snippet_type(&typ) {
                    *snippet_totals.entry(typ).or_insert(0) += count;
                }
            }
        }
    }

    let mut snippet_types: Vec<(String, usize)> = snippet_totals.into_iter().collect();
    snippet_types.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    DetailedInfo {
        broken_links: analysis.broken_links,
        orphan_ids,
        snippet_types,
    }
}

/// Returns true if the type string looks like a snippet type (as opposed to
/// a container type like "page" or "snippets").
fn is_snippet_type(typ: &str) -> bool {
    typ.ends_with("Snippet") || typ.ends_with("Rewrite") || typ.ends_with("snippet")
}

fn print_kb_info_text(info: &KbInfo<'_>) {
    println!("Knowledge Base");
    println!("  path: {}", info.path);
    println!("  name: {}", info.name);
    println!("  uuid: {}", info.uuid);
    println!("  schema: {}", info.schema);
    println!("  table_of_contents: {}", info.table_of_contents);
    println!("  pages: {}", info.page_count);
    println!("  unique_tags: {}", info.tag_counts.len());
    println!("  index_issues: {}", info.index_issues.len());
    match (info.min_updated, info.max_updated) {
        (Some(min), Some(max)) => {
            println!(
                "  updated_range: {} .. {}",
                min.to_rfc3339(),
                max.to_rfc3339()
            );
        }
        _ => println!("  updated_range: <none>"),
    }

    if !info.index_issues.is_empty() {
        println!("\nIndex Issues:");
        for issue in info.index_issues {
            println!("  - {}: {}", issue.path.display(), issue.message);
        }
    }

    if let Some(detail) = info.detailed {
        println!("\nBroken Links ({}):", detail.broken_links.len());
        if detail.broken_links.is_empty() {
            println!("  (none)");
        } else {
            for link in &detail.broken_links {
                println!("  - {} -> {}", link.source_title, link.target);
            }
        }

        println!("\nOrphan Pages ({}):", detail.orphan_ids.len());
        if detail.orphan_ids.is_empty() {
            println!("  (none)");
        } else {
            for id in &detail.orphan_ids {
                let title = info
                    .index
                    .pages
                    .get(id)
                    .map(|m| m.title.as_str())
                    .unwrap_or(id);
                println!("  - {title}");
            }
        }

        println!("\nTag Distribution ({}):", info.tag_counts.len());
        if info.tag_counts.is_empty() {
            println!("  (none)");
        } else {
            let mut sorted: Vec<_> = info.tag_counts.iter().collect();
            sorted.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            for (tag, count) in sorted {
                println!("  {count:>4}  {tag}");
            }
        }

        println!("\nSnippet Types ({}):", detail.snippet_types.len());
        if detail.snippet_types.is_empty() {
            println!("  (none)");
        } else {
            for (typ, count) in &detail.snippet_types {
                println!("  {count:>4}  {typ}");
            }
        }
    }
}

fn print_kb_info_json(info: &KbInfo<'_>) {
    let mut obj = serde_json::Map::new();
    obj.insert("path".into(), serde_json::json!(info.path));
    obj.insert("name".into(), serde_json::json!(info.name));
    obj.insert("uuid".into(), serde_json::json!(info.uuid));
    obj.insert("schema".into(), serde_json::json!(info.schema));
    obj.insert(
        "table_of_contents".into(),
        serde_json::json!(info.table_of_contents),
    );
    obj.insert("pages".into(), serde_json::json!(info.page_count));
    obj.insert(
        "unique_tags".into(),
        serde_json::json!(info.tag_counts.len()),
    );
    obj.insert(
        "index_issues".into(),
        serde_json::json!(info.index_issues.len()),
    );
    let updated_range = match (info.min_updated, info.max_updated) {
        (Some(min), Some(max)) => serde_json::json!({
            "min": min.to_rfc3339(),
            "max": max.to_rfc3339(),
        }),
        _ => serde_json::Value::Null,
    };
    obj.insert("updated_range".into(), updated_range);

    if let Some(detail) = info.detailed {
        let broken: Vec<serde_json::Value> = detail
            .broken_links
            .iter()
            .map(|link| {
                serde_json::json!({
                    "source_title": link.source_title,
                    "source_id": link.source_id,
                    "target": link.target,
                })
            })
            .collect();
        obj.insert("broken_links".into(), serde_json::Value::Array(broken));

        let orphans: Vec<serde_json::Value> = detail
            .orphan_ids
            .iter()
            .map(|id| {
                let title = info
                    .index
                    .pages
                    .get(id)
                    .map(|m| m.title.as_str())
                    .unwrap_or(id);
                serde_json::json!({ "id": id, "title": title })
            })
            .collect();
        obj.insert("orphan_pages".into(), serde_json::Value::Array(orphans));

        let tags: serde_json::Map<String, serde_json::Value> = info
            .tag_counts
            .iter()
            .map(|(tag, count)| (tag.clone(), serde_json::json!(count)))
            .collect();
        obj.insert("tag_distribution".into(), serde_json::Value::Object(tags));

        let snippets: serde_json::Map<String, serde_json::Value> = detail
            .snippet_types
            .iter()
            .map(|(typ, count)| (typ.clone(), serde_json::json!(count)))
            .collect();
        obj.insert("snippet_types".into(), serde_json::Value::Object(snippets));
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::Value::Object(obj)).unwrap()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- is_snippet_type ---

    #[test]
    fn is_snippet_type_accepts_text_snippet() {
        assert!(is_snippet_type("textSnippet"));
    }

    #[test]
    fn is_snippet_type_accepts_pharo_snippet() {
        assert!(is_snippet_type("pharoSnippet"));
    }

    #[test]
    fn is_snippet_type_accepts_lowercase_snippet() {
        assert!(is_snippet_type("codeSnippet"));
        assert!(is_snippet_type("pythonSnippet"));
    }

    #[test]
    fn is_snippet_type_accepts_rewrite() {
        assert!(is_snippet_type("pharoRewrite"));
        assert!(is_snippet_type("someRewrite"));
    }

    #[test]
    fn is_snippet_type_rejects_container_types() {
        assert!(!is_snippet_type("page"));
        assert!(!is_snippet_type("snippets"));
        assert!(!is_snippet_type("children"));
    }

    // --- compute_detailed_info ---

    fn make_test_kb(
        pages: &[(&str, &str, &str)],
    ) -> (std::path::PathBuf, lepiter_core::KnowledgeBaseIndex) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("lepiter-tui-test-{ts}"));
        std::fs::create_dir_all(&dir).unwrap();
        for (id, title, body) in pages {
            let content = serde_json::json!({
                "uid": {"uuid": id},
                "pageType": {"title": title},
                "tags": [],
                "children": {"items": [
                    {"__type": "textSnippet", "string": body}
                ]}
            });
            let file_path = dir.join(format!("{id}.lepiter"));
            std::fs::write(&file_path, serde_json::to_vec(&content).unwrap()).unwrap();
        }
        let index = lepiter_core::KnowledgeBase::open(&dir).unwrap();
        (dir, index)
    }

    #[test]
    fn compute_detailed_info_counts_snippet_types() {
        let (dir, index) = make_test_kb(&[("p1", "Page One", "hello world")]);
        let info = compute_detailed_info(&index, "");
        // The textSnippet from the JSON should appear in snippet_types.
        assert!(info.snippet_types.iter().any(|(t, _)| t == "textSnippet"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compute_detailed_info_detects_orphan_pages() {
        // Two pages, neither links to the other — both should be orphans.
        let (dir, index) =
            make_test_kb(&[("p1", "Page One", "hello"), ("p2", "Page Two", "world")]);
        let info = compute_detailed_info(&index, "");
        assert_eq!(info.orphan_ids.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compute_detailed_info_excludes_toc_from_orphans() {
        // Two pages, neither links to the other. "p1" is the TOC page.
        let (dir, index) = make_test_kb(&[
            ("p1", "Table of Contents", "hello"),
            ("p2", "Page Two", "world"),
        ]);
        let info = compute_detailed_info(&index, "p1");
        // p1 excluded as TOC, only p2 should be orphan.
        assert_eq!(info.orphan_ids.len(), 1);
        assert_eq!(info.orphan_ids[0], "p2");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compute_detailed_info_linked_page_not_orphan() {
        // p1 links to p2 via inline markdown link.
        let (dir, index) = make_test_kb(&[
            ("p1", "Page One", "see [link](page:p2) for more"),
            ("p2", "Page Two", "target page"),
        ]);
        let info = compute_detailed_info(&index, "");
        // p2 is linked to by p1, so only p1 should be orphan.
        assert_eq!(info.orphan_ids.len(), 1);
        assert_eq!(info.orphan_ids[0], "p1");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compute_detailed_info_detects_broken_links() {
        // p1 links to a nonexistent page.
        let (dir, index) = make_test_kb(&[("p1", "Page One", "see [link](page:nonexistent) here")]);
        let info = compute_detailed_info(&index, "");
        assert_eq!(info.broken_links.len(), 1);
        assert_eq!(info.broken_links[0].target, "page:nonexistent");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compute_detailed_info_empty_kb() {
        let (dir, index) = make_test_kb(&[]);
        let info = compute_detailed_info(&index, "");
        assert!(info.broken_links.is_empty());
        assert!(info.orphan_ids.is_empty());
        assert!(info.snippet_types.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
