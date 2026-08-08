use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::util::{extract_attachment_relative, sanitize_relative_path};

/// Canonical page identifier used throughout the API.
pub type PageId = String;

/// Metadata for a page discovered during index scanning.
#[derive(Debug, Clone, Serialize)]
pub struct PageMeta {
    /// Canonical page id (preferred key over filename).
    pub id: PageId,
    /// Pre-computed lowercased id for case-insensitive comparisons.
    #[serde(skip)]
    pub id_lower: String,
    /// Human-readable page title.
    pub title: String,
    /// Pre-computed lowercased title for case-insensitive comparisons.
    #[serde(skip)]
    pub title_lower: String,
    /// Absolute or relative path to the source page file.
    pub path: PathBuf,
    /// Last edit timestamp, if present in source metadata.
    pub updated_at: Option<DateTime<FixedOffset>>,
    /// Optional page tags extracted from metadata.
    pub tags: Vec<String>,
    /// Pre-computed lowercased tags for case-insensitive comparisons.
    #[serde(skip)]
    pub tags_lower: Vec<String>,
}

/// Fully parsed page content.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
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
    Unknown {
        #[serde(rename = "source_type")]
        typ: String,
        raw: Value,
    },
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMatchKind {
    /// Match came from page title or id.
    Title,
    /// Match came from a page tag.
    Tag,
    /// Match came from rendered page content.
    Content,
}

impl SearchMatchKind {
    /// Relevance score used for ranking search results.
    /// Higher is more relevant.
    pub fn score(self) -> u32 {
        match self {
            SearchMatchKind::Title => 3,
            SearchMatchKind::Tag => 2,
            SearchMatchKind::Content => 1,
        }
    }

    /// Returns `true` when the match came from metadata (title, id, or tags)
    /// rather than page content.
    pub fn is_meta(self) -> bool {
        matches!(self, SearchMatchKind::Title | SearchMatchKind::Tag)
    }
}

/// Search result entry for one page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

pub(crate) type AttachmentResult<T> = std::result::Result<T, AttachmentError>;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// a directory no other test shares. `tempfile` picks the name: a timestamp
    /// cannot, because `SystemTime::now` ticks at microsecond granularity here, so
    /// two tests in the same process stamp the same value and share a directory.
    fn temp_dir_path(name: &str) -> PathBuf {
        tempfile::Builder::new()
            .prefix(&format!("lepiter-core-{name}-"))
            .tempdir()
            .expect("temp dir")
            .keep()
    }

    #[test]
    fn search_match_kind_score_ordering() {
        assert!(SearchMatchKind::Title.score() > SearchMatchKind::Tag.score());
        assert!(SearchMatchKind::Tag.score() > SearchMatchKind::Content.score());
    }

    #[test]
    fn search_match_kind_is_meta() {
        assert!(SearchMatchKind::Title.is_meta());
        assert!(SearchMatchKind::Tag.is_meta());
        assert!(!SearchMatchKind::Content.is_meta());
    }

    #[test]
    fn page_meta_serializes_without_internal_fields() {
        let meta = PageMeta {
            id: "abc-123".to_string(),
            id_lower: "abc-123".to_string(),
            title: "My Page".to_string(),
            title_lower: "my page".to_string(),
            path: PathBuf::from("/kb/abc-123.lepiter"),
            updated_at: None,
            tags: vec!["rust".to_string()],
            tags_lower: vec!["rust".to_string()],
        };
        let json: serde_json::Value = serde_json::to_value(&meta).unwrap();
        assert_eq!(json["id"], "abc-123");
        assert_eq!(json["title"], "My Page");
        assert_eq!(json["tags"], serde_json::json!(["rust"]));
        // Internal lowercase fields must not appear in output.
        assert!(json.get("id_lower").is_none());
        assert!(json.get("title_lower").is_none());
        assert!(json.get("tags_lower").is_none());
    }

    #[test]
    fn page_serializes_with_content() {
        let page = Page {
            id: "p1".to_string(),
            title: "Test".to_string(),
            updated_at: None,
            tags: Vec::new(),
            content: vec![
                Node::Paragraph {
                    text: "hello".to_string(),
                },
                Node::Code {
                    language: Some("rust".to_string()),
                    code: "fn main() {}".to_string(),
                },
            ],
        };
        let json: serde_json::Value = serde_json::to_value(&page).unwrap();
        let content = json["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "paragraph");
        assert_eq!(content[0]["text"], "hello");
        assert_eq!(content[1]["type"], "code");
        assert_eq!(content[1]["language"], "rust");
    }

    #[test]
    fn node_variants_serialize_with_type_tag() {
        let cases: Vec<(Node, &str)> = vec![
            (
                Node::Heading {
                    level: 2,
                    text: "title".to_string(),
                },
                "heading",
            ),
            (
                Node::Paragraph {
                    text: "p".to_string(),
                },
                "paragraph",
            ),
            (
                Node::Text {
                    text: "t".to_string(),
                },
                "text",
            ),
            (Node::List { items: vec![] }, "list"),
            (
                Node::Code {
                    language: None,
                    code: "x".to_string(),
                },
                "code",
            ),
            (
                Node::Link {
                    text: "a".to_string(),
                    url: "b".to_string(),
                },
                "link",
            ),
            (
                Node::Quote {
                    text: "q".to_string(),
                },
                "quote",
            ),
            (
                Node::Unknown {
                    typ: "wardleyMap".to_string(),
                    raw: serde_json::json!({}),
                },
                "unknown",
            ),
        ];
        for (node, expected_type) in cases {
            let json: serde_json::Value = serde_json::to_value(&node).unwrap();
            assert_eq!(json["type"], expected_type, "wrong type tag for {:?}", node);
        }
    }

    #[test]
    fn unknown_node_serializes_source_type() {
        let node = Node::Unknown {
            typ: "wardleyMap".to_string(),
            raw: serde_json::json!({"data": 1}),
        };
        let json: serde_json::Value = serde_json::to_value(&node).unwrap();
        assert_eq!(json["source_type"], "wardleyMap");
        assert_eq!(json["raw"]["data"], 1);
    }

    #[test]
    fn search_match_kind_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(SearchMatchKind::Title).unwrap(),
            serde_json::json!("title")
        );
        assert_eq!(
            serde_json::to_value(SearchMatchKind::Tag).unwrap(),
            serde_json::json!("tag")
        );
        assert_eq!(
            serde_json::to_value(SearchMatchKind::Content).unwrap(),
            serde_json::json!("content")
        );
    }

    #[test]
    fn search_hit_serializes() {
        let hit = SearchHit {
            id: "p1".to_string(),
            kind: SearchMatchKind::Tag,
        };
        let json: serde_json::Value = serde_json::to_value(&hit).unwrap();
        assert_eq!(json["id"], "p1");
        assert_eq!(json["kind"], "tag");
    }

    #[test]
    fn attachment_resolver_reports_missing_files() -> anyhow::Result<()> {
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
}
