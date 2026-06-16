use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use lepiter_core::{KnowledgeBase, SearchMatchKind};

use super::format::truncate_chars;

pub fn run_search(args: Vec<String>) -> Result<()> {
    let mut full_text = false;
    let mut tsv = false;
    let mut json = false;
    let mut positional = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--full-text" => full_text = true,
            "--tsv" => tsv = true,
            "--json" => json = true,
            _ => positional.push(arg),
        }
    }

    if positional.is_empty() {
        bail!("missing required argument: <query>");
    }

    let query = positional[0].trim().to_string();
    if query.is_empty() {
        bail!("query must not be empty");
    }

    let kb_path = positional
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./lepiter"));
    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;
    let hits = index.search_hits(&query, full_text);

    if json {
        let enriched: Vec<serde_json::Value> = hits
            .iter()
            .filter_map(|hit| {
                index.pages.get(&hit.id).map(|meta| {
                    serde_json::json!({
                        "id": hit.id,
                        "title": meta.title,
                        "kind": hit.kind,
                        "tags": meta.tags,
                    })
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&enriched).unwrap());
        return Ok(());
    }

    let hit_by_id = hits
        .into_iter()
        .map(|hit| {
            let kind = match hit.kind {
                SearchMatchKind::Title => "title",
                SearchMatchKind::Tag => "tag",
                SearchMatchKind::Content => "content",
            };
            (hit.id, kind)
        })
        .collect::<std::collections::HashMap<_, _>>();

    if tsv {
        for meta in index.sorted_pages() {
            if let Some(kind) = hit_by_id.get(&meta.id) {
                println!("{}\t{}\t{}", meta.title, meta.id, kind);
            }
        }
        return Ok(());
    }

    let title_width = index
        .sorted_pages()
        .iter()
        .map(|m| m.title.chars().count())
        .max()
        .unwrap_or(5)
        .clamp(5, 64);

    println!(
        "{:<width$}  {:<36}  match",
        "title",
        "id",
        width = title_width
    );
    println!(
        "{:-<width$}  {:-<36}  {:-<7}",
        "",
        "",
        "",
        width = title_width
    );
    for meta in index.sorted_pages() {
        if let Some(kind) = hit_by_id.get(&meta.id) {
            println!(
                "{:<width$}  {:<36}  {}",
                truncate_chars(&meta.title, title_width),
                meta.id,
                kind,
                width = title_width
            );
        }
    }

    Ok(())
}
