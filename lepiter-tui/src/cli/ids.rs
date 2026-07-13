use std::path::PathBuf;

use anyhow::{Context, Result};
use lepiter_core::KnowledgeBase;

pub fn run_ids(args: Vec<String>) -> Result<()> {
    let mut positional = Vec::new();

    for arg in &args {
        match arg.as_str() {
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
    for meta in index.sorted_pages() {
        println!("{}", meta.id);
    }
    Ok(())
}
