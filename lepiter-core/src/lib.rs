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
    /// Rewrite block (search/replace transformation).
    Rewrite {
        language: Option<String>,
        search: String,
        replace: String,
        scope: Option<String>,
        is_method_pattern: Option<bool>,
    },
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

/// Match category for search results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMatchKind {
    /// Match came from page metadata (title/id/tags).
    Meta,
    /// Match came from rendered page content.
    Content,
}

/// Search result entry for one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Canonical page id.
    pub id: PageId,
    /// How this page matched.
    pub kind: SearchMatchKind,
}

/// Classification of a raw link target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTargetKind {
    /// Resolved to an internal page id.
    InternalPage(PageId),
    /// Resolved to an attachment file path in the knowledge base.
    AttachmentPath(PathBuf),
    /// Resolved to an external URL/scheme target.
    ExternalUrl(String),
    /// Could not classify target.
    Unknown(String),
}

/// Result of resolving a page by title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleResolution {
    /// A unique page id was resolved.
    Unique(PageId),
    /// No matching title found.
    NotFound,
    /// Multiple candidate page ids matched.
    Ambiguous(Vec<PageId>),
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

    /// Returns page ids filtered by metadata query (title/id/tags), sorted by title.
    pub fn filter_page_ids(&self, query: &str) -> Vec<PageId> {
        let needle = query.trim().to_lowercase();
        let mut metas = self.sorted_pages_by_title();
        if !needle.is_empty() {
            metas.retain(|m| page_meta_matches(m, &needle));
        }
        metas.into_iter().map(|m| m.id.clone()).collect()
    }

    /// Searches pages by metadata and optionally content, returning sorted hits.
    pub fn search_hits(&self, query: &str, include_content: bool) -> Vec<SearchHit> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }

        let mut by_id: HashMap<PageId, SearchMatchKind> = HashMap::new();
        let metas = self.sorted_pages_by_title();

        for meta in &metas {
            if page_meta_matches(meta, &needle) {
                by_id.insert(meta.id.clone(), SearchMatchKind::Meta);
            }
        }

        if include_content {
            for meta in &metas {
                if by_id.contains_key(&meta.id) {
                    continue;
                }
                let Ok(page) = self.load_page(&meta.id) else {
                    continue;
                };
                if render_page_to_text(&page).to_lowercase().contains(&needle) {
                    by_id.insert(meta.id.clone(), SearchMatchKind::Content);
                }
            }
        }

        let mut hits = Vec::new();
        for meta in metas {
            if let Some(kind) = by_id.get(&meta.id) {
                hits.push(SearchHit {
                    id: meta.id.clone(),
                    kind: *kind,
                });
            }
        }
        hits
    }

    /// Resolves a page id from title using case-insensitive exact match, then partial match.
    pub fn resolve_page_id_by_title(&self, title: &str) -> TitleResolution {
        let needle = title.trim().to_lowercase();
        if needle.is_empty() {
            return TitleResolution::NotFound;
        }

        let exact = self
            .sorted_pages_by_title()
            .into_iter()
            .filter(|m| m.title.to_lowercase() == needle)
            .map(|m| m.id.clone())
            .collect::<Vec<_>>();
        match exact.len() {
            1 => return TitleResolution::Unique(exact[0].clone()),
            n if n > 1 => return TitleResolution::Ambiguous(exact),
            _ => {}
        }

        let partial = self
            .sorted_pages_by_title()
            .into_iter()
            .filter(|m| m.title.to_lowercase().contains(&needle))
            .map(|m| m.id.clone())
            .collect::<Vec<_>>();
        match partial.len() {
            1 => TitleResolution::Unique(partial[0].clone()),
            0 => TitleResolution::NotFound,
            _ => TitleResolution::Ambiguous(partial),
        }
    }

    /// Classifies a raw link target for navigation/open behavior.
    pub fn classify_link_target(&self, raw: &str) -> LinkTargetKind {
        let target = raw.trim();
        if target.is_empty() {
            return LinkTargetKind::Unknown(raw.to_string());
        }

        if self.pages.contains_key(target) {
            return LinkTargetKind::InternalPage(target.to_string());
        }

        if let Some(rest) = target.strip_prefix("page:") {
            let id = rest.trim();
            if self.pages.contains_key(id) {
                return LinkTargetKind::InternalPage(id.to_string());
            }
        }
        if let Some(rest) = target.strip_prefix("title:") {
            return match self.resolve_page_id_by_title(rest.trim()) {
                TitleResolution::Unique(id) => LinkTargetKind::InternalPage(id),
                _ => LinkTargetKind::Unknown(target.to_string()),
            };
        }

        if let Some(uuid) = extract_uuid_like(target)
            && self.pages.contains_key(uuid)
        {
            return LinkTargetKind::InternalPage(uuid.to_string());
        }

        if is_external_target(target) {
            return LinkTargetKind::ExternalUrl(target.to_string());
        }

        if let Some(rel) = attachment_relative_path(target) {
            return LinkTargetKind::AttachmentPath(self.root.join(rel));
        }

        match self.resolve_page_id_by_title(target) {
            TitleResolution::Unique(id) => LinkTargetKind::InternalPage(id),
            _ => LinkTargetKind::Unknown(target.to_string()),
        }
    }

    /// Returns the root path used to build this index.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn page_meta_matches(meta: &PageMeta, needle: &str) -> bool {
    meta.title.to_lowercase().contains(needle)
        || meta.id.to_lowercase().contains(needle)
        || meta.tags.iter().any(|t| t.to_lowercase().contains(needle))
}

fn is_external_target(target: &str) -> bool {
    let lower = target.to_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with("file://")
        || lower.contains("://")
}

fn attachment_relative_path(target: &str) -> Option<&str> {
    if let Some(rest) = target.strip_prefix("attachments/") {
        return Some(rest).map(|_| target);
    }
    if let Some(pos) = target.find("/attachments/") {
        let start = pos + 1;
        return target.get(start..);
    }
    if let Some(pos) = target.find("attachments/") {
        return target.get(pos..);
    }
    None
}

fn extract_uuid_like(input: &str) -> Option<&str> {
    let bytes = input.as_bytes();
    if bytes.len() < 36 {
        return None;
    }

    for i in 0..=bytes.len() - 36 {
        let cand = &input[i..i + 36];
        let ok = cand.chars().enumerate().all(|(idx, c)| match idx {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        });
        if ok {
            return Some(cand);
        }
    }
    None
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
        Some("pictureSnippet") => parse_picture_node(item),
        Some("youtubeSnippet") => parse_youtube_node(item),
        Some("elementSnippet") => parse_element_node(item),
        Some("pharoRewrite") => parse_rewrite_node(item),
        Some("wordSnippet") => parse_word_node(item),
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

fn parse_picture_node(item: &Value) -> Node {
    let url = item
        .get("url")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| extract_link(item))
        .unwrap_or_default();
    let text = item
        .get("caption")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| extract_text(item))
        .unwrap_or_else(|| "picture".to_string());

    if url.is_empty() {
        Node::Unknown {
            typ: "pictureSnippet".to_string(),
            raw: item.clone(),
        }
    } else {
        Node::Link { text, url }
    }
}

fn parse_youtube_node(item: &Value) -> Node {
    let url = item
        .get("youtubeUrl")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| extract_link(item))
        .unwrap_or_default();
    let text = extract_text(item).unwrap_or_else(|| "youtube".to_string());

    if url.is_empty() {
        Node::Unknown {
            typ: "youtubeSnippet".to_string(),
            raw: item.clone(),
        }
    } else {
        Node::Link { text, url }
    }
}

fn parse_element_node(item: &Value) -> Node {
    let code = extract_code(item).or_else(|| extract_text(item));
    if let Some(code) = code.filter(|c| !c.trim().is_empty()) {
        Node::Code {
            language: Some("element".to_string()),
            code,
        }
    } else {
        Node::Unknown {
            typ: "elementSnippet".to_string(),
            raw: item.clone(),
        }
    }
}

fn parse_rewrite_node(item: &Value) -> Node {
    let search = item
        .get("search")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    let replace = item
        .get("replace")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    let scope = item
        .get("scope")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let is_method_pattern = item.get("isMethodPattern").and_then(Value::as_bool);

    if search.is_empty() && replace.is_empty() {
        Node::Unknown {
            typ: "pharoRewrite".to_string(),
            raw: item.clone(),
        }
    } else {
        Node::Rewrite {
            language: Some("pharo".to_string()),
            search,
            replace,
            scope,
            is_method_pattern,
        }
    }
}

fn parse_word_node(item: &Value) -> Node {
    let mut lines = Vec::new();

    if let Some(word) = item
        .get("wordString")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        lines.push(word.to_string());
    }

    if let Some(explanation) = item
        .get("explanationAttachmentNameString")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        lines.push(format!("explanation: {explanation}"));
    }

    if lines.is_empty() {
        collect_text_fragments(item, &mut lines, 0, 12);
    }

    lines.retain(|s| !s.trim().is_empty());
    lines.truncate(8);

    if lines.is_empty() {
        return Node::Unknown {
            typ: "wordSnippet".to_string(),
            raw: item.clone(),
        };
    }

    let mut text = lines.join("\n");
    if text.chars().count() > 1200 {
        text = text.chars().take(1199).collect::<String>();
        text.push('…');
    }

    Node::Paragraph { text }
}

fn collect_text_fragments(value: &Value, out: &mut Vec<String>, depth: usize, remaining: usize) {
    if remaining == 0 || out.len() >= remaining || depth > 4 {
        return;
    }

    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_string());
            }
        }
        Value::Array(items) => {
            for item in items {
                if out.len() >= remaining {
                    break;
                }
                collect_text_fragments(item, out, depth + 1, remaining);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                if matches!(
                    key.as_str(),
                    "__type"
                        | "children"
                        | "uid"
                        | "createEmail"
                        | "createTime"
                        | "editEmail"
                        | "editTime"
                        | "paragraphStyle"
                ) {
                    continue;
                }
                if out.len() >= remaining {
                    break;
                }
                collect_text_fragments(item, out, depth + 1, remaining);
            }
        }
        _ => {}
    }
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
            Node::Rewrite {
                language,
                search,
                replace,
                scope,
                is_method_pattern,
            } => {
                let lang = language.clone().unwrap_or_else(|| "rewrite".to_string());
                out.push_str(&format!("```diff {lang}\n"));
                if let Some(scope) = scope {
                    out.push_str(&format!("# scope: {scope}\n"));
                }
                if let Some(is_method_pattern) = is_method_pattern {
                    out.push_str(&format!("# method_pattern: {is_method_pattern}\n"));
                }
                for line in normalize_text(search).lines() {
                    out.push('-');
                    out.push_str(line);
                    out.push('\n');
                }
                for line in normalize_text(replace).lines() {
                    out.push('+');
                    out.push_str(line);
                    out.push('\n');
                }
                out.push_str("```\n\n");
            }
            Node::Unknown { typ, .. } => {
                out.push_str(&format!("[[unknown: {typ}]]\n\n"));
            }
        }
    }
    out
}

fn normalize_text(input: &str) -> String {
    input.replace("\r\n", "\n").replace('\r', "\n")
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

        let picture = json!({"__type":"pictureSnippet","url":"attachments/x.png","caption":"img"});
        assert!(matches!(parse_node(&picture), Node::Link { .. }));

        let youtube = json!({"__type":"youtubeSnippet","youtubeUrl":"https://youtu.be/abc"});
        assert!(matches!(parse_node(&youtube), Node::Link { .. }));

        let element = json!({"__type":"elementSnippet","code":"GtInspector newOn: 42"});
        assert!(matches!(parse_node(&element), Node::Code { .. }));

        let rewrite =
            json!({"__type":"pharoRewrite","search":"a","replace":"b","isMethodPattern":true});
        assert!(matches!(parse_node(&rewrite), Node::Rewrite { .. }));

        let word = json!({"__type":"wordSnippet","wordString":"refactoring"});
        assert!(matches!(parse_node(&word), Node::Paragraph { .. }));

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
            Node::Rewrite {
                language: Some("pharo".to_string()),
                search: "a".to_string(),
                replace: "b".to_string(),
                scope: None,
                is_method_pattern: Some(true),
            },
            Node::Unknown {
                typ: "weird".to_string(),
                raw: json!({"a":1}),
            },
        ]);
        assert!(text.contains("para"));
        assert!(text.contains("```diff pharo"));
        assert!(text.contains("-a"));
        assert!(text.contains("+b"));
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

    #[test]
    fn filter_page_ids_matches_title_id_and_tags() {
        let mut pages = HashMap::new();
        pages.insert(
            "id-1".to_string(),
            PageMeta {
                id: "id-1".to_string(),
                title: "Alpha".to_string(),
                path: PathBuf::from("/tmp/a"),
                updated_at: None,
                tags: vec!["rust".to_string()],
            },
        );
        pages.insert(
            "id-2".to_string(),
            PageMeta {
                id: "id-2".to_string(),
                title: "Beta".to_string(),
                path: PathBuf::from("/tmp/b"),
                updated_at: None,
                tags: vec!["pharo".to_string()],
            },
        );
        let index = KnowledgeBaseIndex {
            root: PathBuf::from("/tmp"),
            pages,
            index_issues: Vec::new(),
        };

        assert_eq!(index.filter_page_ids("alpha"), vec!["id-1".to_string()]);
        assert_eq!(index.filter_page_ids("id-2"), vec!["id-2".to_string()]);
        assert_eq!(index.filter_page_ids("pharo"), vec!["id-2".to_string()]);
        assert_eq!(
            index.filter_page_ids(""),
            vec!["id-1".to_string(), "id-2".to_string()]
        );
    }

    #[test]
    fn resolve_page_id_by_title_handles_unique_ambiguous_and_missing() {
        let mut pages = HashMap::new();
        pages.insert(
            "id-1".to_string(),
            PageMeta {
                id: "id-1".to_string(),
                title: "Alpha".to_string(),
                path: PathBuf::from("/tmp/a"),
                updated_at: None,
                tags: Vec::new(),
            },
        );
        pages.insert(
            "id-2".to_string(),
            PageMeta {
                id: "id-2".to_string(),
                title: "Alphabet".to_string(),
                path: PathBuf::from("/tmp/b"),
                updated_at: None,
                tags: Vec::new(),
            },
        );
        let index = KnowledgeBaseIndex {
            root: PathBuf::from("/tmp"),
            pages,
            index_issues: Vec::new(),
        };

        assert_eq!(
            index.resolve_page_id_by_title("Alpha"),
            TitleResolution::Unique("id-1".to_string())
        );
        assert!(matches!(
            index.resolve_page_id_by_title("alp"),
            TitleResolution::Ambiguous(_)
        ));
        assert_eq!(
            index.resolve_page_id_by_title("zzz"),
            TitleResolution::NotFound
        );
    }

    #[test]
    fn classify_link_target_covers_internal_attachment_external_unknown() {
        let mut pages = HashMap::new();
        pages.insert(
            "8a505fa0-2222-3333-4444-555555555555".to_string(),
            PageMeta {
                id: "8a505fa0-2222-3333-4444-555555555555".to_string(),
                title: "Alpha".to_string(),
                path: PathBuf::from("/tmp/a"),
                updated_at: None,
                tags: Vec::new(),
            },
        );
        let index = KnowledgeBaseIndex {
            root: PathBuf::from("/kb"),
            pages,
            index_issues: Vec::new(),
        };

        assert!(matches!(
            index.classify_link_target("8a505fa0-2222-3333-4444-555555555555"),
            LinkTargetKind::InternalPage(_)
        ));
        assert!(matches!(
            index.classify_link_target("title:alpha"),
            LinkTargetKind::InternalPage(_)
        ));
        assert!(matches!(
            index.classify_link_target("go to 8a505fa0-2222-3333-4444-555555555555 now"),
            LinkTargetKind::InternalPage(_)
        ));
        assert!(matches!(
            index.classify_link_target("attachments/image.png"),
            LinkTargetKind::AttachmentPath(_)
        ));
        assert!(matches!(
            index.classify_link_target("https://example.com"),
            LinkTargetKind::ExternalUrl(_)
        ));
        assert!(matches!(
            index.classify_link_target("not a thing"),
            LinkTargetKind::Unknown(_)
        ));
    }

    #[test]
    fn parse_word_node_extracts_primary_fields() {
        let item = json!({
            "__type":"wordSnippet",
            "wordString":"refactoring",
            "explanationAttachmentNameString":"attachments/x/explanation.json"
        });
        let node = parse_node(&item);
        match node {
            Node::Paragraph { text } => {
                assert!(text.contains("refactoring"));
                assert!(text.contains("attachments/x/explanation.json"));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }
}
