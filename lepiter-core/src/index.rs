use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use walkdir::WalkDir;

use crate::model::{
    AttachmentResolver, LinkTargetKind, Page, PageId, PageMeta, ParseIssue, SearchHit,
    SearchMatchKind, TitleResolution,
};
use crate::parse::{parse_item_recursive, parse_page_meta};
use crate::render::render_page_to_text;
use crate::util::{extract_link_targets, extract_uuid_like, is_external_target};

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
                        meta.id_lower = meta.id.to_lowercase();
                    }
                    if meta.title.is_empty() {
                        meta.title = meta.id.clone();
                        meta.title_lower = meta.title.to_lowercase();
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
            metas.retain(|m| page_meta_match_kind(m, &needle).is_some());
        }
        metas.into_iter().map(|m| m.id.clone()).collect()
    }

    /// Returns page ids with their match kinds, filtered by metadata query.
    pub fn filter_page_ids_scored(&self, query: &str) -> Vec<(PageId, SearchMatchKind)> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let metas = self.sorted_pages();
        metas
            .into_iter()
            .filter_map(|m| page_meta_match_kind(m, &needle).map(|kind| (m.id.clone(), kind)))
            .collect()
    }

    /// Searches pages by metadata and optionally content, returning hits
    /// ranked by relevance (title > tag > content), with ties broken
    /// alphabetically by title.
    pub fn search_hits(&self, query: &str, include_content: bool) -> Vec<SearchHit> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }

        let mut by_id: HashMap<PageId, SearchMatchKind> = HashMap::new();
        let metas = self.sorted_pages();

        for meta in &metas {
            if let Some(kind) = page_meta_match_kind(meta, &needle) {
                by_id.insert(meta.id.clone(), kind);
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

        let mut hits: Vec<SearchHit> = metas
            .iter()
            .filter_map(|meta| {
                by_id.get(&meta.id).map(|kind| SearchHit {
                    id: meta.id.clone(),
                    kind: *kind,
                })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.kind.score().cmp(&a.kind.score()).then_with(|| {
                let ta = self
                    .pages
                    .get(&a.id)
                    .map(|m| m.title_lower.as_str())
                    .unwrap_or("");
                let tb = self
                    .pages
                    .get(&b.id)
                    .map(|m| m.title_lower.as_str())
                    .unwrap_or("");
                ta.cmp(tb)
            })
        });
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
                    title_sort_key(&self.pages, a).cmp(title_sort_key(&self.pages, b))
                });
                (target, sorted)
            })
            .collect();
    }

    /// Incrementally updates the backlinks index for a single page.
    ///
    /// Removes any existing outgoing links from `page_id`, then re-extracts
    /// and classifies its current links.  Much cheaper than a full
    /// [`Self::build_backlinks`] call when only one page changed.
    pub fn update_backlinks_for(&mut self, page_id: &str) {
        // 1. Remove page_id as a source from every target's backlink list.
        self.backlinks.retain(|_target, sources| {
            sources.retain(|s| s != page_id);
            !sources.is_empty()
        });

        // 2. Re-extract outgoing links from the (possibly updated) page.
        let page = match self.load_page(page_id) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("update_backlinks_for: failed to load page {page_id}: {e:#}");
                return;
            }
        };
        let mut seen = HashSet::new();
        for target in extract_link_targets(&page.content) {
            if let LinkTargetKind::InternalPage(target_id) = self.classify_link_target(&target)
                && target_id != page_id
                && seen.insert(target_id.clone())
            {
                insert_sorted_by_title(
                    self.backlinks.entry(target_id).or_default(),
                    &self.pages,
                    page_id.to_string(),
                );
            }
        }
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

/// Returns the case-insensitive title for `id`, or `""` if unknown.
///
/// Free function so callers can split-borrow `pages` and `backlinks`
/// without conflicting on `&self`.
fn title_sort_key<'a>(pages: &'a HashMap<PageId, PageMeta>, id: &str) -> &'a str {
    pages.get(id).map(|m| m.title_lower.as_str()).unwrap_or("")
}

/// Inserts `source_id` into `sources` at the position that keeps the vec
/// sorted by title.  Uses `partition_point` (binary search) for O(log n)
/// lookup with zero key clones, vs the old push-then-sort which was
/// O(n log n) with n clones per call.
fn insert_sorted_by_title(
    sources: &mut Vec<PageId>,
    pages: &HashMap<PageId, PageMeta>,
    source_id: String,
) {
    let key = title_sort_key(pages, &source_id);
    let pos = sources.partition_point(|id| title_sort_key(pages, id) < key);
    sources.insert(pos, source_id);
}

fn compute_sorted_ids(pages: &HashMap<PageId, PageMeta>) -> Vec<PageId> {
    let mut entries: Vec<_> = pages.values().collect();
    entries.sort_by(|a, b| a.title_lower.cmp(&b.title_lower));
    entries.into_iter().map(|m| m.id.clone()).collect()
}

fn page_meta_match_kind(meta: &PageMeta, needle: &str) -> Option<SearchMatchKind> {
    if meta.title_lower.contains(needle) || meta.id_lower.contains(needle) {
        Some(SearchMatchKind::Title)
    } else if meta.tags_lower.iter().any(|t| t.contains(needle)) {
        Some(SearchMatchKind::Tag)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir_path(name: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("lepiter-core-{name}-{ts}"))
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
    fn filter_page_ids_matches_title_id_and_tags() {
        let mut pages = HashMap::new();
        pages.insert(
            "id-1".to_string(),
            PageMeta {
                id: "id-1".to_string(),
                id_lower: "id-1".to_string(),
                title: "Alpha".to_string(),
                title_lower: "alpha".to_string(),
                path: PathBuf::from("/tmp/a"),
                updated_at: None,
                tags: vec!["rust".to_string()],
                tags_lower: vec!["rust".to_string()],
            },
        );
        pages.insert(
            "id-2".to_string(),
            PageMeta {
                id: "id-2".to_string(),
                id_lower: "id-2".to_string(),
                title: "Beta".to_string(),
                title_lower: "beta".to_string(),
                path: PathBuf::from("/tmp/b"),
                updated_at: None,
                tags: vec!["pharo".to_string()],
                tags_lower: vec!["pharo".to_string()],
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
                id_lower: "id-1".to_string(),
                title: "Alpha".to_string(),
                title_lower: "alpha".to_string(),
                path: PathBuf::from("/tmp/a"),
                updated_at: None,
                tags: Vec::new(),
                tags_lower: Vec::new(),
            },
        );
        pages.insert(
            "id-2".to_string(),
            PageMeta {
                id: "id-2".to_string(),
                id_lower: "id-2".to_string(),
                title: "Alphabet".to_string(),
                title_lower: "alphabet".to_string(),
                path: PathBuf::from("/tmp/b"),
                updated_at: None,
                tags: Vec::new(),
                tags_lower: Vec::new(),
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
                id_lower: "8a505fa0-2222-3333-4444-555555555555".to_string(),
                title: "Alpha".to_string(),
                title_lower: "alpha".to_string(),
                path: PathBuf::from("/tmp/a"),
                updated_at: None,
                tags: Vec::new(),
                tags_lower: Vec::new(),
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
        assert_eq!(hits[0].kind, SearchMatchKind::Title);
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
        assert_eq!(hits[0].kind, SearchMatchKind::Tag);
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
    fn search_hits_title_match_takes_priority_over_content() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Fox Guide", &[], "the fox jumps")]);
        let hits = index.search_hits("fox", true);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, SearchMatchKind::Title);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_hits_same_score_sorted_alphabetically() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Zebra", &["common"], "body"),
            ("p2", "Alpha", &["common"], "body"),
            ("p3", "Middle", &["common"], "body"),
        ]);
        let hits = index.search_hits("common", false);
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        // all tag matches → same score → alphabetical by title
        assert_eq!(ids, vec!["p2", "p3", "p1"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_hits_title_ranked_above_tag() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Page One", &["rust"], "body"),
            ("p2", "Rust Guide", &[], "body"),
        ]);
        let hits = index.search_hits("rust", false);
        assert_eq!(hits.len(), 2);
        // title match first
        assert_eq!(hits[0].id, "p2");
        assert_eq!(hits[0].kind, SearchMatchKind::Title);
        // tag match second
        assert_eq!(hits[1].id, "p1");
        assert_eq!(hits[1].kind, SearchMatchKind::Tag);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_hits_title_ranked_above_content() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "the word rust appears here"),
            ("p2", "Rust Guide", &[], "no match in body"),
        ]);
        let hits = index.search_hits("rust", true);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "p2");
        assert_eq!(hits[0].kind, SearchMatchKind::Title);
        assert_eq!(hits[1].id, "p1");
        assert_eq!(hits[1].kind, SearchMatchKind::Content);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_hits_tag_ranked_above_content() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "the word rust appears here"),
            ("p2", "Beta", &["rust"], "no match in body"),
        ]);
        let hits = index.search_hits("rust", true);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "p2");
        assert_eq!(hits[0].kind, SearchMatchKind::Tag);
        assert_eq!(hits[1].id, "p1");
        assert_eq!(hits[1].kind, SearchMatchKind::Content);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_hits_mixed_kinds_ranked_correctly() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "cli tools are great"),
            ("p2", "Beta", &["cli"], "no match"),
            ("p3", "CLI Reference", &[], "no match"),
            ("p4", "Delta", &[], "no match"),
        ]);
        let hits = index.search_hits("cli", true);
        assert_eq!(hits.len(), 3);
        // title match first
        assert_eq!(hits[0].id, "p3");
        assert_eq!(hits[0].kind, SearchMatchKind::Title);
        // tag match second
        assert_eq!(hits[1].id, "p2");
        assert_eq!(hits[1].kind, SearchMatchKind::Tag);
        // content match last
        assert_eq!(hits[2].id, "p1");
        assert_eq!(hits[2].kind, SearchMatchKind::Content);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_hits_title_with_tag_stays_title() {
        // page matches both title and tag — kind should be Title (highest)
        let (dir, index) =
            make_kb_on_disk(&[("p1", "Rust Guide", &["rust"], "also mentions rust")]);
        let hits = index.search_hits("rust", true);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, SearchMatchKind::Title);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_hits_tag_match_takes_priority_over_content() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Some Page", &["fox"], "the fox jumps")]);
        let hits = index.search_hits("fox", true);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, SearchMatchKind::Tag);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_hits_id_match_counts_as_title() {
        let (dir, index) = make_kb_on_disk(&[("rustacean", "Some Page", &[], "body")]);
        let hits = index.search_hits("rustacean", false);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, SearchMatchKind::Title);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn filter_page_ids_scored_returns_kinds() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Rust Intro", &[], "body"),
            ("p2", "Beta", &["rust"], "body"),
            ("p3", "Gamma", &[], "body"),
        ]);
        let scored = index.filter_page_ids_scored("rust");
        assert_eq!(scored.len(), 2);
        let map: HashMap<&str, SearchMatchKind> =
            scored.iter().map(|(id, k)| (id.as_str(), *k)).collect();
        assert_eq!(map["p1"], SearchMatchKind::Title);
        assert_eq!(map["p2"], SearchMatchKind::Tag);
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
    fn classify_link_target_mixed_case_urls() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Alpha", &[], "body")]);
        assert!(matches!(
            index.classify_link_target("HTTPS://EXAMPLE.COM"),
            LinkTargetKind::ExternalUrl(_)
        ));
        assert!(matches!(
            index.classify_link_target("Http://Example.Com"),
            LinkTargetKind::ExternalUrl(_)
        ));
        assert!(matches!(
            index.classify_link_target("MAILTO:user@host.com"),
            LinkTargetKind::ExternalUrl(_)
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn classify_link_target_whitespace_only_is_unknown() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Alpha", &[], "body")]);
        assert!(matches!(
            index.classify_link_target("   "),
            LinkTargetKind::Unknown(_)
        ));
        assert!(matches!(
            index.classify_link_target("\t\n"),
            LinkTargetKind::Unknown(_)
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn classify_link_target_trims_whitespace_around_url() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Alpha", &[], "body")]);
        assert!(matches!(
            index.classify_link_target("  https://example.com  "),
            LinkTargetKind::ExternalUrl(_)
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn classify_link_target_unusual_schemes() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Alpha", &[], "body")]);
        assert!(matches!(
            index.classify_link_target("ftp://files.example.com"),
            LinkTargetKind::ExternalUrl(_)
        ));
        assert!(matches!(
            index.classify_link_target("ssh://git.example.com"),
            LinkTargetKind::ExternalUrl(_)
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
    fn open_empty_directory_returns_empty_index() -> anyhow::Result<()> {
        let dir = temp_dir_path("empty-kb");
        fs::create_dir_all(&dir)?;
        let index = KnowledgeBase::open(&dir)?;
        fs::remove_dir_all(&dir)?;

        assert!(index.pages.is_empty());
        assert!(index.index_issues.is_empty());
        Ok(())
    }

    #[test]
    fn open_skips_non_lepiter_files() -> anyhow::Result<()> {
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
    fn open_reports_invalid_json_as_issue() -> anyhow::Result<()> {
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
    fn open_reports_wrong_json_structure_as_issue() -> anyhow::Result<()> {
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
    fn open_fills_in_defaults_for_minimal_page() -> anyhow::Result<()> {
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
    fn load_page_nonexistent_id_errors() -> anyhow::Result<()> {
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
    fn load_page_missing_children_yields_empty_content() -> anyhow::Result<()> {
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
    fn update_backlinks_for_adds_new_links() {
        let (dir, mut index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "no links"),
            ("p2", "Beta", &[], "nothing"),
        ]);
        index.build_backlinks();
        assert!(index.backlinks_for("p2").is_empty());

        // Simulate editing p1 to add a link to Beta.
        let content = json!({
            "uid": {"uuid": "p1"},
            "pageType": {"title": "Alpha"},
            "children": {"items": [
                {"__type": "textSnippet", "string": "now links to [[Beta]]"}
            ]}
        });
        fs::write(
            dir.join("p1.lepiter"),
            serde_json::to_vec(&content).unwrap(),
        )
        .unwrap();

        index.update_backlinks_for("p1");
        assert_eq!(index.backlinks_for("p2"), &["p1"]);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn update_backlinks_for_removes_stale_links() {
        let (dir, mut index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "see [[Beta]]"),
            ("p2", "Beta", &[], "nothing"),
        ]);
        index.build_backlinks();
        assert_eq!(index.backlinks_for("p2"), &["p1"]);

        // Edit p1 to remove the link.
        let content = json!({
            "uid": {"uuid": "p1"},
            "pageType": {"title": "Alpha"},
            "children": {"items": [
                {"__type": "textSnippet", "string": "no links anymore"}
            ]}
        });
        fs::write(
            dir.join("p1.lepiter"),
            serde_json::to_vec(&content).unwrap(),
        )
        .unwrap();

        index.update_backlinks_for("p1");
        assert!(index.backlinks_for("p2").is_empty());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn update_backlinks_for_replaces_changed_link() {
        let (dir, mut index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "see [[Beta]]"),
            ("p2", "Beta", &[], "nothing"),
            ("p3", "Gamma", &[], "nothing"),
        ]);
        index.build_backlinks();
        assert_eq!(index.backlinks_for("p2"), &["p1"]);
        assert!(index.backlinks_for("p3").is_empty());

        // Edit p1 to link to Gamma instead of Beta.
        let content = json!({
            "uid": {"uuid": "p1"},
            "pageType": {"title": "Alpha"},
            "children": {"items": [
                {"__type": "textSnippet", "string": "see [[Gamma]]"}
            ]}
        });
        fs::write(
            dir.join("p1.lepiter"),
            serde_json::to_vec(&content).unwrap(),
        )
        .unwrap();

        index.update_backlinks_for("p1");
        assert!(index.backlinks_for("p2").is_empty());
        assert_eq!(index.backlinks_for("p3"), &["p1"]);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn update_backlinks_for_preserves_other_sources() {
        let (dir, mut index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "see [[Gamma]]"),
            ("p2", "Beta", &[], "see [[Gamma]]"),
            ("p3", "Gamma", &[], "nothing"),
        ]);
        index.build_backlinks();
        assert_eq!(index.backlinks_for("p3"), &["p1", "p2"]);

        // Edit p1 to remove its link; p2's link should remain.
        let content = json!({
            "uid": {"uuid": "p1"},
            "pageType": {"title": "Alpha"},
            "children": {"items": [
                {"__type": "textSnippet", "string": "no link"}
            ]}
        });
        fs::write(
            dir.join("p1.lepiter"),
            serde_json::to_vec(&content).unwrap(),
        )
        .unwrap();

        index.update_backlinks_for("p1");
        assert_eq!(index.backlinks_for("p3"), &["p2"]);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn update_backlinks_for_deduplicates() {
        let (dir, mut index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "nothing"),
            ("p2", "Beta", &[], "nothing"),
        ]);
        index.build_backlinks();

        // Edit p1 to link to Beta twice.
        let content = json!({
            "uid": {"uuid": "p1"},
            "pageType": {"title": "Alpha"},
            "children": {"items": [
                {"__type": "textSnippet", "string": "[[Beta]] and [[Beta]] again"}
            ]}
        });
        fs::write(
            dir.join("p1.lepiter"),
            serde_json::to_vec(&content).unwrap(),
        )
        .unwrap();

        index.update_backlinks_for("p1");
        assert_eq!(index.backlinks_for("p2"), &["p1"]);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn update_backlinks_for_excludes_self_links() {
        let (dir, mut index) = make_kb_on_disk(&[("p1", "Alpha", &[], "nothing")]);
        index.build_backlinks();

        // Edit p1 to link to itself.
        let content = json!({
            "uid": {"uuid": "p1"},
            "pageType": {"title": "Alpha"},
            "children": {"items": [
                {"__type": "textSnippet", "string": "see [[Alpha]]"}
            ]}
        });
        fs::write(
            dir.join("p1.lepiter"),
            serde_json::to_vec(&content).unwrap(),
        )
        .unwrap();

        index.update_backlinks_for("p1");
        assert!(index.backlinks_for("p1").is_empty());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn update_backlinks_for_maintains_sort_order() {
        let (dir, mut index) = make_kb_on_disk(&[
            ("p1", "Zebra", &[], "nothing"),
            ("p2", "Alpha", &[], "links to [[Target]]"),
            ("p3", "Target", &[], "nothing"),
        ]);
        index.build_backlinks();
        assert_eq!(index.backlinks_for("p3"), &["p2"]);

        // Edit p1 (Zebra) to also link to Target; result should be sorted Alpha, Zebra.
        let content = json!({
            "uid": {"uuid": "p1"},
            "pageType": {"title": "Zebra"},
            "children": {"items": [
                {"__type": "textSnippet", "string": "links to [[Target]]"}
            ]}
        });
        fs::write(
            dir.join("p1.lepiter"),
            serde_json::to_vec(&content).unwrap(),
        )
        .unwrap();

        index.update_backlinks_for("p1");
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
            id_lower: "new-page".to_string(),
            title: "My Page".to_string(),
            title_lower: "my page".to_string(),
            path: dir.join("new-page.lepiter"),
            updated_at: None,
            tags: Vec::new(),
            tags_lower: Vec::new(),
        };
        index.register_page(meta);
        assert_eq!(index.sorted_ids.len(), 1);
        assert_eq!(index.sorted_ids[0], "new-page");
        assert!(index.pages.contains_key("new-page"));

        fs::remove_dir_all(&dir).unwrap();
    }
}
