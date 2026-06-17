use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use lepiter_core::{
    BrokenLink, DuplicateTitle, KnowledgeBase, KnowledgeBaseIndex, MissingAttachment, ParseIssue,
};

pub fn run_check(args: Vec<String>) -> Result<()> {
    let mut json = false;
    let mut positional = Vec::new();

    for arg in &args {
        match arg.as_str() {
            "--json" => json = true,
            _ if arg.starts_with('-') => {
                eprintln!("unknown flag: {arg}");
                std::process::exit(2);
            }
            _ => positional.push(arg.clone()),
        }
    }

    let kb_path = positional
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./lepiter"));
    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;

    // Read table-of-contents page id from lepiter.properties (excluded from orphans).
    let props_path = kb_path.join("lepiter.properties");
    let toc_page_id = if props_path.is_file() {
        fs::read(&props_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|v| {
                v.get("tableOfContents")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .unwrap_or_default()
    } else {
        String::new()
    };

    // Compute broken links, orphan pages, and missing attachments in a single pass.
    let analysis = index.analyze_all();
    let orphan_ids = index.orphan_ids(&analysis.linked_pages, &toc_page_id);
    let duplicate_titles = index.find_duplicate_titles();

    // Surface index issues and page-load errors on stderr.
    for issue in &index.index_issues {
        eprintln!(
            "warning: index issue at {}: {}",
            issue.path.display(),
            issue.message
        );
    }
    for err in &analysis.load_errors {
        eprintln!(
            "warning: failed to load page {} ({}): {}",
            err.page_id, err.title, err.error
        );
    }

    let has_issues = !analysis.broken_links.is_empty()
        || !orphan_ids.is_empty()
        || !analysis.load_errors.is_empty()
        || !index.index_issues.is_empty()
        || !duplicate_titles.is_empty()
        || !analysis.missing_attachments.is_empty();

    let mut out = std::io::stdout().lock();
    if json {
        write_check_json(
            &mut out,
            &index,
            &analysis.broken_links,
            &orphan_ids,
            &analysis.load_errors,
            &index.index_issues,
            &duplicate_titles,
            &analysis.missing_attachments,
        );
    } else {
        write_check_text(
            &mut out,
            &index,
            &analysis.broken_links,
            &orphan_ids,
            &analysis.load_errors,
            &index.index_issues,
            &duplicate_titles,
            &analysis.missing_attachments,
        );
    }

    if has_issues {
        std::process::exit(1);
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_check_text(
    out: &mut impl Write,
    index: &KnowledgeBaseIndex,
    broken_links: &[BrokenLink],
    orphan_ids: &[String],
    load_errors: &[lepiter_core::PageLoadError],
    index_issues: &[ParseIssue],
    duplicate_titles: &[DuplicateTitle],
    missing_attachments: &[MissingAttachment],
) {
    let _ = writeln!(out, "Knowledge Base Check");
    let _ = writeln!(out, "  broken_links: {}", broken_links.len());
    let _ = writeln!(out, "  orphan_pages: {}", orphan_ids.len());
    let _ = writeln!(out, "  duplicate_titles: {}", duplicate_titles.len());
    let _ = writeln!(out, "  missing_attachments: {}", missing_attachments.len());
    let _ = writeln!(out, "  load_errors: {}", load_errors.len());
    let _ = writeln!(out, "  index_issues: {}", index_issues.len());

    let _ = writeln!(out, "\nBroken Links ({}):", broken_links.len());
    if broken_links.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for link in broken_links {
            let _ = writeln!(out, "  {} -> {}", link.source_title, link.target);
        }
    }

    let _ = writeln!(out, "\nOrphan Pages ({}):", orphan_ids.len());
    if orphan_ids.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for id in orphan_ids {
            let title = index.pages.get(id).map(|m| m.title.as_str()).unwrap_or(id);
            let _ = writeln!(out, "  {title}");
        }
    }

    let _ = writeln!(out, "\nDuplicate Titles ({}):", duplicate_titles.len());
    if duplicate_titles.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for dup in duplicate_titles {
            let _ = writeln!(out, "  \"{}\" ({} pages)", dup.title, dup.page_ids.len());
            for id in &dup.page_ids {
                let _ = writeln!(out, "    - {id}");
            }
        }
    }

    let _ = writeln!(
        out,
        "\nMissing Attachments ({}):",
        missing_attachments.len()
    );
    if missing_attachments.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for att in missing_attachments {
            let _ = writeln!(
                out,
                "  {} -> {}",
                att.source_title,
                att.resolved_path.display()
            );
        }
    }

    if !load_errors.is_empty() {
        let _ = writeln!(out, "\nLoad Errors ({}):", load_errors.len());
        for err in load_errors {
            let _ = writeln!(out, "  {} ({}): {}", err.title, err.page_id, err.error);
        }
    }

    if !index_issues.is_empty() {
        let _ = writeln!(out, "\nIndex Issues ({}):", index_issues.len());
        for issue in index_issues {
            let _ = writeln!(out, "  {}: {}", issue.path.display(), issue.message);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn write_check_json(
    out: &mut impl Write,
    index: &KnowledgeBaseIndex,
    broken_links: &[BrokenLink],
    orphan_ids: &[String],
    load_errors: &[lepiter_core::PageLoadError],
    index_issues: &[ParseIssue],
    duplicate_titles: &[DuplicateTitle],
    missing_attachments: &[MissingAttachment],
) {
    let broken: Vec<serde_json::Value> = broken_links
        .iter()
        .map(|link| {
            serde_json::json!({
                "source_title": link.source_title,
                "source_id": link.source_id,
                "target": link.target,
            })
        })
        .collect();

    let orphans: Vec<serde_json::Value> = orphan_ids
        .iter()
        .map(|id| {
            let title = index.pages.get(id).map(|m| m.title.as_str()).unwrap_or(id);
            serde_json::json!({ "id": id, "title": title })
        })
        .collect();

    let errors: Vec<serde_json::Value> = load_errors
        .iter()
        .map(|err| {
            serde_json::json!({
                "page_id": err.page_id,
                "title": err.title,
                "error": err.error,
            })
        })
        .collect();

    let dupes: Vec<serde_json::Value> = duplicate_titles
        .iter()
        .map(|dup| {
            serde_json::json!({
                "title": dup.title,
                "page_ids": dup.page_ids,
            })
        })
        .collect();

    let attachments: Vec<serde_json::Value> = missing_attachments
        .iter()
        .map(|att| {
            serde_json::json!({
                "source_title": att.source_title,
                "source_id": att.source_id,
                "target": att.target,
                "resolved_path": att.resolved_path.display().to_string(),
            })
        })
        .collect();

    let idx_issues: Vec<serde_json::Value> = index_issues
        .iter()
        .map(|issue| {
            serde_json::json!({
                "path": issue.path.display().to_string(),
                "message": issue.message,
            })
        })
        .collect();

    let obj = serde_json::json!({
        "broken_links": broken,
        "orphan_pages": orphans,
        "duplicate_titles": dupes,
        "missing_attachments": attachments,
        "load_errors": errors,
        "index_issues": idx_issues,
    });
    let _ = writeln!(out, "{}", serde_json::to_string_pretty(&obj).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;
    use lepiter_core::PageLoadError;

    fn make_test_kb(pages: &[(&str, &str, &str)]) -> (std::path::PathBuf, KnowledgeBaseIndex) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("lepiter-check-test-{ts}"));
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
        let index = KnowledgeBase::open(&dir).unwrap();
        (dir, index)
    }

    fn output_string(f: impl FnOnce(&mut Vec<u8>)) -> String {
        let mut buf = Vec::new();
        f(&mut buf);
        String::from_utf8(buf).unwrap()
    }

    // --- write_check_text: clean KB ---

    #[test]
    fn check_text_clean_kb_shows_zero_counts() {
        let (dir, index) = make_test_kb(&[]);
        let out = output_string(|buf| {
            write_check_text(buf, &index, &[], &[], &[], &[], &[], &[]);
        });
        assert!(out.contains("broken_links: 0"));
        assert!(out.contains("orphan_pages: 0"));
        assert!(out.contains("duplicate_titles: 0"));
        assert!(out.contains("missing_attachments: 0"));
        assert!(out.contains("load_errors: 0"));
        assert!(out.contains("index_issues: 0"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn check_text_clean_kb_shows_none_sections() {
        let (dir, index) = make_test_kb(&[]);
        let out = output_string(|buf| {
            write_check_text(buf, &index, &[], &[], &[], &[], &[], &[]);
        });
        assert!(out.contains("Broken Links (0):"));
        assert!(out.contains("Orphan Pages (0):"));
        assert!(out.contains("Duplicate Titles (0):"));
        assert!(out.contains("Missing Attachments (0):"));
        // Load errors and index issues sections are omitted when empty.
        assert!(!out.contains("Load Errors"));
        assert!(!out.contains("Index Issues"));
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- write_check_text: broken links ---

    #[test]
    fn check_text_broken_links_listed() {
        let (dir, index) = make_test_kb(&[("p1", "Page One", "see [link](page:gone)")]);
        let analysis = index.analyze_all();
        let out = output_string(|buf| {
            write_check_text(buf, &index, &analysis.broken_links, &[], &[], &[], &[], &[]);
        });
        assert!(out.contains("broken_links: 1"));
        assert!(out.contains("Page One -> page:gone"));
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- write_check_text: orphan pages ---

    #[test]
    fn check_text_orphan_pages_listed_by_title() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", "hello"), ("p2", "Beta", "world")]);
        let analysis = index.analyze_all();
        let orphan_ids = index.orphan_ids(&analysis.linked_pages, "");
        let out = output_string(|buf| {
            write_check_text(buf, &index, &[], &orphan_ids, &[], &[], &[], &[]);
        });
        assert!(out.contains("orphan_pages: 2"));
        assert!(out.contains("  Alpha\n"));
        assert!(out.contains("  Beta\n"));
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- write_check_text: duplicate titles ---

    #[test]
    fn check_text_duplicate_titles_listed() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", "body"), ("p2", "Alpha", "body")]);
        let dupes = index.find_duplicate_titles();
        let out = output_string(|buf| {
            write_check_text(buf, &index, &[], &[], &[], &[], &dupes, &[]);
        });
        assert!(out.contains("duplicate_titles: 1"));
        assert!(out.contains("\"Alpha\" (2 pages)"));
        assert!(out.contains("    - p1"));
        assert!(out.contains("    - p2"));
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- write_check_text: missing attachments ---

    #[test]
    fn check_text_missing_attachments_listed() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", "see [img](attachments/missing.png)")]);
        let analysis = index.analyze_all();
        let out = output_string(|buf| {
            write_check_text(
                buf,
                &index,
                &[],
                &[],
                &[],
                &[],
                &[],
                &analysis.missing_attachments,
            );
        });
        assert!(out.contains("missing_attachments: 1"));
        assert!(out.contains("Alpha -> "));
        assert!(out.contains("missing.png"));
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- write_check_text: load errors ---

    #[test]
    fn check_text_load_errors_shown() {
        let (dir, index) = make_test_kb(&[]);
        let errors = vec![PageLoadError {
            page_id: "p1".into(),
            title: "Broken Page".into(),
            error: "invalid JSON".into(),
        }];
        let out = output_string(|buf| {
            write_check_text(buf, &index, &[], &[], &errors, &[], &[], &[]);
        });
        assert!(out.contains("Load Errors (1):"));
        assert!(out.contains("Broken Page (p1): invalid JSON"));
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- write_check_text: index issues ---

    #[test]
    fn check_text_index_issues_shown() {
        let (dir, index) = make_test_kb(&[]);
        let issues = vec![ParseIssue {
            path: PathBuf::from("/kb/bad.lepiter"),
            message: "failed to decode".into(),
        }];
        let out = output_string(|buf| {
            write_check_text(buf, &index, &[], &[], &[], &issues, &[], &[]);
        });
        assert!(out.contains("Index Issues (1):"));
        assert!(out.contains("/kb/bad.lepiter: failed to decode"));
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- write_check_json: clean KB ---

    #[test]
    fn check_json_clean_kb_has_empty_arrays() {
        let (dir, index) = make_test_kb(&[]);
        let out = output_string(|buf| {
            write_check_json(buf, &index, &[], &[], &[], &[], &[], &[]);
        });
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["broken_links"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["orphan_pages"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["duplicate_titles"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["missing_attachments"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["load_errors"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["index_issues"].as_array().unwrap().len(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- write_check_json: broken links ---

    #[test]
    fn check_json_broken_links_populated() {
        let (dir, index) = make_test_kb(&[("p1", "Page One", "see [link](page:gone)")]);
        let analysis = index.analyze_all();
        let out = output_string(|buf| {
            write_check_json(buf, &index, &analysis.broken_links, &[], &[], &[], &[], &[]);
        });
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        let broken = parsed["broken_links"].as_array().unwrap();
        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0]["source_title"], "Page One");
        assert_eq!(broken[0]["source_id"], "p1");
        assert_eq!(broken[0]["target"], "page:gone");
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- write_check_json: orphan pages ---

    #[test]
    fn check_json_orphan_pages_include_title() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", "hello")]);
        let analysis = index.analyze_all();
        let orphan_ids = index.orphan_ids(&analysis.linked_pages, "");
        let out = output_string(|buf| {
            write_check_json(buf, &index, &[], &orphan_ids, &[], &[], &[], &[]);
        });
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        let orphans = parsed["orphan_pages"].as_array().unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0]["id"], "p1");
        assert_eq!(orphans[0]["title"], "Alpha");
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- write_check_json: duplicate titles ---

    #[test]
    fn check_json_duplicate_titles_populated() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", "body"), ("p2", "Alpha", "body")]);
        let dupes = index.find_duplicate_titles();
        let out = output_string(|buf| {
            write_check_json(buf, &index, &[], &[], &[], &[], &dupes, &[]);
        });
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        let dt = parsed["duplicate_titles"].as_array().unwrap();
        assert_eq!(dt.len(), 1);
        assert_eq!(dt[0]["title"], "Alpha");
        let ids = dt[0]["page_ids"].as_array().unwrap();
        assert_eq!(ids.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- write_check_json: missing attachments ---

    #[test]
    fn check_json_missing_attachments_populated() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", "see [img](attachments/missing.png)")]);
        let analysis = index.analyze_all();
        let out = output_string(|buf| {
            write_check_json(
                buf,
                &index,
                &[],
                &[],
                &[],
                &[],
                &[],
                &analysis.missing_attachments,
            );
        });
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        let att = parsed["missing_attachments"].as_array().unwrap();
        assert_eq!(att.len(), 1);
        assert_eq!(att[0]["source_title"], "Alpha");
        assert_eq!(att[0]["source_id"], "p1");
        assert_eq!(att[0]["target"], "attachments/missing.png");
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- write_check_json: load errors ---

    #[test]
    fn check_json_load_errors_populated() {
        let (dir, index) = make_test_kb(&[]);
        let errors = vec![PageLoadError {
            page_id: "p1".into(),
            title: "Broken".into(),
            error: "bad json".into(),
        }];
        let out = output_string(|buf| {
            write_check_json(buf, &index, &[], &[], &errors, &[], &[], &[]);
        });
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        let errs = parsed["load_errors"].as_array().unwrap();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0]["page_id"], "p1");
        assert_eq!(errs[0]["title"], "Broken");
        assert_eq!(errs[0]["error"], "bad json");
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- end-to-end with make_test_kb ---

    #[test]
    fn check_text_full_kb_with_all_issue_types() {
        let (dir, index) = make_test_kb(&[
            ("p1", "Alpha", "see [link](page:gone)"),
            ("p2", "Alpha", "duplicate title"),
            ("p3", "Beta", "see [img](attachments/missing.png)"),
        ]);
        let analysis = index.analyze_all();
        let orphan_ids = index.orphan_ids(&analysis.linked_pages, "");
        let dupes = index.find_duplicate_titles();
        let out = output_string(|buf| {
            write_check_text(
                buf,
                &index,
                &analysis.broken_links,
                &orphan_ids,
                &analysis.load_errors,
                &index.index_issues,
                &dupes,
                &analysis.missing_attachments,
            );
        });
        assert!(out.starts_with("Knowledge Base Check\n"));
        assert!(out.contains("broken_links: 1"));
        assert!(out.contains("duplicate_titles: 1"));
        assert!(out.contains("missing_attachments: 1"));
        // All three pages are orphans (none links to another with page: prefix
        // that resolves, since "page:gone" is broken).
        assert!(out.contains("orphan_pages: 3"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn check_json_full_kb_is_valid_json() {
        let (dir, index) = make_test_kb(&[
            ("p1", "Alpha", "see [link](page:gone)"),
            ("p2", "Alpha", "duplicate title"),
        ]);
        let analysis = index.analyze_all();
        let orphan_ids = index.orphan_ids(&analysis.linked_pages, "");
        let dupes = index.find_duplicate_titles();
        let out = output_string(|buf| {
            write_check_json(
                buf,
                &index,
                &analysis.broken_links,
                &orphan_ids,
                &analysis.load_errors,
                &index.index_issues,
                &dupes,
                &analysis.missing_attachments,
            );
        });
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(parsed.is_object());
        // All six top-level keys present.
        assert!(parsed.get("broken_links").is_some());
        assert!(parsed.get("orphan_pages").is_some());
        assert!(parsed.get("duplicate_titles").is_some());
        assert!(parsed.get("missing_attachments").is_some());
        assert!(parsed.get("load_errors").is_some());
        assert!(parsed.get("index_issues").is_some());
        std::fs::remove_dir_all(&dir).ok();
    }
}
