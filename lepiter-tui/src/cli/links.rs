use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use lepiter_core::{KnowledgeBase, KnowledgeBaseIndex, LinkEdge};

use super::resolve_page_id_by_title;

pub fn run_links(args: Vec<String>) -> Result<()> {
    let mut dot = false;
    let mut json = false;
    let mut for_page: Option<String> = None;
    let mut positional = Vec::new();

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--dot" => dot = true,
            "--json" => json = true,
            "--for" => {
                let val = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--for requires a page argument"))?;
                for_page = Some(val.clone());
            }
            _ if arg.starts_with('-') => {
                eprintln!("unknown flag: {arg}");
                std::process::exit(2);
            }
            _ => positional.push(arg.clone()),
        }
    }

    if dot && json {
        bail!("--dot and --json are mutually exclusive");
    }

    let kb_path = positional
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./lepiter"));
    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;

    let graph = index.build_link_graph();

    let ego_page_id = match &for_page {
        Some(title) => Some(resolve_page_id_by_title(&index, title)?),
        None => None,
    };

    let edges: Vec<&LinkEdge> = match &ego_page_id {
        Some(id) => graph.ego(id),
        None => graph.edges.iter().collect(),
    };

    if json {
        print_links_json(&index, &edges, ego_page_id.as_deref());
    } else if dot {
        print_links_dot(&index, &edges);
    } else {
        print_links_text(&index, &edges, ego_page_id.as_deref());
    }

    Ok(())
}

fn print_links_json(index: &KnowledgeBaseIndex, edges: &[&LinkEdge], ego_id: Option<&str>) {
    use std::collections::BTreeSet;

    let mut node_ids: BTreeSet<&str> = BTreeSet::new();
    for edge in edges {
        node_ids.insert(&edge.source);
        node_ids.insert(&edge.target);
    }
    if let Some(id) = ego_id {
        node_ids.insert(id);
    }

    let nodes: Vec<serde_json::Value> = node_ids
        .iter()
        .map(|id| {
            let title = index.pages.get(*id).map(|m| m.title.as_str()).unwrap_or(id);
            serde_json::json!({ "id": id, "title": title })
        })
        .collect();

    let edge_values: Vec<serde_json::Value> = edges
        .iter()
        .map(|e| serde_json::json!({ "source": e.source, "target": e.target }))
        .collect();

    let obj = serde_json::json!({
        "nodes": nodes,
        "edges": edge_values,
    });
    println!("{}", serde_json::to_string_pretty(&obj).unwrap());
}

fn print_links_dot(index: &KnowledgeBaseIndex, edges: &[&LinkEdge]) {
    fn dot_title(index: &KnowledgeBaseIndex, id: &str) -> String {
        index
            .pages
            .get(id)
            .map(|m| m.title.clone())
            .unwrap_or_else(|| id.to_string())
    }

    fn dot_escape(s: &str) -> String {
        s.replace('\\', "\\\\").replace('"', "\\\"")
    }

    use std::collections::BTreeSet;
    let mut node_ids = BTreeSet::new();
    for edge in edges {
        node_ids.insert(&edge.source);
        node_ids.insert(&edge.target);
    }

    println!("digraph links {{");
    println!("  rankdir=LR;");
    for id in &node_ids {
        let title = dot_title(index, id);
        println!(
            "  \"{}\" [label=\"{}\"];",
            dot_escape(id),
            dot_escape(&title)
        );
    }
    for edge in edges {
        println!(
            "  \"{}\" -> \"{}\";",
            dot_escape(&edge.source),
            dot_escape(&edge.target)
        );
    }
    println!("}}");
}

fn print_links_text(index: &KnowledgeBaseIndex, edges: &[&LinkEdge], ego_id: Option<&str>) {
    use std::collections::{BTreeMap, BTreeSet};

    let mut in_degree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut out_degree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut connected: BTreeSet<&str> = BTreeSet::new();

    for edge in edges {
        *in_degree.entry(&edge.target).or_insert(0) += 1;
        *out_degree.entry(&edge.source).or_insert(0) += 1;
        connected.insert(&edge.source);
        connected.insert(&edge.target);
    }

    let total_pages = if ego_id.is_some() {
        connected.len()
    } else {
        index.pages.len()
    };

    println!(
        "Link Graph{}",
        ego_id.map_or(String::new(), |id| {
            let title = index.pages.get(id).map(|m| m.title.as_str()).unwrap_or(id);
            format!(" (ego: {title})")
        })
    );
    println!("  pages: {total_pages}");
    println!("  links: {}", edges.len());

    // Most linked-to pages (by in-degree), top 10.
    let mut by_in: Vec<(&str, usize)> = in_degree.iter().map(|(k, v)| (*k, *v)).collect();
    by_in.sort_by(|a, b| {
        b.1.cmp(&a.1).then_with(|| {
            let ta = index
                .pages
                .get(a.0)
                .map(|m| m.title_lower.as_str())
                .unwrap_or("");
            let tb = index
                .pages
                .get(b.0)
                .map(|m| m.title_lower.as_str())
                .unwrap_or("");
            ta.cmp(tb)
        })
    });

    if !by_in.is_empty() {
        println!("\nMost Linked Pages:");
        for (id, count) in by_in.iter().take(10) {
            let title = index.pages.get(*id).map(|m| m.title.as_str()).unwrap_or(id);
            println!("  {count:>4}  {title}");
        }
    }

    // Isolated pages (not in any edge) — only in full-graph mode.
    if ego_id.is_none() {
        let isolated: Vec<&str> = index
            .sorted_ids
            .iter()
            .filter(|id| !connected.contains(id.as_str()))
            .map(String::as_str)
            .collect();
        if !isolated.is_empty() {
            println!("\nIsolated Pages ({}):", isolated.len());
            for id in &isolated {
                let title = index.pages.get(*id).map(|m| m.title.as_str()).unwrap_or(id);
                println!("  {title}");
            }
        }
    }
}
