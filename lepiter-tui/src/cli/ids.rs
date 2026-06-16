use std::path::PathBuf;

use anyhow::{Context, Result};
use lepiter_core::KnowledgeBase;

pub fn run_ids(args: Vec<String>) -> Result<()> {
    let kb_path = args
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./lepiter"));
    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;
    for meta in index.sorted_pages() {
        println!("{}", meta.id);
    }
    Ok(())
}
