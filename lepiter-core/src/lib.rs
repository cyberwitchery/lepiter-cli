//! core data model and parser for lepiter knowledge bases stored as page json files.
//!
//! # scope
//! - scans a lepiter directory and builds a metadata index keyed by page id.
//! - loads and parses individual pages lazily by id.
//! - converts page snippet trees into a stable block-oriented node model.
//! - preserves unknown node types as [`Node::Unknown`] to keep consumers resilient.
//! - provides a plugin sdk for external snippet renderers (`plugin` module).
//!
//! # example
//! ```no_run
//! use lepiter_core::KnowledgeBase;
//!
//! # fn main() -> anyhow::Result<()> {
//! let index = KnowledgeBase::open("./lepiter")?;
//! for page in index.sorted_pages() {
//!     println!("{} - {}", page.id, page.title);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # plugin sdk
//! ```no_run
//! use lepiter_core::plugin::{PluginRequest, PluginResponse};
//! use lepiter_core::lepiter_plugin_main;
//!
//! fn handle(req: PluginRequest) -> PluginResponse {
//!     if req.typ != "wardleyMap" {
//!         return PluginResponse::error("unsupported type");
//!     }
//!     PluginResponse::ok(vec!["example".to_string()])
//! }
//!
//! lepiter_plugin_main!(handle);
//! ```

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use walkdir::WalkDir;

pub mod plugin;

#[macro_export]
macro_rules! lepiter_plugin_main {
    ($handler:path) => {
        fn main() -> std::io::Result<()> {
            $crate::plugin::plugin_loop($handler)
        }
    };
}

/// Canonical page identifier used throughout the API.
pub type PageId = String;

/// Metadata for a page discovered during index scanning.
#[derive(Debug, Clone)]
pub struct PageMeta {
    /// Canonical page id (preferred key over filename).
    pub id: PageId,
    /// Human-readable page title.
    pub title: String,
    /// Pre-computed lowercased title for case-insensitive comparisons.
    pub title_lower: String,
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

/// Parses a single raw snippet JSON value into a [`Node`].
///
/// This is a best-effort conversion that preserves unknown snippet types as
/// [`Node::Unknown`]. It is intended for tooling that operates on raw JSON
/// without loading a full page.
pub fn parse_node_from_raw(item: &Value) -> Node {
    parse_node(item)
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

/// Resolved attachment target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAttachment {
    /// Full path to the attachment.
    pub path: PathBuf,
    /// Whether the attachment exists on disk.
    pub exists: bool,
}

/// Attachment resolution failures.
#[derive(Debug, Error)]
pub enum AttachmentError {
    #[error("attachment target was empty")]
    Empty,
    #[error("attachment target not recognized: {0}")]
    NotAttachment(String),
    #[error("attachment path escapes knowledge base root: {0}")]
    EscapesRoot(String),
    #[error("attachment not found: {0}")]
    Missing(PathBuf),
}

type AttachmentResult<T> = std::result::Result<T, AttachmentError>;

/// Resolves attachment targets relative to the knowledge base root.
#[derive(Debug, Clone)]
pub struct AttachmentResolver {
    root: PathBuf,
}

impl AttachmentResolver {
    /// Creates a resolver rooted at the knowledge base path.
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Resolves an attachment target to a path and existence flag.
    pub fn resolve(&self, raw: &str) -> AttachmentResult<ResolvedAttachment> {
        let target = raw.trim();
        if target.is_empty() {
            return Err(AttachmentError::Empty);
        }
        let rel = extract_attachment_relative(target)
            .ok_or_else(|| AttachmentError::NotAttachment(target.to_string()))?;
        let rel = sanitize_relative_path(rel)?;
        let path = self.root.join(rel);
        let exists = path.exists();
        Ok(ResolvedAttachment { path, exists })
    }

    /// Resolves an attachment target to a path only (ignores missing).
    pub fn resolve_path(&self, raw: &str) -> Option<PathBuf> {
        self.resolve(raw).ok().map(|resolved| resolved.path)
    }

    /// Resolves an attachment target and ensures the file exists.
    pub fn resolve_existing(&self, raw: &str) -> AttachmentResult<PathBuf> {
        let resolved = self.resolve(raw)?;
        if resolved.exists {
            Ok(resolved.path)
        } else {
            Err(AttachmentError::Missing(resolved.path))
        }
    }

    /// Returns the resolver root.
    pub fn root(&self) -> &Path {
        &self.root
    }
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
    /// Page ids in case-insensitive title sort order, computed once at open time.
    pub sorted_ids: Vec<PageId>,
    /// Non-fatal issues encountered while scanning metadata.
    pub index_issues: Vec<ParseIssue>,
    /// Reverse link index: target page id -> sorted list of source page ids that link to it.
    backlinks: HashMap<PageId, Vec<PageId>>,
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

        let sorted_ids = compute_sorted_ids(&pages);

        Ok(KnowledgeBaseIndex {
            root,
            pages,
            sorted_ids,
            index_issues: issues,
            backlinks: HashMap::new(),
        })
    }
}

impl KnowledgeBaseIndex {
    /// Registers a new page in the index and re-sorts the id list.
    pub fn register_page(&mut self, meta: PageMeta) {
        self.pages.insert(meta.id.clone(), meta);
        self.sorted_ids = compute_sorted_ids(&self.pages);
    }

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

    /// Returns metadata entries in cached title-sorted order.
    pub fn sorted_pages(&self) -> Vec<&PageMeta> {
        self.sorted_ids
            .iter()
            .filter_map(|id| self.pages.get(id))
            .collect()
    }

    /// Returns page ids filtered by metadata query (title/id/tags), sorted by title.
    pub fn filter_page_ids(&self, query: &str) -> Vec<PageId> {
        let needle = query.trim().to_lowercase();
        let mut metas = self.sorted_pages();
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
        let metas = self.sorted_pages();

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

        let sorted = self.sorted_pages();

        let exact = sorted
            .iter()
            .filter(|m| m.title_lower == needle)
            .map(|m| m.id.clone())
            .collect::<Vec<_>>();
        match exact.len() {
            1 => return TitleResolution::Unique(exact[0].clone()),
            n if n > 1 => return TitleResolution::Ambiguous(exact),
            _ => {}
        }

        let partial = sorted
            .iter()
            .filter(|m| m.title_lower.contains(&needle))
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
            if let TitleResolution::Unique(resolved) = self.resolve_page_id_by_title(id) {
                return LinkTargetKind::InternalPage(resolved);
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

        if let Some(path) = self.attachment_resolver().resolve_path(target) {
            return LinkTargetKind::AttachmentPath(path);
        }

        match self.resolve_page_id_by_title(target) {
            TitleResolution::Unique(id) => LinkTargetKind::InternalPage(id),
            _ => LinkTargetKind::Unknown(target.to_string()),
        }
    }

    /// Builds the reverse link index by loading every page, extracting link
    /// targets, classifying them, and recording which pages link to which.
    ///
    /// Call this once after [`KnowledgeBase::open`] when backlink data is needed.
    pub fn build_backlinks(&mut self) {
        let mut back: HashMap<PageId, HashSet<PageId>> = HashMap::new();
        let ids: Vec<PageId> = self.sorted_ids.clone();
        for source_id in &ids {
            let Ok(page) = self.load_page(source_id) else {
                continue;
            };
            for target in extract_link_targets(&page.content) {
                if let LinkTargetKind::InternalPage(target_id) = self.classify_link_target(&target)
                    && target_id != *source_id
                {
                    back.entry(target_id).or_default().insert(source_id.clone());
                }
            }
        }
        self.backlinks = back
            .into_iter()
            .map(|(target, sources)| {
                let mut sorted: Vec<PageId> = sources.into_iter().collect();
                sorted.sort_by(|a, b| {
                    let a_lower = self
                        .pages
                        .get(a)
                        .map(|m| m.title_lower.as_str())
                        .unwrap_or("");
                    let b_lower = self
                        .pages
                        .get(b)
                        .map(|m| m.title_lower.as_str())
                        .unwrap_or("");
                    a_lower.cmp(b_lower)
                });
                (target, sorted)
            })
            .collect();
    }

    /// Returns the page ids that link to the given page, sorted by title.
    pub fn backlinks_for(&self, id: &str) -> &[PageId] {
        self.backlinks
            .get(id)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Returns the root path used to build this index.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns an attachment resolver rooted at this knowledge base.
    pub fn attachment_resolver(&self) -> AttachmentResolver {
        AttachmentResolver::new(&self.root)
    }
}

/// Extracts raw link target strings from a node tree.
///
/// Finds explicit `Node::Link` urls plus inline `[label](target)` and `[[wikilink]]`
/// syntax in text-bearing nodes.
pub fn extract_link_targets(nodes: &[Node]) -> Vec<String> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            Node::Link { url, .. } => out.push(url.clone()),
            Node::Paragraph { text }
            | Node::Text { text }
            | Node::Quote { text }
            | Node::Heading { text, .. } => {
                extract_inline_link_targets(text, &mut out);
            }
            Node::List { items } => {
                for item in items {
                    out.extend(extract_link_targets(item));
                }
            }
            _ => {}
        }
    }
    out
}

/// Extracts `[label](target)` and `[[wikilink]]` targets from inline text.
fn extract_inline_link_targets(text: &str, out: &mut Vec<String>) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // [[wikilink]]
        if i + 1 < chars.len() && chars[i] == '[' && chars[i + 1] == '[' {
            let start = i + 2;
            if let Some(end) = find_closing_double_bracket(&chars, start) {
                let target: String = chars[start..end].iter().collect();
                let target = target.trim();
                if !target.is_empty() {
                    out.push(target.to_string());
                }
                i = end + 2;
                continue;
            }
        }
        // [label](target)
        if chars[i] == '[' {
            let label_start = i + 1;
            if let Some(label_end) = find_char(&chars, ']', label_start)
                && label_end + 1 < chars.len()
                && chars[label_end + 1] == '('
            {
                let target_start = label_end + 2;
                if let Some(target_end) = find_char(&chars, ')', target_start) {
                    let target: String = chars[target_start..target_end].iter().collect();
                    let target = target.trim();
                    if !target.is_empty() {
                        out.push(target.to_string());
                    }
                    i = target_end + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
}

fn find_closing_double_bracket(chars: &[char], start: usize) -> Option<usize> {
    let mut i = start;
    while i + 1 < chars.len() {
        if chars[i] == ']' && chars[i + 1] == ']' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_char(chars: &[char], target: char, start: usize) -> Option<usize> {
    (start..chars.len()).find(|&i| chars[i] == target)
}

fn compute_sorted_ids(pages: &HashMap<PageId, PageMeta>) -> Vec<PageId> {
    let mut entries: Vec<_> = pages.values().collect();
    entries.sort_by(|a, b| a.title_lower.cmp(&b.title_lower));
    entries.into_iter().map(|m| m.id.clone()).collect()
}

fn page_meta_matches(meta: &PageMeta, needle: &str) -> bool {
    meta.title_lower.contains(needle)
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

fn extract_attachment_relative(target: &str) -> Option<&str> {
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

fn sanitize_relative_path(rel: &str) -> AttachmentResult<PathBuf> {
    let rel = rel.trim();
    if rel.is_empty() {
        return Err(AttachmentError::Empty);
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return Err(AttachmentError::EscapesRoot(rel.to_string()));
    }
    for comp in path.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err(AttachmentError::EscapesRoot(rel.to_string()));
            }
            _ => {}
        }
    }
    Ok(path.to_path_buf())
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

    let title_lower = title.to_lowercase();
    Ok(PageMeta {
        id,
        title,
        title_lower,
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
    if matches!(typ, Some("listSnippet")) {
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

    match typ {
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
        Some(t) if is_code_snippet(t) => Node::Code {
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

/// Parses a markdown-style heading line, returning the level (1–6) and text.
pub fn parse_heading(input: &str) -> Option<(u8, String)> {
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

/// Extracts the snippet type from a JSON value, checking `"type"` then `"__type"`.
pub fn extract_type(item: &Value) -> Option<&str> {
    item.get("type")
        .and_then(Value::as_str)
        .or_else(|| item.get("__type").and_then(Value::as_str))
}

/// Returns `true` if the given snippet type name is a code snippet.
pub fn is_code_snippet(typ: &str) -> bool {
    matches!(
        typ,
        "pharoSnippet"
            | "pythonSnippet"
            | "javascriptSnippet"
            | "jsonSnippet"
            | "yamlSnippet"
            | "shellCommandSnippet"
            | "gemstoneSnippet"
            | "exampleSnippet"
            | "changesSnippet"
            | "robocoderMetamodelSnippet"
    )
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
                    let rendered = render_nodes_to_text(item);
                    let mut lines = rendered.trim().lines();
                    if let Some(first) = lines.next() {
                        out.push_str("- ");
                        out.push_str(first);
                        out.push('\n');
                        for line in lines {
                            out.push_str("  ");
                            out.push_str(line);
                            out.push('\n');
                        }
                    }
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

pub fn normalize_text(input: &str) -> String {
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
            if let Some(typ) = extract_type(value) {
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

    fn temp_dir_path(name: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("lepiter-core-{name}-{ts}"))
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

        let json_code = json!({"__type":"jsonSnippet","code":"{\"key\": 1}"});
        assert!(matches!(parse_node(&json_code), Node::Code { .. }));

        let yaml_code = json!({"__type":"yamlSnippet","code":"key: value"});
        assert!(matches!(parse_node(&yaml_code), Node::Code { .. }));

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
            infer_language(Some("jsonSnippet")),
            Some("json".to_string())
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
    fn render_list_single_line_items() {
        let text = render_nodes_to_text(&[Node::List {
            items: vec![
                vec![Node::Paragraph {
                    text: "first".to_string(),
                }],
                vec![Node::Paragraph {
                    text: "second".to_string(),
                }],
            ],
        }]);
        assert_eq!(text, "- first\n- second\n\n");
    }

    #[test]
    fn render_list_item_with_code_block() {
        let text = render_nodes_to_text(&[Node::List {
            items: vec![vec![Node::Code {
                language: Some("py".to_string()),
                code: "x = 1\ny = 2".to_string(),
            }]],
        }]);
        assert_eq!(text, "- ```py\n  x = 1\n  y = 2\n  ```\n\n");
    }

    #[test]
    fn render_list_item_with_multiple_nodes() {
        let text = render_nodes_to_text(&[Node::List {
            items: vec![vec![
                Node::Paragraph {
                    text: "intro".to_string(),
                },
                Node::Code {
                    language: None,
                    code: "code".to_string(),
                },
            ]],
        }]);
        // First line starts with "- ", continuation lines with "  "
        let lines: Vec<&str> = text.trim().lines().collect();
        assert_eq!(lines[0], "- intro");
        for line in &lines[1..] {
            assert!(
                line.starts_with("  "),
                "continuation line not indented: {line:?}"
            );
        }
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
                title_lower: "alpha".to_string(),
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
                title_lower: "beta".to_string(),
                path: PathBuf::from("/tmp/b"),
                updated_at: None,
                tags: vec!["pharo".to_string()],
            },
        );
        let sorted_ids = compute_sorted_ids(&pages);
        let index = KnowledgeBaseIndex {
            root: PathBuf::from("/tmp"),
            pages,
            sorted_ids,
            index_issues: Vec::new(),
            backlinks: HashMap::new(),
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
                title_lower: "alpha".to_string(),
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
                title_lower: "alphabet".to_string(),
                path: PathBuf::from("/tmp/b"),
                updated_at: None,
                tags: Vec::new(),
            },
        );
        let sorted_ids = compute_sorted_ids(&pages);
        let index = KnowledgeBaseIndex {
            root: PathBuf::from("/tmp"),
            pages,
            sorted_ids,
            index_issues: Vec::new(),
            backlinks: HashMap::new(),
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
                title_lower: "alpha".to_string(),
                path: PathBuf::from("/tmp/a"),
                updated_at: None,
                tags: Vec::new(),
            },
        );
        let sorted_ids = compute_sorted_ids(&pages);
        let index = KnowledgeBaseIndex {
            root: PathBuf::from("/kb"),
            pages,
            sorted_ids,
            index_issues: Vec::new(),
            backlinks: HashMap::new(),
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
        // page: prefix falls back to title resolution
        assert!(matches!(
            index.classify_link_target("page:Alpha"),
            LinkTargetKind::InternalPage(_)
        ));
        // page: prefix with unknown title stays Unknown
        assert!(matches!(
            index.classify_link_target("page:Nonexistent"),
            LinkTargetKind::Unknown(_)
        ));
    }

    #[test]
    fn attachment_resolver_reports_missing_files() -> Result<()> {
        let root = temp_dir_path("attachments");
        let attachments = root.join("attachments");
        fs::create_dir_all(&attachments)?;
        fs::write(attachments.join("ok.txt"), b"ok")?;

        let resolver = AttachmentResolver::new(&root);
        let resolved = resolver.resolve("attachments/ok.txt")?;
        assert!(resolved.exists);

        let missing = resolver.resolve_existing("attachments/missing.txt");
        assert!(matches!(missing, Err(AttachmentError::Missing(_))));

        fs::remove_dir_all(&root)?;
        Ok(())
    }

    fn make_kb_on_disk(pages: &[(&str, &str, &[&str], &str)]) -> (PathBuf, KnowledgeBaseIndex) {
        let dir = temp_dir_path("kb");
        fs::create_dir_all(&dir).unwrap();
        for (id, title, tags, body_text) in pages {
            let tags_json: Vec<Value> = tags.iter().map(|t| json!(t)).collect();
            let content = json!({
                "uid": {"uuid": id},
                "pageType": {"title": title},
                "tags": tags_json,
                "children": {"items": [
                    {"__type": "textSnippet", "string": body_text}
                ]}
            });
            let file_path = dir.join(format!("{id}.lepiter"));
            fs::write(&file_path, serde_json::to_vec(&content).unwrap()).unwrap();
        }
        let index = KnowledgeBase::open(&dir).unwrap();
        (dir, index)
    }

    #[test]
    fn search_hits_empty_query_returns_nothing() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Alpha", &[], "hello world")]);
        assert!(index.search_hits("", false).is_empty());
        assert!(index.search_hits("  ", true).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_hits_matches_title_case_insensitively() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha Guide", &[], "nothing special"),
            ("p2", "Beta Notes", &[], "nothing special"),
        ]);
        let hits = index.search_hits("alpha", false);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "p1");
        assert_eq!(hits[0].kind, SearchMatchKind::Meta);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_hits_matches_tags() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Page One", &["rust", "cli"], "body"),
            ("p2", "Page Two", &["pharo"], "body"),
        ]);
        let hits = index.search_hits("rust", false);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "p1");
        assert_eq!(hits[0].kind, SearchMatchKind::Meta);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_hits_content_flag_searches_page_body() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "the quick brown fox"),
            ("p2", "Beta", &[], "lazy dog sleeps"),
        ]);

        let no_content = index.search_hits("fox", false);
        assert!(no_content.is_empty());

        let with_content = index.search_hits("fox", true);
        assert_eq!(with_content.len(), 1);
        assert_eq!(with_content[0].id, "p1");
        assert_eq!(with_content[0].kind, SearchMatchKind::Content);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_hits_meta_match_takes_priority_over_content() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Fox Guide", &[], "the fox jumps")]);
        let hits = index.search_hits("fox", true);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, SearchMatchKind::Meta);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_hits_returns_results_sorted_by_title() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Zebra", &["common"], "body"),
            ("p2", "Alpha", &["common"], "body"),
            ("p3", "Middle", &["common"], "body"),
        ]);
        let hits = index.search_hits("common", false);
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["p2", "p3", "p1"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn classify_link_target_page_prefix() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Alpha", &[], "body")]);
        assert!(matches!(
            index.classify_link_target("page:p1"),
            LinkTargetKind::InternalPage(id) if id == "p1"
        ));
        assert!(matches!(
            index.classify_link_target("page:nonexistent"),
            LinkTargetKind::Unknown(_)
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn classify_link_target_empty_is_unknown() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Alpha", &[], "body")]);
        assert!(matches!(
            index.classify_link_target(""),
            LinkTargetKind::Unknown(_)
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn classify_link_target_title_fallback() {
        let (dir, index) = make_kb_on_disk(&[("p1", "My Special Page", &[], "body")]);
        assert!(matches!(
            index.classify_link_target("My Special Page"),
            LinkTargetKind::InternalPage(id) if id == "p1"
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_page_id_by_title_empty_and_whitespace() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Alpha", &[], "body")]);
        assert_eq!(
            index.resolve_page_id_by_title(""),
            TitleResolution::NotFound
        );
        assert_eq!(
            index.resolve_page_id_by_title("   "),
            TitleResolution::NotFound
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_page_id_by_title_case_insensitive_exact() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Alpha", &[], "body")]);
        assert_eq!(
            index.resolve_page_id_by_title("ALPHA"),
            TitleResolution::Unique("p1".to_string())
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn filter_page_ids_no_match_returns_empty() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Alpha", &[], "body")]);
        assert!(index.filter_page_ids("zzzzz").is_empty());
        fs::remove_dir_all(&dir).unwrap();
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

    #[test]
    fn parse_page_meta_missing_uid_uses_empty_id() -> Result<()> {
        let path = temp_file_path("no-uid");
        let content = json!({"pageType": {"title": "Some Title"}});
        fs::write(&path, serde_json::to_vec(&content)?)?;
        let meta = parse_page_meta(&path)?;
        fs::remove_file(&path)?;

        assert!(meta.id.is_empty());
        assert_eq!(meta.title, "Some Title");
        Ok(())
    }

    #[test]
    fn parse_page_meta_missing_page_type_uses_empty_title() -> Result<()> {
        let path = temp_file_path("no-pt");
        let content = json!({"uid": {"uuid": "abc-123"}});
        fs::write(&path, serde_json::to_vec(&content)?)?;
        let meta = parse_page_meta(&path)?;
        fs::remove_file(&path)?;

        assert_eq!(meta.id, "abc-123");
        assert!(meta.title.is_empty());
        Ok(())
    }

    #[test]
    fn parse_page_meta_invalid_date_string_yields_none() -> Result<()> {
        let path = temp_file_path("bad-date");
        let content = json!({
            "uid": {"uuid": "id-1"},
            "editTime": {"time": {"dateAndTimeString": "not-a-date"}}
        });
        fs::write(&path, serde_json::to_vec(&content)?)?;
        let meta = parse_page_meta(&path)?;
        fs::remove_file(&path)?;

        assert!(meta.updated_at.is_none());
        Ok(())
    }

    #[test]
    fn open_empty_directory_returns_empty_index() -> Result<()> {
        let dir = temp_dir_path("empty-kb");
        fs::create_dir_all(&dir)?;
        let index = KnowledgeBase::open(&dir)?;
        fs::remove_dir_all(&dir)?;

        assert!(index.pages.is_empty());
        assert!(index.index_issues.is_empty());
        Ok(())
    }

    #[test]
    fn open_skips_non_lepiter_files() -> Result<()> {
        let dir = temp_dir_path("non-lepiter");
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("readme.txt"), b"hello")?;
        fs::write(dir.join("data.json"), b"{}")?;
        let index = KnowledgeBase::open(&dir)?;
        fs::remove_dir_all(&dir)?;

        assert!(index.pages.is_empty());
        assert!(index.index_issues.is_empty());
        Ok(())
    }

    #[test]
    fn open_reports_invalid_json_as_issue() -> Result<()> {
        let dir = temp_dir_path("bad-json");
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("broken.lepiter"), b"not json at all")?;
        let index = KnowledgeBase::open(&dir)?;
        fs::remove_dir_all(&dir)?;

        assert!(index.pages.is_empty());
        assert_eq!(index.index_issues.len(), 1);
        assert!(index.index_issues[0].message.contains("failed to decode"));
        Ok(())
    }

    #[test]
    fn open_reports_wrong_json_structure_as_issue() -> Result<()> {
        let dir = temp_dir_path("wrong-shape");
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("array.lepiter"), b"[1, 2, 3]")?;
        let index = KnowledgeBase::open(&dir)?;
        fs::remove_dir_all(&dir)?;

        assert!(index.pages.is_empty());
        assert_eq!(index.index_issues.len(), 1);
        Ok(())
    }

    #[test]
    fn open_fills_in_defaults_for_minimal_page() -> Result<()> {
        let dir = temp_dir_path("minimal");
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("mypage.lepiter"), b"{}")?;
        let index = KnowledgeBase::open(&dir)?;
        fs::remove_dir_all(&dir)?;

        assert_eq!(index.pages.len(), 1);
        let meta = index.pages.values().next().unwrap();
        assert_eq!(meta.id, "mypage");
        assert_eq!(meta.title, "mypage");
        Ok(())
    }

    #[test]
    fn load_page_nonexistent_id_errors() -> Result<()> {
        let dir = temp_dir_path("no-such-id");
        fs::create_dir_all(&dir)?;
        let index = KnowledgeBase::open(&dir)?;
        fs::remove_dir_all(&dir)?;

        let err = index.load_page("does-not-exist");
        assert!(err.is_err());
        assert!(format!("{:#}", err.unwrap_err()).contains("page id not found"));
        Ok(())
    }

    #[test]
    fn load_page_missing_children_yields_empty_content() -> Result<()> {
        let dir = temp_dir_path("no-children");
        fs::create_dir_all(&dir)?;
        let content = json!({"uid": {"uuid": "pg-1"}, "pageType": {"title": "T"}});
        fs::write(dir.join("pg-1.lepiter"), serde_json::to_vec(&content)?)?;
        let index = KnowledgeBase::open(&dir)?;
        let page = index.load_page("pg-1")?;
        fs::remove_dir_all(&dir)?;

        assert!(page.content.is_empty());
        Ok(())
    }

    #[test]
    fn extract_link_targets_finds_link_nodes() {
        let nodes = vec![
            Node::Link {
                text: "page".into(),
                url: "page:abc".into(),
            },
            Node::Paragraph {
                text: "plain text".into(),
            },
        ];
        assert_eq!(extract_link_targets(&nodes), vec!["page:abc"]);
    }

    #[test]
    fn extract_link_targets_finds_inline_markdown_links() {
        let nodes = vec![Node::Paragraph {
            text: "see [docs](https://docs.rs) and [api](page:xyz)".into(),
        }];
        let targets = extract_link_targets(&nodes);
        assert_eq!(targets, vec!["https://docs.rs", "page:xyz"]);
    }

    #[test]
    fn extract_link_targets_finds_wiki_links() {
        let nodes = vec![Node::Text {
            text: "see also [[My Page]] and [[Other]]".into(),
        }];
        let targets = extract_link_targets(&nodes);
        assert_eq!(targets, vec!["My Page", "Other"]);
    }

    #[test]
    fn extract_link_targets_handles_headings_and_quotes() {
        let nodes = vec![
            Node::Heading {
                level: 2,
                text: "about [[Topic]]".into(),
            },
            Node::Quote {
                text: "see [ref](page:ref-id)".into(),
            },
        ];
        let targets = extract_link_targets(&nodes);
        assert_eq!(targets, vec!["Topic", "page:ref-id"]);
    }

    #[test]
    fn extract_link_targets_recurses_into_lists() {
        let nodes = vec![Node::List {
            items: vec![vec![Node::Paragraph {
                text: "item with [[link]]".into(),
            }]],
        }];
        let targets = extract_link_targets(&nodes);
        assert_eq!(targets, vec!["link"]);
    }

    #[test]
    fn extract_link_targets_ignores_code_and_unknown() {
        let nodes = vec![
            Node::Code {
                language: None,
                code: "[not](a-link)".into(),
            },
            Node::Unknown {
                typ: "x".into(),
                raw: json!({}),
            },
        ];
        assert!(extract_link_targets(&nodes).is_empty());
    }

    #[test]
    fn build_backlinks_computes_reverse_index() {
        let (dir, mut index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "see [[Beta]]"),
            ("p2", "Beta", &[], "links to [a](page:p1) and [[Gamma]]"),
            ("p3", "Gamma", &[], "no links here"),
        ]);
        index.build_backlinks();

        // p1 is linked to by p2 (via page:p1)
        assert_eq!(index.backlinks_for("p1"), &["p2"]);
        // p2 ("Beta") is linked to by p1 (via [[Beta]])
        assert_eq!(index.backlinks_for("p2"), &["p1"]);
        // p3 ("Gamma") is linked to by p2 (via [[Gamma]])
        assert_eq!(index.backlinks_for("p3"), &["p2"]);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_backlinks_excludes_self_links() {
        let (dir, mut index) = make_kb_on_disk(&[("p1", "Alpha", &[], "see [[Alpha]]")]);
        index.build_backlinks();
        assert!(index.backlinks_for("p1").is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn backlinks_for_unknown_page_returns_empty() {
        let (dir, mut index) = make_kb_on_disk(&[("p1", "Alpha", &[], "text")]);
        index.build_backlinks();
        assert!(index.backlinks_for("nonexistent").is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_backlinks_deduplicates_multiple_links() {
        let (dir, mut index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "[[Beta]] and [[Beta]] again"),
            ("p2", "Beta", &[], "nothing"),
        ]);
        index.build_backlinks();
        // p1 links to p2 twice but should only appear once
        assert_eq!(index.backlinks_for("p2"), &["p1"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_backlinks_sorted_by_title() {
        let (dir, mut index) = make_kb_on_disk(&[
            ("p1", "Zebra", &[], "links to [[Target]]"),
            ("p2", "Alpha", &[], "links to [[Target]]"),
            ("p3", "Target", &[], "nothing"),
        ]);
        index.build_backlinks();
        assert_eq!(index.backlinks_for("p3"), &["p2", "p1"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn register_page_adds_and_resorts() {
        let dir = temp_dir_path("register");
        fs::create_dir_all(&dir).unwrap();
        let mut index = KnowledgeBase::open(&dir).unwrap();
        assert!(index.sorted_ids.is_empty());

        let meta = PageMeta {
            id: "new-page".to_string(),
            title: "My Page".to_string(),
            title_lower: "my page".to_string(),
            path: dir.join("new-page.lepiter"),
            updated_at: None,
            tags: Vec::new(),
        };
        index.register_page(meta);
        assert_eq!(index.sorted_ids.len(), 1);
        assert_eq!(index.sorted_ids[0], "new-page");
        assert!(index.pages.contains_key("new-page"));

        fs::remove_dir_all(&dir).unwrap();
    }
}
