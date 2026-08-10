use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use walkdir::WalkDir;

use serde::Serialize;

use crate::model::{
    AttachmentResolver, LinkTargetKind, Page, PageId, PageMeta, ParseIssue, SearchHit,
    SearchMatchKind, TitleResolution,
};
use crate::parse::{parse_item_recursive, parse_page_meta};
use crate::render::page_content_contains;
use crate::util::{
    extract_attachment_relative, extract_link_targets, extract_uuid_like, is_external_target,
};

/// Indexed knowledge base metadata with lazy page loading.
#[derive(Debug, Clone)]
pub struct KnowledgeBaseIndex {
    root: PathBuf,
    /// Metadata map keyed by canonical page id.
    pub pages: HashMap<PageId, PageMeta>,
    /// Page ids in case-insensitive title sort order, computed at open time,
    /// maintained by [`Self::register_page`].
    pub sorted_ids: Vec<PageId>,
    /// Exact-title lookup index: lowercased title -> page ids sharing that title.
    /// Kept in sync with `pages` by [`Self::register_page`].
    title_index: HashMap<String, Vec<PageId>>,
    /// Non-fatal issues encountered while scanning metadata.
    pub index_issues: Vec<ParseIssue>,
    /// Reverse link index: target page id -> sorted list of source page ids that link to it.
    backlinks: HashMap<PageId, Vec<PageId>>,
    /// Forward link index: source page id -> set of target page ids it links to.
    /// Kept in sync with `backlinks`.
    forward_links: HashMap<PageId, HashSet<PageId>>,
    /// Ids claimed by more than one `.lepiter` file, captured during [`KnowledgeBase::open`]
    /// before the collision is lost to `pages`.
    pub duplicate_ids: Vec<DuplicateId>,
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
        let mut id_paths: HashMap<PageId, Vec<PathBuf>> = HashMap::new();

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
                    id_paths
                        .entry(meta.id.clone())
                        .or_default()
                        .push(file_path.to_path_buf());
                    pages.insert(meta.id.clone(), meta);
                }
                Err(err) => issues.push(ParseIssue {
                    path: file_path.to_path_buf(),
                    message: format!("{err:#}"),
                }),
            }
        }

        let sorted_ids = compute_sorted_ids(&pages);
        let title_index = compute_title_index(&pages, &sorted_ids);
        let duplicate_ids = collect_duplicate_ids(id_paths);

        Ok(KnowledgeBaseIndex {
            root,
            pages,
            sorted_ids,
            title_index,
            index_issues: issues,
            backlinks: HashMap::new(),
            forward_links: HashMap::new(),
            duplicate_ids,
        })
    }
}

impl KnowledgeBaseIndex {
    /// Registers a page in the index, inserting it at the correct
    /// sorted position via binary search.  If the page already exists, its old sort position
    /// is removed first so re-registration never creates duplicates
    /// (and handles title changes correctly).
    ///
    /// The exact-title index is kept in sync as well: on a title change the
    /// page id is moved from its stale title bucket to the new one, and
    /// now-empty buckets are dropped.
    pub fn register_page(&mut self, meta: PageMeta) {
        let id = meta.id.clone();
        let new_title_lower = meta.title_lower.clone();
        let old_title_lower = self.pages.get(&id).map(|m| m.title_lower.clone());

        if old_title_lower.is_some()
            && let Some(pos) = self.sorted_ids.iter().position(|i| i == &id)
        {
            self.sorted_ids.remove(pos);
        }
        self.pages.insert(id.clone(), meta);
        insert_sorted_by_title(&mut self.sorted_ids, &self.pages, id.clone());

        if old_title_lower.as_deref() != Some(new_title_lower.as_str()) {
            if let Some(old) = &old_title_lower {
                self.remove_from_title_index(old, &id);
            }
            self.title_index
                .entry(new_title_lower)
                .or_default()
                .push(id);
        }
    }

    /// Removes `id` from the `title_lower` bucket of the exact-title index,
    /// dropping the bucket entirely once it is empty.
    fn remove_from_title_index(&mut self, title_lower: &str, id: &str) {
        if let Some(bucket) = self.title_index.get_mut(title_lower) {
            bucket.retain(|i| i != id);
            if bucket.is_empty() {
                self.title_index.remove(title_lower);
            }
        }
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
                if page_content_contains(&page, &needle) {
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
        hits.sort_by_cached_key(|h| {
            let title = self
                .pages
                .get(&h.id)
                .map(|m| m.title_lower.clone())
                .unwrap_or_default();
            (std::cmp::Reverse(h.kind.score()), title)
        });
        hits
    }

    /// Resolves a page id from title using case-insensitive exact match, then partial match.
    pub fn resolve_page_id_by_title(&self, title: &str) -> TitleResolution {
        let needle = title.trim().to_lowercase();
        if needle.is_empty() {
            return TitleResolution::NotFound;
        }

        if let Some(exact) = self.resolve_exact_from_index(&needle) {
            return exact;
        }

        let partial = self
            .sorted_pages()
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

    /// Looks up exact (case-insensitive) title matches via the precomputed
    /// index.  `needle` must already be trimmed and lowercased.  Returns
    /// `None` when no page has that exact title, so callers can decide whether
    /// to report `NotFound` or fall back to a substring search.
    fn resolve_exact_from_index(&self, needle: &str) -> Option<TitleResolution> {
        match self.title_index.get(needle).map(Vec::as_slice) {
            Some([id]) => Some(TitleResolution::Unique(id.clone())),
            Some(ids) if ids.len() > 1 => Some(TitleResolution::Ambiguous(ids.to_vec())),
            _ => None,
        }
    }

    /// Resolves a page id from title using a case-insensitive *exact* match
    /// only, never falling back to a substring like [`Self::resolve_page_id_by_title`].
    /// Used for wikilink/`page:`/`title:` resolution, where a substring hit would
    /// fabricate a graph edge and hide a genuinely broken link.
    pub fn resolve_page_id_by_title_exact(&self, title: &str) -> TitleResolution {
        let needle = title.trim().to_lowercase();
        if needle.is_empty() {
            return TitleResolution::NotFound;
        }

        self.resolve_exact_from_index(&needle)
            .unwrap_or(TitleResolution::NotFound)
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
            if let TitleResolution::Unique(resolved) = self.resolve_page_id_by_title_exact(id) {
                return LinkTargetKind::InternalPage(resolved);
            }
        }
        if let Some(rest) = target.strip_prefix("title:") {
            return match self.resolve_page_id_by_title_exact(rest.trim()) {
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

        match self.resolve_page_id_by_title_exact(target) {
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
        let mut forward: HashMap<PageId, HashSet<PageId>> = HashMap::new();
        for source_id in &self.sorted_ids {
            let Ok(page) = self.load_page(source_id) else {
                continue;
            };
            let mut source_targets = HashSet::new();
            for target in extract_link_targets(&page.content) {
                if let LinkTargetKind::InternalPage(target_id) = self.classify_link_target(&target)
                    && target_id != *source_id
                    && source_targets.insert(target_id.clone())
                {
                    back.entry(target_id).or_default().insert(source_id.clone());
                }
            }
            if !source_targets.is_empty() {
                forward.insert(source_id.clone(), source_targets);
            }
        }
        self.forward_links = forward;
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
        // 1. Remove page_id from only the targets it previously linked to.
        if let Some(old_targets) = self.forward_links.remove(page_id) {
            for target_id in &old_targets {
                if let Some(sources) = self.backlinks.get_mut(target_id) {
                    sources.retain(|s| s != page_id);
                    if sources.is_empty() {
                        self.backlinks.remove(target_id);
                    }
                }
            }
        }

        // 2. Re-extract outgoing links from the (possibly updated) page.
        let page = match self.load_page(page_id) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("update_backlinks_for: failed to load page {page_id}: {e:#}");
                return;
            }
        };
        let mut new_targets = HashSet::new();
        for target in extract_link_targets(&page.content) {
            if let LinkTargetKind::InternalPage(target_id) = self.classify_link_target(&target)
                && target_id != page_id
                && new_targets.insert(target_id.clone())
            {
                insert_sorted_by_title(
                    self.backlinks.entry(target_id).or_default(),
                    &self.pages,
                    page_id.to_string(),
                );
            }
        }
        if !new_targets.is_empty() {
            self.forward_links.insert(page_id.to_string(), new_targets);
        }
    }

    /// Returns the page ids that link to the given page, sorted by title.
    pub fn backlinks_for(&self, id: &str) -> &[PageId] {
        self.backlinks
            .get(id)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Builds the full directed link graph across all pages.
    ///
    /// Each edge represents one internal page-to-page link (deduplicated per
    /// source/target pair).  Self-links are excluded.
    pub fn build_link_graph(&self) -> LinkGraph {
        LinkGraph {
            edges: self.scan_all_pages().edges,
        }
    }

    /// Returns the root path used to build this index.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns an attachment resolver rooted at this knowledge base.
    pub fn attachment_resolver(&self) -> AttachmentResolver {
        AttachmentResolver::new(&self.root)
    }
    /// Scans all pages in a single pass, collecting broken links, linked
    /// pages, link graph edges, missing attachments, and load errors.
    pub fn scan_all_pages(&self) -> LinkAnalysisResult {
        let resolver = self.attachment_resolver();
        let mut broken_links = Vec::new();
        let mut linked_pages: HashSet<PageId> = HashSet::new();
        let mut load_errors = Vec::new();
        let mut missing_attachments = Vec::new();
        let mut seen_attachments: HashSet<PathBuf> = HashSet::new();
        let mut edges = Vec::new();
        let mut seen_edges = HashSet::new();

        for id in &self.sorted_ids {
            let meta = match self.pages.get(id) {
                Some(m) => m,
                None => continue,
            };
            let page = match self.load_page(id) {
                Ok(p) => p,
                Err(e) => {
                    load_errors.push(PageLoadError {
                        page_id: id.clone(),
                        title: meta.title.clone(),
                        error: format!("{e:#}"),
                    });
                    continue;
                }
            };
            seen_edges.clear();
            seen_attachments.clear();
            for target in extract_link_targets(&page.content) {
                match self.classify_link_target(&target) {
                    LinkTargetKind::InternalPage(target_id) if target_id != *id => {
                        linked_pages.insert(target_id.clone());
                        if seen_edges.insert(target_id.clone()) {
                            edges.push(LinkEdge {
                                source: id.clone(),
                                target: target_id,
                            });
                        }
                    }
                    LinkTargetKind::Unknown(_) => {
                        broken_links.push(BrokenLink {
                            source_title: meta.title.clone(),
                            source_id: id.clone(),
                            target: target.clone(),
                        });
                    }
                    _ => {}
                }

                if extract_attachment_relative(&target).is_some()
                    && let Ok(resolved) = resolver.resolve(&target)
                    && !resolved.exists
                    && seen_attachments.insert(resolved.path.clone())
                {
                    missing_attachments.push(MissingAttachment {
                        source_title: meta.title.clone(),
                        source_id: id.clone(),
                        target,
                        resolved_path: resolved.path,
                    });
                }
            }
        }

        LinkAnalysisResult {
            broken_links,
            linked_pages,
            load_errors,
            missing_attachments,
            edges,
        }
    }

    /// alias for [`Self::scan_all_pages`].
    pub fn analyze_all(&self) -> LinkAnalysisResult {
        self.scan_all_pages()
    }

    /// alias for [`Self::scan_all_pages`].
    pub fn analyze_links(&self) -> LinkAnalysisResult {
        self.scan_all_pages()
    }

    /// Returns page ids that are not linked to by any other page.
    ///
    /// The `toc_page_id` (table-of-contents), when present, is excluded from the
    /// result since it serves as the root entry point and is not expected to be
    /// linked to.
    pub fn orphan_ids(
        &self,
        linked_pages: &HashSet<PageId>,
        toc_page_id: Option<&str>,
    ) -> Vec<PageId> {
        self.sorted_ids
            .iter()
            .filter(|id| !linked_pages.contains(*id) && Some(id.as_str()) != toc_page_id)
            .cloned()
            .collect()
    }

    /// Finds page titles shared by more than one page (case-insensitive).
    pub fn find_duplicate_titles(&self) -> Vec<DuplicateTitle> {
        let mut by_title: HashMap<&str, Vec<&PageMeta>> = HashMap::new();
        for meta in self.pages.values() {
            by_title.entry(&meta.title_lower).or_default().push(meta);
        }
        let mut dupes: Vec<DuplicateTitle> = by_title
            .into_iter()
            .filter(|(_, metas)| metas.len() > 1)
            .map(|(_, metas)| {
                let title = metas[0].title.clone();
                let mut page_ids: Vec<PageId> = metas.iter().map(|m| m.id.clone()).collect();
                page_ids.sort();
                DuplicateTitle { title, page_ids }
            })
            .collect();
        dupes.sort_by_key(|a| a.title.to_lowercase());
        dupes
    }

    /// Returns page ids claimed by more than one file, captured at open time.
    pub fn find_duplicate_ids(&self) -> Vec<DuplicateId> {
        self.duplicate_ids.clone()
    }

    /// Finds attachment references whose files are missing from disk; see
    /// [`LinkAnalysisResult::missing_attachments`].
    pub fn find_missing_attachments(&self) -> Vec<MissingAttachment> {
        self.scan_all_pages().missing_attachments
    }
}

/// A link target that could not be resolved to any known page.
#[derive(Debug, Clone)]
pub struct BrokenLink {
    /// Title of the page containing the broken link.
    pub source_title: String,
    /// Id of the page containing the broken link.
    pub source_id: PageId,
    /// The raw unresolved link target.
    pub target: String,
}

/// A page that could not be loaded during link analysis.
#[derive(Debug, Clone)]
pub struct PageLoadError {
    /// Id of the page that failed to load.
    pub page_id: PageId,
    /// Title from the metadata index.
    pub title: String,
    /// Human-readable error message.
    pub error: String,
}

/// Result of [`KnowledgeBaseIndex::scan_all_pages`].
#[derive(Debug, Clone)]
pub struct LinkAnalysisResult {
    /// Links whose targets could not be resolved.
    pub broken_links: Vec<BrokenLink>,
    /// Set of page ids that are linked to by at least one other page.
    pub linked_pages: HashSet<PageId>,
    /// Pages that could not be loaded (e.g. corrupted JSON).
    pub load_errors: Vec<PageLoadError>,
    /// Attachment references whose files are missing from disk, at most one
    /// per (referencing page, resolved path).
    pub missing_attachments: Vec<MissingAttachment>,
    /// Deduplicated directed link graph edges (self-links excluded).
    pub edges: Vec<LinkEdge>,
}

/// A directed edge in the page link graph.
#[derive(Debug, Clone, Serialize)]
pub struct LinkEdge {
    /// Page id of the linking page.
    pub source: PageId,
    /// Page id of the linked-to page.
    pub target: PageId,
}

/// Directed link graph across all pages in a knowledge base.
#[derive(Debug, Clone)]
pub struct LinkGraph {
    /// Deduplicated directed edges (self-links excluded).
    pub edges: Vec<LinkEdge>,
}

/// A set of pages that share the same title (case-insensitive).
#[derive(Debug, Clone)]
pub struct DuplicateTitle {
    /// The shared title (original casing from the first match).
    pub title: String,
    /// Page ids sharing this title.
    pub page_ids: Vec<PageId>,
}

/// A set of `.lepiter` files that resolve to the same page id.
#[derive(Debug, Clone)]
pub struct DuplicateId {
    /// The shared page id.
    pub id: PageId,
    /// Paths of the files claiming this id, sorted.
    pub paths: Vec<PathBuf>,
}

/// An attachment reference that points to a file not found on disk.
#[derive(Debug, Clone)]
pub struct MissingAttachment {
    /// Title of the page referencing the attachment.
    pub source_title: String,
    /// Id of the page referencing the attachment.
    pub source_id: PageId,
    /// The raw attachment target string from the page content.
    pub target: String,
    /// The resolved path that was not found.
    pub resolved_path: PathBuf,
}

impl LinkGraph {
    /// Returns edges filtered to only those involving `page_id` (as source or target).
    pub fn ego(&self, page_id: &str) -> Vec<&LinkEdge> {
        self.edges
            .iter()
            .filter(|e| e.source == page_id || e.target == page_id)
            .collect()
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
/// sorted by title.  Uses `partition_point` (binary search).
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

/// Collapses the scan-time id -> paths map into sorted `DuplicateId`s for every
/// id claimed by more than one file.
fn collect_duplicate_ids(id_paths: HashMap<PageId, Vec<PathBuf>>) -> Vec<DuplicateId> {
    let mut dupes: Vec<DuplicateId> = id_paths
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(id, mut paths)| {
            paths.sort();
            DuplicateId { id, paths }
        })
        .collect();
    dupes.sort_by(|a, b| a.id.cmp(&b.id));
    dupes
}

/// Builds the exact-title lookup index keyed by lowercased title.  Ids are
/// collected in `sorted_ids` order so a bucket matches what a title-sorted
/// full scan would have produced.
fn compute_title_index(
    pages: &HashMap<PageId, PageMeta>,
    sorted_ids: &[PageId],
) -> HashMap<String, Vec<PageId>> {
    let mut index: HashMap<String, Vec<PageId>> = HashMap::new();
    for id in sorted_ids {
        if let Some(meta) = pages.get(id) {
            index
                .entry(meta.title_lower.clone())
                .or_default()
                .push(id.clone());
        }
    }
    index
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
        let title_index = compute_title_index(&pages, &sorted_ids);
        let index = KnowledgeBaseIndex {
            root: PathBuf::from("/tmp"),
            pages,
            sorted_ids,
            title_index,
            index_issues: Vec::new(),
            backlinks: HashMap::new(),
            forward_links: HashMap::new(),
            duplicate_ids: Vec::new(),
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
        let title_index = compute_title_index(&pages, &sorted_ids);
        let index = KnowledgeBaseIndex {
            root: PathBuf::from("/tmp"),
            pages,
            sorted_ids,
            title_index,
            index_issues: Vec::new(),
            backlinks: HashMap::new(),
            forward_links: HashMap::new(),
            duplicate_ids: Vec::new(),
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

        // The exact resolver agrees on a genuine exact match (case-insensitive)...
        assert_eq!(
            index.resolve_page_id_by_title_exact("ALPHA"),
            TitleResolution::Unique("id-1".to_string())
        );
        // ...but never falls back to a substring: "alp" resolves to nothing,
        // "alpha" only to its exact match.
        assert_eq!(
            index.resolve_page_id_by_title_exact("alp"),
            TitleResolution::NotFound
        );
        assert_eq!(
            index.resolve_page_id_by_title_exact("alpha"),
            TitleResolution::Unique("id-1".to_string())
        );
    }

    #[test]
    fn resolve_page_id_by_title_exact_reports_duplicate_titles_as_ambiguous() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Rust", &[], "one"),
            ("p2", "Rust", &[], "two"),
            ("p3", "Rust Programming", &[], "three"),
        ]);
        assert!(matches!(
            index.resolve_page_id_by_title_exact("Rust"),
            TitleResolution::Ambiguous(ids) if ids.len() == 2
        ));
        fs::remove_dir_all(&dir).unwrap();
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
        let title_index = compute_title_index(&pages, &sorted_ids);
        let index = KnowledgeBaseIndex {
            root: PathBuf::from("/kb"),
            pages,
            sorted_ids,
            title_index,
            index_issues: Vec::new(),
            backlinks: HashMap::new(),
            forward_links: HashMap::new(),
            duplicate_ids: Vec::new(),
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
    fn open_reports_duplicate_ids_across_files() -> anyhow::Result<()> {
        let dir = temp_dir_path("dup-ids");
        fs::create_dir_all(&dir)?;
        let page = |body: &str| {
            json!({
                "uid": {"uuid": "shared"},
                "pageType": {"title": "Whatever"},
                "tags": [],
                "children": {"items": [{"__type": "textSnippet", "string": body}]}
            })
        };
        let a = dir.join("a.lepiter");
        let b = dir.join("b.lepiter");
        fs::write(&a, serde_json::to_vec(&page("first"))?)?;
        fs::write(&b, serde_json::to_vec(&page("second"))?)?;

        let index = KnowledgeBase::open(&dir)?;
        fs::remove_dir_all(&dir)?;

        assert_eq!(index.pages.len(), 1);
        let dupes = index.find_duplicate_ids();
        assert_eq!(dupes.len(), 1);
        assert_eq!(dupes[0].id, "shared");
        assert_eq!(dupes[0].paths, vec![a, b]);
        Ok(())
    }

    #[test]
    fn open_reports_no_duplicate_ids_for_unique_kb() -> anyhow::Result<()> {
        let (dir, index) = make_kb_on_disk(&[("p1", "Alpha", &[], "a"), ("p2", "Beta", &[], "b")]);
        assert!(index.find_duplicate_ids().is_empty());
        fs::remove_dir_all(&dir).unwrap();
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

    #[test]
    fn register_page_reregistration_no_duplicate() {
        let dir = temp_dir_path("reregister");
        fs::create_dir_all(&dir).unwrap();
        let mut index = KnowledgeBase::open(&dir).unwrap();

        let meta = PageMeta {
            id: "dup".to_string(),
            id_lower: "dup".to_string(),
            title: "Original".to_string(),
            title_lower: "original".to_string(),
            path: dir.join("dup.lepiter"),
            updated_at: None,
            tags: Vec::new(),
            tags_lower: Vec::new(),
        };
        index.register_page(meta);
        assert_eq!(index.sorted_ids.len(), 1);

        // Re-register same id with a different title.
        let updated = PageMeta {
            id: "dup".to_string(),
            id_lower: "dup".to_string(),
            title: "Renamed".to_string(),
            title_lower: "renamed".to_string(),
            path: dir.join("dup.lepiter"),
            updated_at: None,
            tags: Vec::new(),
            tags_lower: Vec::new(),
        };
        index.register_page(updated);

        // sorted_ids must still contain the id exactly once.
        assert_eq!(index.sorted_ids.len(), 1);
        assert_eq!(index.sorted_ids[0], "dup");
        // The page data should reflect the updated title.
        assert_eq!(index.pages["dup"].title, "Renamed");

        fs::remove_dir_all(&dir).unwrap();
    }

    fn meta_with_title(id: &str, title: &str) -> PageMeta {
        PageMeta {
            id: id.to_string(),
            id_lower: id.to_lowercase(),
            title: title.to_string(),
            title_lower: title.to_lowercase(),
            path: PathBuf::from(format!("/tmp/{id}.lepiter")),
            updated_at: None,
            tags: Vec::new(),
            tags_lower: Vec::new(),
        }
    }

    #[test]
    fn register_page_title_change_updates_exact_title_index() {
        let dir = temp_dir_path("title-index-rename");
        fs::create_dir_all(&dir).unwrap();
        let mut index = KnowledgeBase::open(&dir).unwrap();

        index.register_page(meta_with_title("pg", "Old Title"));
        assert_eq!(
            index.resolve_page_id_by_title_exact("Old Title"),
            TitleResolution::Unique("pg".to_string())
        );

        // Rename the page; the exact index must follow the new title only.
        index.register_page(meta_with_title("pg", "New Title"));
        assert_eq!(
            index.resolve_page_id_by_title_exact("new title"),
            TitleResolution::Unique("pg".to_string())
        );
        assert_eq!(
            index.resolve_page_id_by_title_exact("Old Title"),
            TitleResolution::NotFound
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn register_page_duplicate_titles_resolve_ambiguous_via_index() {
        let dir = temp_dir_path("title-index-dup");
        fs::create_dir_all(&dir).unwrap();
        let mut index = KnowledgeBase::open(&dir).unwrap();

        index.register_page(meta_with_title("p1", "Shared"));
        index.register_page(meta_with_title("p2", "Shared"));

        match index.resolve_page_id_by_title_exact("shared") {
            TitleResolution::Ambiguous(ids) => {
                assert_eq!(ids.len(), 2);
                assert!(ids.contains(&"p1".to_string()));
                assert!(ids.contains(&"p2".to_string()));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn register_page_rename_out_of_shared_bucket_leaves_other_unique() {
        let dir = temp_dir_path("title-index-shrink");
        fs::create_dir_all(&dir).unwrap();
        let mut index = KnowledgeBase::open(&dir).unwrap();

        index.register_page(meta_with_title("p1", "Shared"));
        index.register_page(meta_with_title("p2", "Shared"));

        // Rename p1 away from the shared title: the bucket shrinks to one, so
        // "Shared" now resolves uniquely to p2 and "Renamed" to p1.
        index.register_page(meta_with_title("p1", "Renamed"));
        assert_eq!(
            index.resolve_page_id_by_title_exact("Shared"),
            TitleResolution::Unique("p2".to_string())
        );
        assert_eq!(
            index.resolve_page_id_by_title_exact("Renamed"),
            TitleResolution::Unique("p1".to_string())
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_link_graph_collects_edges() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "see [[Beta]]"),
            ("p2", "Beta", &[], "links to [a](page:p1) and [[Gamma]]"),
            ("p3", "Gamma", &[], "no links here"),
        ]);
        let graph = index.build_link_graph();
        assert_eq!(graph.edges.len(), 3);
        let pairs: Vec<(&str, &str)> = graph
            .edges
            .iter()
            .map(|e| (e.source.as_str(), e.target.as_str()))
            .collect();
        assert!(pairs.contains(&("p1", "p2")));
        assert!(pairs.contains(&("p2", "p1")));
        assert!(pairs.contains(&("p2", "p3")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_link_graph_excludes_self_links() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Alpha", &[], "see [[Alpha]]")]);
        let graph = index.build_link_graph();
        assert!(graph.edges.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_link_graph_deduplicates() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "[[Beta]] and [[Beta]] again"),
            ("p2", "Beta", &[], "nothing"),
        ]);
        let graph = index.build_link_graph();
        assert_eq!(graph.edges.len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn link_graph_ego_filters_by_page() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "see [[Beta]]"),
            ("p2", "Beta", &[], "see [[Gamma]]"),
            ("p3", "Gamma", &[], "nothing"),
        ]);
        let graph = index.build_link_graph();
        let ego = graph.ego("p2");
        assert_eq!(ego.len(), 2);
        let pairs: Vec<(&str, &str)> = ego
            .iter()
            .map(|e| (e.source.as_str(), e.target.as_str()))
            .collect();
        assert!(pairs.contains(&("p1", "p2")));
        assert!(pairs.contains(&("p2", "p3")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn link_graph_ego_unconnected_page() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "see [[Beta]]"),
            ("p2", "Beta", &[], "nothing"),
            ("p3", "Gamma", &[], "nothing"),
        ]);
        let graph = index.build_link_graph();
        assert!(graph.ego("p3").is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    // -----------------------------------------------------------------------
    // analyze_links
    // -----------------------------------------------------------------------

    #[test]
    fn analyze_links_detects_broken_links() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Page One", &[], "see [link](page:nonexistent) here"),
            ("p2", "Page Two", &[], "hello"),
        ]);
        let result = index.analyze_links();
        assert_eq!(result.broken_links.len(), 1);
        assert_eq!(result.broken_links[0].source_id, "p1");
        assert_eq!(result.broken_links[0].target, "page:nonexistent");
        assert!(result.load_errors.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_links_tracks_linked_pages() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Page One", &[], "see [link](page:p2) for more"),
            ("p2", "Page Two", &[], "target page"),
        ]);
        let result = index.analyze_links();
        assert!(result.linked_pages.contains("p2"));
        assert!(!result.linked_pages.contains("p1"));
        assert!(result.broken_links.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_links_empty_kb() {
        let (dir, index) = make_kb_on_disk(&[]);
        let result = index.analyze_links();
        assert!(result.broken_links.is_empty());
        assert!(result.linked_pages.is_empty());
        assert!(result.load_errors.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_links_captures_load_errors() {
        // Create a valid page, then corrupt its file after indexing.
        let (dir, index) = make_kb_on_disk(&[("p1", "Page One", &[], "hello")]);
        let page_path = dir.join("p1.lepiter");
        fs::write(&page_path, b"NOT VALID JSON").unwrap();
        let result = index.analyze_links();
        assert_eq!(result.load_errors.len(), 1);
        assert_eq!(result.load_errors[0].page_id, "p1");
        assert_eq!(result.load_errors[0].title, "Page One");
        fs::remove_dir_all(&dir).unwrap();
    }

    // -----------------------------------------------------------------------
    // orphan_ids
    // -----------------------------------------------------------------------

    #[test]
    fn orphan_ids_excludes_linked_pages() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Page One", &[], "see [link](page:p2) for more"),
            ("p2", "Page Two", &[], "target page"),
        ]);
        let result = index.analyze_links();
        let orphans = index.orphan_ids(&result.linked_pages, None);
        // p2 is linked to by p1, so only p1 should be orphan.
        assert_eq!(orphans, vec!["p1"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn orphan_ids_excludes_toc_page() {
        let (dir, index) = make_kb_on_disk(&[
            ("toc", "Table of Contents", &[], "hello"),
            ("p1", "Page One", &[], "world"),
        ]);
        let result = index.analyze_links();
        let orphans = index.orphan_ids(&result.linked_pages, Some("toc"));
        // toc excluded, only p1 should be orphan.
        assert_eq!(orphans, vec!["p1"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    // -----------------------------------------------------------------------
    // find_duplicate_titles
    // -----------------------------------------------------------------------

    #[test]
    fn find_duplicate_titles_none_when_unique() {
        let (dir, index) =
            make_kb_on_disk(&[("p1", "Alpha", &[], "body"), ("p2", "Beta", &[], "body")]);
        assert!(index.find_duplicate_titles().is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn find_duplicate_titles_detects_exact_match() {
        let (dir, index) =
            make_kb_on_disk(&[("p1", "Alpha", &[], "body"), ("p2", "Alpha", &[], "body")]);
        let dupes = index.find_duplicate_titles();
        assert_eq!(dupes.len(), 1);
        assert_eq!(dupes[0].title, "Alpha");
        assert_eq!(dupes[0].page_ids.len(), 2);
        assert!(dupes[0].page_ids.contains(&"p1".to_string()));
        assert!(dupes[0].page_ids.contains(&"p2".to_string()));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn find_duplicate_titles_case_insensitive() {
        let (dir, index) =
            make_kb_on_disk(&[("p1", "Alpha", &[], "body"), ("p2", "ALPHA", &[], "body")]);
        let dupes = index.find_duplicate_titles();
        assert_eq!(dupes.len(), 1);
        assert_eq!(dupes[0].page_ids.len(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn find_duplicate_titles_multiple_groups() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "body"),
            ("p2", "Alpha", &[], "body"),
            ("p3", "Beta", &[], "body"),
            ("p4", "Beta", &[], "body"),
            ("p5", "Gamma", &[], "body"),
        ]);
        let dupes = index.find_duplicate_titles();
        assert_eq!(dupes.len(), 2);
        // sorted alphabetically
        assert_eq!(dupes[0].title, "Alpha");
        assert_eq!(dupes[1].title, "Beta");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn find_duplicate_titles_empty_kb() {
        let (dir, index) = make_kb_on_disk(&[]);
        assert!(index.find_duplicate_titles().is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    // -----------------------------------------------------------------------
    // find_missing_attachments
    // -----------------------------------------------------------------------

    #[test]
    fn find_missing_attachments_none_when_no_refs() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Alpha", &[], "no attachment refs")]);
        assert!(index.find_missing_attachments().is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn find_missing_attachments_detects_missing_file() {
        let (dir, index) =
            make_kb_on_disk(&[("p1", "Alpha", &[], "see [img](attachments/missing.png)")]);
        let missing = index.find_missing_attachments();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].source_id, "p1");
        assert_eq!(missing[0].target, "attachments/missing.png");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn find_missing_attachments_ignores_existing_file() {
        let (dir, index) =
            make_kb_on_disk(&[("p1", "Alpha", &[], "see [img](attachments/present.png)")]);
        let att_dir = dir.join("attachments");
        fs::create_dir_all(&att_dir).unwrap();
        fs::write(att_dir.join("present.png"), b"data").unwrap();
        let missing = index.find_missing_attachments();
        assert!(missing.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn find_missing_attachments_mixed() {
        let (dir, index) = make_kb_on_disk(&[(
            "p1",
            "Alpha",
            &[],
            "see [a](attachments/ok.png) and [b](attachments/gone.png)",
        )]);
        let att_dir = dir.join("attachments");
        fs::create_dir_all(&att_dir).unwrap();
        fs::write(att_dir.join("ok.png"), b"data").unwrap();
        let missing = index.find_missing_attachments();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].target, "attachments/gone.png");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn find_missing_attachments_reports_every_referencing_page() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "see [img](attachments/missing.png)"),
            ("p2", "Beta", &[], "see [img](attachments/missing.png)"),
        ]);
        let missing = index.find_missing_attachments();
        assert_eq!(missing.len(), 2);
        let sources: Vec<&str> = missing.iter().map(|m| m.source_id.as_str()).collect();
        assert_eq!(sources, vec!["p1", "p2"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn find_missing_attachments_deduplicates_within_a_page() {
        let (dir, index) = make_kb_on_disk(&[(
            "p1",
            "Alpha",
            &[],
            "see [a](attachments/missing.png) and [b](attachments/missing.png)",
        )]);
        let missing = index.find_missing_attachments();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].source_id, "p1");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn find_missing_attachments_ignores_non_attachment_links() {
        let (dir, index) = make_kb_on_disk(&[(
            "p1",
            "Alpha",
            &[],
            "see [link](page:p2) and [ext](https://example.com)",
        )]);
        assert!(index.find_missing_attachments().is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    // -----------------------------------------------------------------------
    // analyze_all
    // -----------------------------------------------------------------------

    #[test]
    fn analyze_all_combines_broken_links_and_missing_attachments() {
        let (dir, index) = make_kb_on_disk(&[(
            "p1",
            "Alpha",
            &[],
            "see [link](page:nonexistent) and [img](attachments/gone.png)",
        )]);
        let result = index.analyze_all();
        assert_eq!(result.broken_links.len(), 1);
        assert_eq!(result.broken_links[0].target, "page:nonexistent");
        assert_eq!(result.missing_attachments.len(), 1);
        assert_eq!(result.missing_attachments[0].target, "attachments/gone.png");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_all_tracks_linked_pages() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "see [link](page:p2) here"),
            ("p2", "Beta", &[], "nothing"),
        ]);
        let result = index.analyze_all();
        assert!(result.linked_pages.contains("p2"));
        assert!(!result.linked_pages.contains("p1"));
        assert!(result.broken_links.is_empty());
        assert!(result.missing_attachments.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_all_empty_kb() {
        let (dir, index) = make_kb_on_disk(&[]);
        let result = index.analyze_all();
        assert!(result.broken_links.is_empty());
        assert!(result.linked_pages.is_empty());
        assert!(result.load_errors.is_empty());
        assert!(result.missing_attachments.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_all_captures_load_errors() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Page One", &[], "hello")]);
        fs::write(dir.join("p1.lepiter"), b"NOT VALID JSON").unwrap();
        let result = index.analyze_all();
        assert_eq!(result.load_errors.len(), 1);
        assert_eq!(result.load_errors[0].page_id, "p1");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_all_skips_existing_attachments() {
        let (dir, index) =
            make_kb_on_disk(&[("p1", "Alpha", &[], "see [img](attachments/present.png)")]);
        let att_dir = dir.join("attachments");
        fs::create_dir_all(&att_dir).unwrap();
        fs::write(att_dir.join("present.png"), b"data").unwrap();
        let result = index.analyze_all();
        assert!(result.missing_attachments.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_all_reports_missing_attachment_once_per_page() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "see [img](attachments/missing.png)"),
            (
                "p2",
                "Beta",
                &[],
                "see [a](attachments/missing.png) and [b](attachments/missing.png)",
            ),
            ("p3", "Gamma", &[], "see [img](attachments/missing.png)"),
        ]);
        let result = index.analyze_all();
        let reported: Vec<(&str, &str)> = result
            .missing_attachments
            .iter()
            .map(|m| (m.source_id.as_str(), m.source_title.as_str()))
            .collect();
        assert_eq!(
            reported,
            vec![("p1", "Alpha"), ("p2", "Beta"), ("p3", "Gamma")]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_all_mixed_links_and_attachments() {
        let (dir, index) = make_kb_on_disk(&[
            (
                "p1",
                "Alpha",
                &[],
                "see [link](page:p2) and [img](attachments/gone.png)",
            ),
            (
                "p2",
                "Beta",
                &[],
                "see [link](page:nonexistent) and [ext](https://example.com)",
            ),
        ]);
        let result = index.analyze_all();
        // p1 links to p2 (valid), p2 has a broken link
        assert!(result.linked_pages.contains("p2"));
        assert_eq!(result.broken_links.len(), 1);
        assert_eq!(result.broken_links[0].source_id, "p2");
        // p1 has a missing attachment
        assert_eq!(result.missing_attachments.len(), 1);
        assert_eq!(result.missing_attachments[0].source_id, "p1");
        assert!(result.load_errors.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    // -----------------------------------------------------------------------
    // scan_all_pages
    // -----------------------------------------------------------------------

    #[test]
    fn scan_all_pages_collects_edges_and_analysis() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "see [[Beta]]"),
            ("p2", "Beta", &[], "links to [a](page:p1) and [[Gamma]]"),
            ("p3", "Gamma", &[], "no links here"),
        ]);
        let result = index.scan_all_pages();
        // edges match build_link_graph output
        assert_eq!(result.edges.len(), 3);
        let pairs: Vec<(&str, &str)> = result
            .edges
            .iter()
            .map(|e| (e.source.as_str(), e.target.as_str()))
            .collect();
        assert!(pairs.contains(&("p1", "p2")));
        assert!(pairs.contains(&("p2", "p1")));
        assert!(pairs.contains(&("p2", "p3")));
        // linked_pages consistent with edges
        assert!(result.linked_pages.contains("p1"));
        assert!(result.linked_pages.contains("p2"));
        assert!(result.linked_pages.contains("p3"));
        assert!(result.broken_links.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_all_pages_edges_exclude_self_links() {
        let (dir, index) = make_kb_on_disk(&[("p1", "Alpha", &[], "see [[Alpha]]")]);
        let result = index.scan_all_pages();
        assert!(result.edges.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_all_pages_edges_deduplicated() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "[[Beta]] and [[Beta]] again"),
            ("p2", "Beta", &[], "nothing"),
        ]);
        let result = index.scan_all_pages();
        assert_eq!(result.edges.len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_all_pages_mixed_edges_and_broken() {
        let (dir, index) = make_kb_on_disk(&[
            (
                "p1",
                "Alpha",
                &[],
                "see [link](page:p2) and [bad](page:nonexistent)",
            ),
            ("p2", "Beta", &[], "nothing"),
        ]);
        let result = index.scan_all_pages();
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].source, "p1");
        assert_eq!(result.edges[0].target, "p2");
        assert_eq!(result.broken_links.len(), 1);
        assert_eq!(result.broken_links[0].target, "page:nonexistent");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_all_pages_wikilink_requires_exact_title_not_substring() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Guide", &[], "see [[Rust]]"),
            ("p2", "Rust Programming", &[], "nothing"),
        ]);
        let result = index.scan_all_pages();
        // `[[Rust]]` has no exact-title match: no edge, one broken link.
        assert!(
            result.edges.is_empty(),
            "substring title match fabricated a graph edge: {:?}",
            result.edges
        );
        assert_eq!(result.broken_links.len(), 1);
        assert_eq!(result.broken_links[0].source_id, "p1");
        assert_eq!(result.broken_links[0].target, "Rust");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_all_pages_empty_kb() {
        let (dir, index) = make_kb_on_disk(&[]);
        let result = index.scan_all_pages();
        assert!(result.edges.is_empty());
        assert!(result.broken_links.is_empty());
        assert!(result.linked_pages.is_empty());
        assert!(result.load_errors.is_empty());
        assert!(result.missing_attachments.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_all_pages_edges_match_build_link_graph() {
        let (dir, index) = make_kb_on_disk(&[
            ("p1", "Alpha", &[], "see [[Beta]]"),
            ("p2", "Beta", &[], "see [[Gamma]]"),
            ("p3", "Gamma", &[], "nothing"),
        ]);
        let scan = index.scan_all_pages();
        let graph = index.build_link_graph();
        assert_eq!(scan.edges.len(), graph.edges.len());
        for (a, b) in scan.edges.iter().zip(graph.edges.iter()) {
            assert_eq!(a.source, b.source);
            assert_eq!(a.target, b.target);
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}
