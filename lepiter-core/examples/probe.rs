use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use lepiter_core::{KnowledgeBase, ParseIssue, collect_node_types_in_file};

fn main() -> Result<()> {
    let kb_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./lepiter"));

    let index = KnowledgeBase::open(&kb_path)?;

    let pages = index.sorted_pages_by_title();
    println!("pages: {}", pages.len());
    for page in &pages {
        println!("{}\t{}", page.id, page.title);
    }

    let mut global_types: HashMap<String, usize> = HashMap::new();
    let mut issues: Vec<ParseIssue> = index.index_issues.clone();

    for page in pages {
        match collect_node_types_in_file(&page.path) {
            Ok(counts) => {
                for (typ, count) in counts {
                    *global_types.entry(typ).or_insert(0) += count;
                }
            }
            Err(err) => issues.push(ParseIssue {
                path: page.path.clone(),
                message: format!("{err:#}"),
            }),
        }
    }

    let mut type_rows = global_types.into_iter().collect::<Vec<_>>();
    type_rows.sort_by(|a, b| a.0.cmp(&b.0));

    println!("\nnode types observed:");
    for (typ, count) in type_rows {
        println!("{typ}\t{count}");
    }

    println!("\nparse failures: {}", issues.len());
    for issue in issues {
        println!("{}\t{}", issue.path.display(), issue.message);
    }

    Ok(())
}
