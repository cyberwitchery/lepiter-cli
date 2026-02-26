//! Core data model and parser for Lepiter knowledge bases stored as page JSON files.
//!
//! # Scope
//! - Scans a Lepiter directory and builds a metadata index keyed by page id.
//! - Loads and parses individual pages lazily by id.
//! - Converts page snippet trees into a stable block-oriented node model.
//! - Preserves unknown node types as [`Node::Unknown`] to keep consumers resilient.
//!
//! # Example
//! ```no_run
//! use lepiter_core::KnowledgeBase;
//!
//! # fn main() -> anyhow::Result<()> {
//! let index = KnowledgeBase::open("./lepiter")?;
//! for page in index.sorted_pages_by_title() {
//!     println!("{} - {}", page.id, page.title);
//! }
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset};
use serde::Deserialize;
use serde_json::Value;
use walkdir::WalkDir;

/// Canonical page identifier used throughout the API.
pub type PageId = String;

/// Metadata for a page discovered during index scanning.
#[derive(Debug, Clone)]
pub struct PageMeta {
    /// Canonical page id (preferred key over filename).
    pub id: PageId,
    /// Human-readable page title.
    pub title: String,
    /// Absolute or relative path to the source page file.
    pub path: PathBuf,
    /// Last edit timestamp, if present in source metadata.
    pub updated_at: Option<DateTime<FixedOffset>>,
    /// Optional page tags extracted from metadata.
    pub tags: Vec<String>,
}

/// Fully parsed page content.
#[derive(Debug, Clone)]
pub struct Page {
    /// Canonical page id.
    pub id: PageId,
    /// Page title.
    pub title: String,
    /// Last edit timestamp, if present.
    pub updated_at: Option<DateTime<FixedOffset>>,
    /// Page tags.
    pub tags: Vec<String>,
    /// Parsed block-level content.
    pub content: Vec<Node>,
}

/// Block-oriented normalized node model used by consumers (e.g. TUI).
#[derive(Debug, Clone)]
pub enum Node {
    /// Markdown-style heading.
    Heading { level: u8, text: String },
    /// Paragraph text.
    Paragraph { text: String },
    /// Plain text line.
    Text { text: String },
    /// List with item nodes.
    List { items: Vec<Vec<Node>> },
    /// Code block with optional language.
    Code {
        language: Option<String>,
        code: String,
    },
    /// Link block.
    Link { text: String, url: String },
    /// Quote block.
    Quote { text: String },
    /// Unknown/unsupported source node type preserved losslessly.
    Unknown { typ: String, raw: Value },
}

/// Non-fatal parse/indexing issue associated with a source file.
#[derive(Debug, Clone)]
pub struct ParseIssue {
    /// File path where the issue occurred.
    pub path: PathBuf,
    /// Human-readable error description.
    pub message: String,
}

/// Indexed knowledge base metadata with lazy page loading.
#[derive(Debug, Clone)]
pub struct KnowledgeBaseIndex {
    root: PathBuf,
    /// Metadata map keyed by canonical page id.
    pub pages: HashMap<PageId, PageMeta>,
    /// Non-fatal issues encountered while scanning metadata.
    pub index_issues: Vec<ParseIssue>,
}

/// Entry point for opening a Lepiter knowledge base directory.
pub struct KnowledgeBase;

impl KnowledgeBase {
    /// Scans a knowledge base directory and builds a page metadata index.
    ///
    /// This operation only reads metadata and does not parse full page content.
    /// Full parsing is done lazily via [`KnowledgeBaseIndex::load_page`].
    pub fn open(path: impl AsRef<Path>) -> Result<KnowledgeBaseIndex> {
        let root = path.as_ref().to_path_buf();
        let mut pages = HashMap::new();
        let mut issues = Vec::new();

        for entry in WalkDir::new(&root)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let file_type = entry.file_type();
            let file_path = entry.path();
            if !file_type.is_file()
                || file_path.extension().and_then(|e| e.to_str()) != Some("lepiter")
            {
                continue;
            }

            match parse_page_meta(file_path) {
                Ok(mut meta) => {
                    if meta.id.is_empty()
                        && let Some(stem) = file_path.file_stem().and_then(|s| s.to_str())
                    {
                        meta.id = stem.to_string();
                    }
                    if meta.title.is_empty() {
                        meta.title = meta.id.clone();
                    }
                    pages.insert(meta.id.clone(), meta);
                }
                Err(err) => issues.push(ParseIssue {
                    path: file_path.to_path_buf(),
                    message: format!("{err:#}"),
                }),
            }
        }

        Ok(KnowledgeBaseIndex {
            root,
            pages,
            index_issues: issues,
        })
    }
}

impl KnowledgeBaseIndex {
    /// Loads and parses a single page by canonical id.
    ///
    /// Returns an error if the id is missing from the index or if JSON parsing fails.
    pub fn load_page(&self, id: &str) -> Result<Page> {
        let meta = self
            .pages
            .get(id)
            .with_context(|| format!("page id not found: {id}"))?;

        let file = File::open(&meta.path)
            .with_context(|| format!("failed to open page file {}", meta.path.display()))?;
        let reader = BufReader::new(file);
        let raw: Value =
            serde_json::from_reader(reader).with_context(|| "failed to decode page JSON")?;

        let mut content = Vec::new();
        if let Some(items) = raw
            .get("children")
            .and_then(|v| v.get("items"))
            .and_then(Value::as_array)
        {
            for item in items {
                parse_item_recursive(item, &mut content);
            }
        }

        Ok(Page {
            id: meta.id.clone(),
            title: meta.title.clone(),
            updated_at: meta.updated_at,
            tags: meta.tags.clone(),
            content,
        })
    }

    /// Returns metadata entries sorted case-insensitively by title.
    pub fn sorted_pages_by_title(&self) -> Vec<&PageMeta> {
        let mut pages = self.pages.values().collect::<Vec<_>>();
        pages.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        pages
    }

    /// Returns the root path used to build this index.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[derive(Debug, Deserialize)]
struct RawMeta {
    #[serde(default)]
    uid: Option<RawUid>,
    #[serde(default)]
    #[serde(rename = "pageType")]
    page_type: Option<RawPageType>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    #[serde(rename = "editTime")]
    edit_time: Option<RawEditTime>,
    #[serde(default)]
    tags: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct RawUid {
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    #[serde(rename = "uidString")]
    uid_string: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawPageType {
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawEditTime {
    #[serde(default)]
    time: Option<RawTimeValue>,
}

#[derive(Debug, Deserialize)]
struct RawTimeValue {
    #[serde(default)]
    #[serde(rename = "dateAndTimeString")]
    date_and_time_string: Option<String>,
}

fn parse_page_meta(path: &Path) -> Result<PageMeta> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let raw: RawMeta =
        serde_json::from_reader(reader).with_context(|| "failed to decode page metadata")?;

    let id = raw
        .uid
        .as_ref()
        .and_then(|u| u.uuid.clone().or_else(|| u.uid_string.clone()))
        .unwrap_or_default();
    let title = raw
        .page_type
        .and_then(|pt| pt.title)
        .or(raw.title)
        .unwrap_or_default();
    let updated_at = raw
        .edit_time
        .and_then(|e| e.time)
        .and_then(|t| t.date_and_time_string)
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok());
    let tags = parse_tags(raw.tags.as_ref());

    Ok(PageMeta {
        id,
        title,
        path: path.to_path_buf(),
        updated_at,
        tags,
    })
}

fn parse_tags(value: Option<&Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.as_str()
                    .map(ToOwned::to_owned)
                    .or_else(|| {
                        item.get("name")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    })
                    .or_else(|| {
                        item.get("title")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    })
            })
            .collect(),
        Value::Object(obj) => obj
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| {
                        i.get("title")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn parse_item_recursive(item: &Value, out: &mut Vec<Node>) {
    let typ = extract_type(item);
    out.push(parse_node(item));
    if matches!(typ.as_deref(), Some("listSnippet")) {
        // list snippets already materialize children into Node::List items.
        return;
    }
    if let Some(children) = item
        .get("children")
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
    {
        for child in children {
            parse_item_recursive(child, out);
        }
    }
}

fn parse_node(item: &Value) -> Node {
    let typ = extract_type(item);

    match typ.as_deref() {
        Some("textSnippet") => parse_text_like_node(item),
        Some("quoteSnippet") | Some("blockQuoteSnippet") | Some("commentSnippet") => Node::Quote {
            text: extract_text(item).unwrap_or_default(),
        },
        Some("listSnippet") => parse_list_node(item),
        Some(
            t @ ("pharoSnippet"
            | "pythonSnippet"
            | "javascriptSnippet"
            | "shellCommandSnippet"
            | "gemstoneSnippet"
            | "exampleSnippet"
            | "changesSnippet"
            | "robocoderMetamodelSnippet"),
        ) => Node::Code {
            language: infer_language(Some(t)),
            code: extract_code(item)
                .or_else(|| extract_text(item))
                .unwrap_or_default(),
        },
        Some(t @ "pharoLinkSnippet") if has_link(item) => Node::Link {
            text: extract_text(item).unwrap_or_else(|| t.to_string()),
            url: extract_link(item).unwrap_or_default(),
        },
        Some("linkSnippet") if has_link(item) => Node::Link {
            text: extract_text(item).unwrap_or_else(|| "link".to_string()),
            url: extract_link(item).unwrap_or_default(),
        },
        Some(t) => Node::Unknown {
            typ: t.to_string(),
            raw: item.clone(),
        },
        None => Node::Unknown {
            typ: "<missing-type>".to_string(),
            raw: item.clone(),
        },
    }
}

fn parse_text_like_node(item: &Value) -> Node {
    let text = extract_text(item).unwrap_or_default();
    if let Some((level, heading)) = parse_heading(&text) {
        Node::Heading {
            level,
            text: heading,
        }
    } else if let Some(stripped) = text.strip_prefix("> ") {
        Node::Quote {
            text: stripped.to_string(),
        }
    } else if text.trim().is_empty() {
        Node::Text { text }
    } else {
        Node::Paragraph { text }
    }
}

fn parse_list_node(item: &Value) -> Node {
    let mut items = Vec::new();
    if let Some(children) = item
        .get("children")
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
    {
        for child in children {
            items.push(vec![parse_node(child)]);
        }
    }
    Node::List { items }
}

fn parse_heading(input: &str) -> Option<(u8, String)> {
    let trimmed = input.trim();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes == 0 {
        return None;
    }
    let rest = trimmed[hashes..].trim_start();
    if rest.is_empty() {
        return None;
    }
    Some((hashes.min(6) as u8, rest.to_string()))
}

fn extract_type(item: &Value) -> Option<String> {
    item.get("type")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            item.get("__type")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn extract_text(item: &Value) -> Option<String> {
    item.get("string")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            item.get("text")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            item.get("content")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn extract_code(item: &Value) -> Option<String> {
    item.get("code")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            item.get("source")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn extract_link(item: &Value) -> Option<String> {
    item.get("url")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            item.get("href")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn has_link(item: &Value) -> bool {
    item.get("url").and_then(Value::as_str).is_some()
        || item.get("href").and_then(Value::as_str).is_some()
}

fn infer_language(typ: Option<&str>) -> Option<String> {
    let typ = typ?;
    match typ {
        "pharoSnippet" => Some("pharo".to_string()),
        "pythonSnippet" => Some("python".to_string()),
        "javascriptSnippet" => Some("javascript".to_string()),
        "jsonSnippet" => Some("json".to_string()),
        "yamlSnippet" => Some("yaml".to_string()),
        _ => {
            if typ.ends_with("Snippet") {
                Some(typ.trim_end_matches("Snippet").to_lowercase())
            } else {
                None
            }
        }
    }
}

/// Renders a parsed page to plain text.
pub fn render_page_to_text(page: &Page) -> String {
    render_nodes_to_text(&page.content)
}

/// Renders normalized nodes to plain text.
pub fn render_nodes_to_text(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Heading { level, text } => {
                out.push_str(&"#".repeat((*level).max(1) as usize));
                out.push(' ');
                out.push_str(text);
                out.push_str("\n\n");
            }
            Node::Paragraph { text } => {
                out.push_str(text);
                out.push_str("\n\n");
            }
            Node::Text { text } => {
                out.push_str(text);
                out.push('\n');
            }
            Node::List { items } => {
                for item in items {
                    out.push_str("- ");
                    out.push_str(render_nodes_to_text(item).trim());
                    out.push('\n');
                }
                out.push('\n');
            }
            Node::Code { language, code } => {
                out.push_str("```");
                if let Some(lang) = language {
                    out.push_str(lang);
                }
                out.push('\n');
                out.push_str(code);
                out.push_str("\n```\n\n");
            }
            Node::Link { text, url } => {
                out.push_str(&format!("[{text}]({url})\n\n"));
            }
            Node::Quote { text } => {
                out.push_str(&format!("> {text}\n\n"));
            }
            Node::Unknown { typ, .. } => {
                out.push_str(&format!("[[unknown: {typ}]]\n\n"));
            }
        }
    }
    out
}

/// Collects all observed `type`/`__type` values and their counts in one page file.
pub fn collect_node_types_in_file(path: &Path) -> Result<HashMap<String, usize>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let raw: Value = serde_json::from_reader(reader).with_context(|| "failed to decode JSON")?;

    let mut out = HashMap::new();
    collect_node_types_value(&raw, &mut out);
    Ok(out)
}

fn collect_node_types_value(value: &Value, out: &mut HashMap<String, usize>) {
    match value {
        Value::Object(map) => {
            if let Some(typ) = map
                .get("type")
                .and_then(Value::as_str)
                .or_else(|| map.get("__type").and_then(Value::as_str))
            {
                *out.entry(typ.to_string()).or_insert(0) += 1;
            }
            for v in map.values() {
                collect_node_types_value(v, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_node_types_value(item, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file_path(name: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("lepiter-core-{name}-{ts}.lepiter"))
    }

    #[test]
    fn parse_heading_detects_markdown_style() {
        assert_eq!(
            parse_heading("## Heading"),
            Some((2, "Heading".to_string()))
        );
        assert_eq!(parse_heading("No heading"), None);
    }

    #[test]
    fn parse_tags_supports_array_and_object_items() {
        let arr = json!(["a", {"name": "b"}, {"title": "c"}]);
        assert_eq!(parse_tags(Some(&arr)), vec!["a", "b", "c"]);

        let obj = json!({"items": [{"title":"x"}, {"title":"y"}]});
        assert_eq!(parse_tags(Some(&obj)), vec!["x", "y"]);
    }

    #[test]
    fn parse_node_covers_known_and_unknown_types() {
        let heading = json!({"__type":"textSnippet","string":"# Title"});
        assert!(matches!(parse_node(&heading), Node::Heading { .. }));

        let quote = json!({"__type":"blockQuoteSnippet","string":"quoted"});
        assert!(matches!(parse_node(&quote), Node::Quote { .. }));

        let code = json!({"__type":"pythonSnippet","code":"print(1)"});
        assert!(matches!(parse_node(&code), Node::Code { .. }));

        let link = json!({"__type":"pharoLinkSnippet","string":"link","url":"page:abc"});
        assert!(matches!(parse_node(&link), Node::Link { .. }));

        let list = json!({
            "__type":"listSnippet",
            "children":{"items":[{"__type":"textSnippet","string":"item"}]}
        });
        assert!(matches!(parse_node(&list), Node::List { .. }));

        let unknown = json!({"__type":"mysterySnippet","x":1});
        assert!(matches!(parse_node(&unknown), Node::Unknown { .. }));

        let missing = json!({"x":1});
        assert!(matches!(parse_node(&missing), Node::Unknown { .. }));
    }

    #[test]
    fn infer_language_maps_common_snippet_types() {
        assert_eq!(
            infer_language(Some("pharoSnippet")),
            Some("pharo".to_string())
        );
        assert_eq!(
            infer_language(Some("javascriptSnippet")),
            Some("javascript".to_string())
        );
        assert_eq!(
            infer_language(Some("yamlSnippet")),
            Some("yaml".to_string())
        );
        assert_eq!(
            infer_language(Some("customSnippet")),
            Some("custom".to_string())
        );
        assert_eq!(infer_language(None), None);
    }

    #[test]
    fn render_nodes_outputs_unknown_placeholder() {
        let text = render_nodes_to_text(&[
            Node::Paragraph {
                text: "para".to_string(),
            },
            Node::Unknown {
                typ: "weird".to_string(),
                raw: json!({"a":1}),
            },
        ]);
        assert!(text.contains("para"));
        assert!(text.contains("[[unknown: weird]]"));
    }

    #[test]
    fn collect_node_types_counts_nested_values() -> Result<()> {
        let path = temp_file_path("types");
        let content = json!({
            "__type":"page",
            "children":{"__type":"snippets","items":[
                {"__type":"textSnippet","children":{"__type":"snippets","items":[]}},
                {"__type":"pythonSnippet","code":"print(1)"}
            ]}
        });
        fs::write(&path, serde_json::to_vec(&content)?)?;
        let counts = collect_node_types_in_file(&path)?;
        fs::remove_file(&path)?;

        assert_eq!(counts.get("page"), Some(&1));
        assert_eq!(counts.get("textSnippet"), Some(&1));
        assert_eq!(counts.get("pythonSnippet"), Some(&1));
        Ok(())
    }

    #[test]
    fn parse_page_meta_extracts_core_fields() -> Result<()> {
        let path = temp_file_path("meta");
        let content = json!({
            "uid":{"uuid":"id-123"},
            "pageType":{"title":"Title"},
            "editTime":{"time":{"dateAndTimeString":"2024-01-01T00:00:00+00:00"}},
            "tags":["t1","t2"]
        });
        fs::write(&path, serde_json::to_vec(&content)?)?;
        let meta = parse_page_meta(&path)?;
        fs::remove_file(&path)?;

        assert_eq!(meta.id, "id-123");
        assert_eq!(meta.title, "Title");
        assert_eq!(meta.tags, vec!["t1", "t2"]);
        assert!(meta.updated_at.is_some());
        Ok(())
    }

    #[test]
    fn parse_item_recursive_includes_children() {
        let root = json!({
            "__type":"textSnippet",
            "string":"parent",
            "children":{"items":[
                {"__type":"textSnippet","string":"child"}
            ]}
        });
        let mut out = Vec::new();
        parse_item_recursive(&root, &mut out);
        assert_eq!(out.len(), 2);
    }
}
