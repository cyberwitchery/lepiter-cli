use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};
use serde_json::Value;
use thiserror::Error;

use crate::util::{extract_attachment_relative, sanitize_relative_path};

/// Canonical page identifier used throughout the API.
pub type PageId = String;

/// Metadata for a page discovered during index scanning.
#[derive(Debug, Clone)]
pub struct PageMeta {
    /// Canonical page id (preferred key over filename).
    pub id: PageId,
    /// Pre-computed lowercased id for case-insensitive comparisons.
    pub id_lower: String,
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
    /// Pre-computed lowercased tags for case-insensitive comparisons.
    pub tags_lower: Vec<String>,
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

    fn temp_dir_path(name: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("lepiter-core-{name}-{ts}"))
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
