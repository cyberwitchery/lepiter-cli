use std::io::Write;

use anyhow::Result;
use lepiter_core::{BrokenLink, DuplicateTitle, KnowledgeBaseIndex, MissingAttachment, ParseIssue};

use super::{ArgSpec, open_kb, parse_args, read_kb_properties};

const SPEC: ArgSpec<'static> = ArgSpec {
    usage: "usage: lepiter-cli check [--json] [kb-path]\n\n\
            validates knowledge base integrity. exits with status 1 if any issues\n\
            are found (broken links, orphan pages, duplicate titles, missing\n\
            attachments).\n\n\
            flags:\n  \
              --json  output as json",
    toggles: &["--json"],
    valued: &[],
};

struct CheckReport<'a> {
    index: &'a KnowledgeBaseIndex,
    broken_links: &'a [BrokenLink],
    orphan_ids: &'a [String],
    load_errors: &'a [lepiter_core::PageLoadError],
    index_issues: &'a [ParseIssue],
    duplicate_titles: &'a [DuplicateTitle],
    missing_attachments: &'a [MissingAttachment],
}

impl CheckReport<'_> {
    fn ok(&self) -> bool {
        self.broken_links.is_empty()
            && self.orphan_ids.is_empty()
            && self.load_errors.is_empty()
            && self.index_issues.is_empty()
            && self.duplicate_titles.is_empty()
            && self.missing_attachments.is_empty()
    }
}

pub fn run_check(args: Vec<String>) -> Result<()> {
    let Some(args) = parse_args(args, &SPEC)? else {
        return Ok(());
    };
    let json = args.has("--json");

    let kb_path = args.kb_path(0);
    let index = open_kb(&kb_path)?;

    let toc_page_id = read_kb_properties(&kb_path)?.and_then(|v| {
        v.get("tableOfContents")
            .and_then(|v| v.as_str())
            .map(String::from)
    });

    // Compute broken links, orphan pages, and missing attachments in a single pass.
    let analysis = index.analyze_all();
    let orphan_ids = index.orphan_ids(&analysis.linked_pages, toc_page_id.as_deref());
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

    let report = CheckReport {
        index: &index,
        broken_links: &analysis.broken_links,
        orphan_ids: &orphan_ids,
        load_errors: &analysis.load_errors,
        index_issues: &index.index_issues,
        duplicate_titles: &duplicate_titles,
        missing_attachments: &analysis.missing_attachments,
    };

    let mut out = std::io::stdout().lock();
    if json {
        write_check_json(&mut out, &report);
    } else {
        write_check_text(&mut out, &report);
    }

    if !report.ok() {
        std::process::exit(1);
    }

    Ok(())
}

fn write_check_text(out: &mut impl Write, report: &CheckReport) {
    let _ = writeln!(out, "Knowledge Base Check");
    let _ = writeln!(out, "  broken_links: {}", report.broken_links.len());
    let _ = writeln!(out, "  orphan_pages: {}", report.orphan_ids.len());
    let _ = writeln!(out, "  duplicate_titles: {}", report.duplicate_titles.len());
    let _ = writeln!(
        out,
        "  missing_attachments: {}",
        report.missing_attachments.len()
    );
    let _ = writeln!(out, "  load_errors: {}", report.load_errors.len());
    let _ = writeln!(out, "  index_issues: {}", report.index_issues.len());

    let _ = writeln!(out, "\nBroken Links ({}):", report.broken_links.len());
    if report.broken_links.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for link in report.broken_links {
            let _ = writeln!(out, "  {} -> {}", link.source_title, link.target);
        }
    }

    let _ = writeln!(out, "\nOrphan Pages ({}):", report.orphan_ids.len());
    if report.orphan_ids.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for id in report.orphan_ids {
            let title = report
                .index
                .pages
                .get(id)
                .map(|m| m.title.as_str())
                .unwrap_or(id);
            let _ = writeln!(out, "  {title}");
        }
    }

    let _ = writeln!(
        out,
        "\nDuplicate Titles ({}):",
        report.duplicate_titles.len()
    );
    if report.duplicate_titles.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for dup in report.duplicate_titles {
            let _ = writeln!(out, "  \"{}\" ({} pages)", dup.title, dup.page_ids.len());
            for id in &dup.page_ids {
                let _ = writeln!(out, "    - {id}");
            }
        }
    }

    let _ = writeln!(
        out,
        "\nMissing Attachments ({}):",
        report.missing_attachments.len()
    );
    if report.missing_attachments.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for att in report.missing_attachments {
            let _ = writeln!(
                out,
                "  {} -> {}",
                att.source_title,
                att.resolved_path.display()
            );
        }
    }

    if !report.load_errors.is_empty() {
        let _ = writeln!(out, "\nLoad Errors ({}):", report.load_errors.len());
        for err in report.load_errors {
            let _ = writeln!(out, "  {} ({}): {}", err.title, err.page_id, err.error);
        }
    }

    if !report.index_issues.is_empty() {
        let _ = writeln!(out, "\nIndex Issues ({}):", report.index_issues.len());
        for issue in report.index_issues {
            let _ = writeln!(out, "  {}: {}", issue.path.display(), issue.message);
        }
    }
}

fn write_check_json(out: &mut impl Write, report: &CheckReport) {
    let broken: Vec<serde_json::Value> = report
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

    let orphans: Vec<serde_json::Value> = report
        .orphan_ids
        .iter()
        .map(|id| {
            let title = report
                .index
                .pages
                .get(id)
                .map(|m| m.title.as_str())
                .unwrap_or(id);
            serde_json::json!({ "id": id, "title": title })
        })
        .collect();

    let errors: Vec<serde_json::Value> = report
        .load_errors
        .iter()
        .map(|err| {
            serde_json::json!({
                "page_id": err.page_id,
                "title": err.title,
                "error": err.error,
            })
        })
        .collect();

    let dupes: Vec<serde_json::Value> = report
        .duplicate_titles
        .iter()
        .map(|dup| {
            serde_json::json!({
                "title": dup.title,
                "page_ids": dup.page_ids,
            })
        })
        .collect();

    let attachments: Vec<serde_json::Value> = report
        .missing_attachments
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

    let idx_issues: Vec<serde_json::Value> = report
        .index_issues
        .iter()
        .map(|issue| {
            serde_json::json!({
                "path": issue.path.display().to_string(),
                "message": issue.message,
            })
        })
        .collect();

    let obj = serde_json::json!({
        "ok": report.ok(),
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
    use lepiter_core::{KnowledgeBase, PageLoadError};
    use std::path::PathBuf;

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

    fn empty_report(index: &KnowledgeBaseIndex) -> CheckReport<'_> {
        CheckReport {
            index,
            broken_links: &[],
            orphan_ids: &[],
            load_errors: &[],
            index_issues: &[],
            duplicate_titles: &[],
            missing_attachments: &[],
        }
    }

    // --- CheckReport::ok ---

    #[test]
    fn report_ok_when_empty() {
        let (_dir, index) = make_test_kb(&[]);
        let report = empty_report(&index);
        assert!(report.ok());
    }

    #[test]
    fn report_not_ok_with_broken_links() {
        let (_dir, index) = make_test_kb(&[("p1", "Page One", "see [link](page:gone)")]);
        let analysis = index.analyze_all();
        let report = CheckReport {
            broken_links: &analysis.broken_links,
            ..empty_report(&index)
        };
        assert!(!report.ok());
    }

    // --- write_check_text: clean KB ---

    #[test]
    fn check_text_clean_kb_shows_zero_counts() {
        let (dir, index) = make_test_kb(&[]);
        let report = empty_report(&index);
        let out = output_string(|buf| {
            write_check_text(buf, &report);
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
        let report = empty_report(&index);
        let out = output_string(|buf| {
            write_check_text(buf, &report);
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
        let report = CheckReport {
            broken_links: &analysis.broken_links,
            ..empty_report(&index)
        };
        let out = output_string(|buf| {
            write_check_text(buf, &report);
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
        let orphan_ids = index.orphan_ids(&analysis.linked_pages, None);
        let report = CheckReport {
            orphan_ids: &orphan_ids,
            ..empty_report(&index)
        };
        let out = output_string(|buf| {
            write_check_text(buf, &report);
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
        let report = CheckReport {
            duplicate_titles: &dupes,
            ..empty_report(&index)
        };
        let out = output_string(|buf| {
            write_check_text(buf, &report);
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
        let report = CheckReport {
            missing_attachments: &analysis.missing_attachments,
            ..empty_report(&index)
        };
        let out = output_string(|buf| {
            write_check_text(buf, &report);
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
        let report = CheckReport {
            load_errors: &errors,
            ..empty_report(&index)
        };
        let out = output_string(|buf| {
            write_check_text(buf, &report);
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
        let report = CheckReport {
            index_issues: &issues,
            ..empty_report(&index)
        };
        let out = output_string(|buf| {
            write_check_text(buf, &report);
        });
        assert!(out.contains("Index Issues (1):"));
        assert!(out.contains("/kb/bad.lepiter: failed to decode"));
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- write_check_json: clean KB ---

    #[test]
    fn check_json_clean_kb_has_empty_arrays_and_ok_true() {
        let (dir, index) = make_test_kb(&[]);
        let report = empty_report(&index);
        let out = output_string(|buf| {
            write_check_json(buf, &report);
        });
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["ok"], true);
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
        let report = CheckReport {
            broken_links: &analysis.broken_links,
            ..empty_report(&index)
        };
        let out = output_string(|buf| {
            write_check_json(buf, &report);
        });
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["ok"], false);
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
        let orphan_ids = index.orphan_ids(&analysis.linked_pages, None);
        let report = CheckReport {
            orphan_ids: &orphan_ids,
            ..empty_report(&index)
        };
        let out = output_string(|buf| {
            write_check_json(buf, &report);
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
        let report = CheckReport {
            duplicate_titles: &dupes,
            ..empty_report(&index)
        };
        let out = output_string(|buf| {
            write_check_json(buf, &report);
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
        let report = CheckReport {
            missing_attachments: &analysis.missing_attachments,
            ..empty_report(&index)
        };
        let out = output_string(|buf| {
            write_check_json(buf, &report);
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
        let report = CheckReport {
            load_errors: &errors,
            ..empty_report(&index)
        };
        let out = output_string(|buf| {
            write_check_json(buf, &report);
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
        let orphan_ids = index.orphan_ids(&analysis.linked_pages, None);
        let dupes = index.find_duplicate_titles();
        let report = CheckReport {
            broken_links: &analysis.broken_links,
            orphan_ids: &orphan_ids,
            load_errors: &analysis.load_errors,
            index_issues: &index.index_issues,
            duplicate_titles: &dupes,
            missing_attachments: &analysis.missing_attachments,
            ..empty_report(&index)
        };
        let out = output_string(|buf| {
            write_check_text(buf, &report);
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
        let orphan_ids = index.orphan_ids(&analysis.linked_pages, None);
        let dupes = index.find_duplicate_titles();
        let report = CheckReport {
            broken_links: &analysis.broken_links,
            orphan_ids: &orphan_ids,
            load_errors: &analysis.load_errors,
            index_issues: &index.index_issues,
            duplicate_titles: &dupes,
            missing_attachments: &analysis.missing_attachments,
            ..empty_report(&index)
        };
        let out = output_string(|buf| {
            write_check_json(buf, &report);
        });
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(parsed.is_object());
        assert_eq!(parsed["ok"], false);
        // All seven top-level keys present.
        assert!(parsed.get("ok").is_some());
        assert!(parsed.get("broken_links").is_some());
        assert!(parsed.get("orphan_pages").is_some());
        assert!(parsed.get("duplicate_titles").is_some());
        assert!(parsed.get("missing_attachments").is_some());
        assert!(parsed.get("load_errors").is_some());
        assert!(parsed.get("index_issues").is_some());
        std::fs::remove_dir_all(&dir).ok();
    }
}
