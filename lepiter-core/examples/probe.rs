use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use lepiter_core::{KnowledgeBase, Node, ParseIssue, collect_node_types_in_file};

fn main() -> Result<()> {
    let mut matrix_md = false;
    let mut kb_path = PathBuf::from("./lepiter");

    for arg in std::env::args().skip(1) {
        if arg == "--matrix-md" {
            matrix_md = true;
        } else {
            kb_path = PathBuf::from(arg);
        }
    }

    let index = KnowledgeBase::open(&kb_path)?;
    let pages = index.sorted_pages();

    let mut global_types: HashMap<String, usize> = HashMap::new();
    let mut unknown_types: HashMap<String, usize> = HashMap::new();
    let mut issues: Vec<ParseIssue> = index.index_issues.clone();

    for page in &pages {
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

    for page in &pages {
        match index.load_page(&page.id) {
            Ok(parsed) => collect_unknown_counts(&parsed.content, &mut unknown_types),
            Err(err) => issues.push(ParseIssue {
                path: page.path.clone(),
                message: format!("{err:#}"),
            }),
        }
    }

    if matrix_md {
        print_matrix_markdown(&global_types, &unknown_types);
        return Ok(());
    }

    println!("pages: {}", pages.len());
    for page in &pages {
        println!("{}\t{}", page.id, page.title);
    }

    let mut type_rows = global_types.into_iter().collect::<Vec<_>>();
    type_rows.sort_by(|a, b| a.0.cmp(&b.0));
    println!("\nnode types observed:");
    for (typ, count) in type_rows {
        println!("{typ}\t{count}");
    }

    let mut unknown_rows = unknown_types.into_iter().collect::<Vec<_>>();
    unknown_rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    println!("\nunknown node types:");
    if unknown_rows.is_empty() {
        println!("<none>");
    } else {
        for (typ, count) in unknown_rows {
            println!("{typ}\t{count}");
        }
    }

    println!("\nparse failures: {}", issues.len());
    for issue in issues {
        println!("{}\t{}", issue.path.display(), issue.message);
    }

    Ok(())
}

fn collect_unknown_counts(nodes: &[Node], out: &mut HashMap<String, usize>) {
    for node in nodes {
        match node {
            Node::Unknown { typ, .. } => {
                *out.entry(typ.clone()).or_insert(0) += 1;
            }
            Node::List { items } => {
                for item in items {
                    collect_unknown_counts(item, out);
                }
            }
            _ => {}
        }
    }
}

fn print_matrix_markdown(
    global_types: &HashMap<String, usize>,
    unknown_types: &HashMap<String, usize>,
) {
    let mut rows = global_types
        .iter()
        .filter(|(typ, _)| is_snippet_like(typ))
        .map(|(typ, count)| (typ.clone(), *count))
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    println!("# snippet support matrix");
    println!();
    println!(
        "this matrix is generated from `cargo run -p lepiter-core --example probe -- --matrix-md <kb-path>`."
    );
    println!();
    println!("| source type | observed | parser mapping | render | link nav | status |");
    println!("|---|---:|---|---|---|---|");

    for (typ, count) in rows {
        let support = classify_type(&typ, unknown_types.contains_key(&typ));
        println!(
            "| `{}` | {} | {} | {} | {} | {} |",
            typ, count, support.mapping, support.render, support.link_nav, support.status
        );
    }
}

fn is_snippet_like(typ: &str) -> bool {
    typ.ends_with("Snippet") || typ == "pharoRewrite"
}

struct SupportInfo<'a> {
    mapping: &'a str,
    render: &'a str,
    link_nav: &'a str,
    status: &'a str,
}

fn classify_type<'a>(typ: &'a str, is_unknown: bool) -> SupportInfo<'a> {
    match typ {
        "textSnippet" => SupportInfo {
            mapping: "`Node::Paragraph`/`Node::Heading`/`Node::Text`",
            render: "markdown-like",
            link_nav: "yes",
            status: "full",
        },
        "listSnippet" => SupportInfo {
            mapping: "`Node::List`",
            render: "list block",
            link_nav: "no",
            status: "full",
        },
        "blockQuoteSnippet" | "quoteSnippet" | "commentSnippet" => SupportInfo {
            mapping: "`Node::Quote`",
            render: "quote block",
            link_nav: "no",
            status: "full",
        },
        "pharoLinkSnippet" | "linkSnippet" => SupportInfo {
            mapping: "`Node::Link`",
            render: "link line",
            link_nav: "yes",
            status: "full",
        },
        "pictureSnippet" => SupportInfo {
            mapping: "`Node::Link`",
            render: "link line (media reference)",
            link_nav: "yes (target-dependent)",
            status: "partial",
        },
        "youtubeSnippet" => SupportInfo {
            mapping: "`Node::Link`",
            render: "link line (youtube url)",
            link_nav: "yes (target-dependent)",
            status: "partial",
        },
        "pharoSnippet" | "pythonSnippet" | "javascriptSnippet" => SupportInfo {
            mapping: "`Node::Code`",
            render: "highlighted code",
            link_nav: "no",
            status: "full",
        },
        "elementSnippet" => SupportInfo {
            mapping: "`Node::Code`",
            render: "code block",
            link_nav: "no",
            status: "partial",
        },
        "wordSnippet" => SupportInfo {
            mapping: "`Node::Paragraph`",
            render: "paragraph text",
            link_nav: "no",
            status: "full",
        },
        "pharoRewrite" => SupportInfo {
            mapping: "`Node::Rewrite`",
            render: "rewrite diff block",
            link_nav: "no",
            status: "full",
        },
        "shellCommandSnippet"
        | "gemstoneSnippet"
        | "exampleSnippet"
        | "changesSnippet"
        | "robocoderMetamodelSnippet" => SupportInfo {
            mapping: "`Node::Code`",
            render: "code block",
            link_nav: "no",
            status: "partial",
        },
        _ if is_unknown => SupportInfo {
            mapping: "`Node::Unknown`",
            render: "`[[unknown: <type>]]`",
            link_nav: "no",
            status: "fallback",
        },
        _ => SupportInfo {
            mapping: "`Node::Unknown`",
            render: "`[[unknown: <type>]]`",
            link_nav: "no",
            status: "fallback",
        },
    }
}
