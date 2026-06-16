use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use lepiter_core::{KnowledgeBase, KnowledgeBaseIndex, PageMeta};

use super::format::truncate_chars;

pub fn run_tags(args: Vec<String>) -> Result<()> {
    let mut json = false;
    let mut tsv = false;
    let mut for_tag: Option<String> = None;
    let mut positional = Vec::new();

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--tsv" => tsv = true,
            "--for" => {
                let val = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--for requires a tag argument"))?;
                for_tag = Some(val.clone());
            }
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

    match for_tag {
        Some(tag) => print_tag_pages(&index, &tag, json, tsv),
        None => print_tag_summary(&index, json, tsv),
    }

    Ok(())
}

fn collect_tag_counts(index: &KnowledgeBaseIndex) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for meta in index.pages.values() {
        for tag in &meta.tags {
            *counts.entry(tag.clone()).or_insert(0) += 1;
        }
    }
    let mut sorted: Vec<(String, usize)> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    sorted
}

fn print_tag_summary(index: &KnowledgeBaseIndex, json: bool, tsv: bool) {
    let tags = collect_tag_counts(index);

    if json {
        let arr: Vec<serde_json::Value> = tags
            .iter()
            .map(|(tag, count)| serde_json::json!({ "tag": tag, "count": count }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap());
        return;
    }

    if tsv {
        for (tag, count) in &tags {
            println!("{tag}\t{count}");
        }
        return;
    }

    println!("Tags ({} unique)", tags.len());
    if tags.is_empty() {
        println!("  (none)");
    } else {
        for (tag, count) in &tags {
            println!("  {count:>4}  {tag}");
        }
    }
}

fn print_tag_pages(index: &KnowledgeBaseIndex, tag: &str, json: bool, tsv: bool) {
    let needle = tag.to_lowercase();
    let pages: Vec<&PageMeta> = index
        .sorted_pages()
        .into_iter()
        .filter(|m| m.tags_lower.iter().any(|t| t == &needle))
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&pages).unwrap());
        return;
    }

    if tsv {
        for meta in &pages {
            println!("{}\t{}", meta.title, meta.id);
        }
        return;
    }

    println!("Pages tagged \"{tag}\" ({})", pages.len());
    if pages.is_empty() {
        println!("  (none)");
    } else {
        let title_width = pages
            .iter()
            .map(|m| m.title.chars().count())
            .max()
            .unwrap_or(5)
            .clamp(5, 64);
        for meta in &pages {
            println!(
                "  {:<width$}  {}",
                truncate_chars(&meta.title, title_width),
                meta.id,
                width = title_width
            );
        }
    }
}
