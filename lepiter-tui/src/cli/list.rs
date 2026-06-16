use std::path::PathBuf;

use anyhow::{Context, Result};
use lepiter_core::KnowledgeBase;

use super::format::truncate_chars;

pub fn run_list(args: Vec<String>) -> Result<()> {
    let mut tsv = false;
    let mut json = false;
    let mut positional = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--tsv" => tsv = true,
            "--json" => json = true,
            _ => positional.push(arg),
        }
    }
    let kb_path = positional
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./lepiter"));
    print_page_list(kb_path, tsv, json)
}

fn print_page_list(kb_path: PathBuf, tsv: bool, json: bool) -> Result<()> {
    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;

    if json {
        let pages: Vec<&lepiter_core::PageMeta> = index.sorted_pages();
        println!("{}", serde_json::to_string_pretty(&pages).unwrap());
        return Ok(());
    }

    if tsv {
        for meta in index.sorted_pages() {
            println!("{}\t{}", meta.title, meta.id);
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

    println!("{:<width$}  id", "title", width = title_width);
    println!("{:-<width$}  {:-<36}", "", "", width = title_width);
    for meta in index.sorted_pages() {
        println!(
            "{:<width$}  {}",
            truncate_chars(&meta.title, title_width),
            meta.id,
            width = title_width
        );
    }
    Ok(())
}
