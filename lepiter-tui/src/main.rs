//! terminal cli and tui for browsing and editing lepiter knowledge bases.
//!
//! the binary is `lepiter-cli` and supports both command-line subcommands and
//! an interactive tui. the tui uses lazy loading for pages and renders snippets
//! with a markdown-like style.
//!
//! # quick start
//!
//! ```bash
//! lepiter-cli ./lepiter
//! lepiter-cli tui ./lepiter
//! ```
//!
//! # subcommands
//!
//! - `tui`: full-screen list, page reader, and snippet editor
//! - `list`, `ids`, `search`, `show`, `info`: non-interactive output
//!
//! # configuration
//!
//! - `LEPITER_TUI_PARSED_CACHE` / `LEPITER_TUI_RENDERED_CACHE` / `LEPITER_TUI_TEXT_INDEX_CACHE`: cache sizes
//! - `LEPITER_EDIT_AUTOSAVE_MS`: edit autosave delay
//! - `LEPITER_PLUGIN_CONFIG`: external snippet renderer config
//! - `LEPITER_OPEN_CMD`: shell command to open a page externally (receives
//!   `LEPITER_PAGE_ID` and `LEPITER_PAGE_PATH` as env vars)
//!
//! # docs
//!
//! - tui behavior: `docs/tui.md`
//! - editor: `docs/editor.md`
//! - plugins: `docs/plugins.md`
mod cli;
mod edit;
mod highlight;
mod inline;
mod keybindings;
mod plugins;
mod render;
mod util;

use std::cmp::Reverse;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::edit::{
    EditState, apply_cursor_marker, collect_snippets, ensure_scroll, load_raw_page,
    render_edit_page, wrap_lines_with_cursor_and_highlight,
};
use crate::keybindings::KeyResult;
use crate::plugins::PluginManager;
use crate::render::{
    RenderedPage, highlight_page_search, highlight_selected_link_markers, render_page,
    sanitize_for_terminal,
};
use crate::util::{LruCache, cache_limit_from_env, lower_byte_to_raw_byte, matching_snippet};
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyEventKind};
use lepiter_core::{
    KnowledgeBase, KnowledgeBaseIndex, LinkTargetKind, Page, PageId, PageMeta, SearchMatchKind,
    render_page_to_text,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    List,
    Search,
    Page,
    PageSearch,
    Backlinks,
    Edit,
    NewPageTitle,
}

#[derive(Debug, Clone)]
struct IndexedPageText {
    raw: String,
    lower: String,
}

struct App {
    index: KnowledgeBaseIndex,
    plugins: PluginManager,
    edit: Option<EditState>,
    visible_ids: Vec<PageId>,
    selected: usize,
    opened: Option<PageId>,
    parsed_cache: LruCache<String, Arc<Page>>,
    rendered_cache: LruCache<String, RenderedPage>,
    page_scroll: usize,
    selected_link: usize,
    search: String,
    search_needle: String,
    search_hit_kind: HashMap<PageId, SearchMatchKind>,
    text_index: LruCache<String, IndexedPageText>,
    text_index_queue: VecDeque<PageId>,
    history: VecDeque<PageId>,
    page_search: String,
    page_search_needle: String,
    page_search_match_lines: Vec<usize>,
    page_search_current: usize,
    backlink_ids: Vec<PageId>,
    backlink_selected: usize,
    new_page_title: String,
    mode: Mode,
    show_help: bool,
    status: String,
}

impl App {
    fn new(mut index: KnowledgeBaseIndex) -> Self {
        index.build_backlinks();
        let plugins = PluginManager::from_env();
        let max_parsed_cache = cache_limit_from_env("LEPITER_TUI_PARSED_CACHE", 128);
        let max_rendered_cache = cache_limit_from_env("LEPITER_TUI_RENDERED_CACHE", 128);
        let max_text_index = cache_limit_from_env("LEPITER_TUI_TEXT_INDEX_CACHE", 512);
        let mut app = Self {
            index,
            plugins,
            edit: None,
            visible_ids: Vec::new(),
            selected: 0,
            opened: None,
            parsed_cache: LruCache::new(max_parsed_cache),
            rendered_cache: LruCache::new(max_rendered_cache),
            page_scroll: 0,
            selected_link: 0,
            search: String::new(),
            search_needle: String::new(),
            search_hit_kind: HashMap::new(),
            text_index: LruCache::new(max_text_index),
            text_index_queue: VecDeque::new(),
            history: VecDeque::new(),
            page_search: String::new(),
            page_search_needle: String::new(),
            page_search_match_lines: Vec::new(),
            page_search_current: 0,
            backlink_ids: Vec::new(),
            backlink_selected: 0,
            new_page_title: String::new(),
            mode: Mode::List,
            show_help: false,
            status: String::new(),
        };
        app.plugins.apply_status(&mut app.status);
        app.reset_text_index_queue();
        app.rebuild_visible_ids();
        app
    }

    fn tick(&mut self) {
        self.advance_full_text_index(4);
        if self.mode == Mode::Edit {
            let mut saved_page = None;
            if let Some(edit) = self.edit.as_mut()
                && edit.maybe_autosave()
            {
                if let Err(err) = edit.save_to_disk() {
                    self.status = format!("edit save failed: {err:#}");
                } else {
                    saved_page = Some(edit.page_id.clone());
                }
            }
            if let Some(id) = saved_page {
                self.refresh_after_edit(&id);
            }
        }
    }

    fn update_search_needle(&mut self) {
        self.search_needle = self.search.trim().to_lowercase();
    }

    fn rebuild_visible_ids(&mut self) {
        let query = self.search.trim();
        if query.is_empty() {
            self.visible_ids = self.index.sorted_ids.clone();
            self.search_hit_kind.clear();
            self.reset_text_index_queue();
        } else {
            let mut hit_kind: HashMap<PageId, SearchMatchKind> = HashMap::new();
            for (id, kind) in self.index.filter_page_ids_scored(query) {
                hit_kind.insert(id, kind);
            }
            for (id, text) in self.text_index.iter() {
                if hit_kind.contains_key(id) {
                    continue;
                }
                if text.lower.contains(&self.search_needle) {
                    hit_kind.insert(id.clone(), SearchMatchKind::Content);
                }
            }

            let mut ids: Vec<_> = hit_kind.keys().cloned().collect();
            ids.sort_by_cached_key(|id| {
                let score = Reverse(hit_kind[id].score());
                let title = self
                    .index
                    .pages
                    .get(id)
                    .map(|m| m.title_lower.as_str())
                    .unwrap_or("")
                    .to_owned();
                (score, title)
            });
            self.visible_ids = ids;
            self.search_hit_kind = hit_kind;
        }
        if self.selected >= self.visible_ids.len() {
            self.selected = self.visible_ids.len().saturating_sub(1);
        }
    }

    fn selected_id(&self) -> Option<&str> {
        self.visible_ids.get(self.selected).map(String::as_str)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.visible_ids.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.visible_ids.len() as isize - 1;
        let next = (self.selected as isize + delta).clamp(0, max);
        self.selected = next as usize;
    }

    fn open_selected_page(&mut self) {
        let Some(id) = self.selected_id().map(ToOwned::to_owned) else {
            return;
        };
        self.open_page(&id, false);
        if self.search_hit_kind.get(&id) == Some(&SearchMatchKind::Content) {
            self.jump_to_search_match(&id);
        }
    }

    fn open_page(&mut self, id: &str, from_link: bool) {
        if !self.rendered_cache.touch(id) {
            let page = match self.get_or_load_page(id) {
                Ok(page) => page,
                Err(err) => {
                    self.status = format!("failed to load page: {err:#}");
                    return;
                }
            };
            let rendered = render_page(&page, &mut self.plugins);
            self.rendered_cache.insert(id.to_string(), rendered);
        }

        if from_link && let Some(current) = self.opened.as_ref() {
            if self.history.len() >= 200 {
                self.history.pop_front();
            }
            self.history.push_back(current.clone());
        }

        self.opened = Some(id.to_string());
        self.page_scroll = 0;
        self.selected_link = 0;
        self.clear_page_search();
        self.mode = Mode::Page;
        self.status.clear();
    }

    fn enter_edit_mode(&mut self) {
        let Some(page_id) = self.opened.clone() else {
            self.status = "no page loaded".to_string();
            return;
        };
        let Some(meta) = self.index.pages.get(&page_id) else {
            self.status = "page not found".to_string();
            return;
        };
        match load_raw_page(&meta.path) {
            Ok(raw) => {
                let mut snippets = Vec::new();
                collect_snippets(&raw, Vec::new(), &mut snippets);
                if snippets.is_empty() {
                    self.status = "no editable snippets".to_string();
                    return;
                }
                let buffer = snippets[0].text.clone();
                let edit = EditState::new(page_id, meta.path.clone(), raw, snippets, buffer);
                self.edit = Some(edit);
                self.mode = Mode::Edit;
                self.status.clear();
            }
            Err(err) => {
                self.status = format!("failed to load page json: {err:#}");
            }
        }
    }

    fn exit_edit_mode(&mut self) {
        if let Some(mut edit) = self.edit.take()
            && edit.dirty
        {
            if let Err(err) = edit.save_to_disk() {
                self.status = format!("edit save failed: {err:#}");
            } else {
                self.refresh_after_edit(&edit.page_id);
            }
        }
        self.mode = Mode::Page;
    }

    fn create_page(&mut self, title: &str) {
        let page_uuid = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let raw = serde_json::json!({
            "uid": { "uuid": &page_uuid },
            "pageType": { "title": title },
            "editTime": { "time": { "dateAndTimeString": &now } },
            "children": {
                "items": [
                    { "__type": "textSnippet", "string": "" }
                ]
            }
        });

        let file_path = self.index.root().join(format!("{page_uuid}.lepiter"));
        let bytes = match serde_json::to_vec_pretty(&raw) {
            Ok(b) => b,
            Err(err) => {
                self.status = format!("failed to serialize page: {err:#}");
                return;
            }
        };
        let dir = self.index.root();
        let tmp = match tempfile::NamedTempFile::new_in(dir) {
            Ok(t) => t,
            Err(err) => {
                self.status = format!("failed to create temp file: {err:#}");
                return;
            }
        };
        if let Err(err) = (|| -> anyhow::Result<()> {
            let mut tmp = tmp;
            tmp.write_all(&bytes)?;
            tmp.as_file().sync_all()?;
            tmp.persist(&file_path)?;
            Ok(())
        })() {
            self.status = format!("failed to write page: {err:#}");
            return;
        }

        let id_lower = page_uuid.to_lowercase();
        let title_lower = title.to_lowercase();
        let meta = PageMeta {
            id: page_uuid.clone(),
            id_lower,
            title: title.to_string(),
            title_lower,
            path: file_path,
            updated_at: chrono::DateTime::parse_from_rfc3339(&now).ok(),
            tags: Vec::new(),
            tags_lower: Vec::new(),
        };
        self.index.register_page(meta);
        // Existing pages may link to this title; rebuild to pick them up.
        self.index.build_backlinks();
        self.rebuild_visible_ids();
        self.open_page(&page_uuid, false);
        self.enter_edit_mode();
    }

    fn refresh_after_edit(&mut self, id: &str) {
        if let Ok(page) = self.index.load_page(id) {
            let page = Arc::new(page);
            self.parsed_cache.insert(id.to_string(), Arc::clone(&page));
            let rendered = render_page(&page, &mut self.plugins);
            self.rendered_cache.insert(id.to_string(), rendered);
            let raw = render_page_to_text(&page);
            self.text_index.insert(
                id.to_string(),
                IndexedPageText {
                    raw: raw.clone(),
                    lower: raw.to_lowercase(),
                },
            );
        }
        self.index.update_backlinks_for(id);
    }

    fn get_or_load_page(&mut self, id: &str) -> Result<Arc<Page>> {
        if let Some(page) = self.parsed_cache.get(id) {
            return Ok(Arc::clone(page));
        }

        let page = Arc::new(self.index.load_page(id)?);
        self.parsed_cache.insert(id.to_string(), Arc::clone(&page));
        Ok(page)
    }

    fn reset_text_index_queue(&mut self) {
        self.text_index_queue = self.index.sorted_ids.iter().cloned().collect();
    }

    fn back_to_list(&mut self) {
        self.mode = Mode::List;
    }

    fn back_in_history(&mut self) {
        if let Some(prev) = self.history.pop_back() {
            self.open_page(&prev, false);
        } else {
            self.mode = Mode::List;
        }
    }

    fn show_backlinks(&mut self) {
        let Some(page_id) = &self.opened else {
            self.status = "no page open".to_string();
            return;
        };
        let backlinks = self.index.backlinks_for(page_id).to_vec();
        if backlinks.is_empty() {
            self.status = "no backlinks for this page".to_string();
            return;
        }
        self.backlink_ids = backlinks;
        self.backlink_selected = 0;
        self.mode = Mode::Backlinks;
    }

    fn move_backlink_selection(&mut self, delta: isize) {
        if self.backlink_ids.is_empty() {
            self.backlink_selected = 0;
            return;
        }
        let max = self.backlink_ids.len() as isize - 1;
        let next = (self.backlink_selected as isize + delta).clamp(0, max);
        self.backlink_selected = next as usize;
    }

    fn open_selected_backlink(&mut self) {
        let Some(id) = self.backlink_ids.get(self.backlink_selected).cloned() else {
            return;
        };
        self.open_page(&id, true);
    }

    fn scroll_page(&mut self, delta: isize) {
        // Clamp page_scroll to content length before applying delta.
        // 'G' sets page_scroll to a huge sentinel; without this clamp,
        // small deltas (k/Up) cannot bring it back to a usable range.
        if let Some(max) = self
            .opened
            .as_ref()
            .and_then(|id| self.rendered_cache.peek(id))
            .map(|p| p.lines.len())
        {
            self.page_scroll = self.page_scroll.min(max);
        }
        let next = self.page_scroll as isize + delta;
        self.page_scroll = next.max(0) as usize;
    }

    fn current_rendered_page(&self) -> Option<&RenderedPage> {
        let id = self.opened.as_ref()?;
        self.rendered_cache.peek(id)
    }

    fn move_link_selection(&mut self, delta: isize) {
        let Some(page) = self.current_rendered_page() else {
            return;
        };
        if page.links.is_empty() {
            self.selected_link = 0;
            return;
        }
        let max = page.links.len() as isize - 1;
        let next = (self.selected_link as isize + delta).clamp(0, max);
        self.selected_link = next as usize;
    }

    fn open_externally(&mut self) {
        let Some(page_id) = &self.opened else {
            self.status = "no page open".to_string();
            return;
        };
        let Some(meta) = self.index.pages.get(page_id) else {
            self.status = "page not found in index".to_string();
            return;
        };
        let cmd = match std::env::var("LEPITER_OPEN_CMD") {
            Ok(cmd) if !cmd.trim().is_empty() => cmd,
            _ => {
                self.status = "set LEPITER_OPEN_CMD to enable open-externally".to_string();
                return;
            }
        };
        match std::process::Command::new("sh")
            .args(["-c", &cmd])
            .env("LEPITER_PAGE_ID", page_id)
            .env("LEPITER_PAGE_PATH", &meta.path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => self.status = "opened externally".to_string(),
            Err(err) => self.status = format!("failed to open externally: {err:#}"),
        }
    }

    fn follow_selected_link(&mut self) {
        let Some(page) = self.current_rendered_page() else {
            return;
        };
        let Some(link) = page.links.get(self.selected_link) else {
            self.status = "no link selected".to_string();
            return;
        };

        match self.index.classify_link_target(&link.target) {
            LinkTargetKind::InternalPage(target_id) => {
                self.open_page(&target_id, true);
            }
            LinkTargetKind::AttachmentPath(_path) => {
                let resolver = self.index.attachment_resolver();
                match resolver.resolve_existing(&link.target) {
                    Ok(path) => {
                        let display = path.display().to_string();
                        match open_with_system(&display) {
                            Ok(()) => self.status = format!("opened attachment: {display}"),
                            Err(err) => self.status = format!("failed to open attachment: {err:#}"),
                        }
                    }
                    Err(err) => self.status = format!("attachment error: {err}"),
                }
            }
            LinkTargetKind::ExternalUrl(url) => match open_with_system(&url) {
                Ok(()) => self.status = format!("opened external link: {url}"),
                Err(err) => self.status = format!("failed to open external link: {err:#}"),
            },
            LinkTargetKind::Unknown(raw) => {
                self.status = format!("unresolved link target: {raw}");
            }
        }
    }

    fn advance_full_text_index(&mut self, batch_size: usize) {
        if self.search_needle.is_empty() {
            return;
        }

        let mut changed = false;

        for _ in 0..batch_size {
            let Some(id) = self.text_index_queue.pop_front() else {
                break;
            };
            if self.text_index.contains_key(&id) {
                continue;
            }

            let Ok(page) = self.get_or_load_page(&id) else {
                continue;
            };
            let raw = render_page_to_text(&page);
            let lower = raw.to_lowercase();
            if lower.contains(&self.search_needle) {
                changed = true;
            }
            self.text_index.insert(id, IndexedPageText { raw, lower });
        }

        if changed {
            self.rebuild_visible_ids();
        }
    }

    fn snippet_for(&self, id: &str) -> Option<String> {
        let text = self.text_index.peek(id)?;
        matching_snippet(&text.raw, &self.search_needle)
    }

    fn jump_to_search_match(&mut self, id: &str) {
        if self.search_needle.is_empty() {
            return;
        }
        let Some(page) = self.rendered_cache.peek(id) else {
            return;
        };
        let mut line_idx = 0usize;
        for (idx, line) in page.lines.iter().enumerate() {
            let text = line
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .to_lowercase();
            if text.contains(&self.search_needle) {
                line_idx = idx;
                break;
            }
        }
        self.page_scroll = line_idx;
    }

    fn update_page_search_needle(&mut self) {
        self.page_search_needle = self.page_search.trim().to_lowercase();
    }

    fn rebuild_page_search_matches(&mut self) {
        self.page_search_match_lines.clear();
        self.page_search_current = 0;
        if self.page_search_needle.is_empty() {
            return;
        }
        let id = match self.opened.as_ref() {
            Some(id) => id.clone(),
            None => return,
        };
        let Some(page) = self.rendered_cache.peek(&id) else {
            return;
        };
        let needle = self.page_search_needle.clone();
        let matches: Vec<usize> = page
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                let text = line
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .to_lowercase();
                text.contains(&needle)
            })
            .map(|(idx, _)| idx)
            .collect();
        self.page_search_match_lines = matches;
    }

    fn page_search_next(&mut self) {
        if self.page_search_match_lines.is_empty() {
            return;
        }
        self.page_search_current =
            (self.page_search_current + 1) % self.page_search_match_lines.len();
        self.page_scroll = self.page_search_match_lines[self.page_search_current];
    }

    fn page_search_prev(&mut self) {
        if self.page_search_match_lines.is_empty() {
            return;
        }
        if self.page_search_current == 0 {
            self.page_search_current = self.page_search_match_lines.len() - 1;
        } else {
            self.page_search_current -= 1;
        }
        self.page_scroll = self.page_search_match_lines[self.page_search_current];
    }

    fn clear_page_search(&mut self) {
        self.page_search.clear();
        self.page_search_needle.clear();
        self.page_search_match_lines.clear();
        self.page_search_current = 0;
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(cmd) = args.next() else {
        print_usage();
        return Ok(());
    };

    let result = match cmd.as_str() {
        "tui" => {
            let kb_path = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("./lepiter"));
            run_tui(kb_path)
        }
        "info" => cli::run_info(args.collect()),
        "list" => cli::run_list(args.collect()),
        "ids" => cli::run_ids(args.collect()),
        "search" => cli::run_search(args.collect()),
        "show" => cli::run_show(args.collect()),
        "links" => cli::run_links(args.collect()),
        "tags" => cli::run_tags(args.collect()),
        "check" => cli::run_check(args.collect()),
        "export" => cli::run_export(args.collect()),
        "import" => cli::run_import(args.collect()),
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        other => {
            let maybe_path = PathBuf::from(other);
            if maybe_path.is_dir() {
                cli::print_kb_info(maybe_path, false, false)
            } else {
                eprintln!("unknown subcommand: {other}");
                print_usage();
                std::process::exit(2);
            }
        }
    };

    match result {
        Err(err) if err.downcast_ref::<cli::UsageError>().is_some() => {
            eprintln!("{err}");
            std::process::exit(2);
        }
        other => other,
    }
}

fn run_tui(kb_path: PathBuf) -> Result<()> {
    let index = KnowledgeBase::open(kb_path)?;

    let mut terminal = ratatui::init();
    let app = App::new(index);
    let result = run_app(&mut terminal, app);
    ratatui::restore();
    result
}

fn print_usage() {
    eprintln!(
        "lepiter-cli <subcommand|kb-path> [args]\n\nsubcommands:\n  tui [kb-path]                                      launch the terminal reader (default path: ./lepiter)\n  info [--detail] [--json] [kb-path]                 print knowledge base metadata summary\n  list [--tsv] [--json] [kb-path]                    list pages (pretty columns by default)\n  ids [kb-path]                                      print page ids only (sorted by title)\n  search [--full-text] [--tsv] [--json] <query> [kb-path]  search by title/id/tags, optionally page content\n  show [--id|--by-title] [--open-links] [--json] <value> [kb-path]  render one page (default: title lookup)\n  links [--dot] [--json] [--for <page>] [kb-path]    show the page link graph\n  tags [--tsv] [--json] [--for <tag>] [kb-path]      list tags with page counts, or pages for a tag\n  check [--json] [kb-path]                           validate knowledge base integrity\n  export <output-dir> [kb-path]                      bulk-export pages to markdown with yaml frontmatter\n  import <input-dir> [kb-path]                       import markdown files back into lepiter page json\n\nIf the first argument is a directory path, `info` mode is used implicitly.\n\ninfo flags:\n  --detail  show broken links, orphan pages, tag distribution, snippet type breakdown\n  --json    output as json (combinable with --detail)\n\ncheck flags:\n  --json  output as json\n  exits with status 1 if any issues are found (broken links, orphan pages, duplicate titles, duplicate ids, missing attachments)\n\nlinks flags:\n  --dot       output as graphviz dot\n  --json      output as json with nodes and edges arrays\n  --for PAGE  show only links involving PAGE (ego graph, resolved by title)\n\ntags flags:\n  --for TAG   list pages tagged with TAG (case-insensitive)\n  --tsv       output as tab-separated values\n  --json      output as json\n\njson flags:\n  --json on list outputs page metadata as a json array\n  --json on search includes match kind alongside page metadata\n  --json on show serializes the full parsed page structure"
    );
}

fn highlight_search_match(text: &str, query: &str) -> Line<'static> {
    let needle = query.trim();
    if needle.is_empty() {
        return Line::raw(text.to_string());
    }
    let lower_text = text.to_lowercase();
    let lower_needle = needle.to_lowercase();
    let Some(pos) = lower_text.find(&lower_needle) else {
        return Line::raw(text.to_string());
    };
    let start_byte = lower_byte_to_raw_byte(text, pos);
    let end_byte = lower_byte_to_raw_byte(text, pos + lower_needle.len());
    let before = &text[..start_byte];
    let matched = &text[start_byte..end_byte];
    let after = &text[end_byte..];
    let style = Style::default()
        .bg(Color::Yellow)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);
    Line::from(vec![
        Span::raw(before.to_string()),
        Span::styled(matched.to_string(), style),
        Span::raw(after.to_string()),
    ])
}

fn run_app(terminal: &mut DefaultTerminal, mut app: App) -> Result<()> {
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if !event::poll(Duration::from_millis(100))? {
            app.tick();
            continue;
        }

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match app.handle_key(key) {
            KeyResult::Quit => break,
            KeyResult::Continue => {}
            KeyResult::Tick => app.tick(),
            KeyResult::SearchTick => app.advance_full_text_index(4),
        }
    }

    Ok(())
}

fn ui(frame: &mut Frame, app: &mut App) {
    match app.mode {
        Mode::List | Mode::Search | Mode::NewPageTitle => render_list_view(frame, app),
        Mode::Page | Mode::PageSearch => render_page_view(frame, app),
        Mode::Backlinks => render_backlinks_view(frame, app),
        Mode::Edit => render_edit_view(frame, app),
    }
    if app.show_help {
        render_help_overlay(frame, app.mode);
    }
}

fn render_list_view(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(frame.area());

    if app.mode == Mode::NewPageTitle {
        let title_style = Style::default().fg(Color::Green);
        let title_bar = Paragraph::new(Line::from(vec![
            Span::styled("> ", title_style.add_modifier(Modifier::BOLD)),
            Span::styled(
                app.new_page_title.clone(),
                Style::default().fg(Color::White),
            ),
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("New Page Title (Enter to create, Esc to cancel)")
                .border_style(title_style),
        );
        frame.render_widget(title_bar, chunks[0]);
    } else {
        let search_title = if app.mode == Mode::Search {
            "Search (typing)"
        } else {
            "Search (/)"
        };
        let search_style = if app.mode == Mode::Search {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Gray)
        };
        let search_bar = Paragraph::new(Line::from(vec![
            Span::styled("> ", search_style.add_modifier(Modifier::BOLD)),
            Span::styled(app.search.clone(), Style::default().fg(Color::White)),
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(search_title)
                .border_style(search_style),
        );
        frame.render_widget(search_bar, chunks[0]);
    }

    let items = app
        .visible_ids
        .iter()
        .map(|id| {
            let meta = &app.index.pages[id];
            let mut text = format!("{}  [{}]", meta.title, meta.id);
            if let Some(kind) = app.search_hit_kind.get(id)
                && *kind == SearchMatchKind::Content
                && let Some(snippet) = app.snippet_for(id)
            {
                text.push_str("  :: ");
                text.push_str(&snippet);
            }
            if !meta.tags.is_empty() {
                text.push_str("  #");
                text.push_str(&meta.tags.join(" #"));
            }
            let line = highlight_search_match(&sanitize_for_terminal(&text), &app.search);
            ListItem::new(line)
        })
        .collect::<Vec<_>>();

    let mut state = ListState::default();
    state.select(if app.visible_ids.is_empty() {
        None
    } else {
        Some(app.selected)
    });

    let title = if app.mode == Mode::Search {
        "Pages (filtered)".to_string()
    } else {
        "Pages".to_string()
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .highlight_style(Style::default().bg(Color::DarkGray));
    frame.render_stateful_widget(list, chunks[1], &mut state);

    let mut status = format!(
        "matches: {} | index: {}/{} | cache p/r: {}/{} | j/k move | enter open | / search | n new | ? help | q quit",
        app.visible_ids.len(),
        app.text_index.len(),
        app.index.pages.len(),
        app.parsed_cache.len(),
        app.rendered_cache.len()
    );
    if !app.status.is_empty() {
        status.push_str(" | ");
        status.push_str(&app.status);
    }
    let dashboard = format!("{}\n{}", app.plugins.status_line(), status);
    frame.render_widget(
        Paragraph::new(dashboard).style(Style::default().fg(Color::Gray)),
        chunks[2],
    );
}

fn render_page_view(frame: &mut Frame, app: &App) {
    let has_search_bar = app.mode == Mode::PageSearch;
    let mut constraints = vec![
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(3),
    ];
    if has_search_bar {
        constraints.push(Constraint::Length(1));
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(frame.area());

    let header = if let Some(page) = app.current_rendered_page() {
        format!("{} [{}]", page.title, page.id)
    } else {
        "No page loaded".to_string()
    };
    frame.render_widget(
        Paragraph::new(header).style(Style::default().fg(Color::Cyan)),
        chunks[0],
    );

    let text = if let Some(page) = app.current_rendered_page() {
        let mut lines = if page.links.is_empty() {
            page.lines.clone()
        } else {
            highlight_selected_link_markers(&page.lines, app.selected_link + 1)
        };
        if !app.page_search_needle.is_empty() {
            let current_line = app
                .page_search_match_lines
                .get(app.page_search_current)
                .copied();
            lines = highlight_page_search(&lines, &app.page_search_needle, current_line);
        }
        Text::from(lines)
    } else {
        Text::from(vec![Line::raw("Press Enter on a page from the list")])
    };

    let paragraph = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Page")
                .border_style(Style::default().fg(Color::Blue)),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.page_scroll.min(u16::MAX as usize) as u16, 0));
    frame.render_widget(paragraph, chunks[1]);

    let mut footer = if !app.page_search_needle.is_empty() {
        let total = app.page_search_match_lines.len();
        if total > 0 {
            format!(
                "match {}/{} | n/N next/prev | / search | Esc clear | ? help | q quit",
                app.page_search_current + 1,
                total
            )
        } else {
            "no matches | / search | Esc clear | ? help | q quit".to_string()
        }
    } else {
        "j/k scroll | tab/backtab link | enter follow | / search | B backlinks | h back | b list | ? help | q quit".to_string()
    };
    if let Some(page) = app.current_rendered_page() {
        if let Some(link) = page.links.get(app.selected_link) {
            footer.push('\n');
            footer.push_str(&format!(
                "link {}/{}: {} -> {}",
                app.selected_link + 1,
                page.links.len(),
                link.label,
                link.target
            ));
        } else {
            footer.push_str("\nno links on page");
        }
    }
    let dashboard = format!("{}\n{}", app.plugins.status_line(), footer);
    frame.render_widget(Paragraph::new(dashboard), chunks[2]);

    if has_search_bar {
        let search_input = Line::from(vec![
            Span::styled(
                "/",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(app.page_search.clone(), Style::default().fg(Color::White)),
        ]);
        frame.render_widget(
            Paragraph::new(search_input).style(Style::default().bg(Color::Black)),
            chunks[3],
        );
    }
}

fn render_backlinks_view(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(frame.area());

    let source_title = app
        .opened
        .as_ref()
        .and_then(|id| app.index.pages.get(id))
        .map(|m| m.title.as_str())
        .unwrap_or("?");
    let header = Paragraph::new(format!("Backlinks to: {source_title}")).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Backlinks (B)")
            .border_style(Style::default().fg(Color::Magenta)),
    );
    frame.render_widget(header, chunks[0]);

    let items = app
        .backlink_ids
        .iter()
        .map(|id| {
            let meta = &app.index.pages[id];
            let text = format!("{}  [{}]", meta.title, meta.id);
            ListItem::new(sanitize_for_terminal(&text))
        })
        .collect::<Vec<_>>();

    let mut state = ListState::default();
    state.select(if app.backlink_ids.is_empty() {
        None
    } else {
        Some(app.backlink_selected)
    });

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("{} incoming links", app.backlink_ids.len()))
                .border_style(Style::default().fg(Color::Blue)),
        )
        .highlight_style(Style::default().bg(Color::DarkGray));
    frame.render_stateful_widget(list, chunks[1], &mut state);

    let footer = "j/k navigate | enter open | esc back to page | q quit";
    let dashboard = format!("{}\n{}", app.plugins.status_line(), footer);
    frame.render_widget(
        Paragraph::new(dashboard).style(Style::default().fg(Color::Gray)),
        chunks[2],
    );
}

fn render_edit_view(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let (header_text, mut page_lines, mut cursor, mut highlight_line, scroll) =
        if let Some(edit) = app.edit.as_mut() {
            let total = edit.snippets.len();
            let (typ, editable) = edit
                .current()
                .map(|s| (s.typ.clone(), s.editable))
                .unwrap_or_else(|| ("<unknown>".to_string(), false));
            let status = if editable { "editable" } else { "read-only" };
            let header = format!(
                "edit {} ({}/{}) [{}] {}",
                edit.page_id,
                edit.selected + 1,
                total,
                typ,
                status
            );
            let (lines, cursor, highlight_start) = render_edit_page(edit, &mut app.plugins);
            let scroll = edit.preview_scroll;
            (header, lines, cursor, highlight_start, scroll)
        } else {
            ("edit".to_string(), Vec::new(), None, None, 0usize)
        };

    frame.render_widget(
        Paragraph::new(header_text).style(Style::default().fg(Color::Cyan)),
        chunks[0],
    );

    let wrap_width = chunks[1].width.saturating_sub(2) as usize;
    let (wrapped_lines, wrapped_cursor, wrapped_highlight) =
        wrap_lines_with_cursor_and_highlight(&page_lines, wrap_width, cursor, highlight_line);
    cursor = wrapped_cursor;
    highlight_line = wrapped_highlight;
    page_lines = wrapped_lines;

    if let Some(edit) = app.edit.as_mut() {
        let view_height = chunks[1].height.saturating_sub(2) as usize;
        if edit.follow_cursor {
            if let Some((cursor_line, _)) = cursor {
                edit.preview_scroll = ensure_scroll(cursor_line, edit.preview_scroll, view_height);
            } else if let Some(line) = highlight_line {
                edit.preview_scroll = ensure_scroll(line, edit.preview_scroll, view_height);
            }
        }
        let max_scroll = page_lines.len().saturating_sub(view_height);
        if edit.preview_scroll > max_scroll {
            edit.preview_scroll = max_scroll;
        }
    }

    let scroll = app
        .edit
        .as_ref()
        .map(|e| e.preview_scroll)
        .unwrap_or(scroll);
    if let Some((cursor_line, cursor_col)) = cursor {
        apply_cursor_marker(&mut page_lines, cursor_line, cursor_col);
    }
    let text = Text::from(page_lines);
    let paragraph = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Page")
                .border_style(Style::default().fg(Color::Blue)),
        )
        .scroll((scroll as u16, 0));
    frame.render_widget(paragraph, chunks[1]);

    let footer = "tab/backtab snippet | arrows move | pgup/pgdn page | type to edit | ctrl+u undo | esc done";
    let dashboard = format!("{}\n{}", app.plugins.status_line(), footer);
    frame.render_widget(
        Paragraph::new(dashboard).style(Style::default().fg(Color::Gray)),
        chunks[2],
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect::new(x, y, w, h)
}

fn render_help_overlay(frame: &mut Frame, mode: Mode) {
    let key = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let desc = Style::default().fg(Color::White);
    let heading = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);

    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        "  Keyboard Shortcuts",
        heading,
    )]));
    lines.push(Line::raw(""));

    let add_section =
        |lines: &mut Vec<Line<'static>>, title: &'static str, bindings: &[(&str, &str)]| {
            lines.push(Line::from(vec![
                Span::styled("  ", dim),
                Span::styled(title, heading),
            ]));
            for (k, d) in bindings {
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(format!("{:<16}", k), key),
                    Span::styled((*d).to_string(), desc),
                ]));
            }
            lines.push(Line::raw(""));
        };

    add_section(
        &mut lines,
        "Global",
        &[
            ("q", "quit"),
            ("?", "toggle this help"),
            ("Esc", "back / dismiss"),
        ],
    );

    match mode {
        Mode::List | Mode::NewPageTitle => {
            add_section(
                &mut lines,
                "List",
                &[
                    ("j / Down", "move down"),
                    ("k / Up", "move up"),
                    ("Enter", "open page"),
                    ("/", "search pages"),
                    ("n", "create new page"),
                ],
            );
        }
        Mode::Search => {
            add_section(
                &mut lines,
                "Search",
                &[
                    ("type", "filter pages"),
                    ("Backspace", "delete character"),
                    ("Up / Down", "navigate results"),
                    ("Enter", "open selected"),
                    ("Esc", "cancel search"),
                ],
            );
        }
        Mode::Page => {
            add_section(
                &mut lines,
                "Page",
                &[
                    ("j / Down", "scroll down"),
                    ("k / Up", "scroll up"),
                    ("PgUp / PgDn", "half-page scroll"),
                    ("g", "go to top"),
                    ("G", "go to bottom"),
                    ("/", "search in page"),
                    ("n / N", "next / prev match"),
                    ("Tab / Shift+Tab", "next / prev link"),
                    ("Enter", "follow link"),
                    ("B", "show backlinks"),
                    ("e", "edit page"),
                    ("O", "open externally"),
                    ("h", "back in history"),
                    ("b", "back to list"),
                    ("Esc", "clear search / back"),
                ],
            );
        }
        Mode::PageSearch => {
            add_section(
                &mut lines,
                "Page Search",
                &[
                    ("type", "search page content"),
                    ("Backspace", "delete character"),
                    ("Enter", "confirm search"),
                    ("Esc", "cancel search"),
                ],
            );
        }
        Mode::Backlinks => {
            add_section(
                &mut lines,
                "Backlinks",
                &[
                    ("j / Down", "move down"),
                    ("k / Up", "move up"),
                    ("Enter", "open page"),
                    ("Esc", "back to page"),
                ],
            );
        }
        Mode::Edit => {
            add_section(
                &mut lines,
                "Edit",
                &[
                    ("Arrows", "move cursor"),
                    ("Home / End", "start / end of text"),
                    ("PgUp / PgDn", "scroll preview"),
                    ("Tab / Shift+Tab", "next / prev snippet"),
                    ("Ctrl+A", "append new text snippet"),
                    ("Ctrl+U", "undo"),
                    ("Esc", "save and exit"),
                ],
            );
        }
    }

    lines.push(Line::from(vec![Span::styled(
        "  press ? or Esc to close",
        dim,
    )]));

    let content_height = lines.len() as u16 + 2; // +2 for borders
    let content_width = 46;
    let area = centered_rect(content_width, content_height, frame.area());

    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help ")
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black));
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn open_with_system(target: &str) -> Result<()> {
    open::that(target).with_context(|| format!("failed to open target `{target}`"))?;
    Ok(())
}
