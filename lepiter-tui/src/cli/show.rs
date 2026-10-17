use std::io::IsTerminal;

use anyhow::{Context, Result, bail};
use lepiter_core::{KnowledgeBaseIndex, LinkKind, LinkTargetKind, Node};

use super::format::render_page_pretty;
use super::{ArgSpec, open_kb, parse_args, resolve_page_id_by_title};
use crate::render::LinkTarget;

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
            for (idx, link) in links.iter().enumerate() {
                let kind = match index.classify_link(link.kind, &link.target) {
                    LinkTargetKind::InternalPage(id) => format!("internal:{id}"),
                    LinkTargetKind::AttachmentPath(_) => {
                        match attachment_resolver.resolve(&link.target) {
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
                println!("  [{}] {} -> {}", idx + 1, link.label, kind);
            }
        }
    }
    Ok(())
}

pub(crate) fn collect_page_links(nodes: &[Node]) -> Vec<LinkTarget> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            Node::Link { text, url } => out.push(LinkTarget {
                label: text.clone(),
                target: url.clone(),
                kind: LinkKind::Markdown,
            }),
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

fn collect_inline_links(text: &str, out: &mut Vec<LinkTarget>) {
    use crate::inline;
    for elem in inline::parse_inline(text) {
        match elem {
            inline::InlineElement::Link { label, target }
            | inline::InlineElement::Image { alt: label, target } => {
                out.push(LinkTarget {
                    label,
                    target,
                    kind: LinkKind::Markdown,
                });
            }
            inline::InlineElement::WikiLink { text, name } => {
                out.push(LinkTarget {
                    label: text,
                    target: name,
                    kind: LinkKind::Wiki,
                });
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lepiter_core::Node;

    fn paragraph(text: &str) -> Node {
        Node::Paragraph { text: text.into() }
    }

    fn link(text: &str, url: &str) -> Node {
        Node::Link {
            text: text.into(),
            url: url.into(),
        }
    }

    fn pairs(links: Vec<LinkTarget>) -> Vec<(String, String)> {
        links
            .into_iter()
            .map(|link| (link.label, link.target))
            .collect()
    }

    #[test]
    fn collect_page_links_cases() {
        type Case = (Vec<Node>, &'static [(&'static str, &'static str)]);
        let cases: Vec<Case> = vec![
            (
                vec![link("example", "https://example.com")],
                &[("example", "https://example.com")],
            ),
            (
                vec![paragraph("see [docs](https://docs.rs) here")],
                &[("docs", "https://docs.rs")],
            ),
            (
                vec![paragraph("see ![alt](attachments/x.png) here")],
                &[("alt", "attachments/x.png")],
            ),
            (
                vec![paragraph("[![alt](img.png)](page:t)")],
                &[("![alt](img.png)", "page:t")],
            ),
            (
                vec![Node::Text {
                    text: "click [here](https://example.com)".into(),
                }],
                &[("here", "https://example.com")],
            ),
            (
                vec![Node::Heading {
                    level: 2,
                    text: "see [API](https://api.example.com)".into(),
                }],
                &[("API", "https://api.example.com")],
            ),
            (
                vec![Node::Quote {
                    text: "as noted in [RFC 123](https://rfc.example.com)".into(),
                }],
                &[("RFC 123", "https://rfc.example.com")],
            ),
            (
                vec![paragraph("see also [[My Other Page]]")],
                &[("My Other Page", "My Other Page")],
            ),
            (vec![paragraph("see [[Target ]]")], &[("Target", "Target ")]),
            (
                vec![paragraph("[first](url1) and [second](url2) and [[wiki]]")],
                &[("first", "url1"), ("second", "url2"), ("wiki", "wiki")],
            ),
            (
                vec![
                    link("standalone", "https://standalone.com"),
                    paragraph("text with [inline](https://inline.com) link"),
                ],
                &[
                    ("standalone", "https://standalone.com"),
                    ("inline", "https://inline.com"),
                ],
            ),
            (vec![paragraph("just plain text, no links")], &[]),
            (
                vec![Node::List {
                    items: vec![vec![paragraph("item with [link](url)")]],
                }],
                &[("link", "url")],
            ),
            (
                vec![Node::Code {
                    language: Some("rust".into()),
                    code: "[not_a_link](http://example.com)".into(),
                }],
                &[],
            ),
        ];
        for (nodes, expected) in cases {
            let expected: Vec<(String, String)> = expected
                .iter()
                .map(|(label, target)| ((*label).to_string(), (*target).to_string()))
                .collect();
            assert_eq!(pairs(collect_page_links(&nodes)), expected, "{nodes:?}");
        }
    }

    #[test]
    fn collect_page_links_inline_image_in_paragraph() {
        let nodes = vec![Node::Paragraph {
            text: "see ![alt](attachments/x.png) here".into(),
        }];
        let links = pairs(collect_page_links(&nodes));
        assert_eq!(links, vec![("alt".into(), "attachments/x.png".into())]);
    }
}
