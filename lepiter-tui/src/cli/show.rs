use std::io::IsTerminal;

use anyhow::{Context, Result, bail};
use lepiter_core::{KnowledgeBaseIndex, LinkTargetKind, Node};

use super::format::render_page_pretty;
use super::{ArgSpec, open_kb, parse_args, resolve_page_id_by_title};

const SPEC: ArgSpec<'static> = ArgSpec {
    usage: "usage: lepiter-cli show [--id|--by-title] [--open-links] [--json] <value> [kb-path]\n\n\
            renders one page, looked up by title unless told otherwise.\n\n\
            flags:\n  \
              --id, -i      look the page up by id instead of title\n  \
              --by-title    look the page up by title (the default)\n  \
              --open-links  list the page's resolved links after the page body\n  \
              --json        serialize the full parsed page structure",
    toggles: &["--id", "-i", "--by-title", "--open-links", "--json"],
    valued: &[],
};

pub fn run_show(args: Vec<String>) -> Result<()> {
    let Some(args) = parse_args(args, &SPEC)? else {
        return Ok(());
    };
    let by_id = args.has("--id") || args.has("-i");
    let json = args.has("--json");

    if by_id && args.has("--by-title") {
        bail!("--id and --by-title are mutually exclusive");
    }
    let Some(value) = args.positional(0) else {
        bail!("missing required argument: <value>");
    };
    let value = value.trim();
    if value.is_empty() {
        bail!("value must not be empty");
    }
    let index = open_kb(&args.kb_path(1))?;

    let page_id = if by_id {
        value.to_string()
    } else {
        resolve_page_id_by_title(&index, value)?
    };

    if json {
        let page = index
            .load_page(&page_id)
            .with_context(|| format!("failed to load page id `{page_id}`"))?;
        println!("{}", serde_json::to_string_pretty(&page).unwrap());
        return Ok(());
    }

    print_page(&index, &page_id, args.has("--open-links"))
}

fn print_page(index: &KnowledgeBaseIndex, page_id: &str, show_links: bool) -> Result<()> {
    let page = index
        .load_page(page_id)
        .with_context(|| format!("failed to load page id `{page_id}`"))?;
    let attachment_resolver = index.attachment_resolver();
    let colored = std::io::stdout().is_terminal();
    print!("{}", render_page_pretty(&page, colored));
    if show_links {
        let links = collect_page_links(&page.content);
        println!();
        println!("resolved links:");
        if links.is_empty() {
            println!("  <none>");
        } else {
            for (idx, (label, target)) in links.iter().enumerate() {
                let kind = match index.classify_link_target(target) {
                    LinkTargetKind::InternalPage(id) => format!("internal:{id}"),
                    LinkTargetKind::AttachmentPath(_) => {
                        match attachment_resolver.resolve(target) {
                            Ok(resolved) => {
                                let mut out = format!("attachment:{}", resolved.path.display());
                                if !resolved.exists {
                                    out.push_str(" (missing)");
                                }
                                out
                            }
                            Err(err) => format!("attachment-error:{err}"),
                        }
                    }
                    LinkTargetKind::ExternalUrl(url) => format!("external:{url}"),
                    LinkTargetKind::Unknown(raw) => format!("unknown:{raw}"),
                };
                println!("  [{}] {} -> {}", idx + 1, label, kind);
            }
        }
    }
    Ok(())
}

pub(crate) fn collect_page_links(nodes: &[Node]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            Node::Link { text, url } => out.push((text.clone(), url.clone())),
            Node::Paragraph { text } | Node::Text { text } | Node::Quote { text } => {
                collect_inline_links(text, &mut out);
            }
            Node::Heading { text, .. } => {
                collect_inline_links(text, &mut out);
            }
            Node::List { items } => {
                for item in items {
                    out.extend(collect_page_links(item));
                }
            }
            _ => {}
        }
    }
    out
}

fn collect_inline_links(text: &str, out: &mut Vec<(String, String)>) {
    use crate::inline;
    for elem in inline::parse_inline(text) {
        match elem {
            inline::InlineElement::Link { label, target }
            | inline::InlineElement::Image { alt: label, target } => {
                out.push((label, target));
            }
            inline::InlineElement::WikiLink { text } => {
                out.push((text.clone(), text));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lepiter_core::Node;

    #[test]
    fn collect_page_links_standalone_link_node() {
        let nodes = vec![Node::Link {
            text: "example".into(),
            url: "https://example.com".into(),
        }];
        let links = collect_page_links(&nodes);
        assert_eq!(
            links,
            vec![("example".into(), "https://example.com".into())]
        );
    }

    #[test]
    fn collect_page_links_inline_link_in_paragraph() {
        let nodes = vec![Node::Paragraph {
            text: "see [docs](https://docs.rs) here".into(),
        }];
        let links = collect_page_links(&nodes);
        assert_eq!(links, vec![("docs".into(), "https://docs.rs".into())]);
    }

    #[test]
    fn collect_page_links_inline_image_in_paragraph() {
        let nodes = vec![Node::Paragraph {
            text: "see ![alt](attachments/x.png) here".into(),
        }];
        let links = collect_page_links(&nodes);
        assert_eq!(links, vec![("alt".into(), "attachments/x.png".into())]);
    }

    #[test]
    fn collect_page_links_linked_image_reports_the_outer_target() {
        let nodes = vec![Node::Paragraph {
            text: "[![alt](img.png)](page:t)".into(),
        }];
        let links = collect_page_links(&nodes);
        assert_eq!(links, vec![("![alt](img.png)".into(), "page:t".into())]);
    }

    #[test]
    fn collect_page_links_inline_link_in_text() {
        let nodes = vec![Node::Text {
            text: "click [here](https://example.com)".into(),
        }];
        let links = collect_page_links(&nodes);
        assert_eq!(links, vec![("here".into(), "https://example.com".into())]);
    }

    #[test]
    fn collect_page_links_inline_link_in_heading() {
        let nodes = vec![Node::Heading {
            level: 2,
            text: "see [API](https://api.example.com)".into(),
        }];
        let links = collect_page_links(&nodes);
        assert_eq!(
            links,
            vec![("API".into(), "https://api.example.com".into())]
        );
    }

    #[test]
    fn collect_page_links_inline_link_in_quote() {
        let nodes = vec![Node::Quote {
            text: "as noted in [RFC 123](https://rfc.example.com)".into(),
        }];
        let links = collect_page_links(&nodes);
        assert_eq!(
            links,
            vec![("RFC 123".into(), "https://rfc.example.com".into())]
        );
    }

    #[test]
    fn collect_page_links_wiki_link_in_paragraph() {
        let nodes = vec![Node::Paragraph {
            text: "see also [[My Other Page]]".into(),
        }];
        let links = collect_page_links(&nodes);
        assert_eq!(
            links,
            vec![("My Other Page".into(), "My Other Page".into())]
        );
    }

    #[test]
    fn collect_page_links_multiple_inline_links() {
        let nodes = vec![Node::Paragraph {
            text: "[first](url1) and [second](url2) and [[wiki]]".into(),
        }];
        let links = collect_page_links(&nodes);
        assert_eq!(links.len(), 3);
        assert_eq!(links[0], ("first".into(), "url1".into()));
        assert_eq!(links[1], ("second".into(), "url2".into()));
        assert_eq!(links[2], ("wiki".into(), "wiki".into()));
    }

    #[test]
    fn collect_page_links_mixed_standalone_and_inline() {
        let nodes = vec![
            Node::Link {
                text: "standalone".into(),
                url: "https://standalone.com".into(),
            },
            Node::Paragraph {
                text: "text with [inline](https://inline.com) link".into(),
            },
        ];
        let links = collect_page_links(&nodes);
        assert_eq!(links.len(), 2);
        assert_eq!(
            links[0],
            ("standalone".into(), "https://standalone.com".into())
        );
        assert_eq!(links[1], ("inline".into(), "https://inline.com".into()));
    }

    #[test]
    fn collect_page_links_no_links_in_plain_text() {
        let nodes = vec![Node::Paragraph {
            text: "just plain text, no links".into(),
        }];
        let links = collect_page_links(&nodes);
        assert!(links.is_empty());
    }

    #[test]
    fn collect_page_links_list_items_with_inline_links() {
        let nodes = vec![Node::List {
            items: vec![vec![Node::Paragraph {
                text: "item with [link](url)".into(),
            }]],
        }];
        let links = collect_page_links(&nodes);
        assert_eq!(links, vec![("link".into(), "url".into())]);
    }

    #[test]
    fn collect_page_links_code_nodes_ignored() {
        let nodes = vec![Node::Code {
            language: Some("rust".into()),
            code: "[not_a_link](http://example.com)".into(),
        }];
        let links = collect_page_links(&nodes);
        assert!(links.is_empty());
    }
}
