use std::fs;
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

    // Compute broken links and orphan pages via shared core function.
    let analysis = index.analyze_links();
    let orphan_ids = index.orphan_ids(&analysis.linked_pages, &toc_page_id);
    let duplicate_titles = index.find_duplicate_titles();
    let missing_attachments = index.find_missing_attachments();

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
        || !missing_attachments.is_empty();

    if json {
        print_check_json(
            &index,
            &analysis.broken_links,
            &orphan_ids,
            &analysis.load_errors,
            &index.index_issues,
            &duplicate_titles,
            &missing_attachments,
        );
    } else {
        print_check_text(
            &index,
            &analysis.broken_links,
            &orphan_ids,
            &analysis.load_errors,
            &index.index_issues,
            &duplicate_titles,
            &missing_attachments,
        );
    }

    if has_issues {
        std::process::exit(1);
    }

    Ok(())
}

fn print_check_text(
    index: &KnowledgeBaseIndex,
    broken_links: &[BrokenLink],
    orphan_ids: &[String],
    load_errors: &[lepiter_core::PageLoadError],
    index_issues: &[ParseIssue],
    duplicate_titles: &[DuplicateTitle],
    missing_attachments: &[MissingAttachment],
) {
    println!("Knowledge Base Check");
    println!("  broken_links: {}", broken_links.len());
    println!("  orphan_pages: {}", orphan_ids.len());
    println!("  duplicate_titles: {}", duplicate_titles.len());
    println!("  missing_attachments: {}", missing_attachments.len());
    println!("  load_errors: {}", load_errors.len());
    println!("  index_issues: {}", index_issues.len());

    println!("\nBroken Links ({}):", broken_links.len());
    if broken_links.is_empty() {
        println!("  (none)");
    } else {
        for link in broken_links {
            println!("  {} -> {}", link.source_title, link.target);
        }
    }

    println!("\nOrphan Pages ({}):", orphan_ids.len());
    if orphan_ids.is_empty() {
        println!("  (none)");
    } else {
        for id in orphan_ids {
            let title = index.pages.get(id).map(|m| m.title.as_str()).unwrap_or(id);
            println!("  {title}");
        }
    }

    println!("\nDuplicate Titles ({}):", duplicate_titles.len());
    if duplicate_titles.is_empty() {
        println!("  (none)");
    } else {
        for dup in duplicate_titles {
            println!("  \"{}\" ({} pages)", dup.title, dup.page_ids.len());
            for id in &dup.page_ids {
                println!("    - {id}");
            }
        }
    }

    println!("\nMissing Attachments ({}):", missing_attachments.len());
    if missing_attachments.is_empty() {
        println!("  (none)");
    } else {
        for att in missing_attachments {
            println!("  {} -> {}", att.source_title, att.resolved_path.display());
        }
    }

    if !load_errors.is_empty() {
        println!("\nLoad Errors ({}):", load_errors.len());
        for err in load_errors {
            println!("  {} ({}): {}", err.title, err.page_id, err.error);
        }
    }

    if !index_issues.is_empty() {
        println!("\nIndex Issues ({}):", index_issues.len());
        for issue in index_issues {
            println!("  {}: {}", issue.path.display(), issue.message);
        }
    }
}

fn print_check_json(
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
    println!("{}", serde_json::to_string_pretty(&obj).unwrap());
}
