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
mod edit;
mod highlight;
mod inline;
mod keybindings;
mod plugins;
mod render;
mod util;

use std::collections::HashMap;
use std::collections::VecDeque;
use std::fs;
use std::io::{IsTerminal, Write as _};
use std::path::PathBuf;
use std::time::Duration;

use crate::edit::{
    EditState, apply_cursor_marker, collect_snippets, ensure_scroll, load_raw_page,
    render_edit_page, wrap_lines_with_cursor_and_highlight,
};
use crate::highlight::{CodeToken, tokenize_code_line};
use crate::keybindings::KeyResult;
use crate::plugins::PluginManager;
use crate::render::{
    RenderedPage, highlight_page_search, highlight_selected_link_markers, render_page,
    sanitize_for_terminal,
};
use crate::util::{LruCache, cache_limit_from_env, lower_byte_to_raw_byte};
use anyhow::{Context, Result, bail};
use crossterm::event::{self, Event, KeyEventKind};
use lepiter_core::{
    KnowledgeBase, KnowledgeBaseIndex, LinkEdge, LinkTargetKind, Node, Page, PageId, PageMeta,
    ParseIssue, SearchMatchKind, TitleResolution, render_page_to_text,
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
    parsed_cache: LruCache<String, Page>,
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
            ids.sort_by(|a, b| {
                let sa = hit_kind[a].score();
                let sb = hit_kind[b].score();
                sb.cmp(&sa).then_with(|| {
                    let ta = self
                        .index
                        .pages
                        .get(a)
                        .map(|m| m.title_lower.as_str())
                        .unwrap_or("");
                    let tb = self
                        .index
                        .pages
                        .get(b)
                        .map(|m| m.title_lower.as_str())
                        .unwrap_or("");
                    ta.cmp(tb)
                })
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
            self.parsed_cache.insert(id.to_string(), page.clone());
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

    fn get_or_load_page(&mut self, id: &str) -> Result<Page> {
        if let Some(page) = self.parsed_cache.get(id) {
            return Ok(page.clone());
        }

        let page = self.index.load_page(id)?;
        self.parsed_cache.insert(id.to_string(), page.clone());
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
        if self.search_needle.is_empty() {
            return None;
        }
        let lower_idx = text.lower.find(&self.search_needle)?;

        // Map byte offsets from lowered text to raw text — lowercasing can
        // change byte lengths for non-ASCII characters.
        let raw_match = lower_byte_to_raw_byte(&text.raw, lower_idx);
        let raw_end = lower_byte_to_raw_byte(&text.raw, lower_idx + self.search_needle.len());

        let start = text.raw.floor_char_boundary(raw_match.saturating_sub(40));
        let end = text
            .raw
            .ceil_char_boundary((raw_end + 80).min(text.raw.len()));
        let fragment = text.raw[start..end].replace('\n', " ");
        let fragment = fragment.trim();
        if fragment.is_empty() {
            None
        } else {
            Some(truncate_chars(fragment, 120))
        }
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

    match cmd.as_str() {
        "tui" => {
            let kb_path = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("./lepiter"));
            run_tui(kb_path)
        }
        "info" => {
            let rest = args.collect::<Vec<_>>();
            run_info(rest)
        }
        "list" => {
            let rest = args.collect::<Vec<_>>();
            run_list(rest)
        }
        "ids" => {
            let kb_path = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("./lepiter"));
            print_page_ids(kb_path)
        }
        "search" => {
            let rest = args.collect::<Vec<_>>();
            run_search(rest)
        }
        "show" => {
            let rest = args.collect::<Vec<_>>();
            run_show(rest)
        }
        "links" => {
            let rest = args.collect::<Vec<_>>();
            run_links(rest)
        }
        "tags" => {
            let rest = args.collect::<Vec<_>>();
            run_tags(rest)
        }
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        other => {
            let maybe_path = PathBuf::from(other);
            if maybe_path.is_dir() {
                print_kb_info(maybe_path, false, false)
            } else {
                eprintln!("unknown subcommand: {other}");
                print_usage();
                std::process::exit(2);
            }
        }
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
        "lepiter-cli <subcommand|kb-path> [args]\n\nsubcommands:\n  tui [kb-path]                                      launch the terminal reader (default path: ./lepiter)\n  info [--detail] [--json] [kb-path]                 print knowledge base metadata summary\n  list [--tsv] [--json] [kb-path]                    list pages (pretty columns by default)\n  ids [kb-path]                                      print page ids only (sorted by title)\n  search [--full-text] [--tsv] [--json] <query> [kb-path]  search by title/id/tags, optionally page content\n  show [--id|--by-title] [--open-links] [--json] <value> [kb-path]  render one page (default: title lookup)\n  links [--dot] [--json] [--for <page>] [kb-path]    show the page link graph\n  tags [--tsv] [--json] [--for <tag>] [kb-path]      list tags with page counts, or pages for a tag\n\nIf the first argument is a directory path, `info` mode is used implicitly.\n\ninfo flags:\n  --detail  show broken links, orphan pages, tag distribution, snippet type breakdown\n  --json    output as json (combinable with --detail)\n\nlinks flags:\n  --dot       output as graphviz dot\n  --json      output as json with nodes and edges arrays\n  --for PAGE  show only links involving PAGE (ego graph, resolved by title)\n\ntags flags:\n  --for TAG   list pages tagged with TAG (case-insensitive)\n  --tsv       output as tab-separated values\n  --json      output as json\n\njson flags:\n  --json on list outputs page metadata as a json array\n  --json on search includes match kind alongside page metadata\n  --json on show serializes the full parsed page structure"
    );
}

fn run_info(args: Vec<String>) -> Result<()> {
    let mut detail = false;
    let mut json = false;
    let mut kb_path = None;
    for arg in &args {
        match arg.as_str() {
            "--detail" => detail = true,
            "--json" => json = true,
            _ if arg.starts_with('-') => {
                eprintln!("unknown flag: {arg}");
                std::process::exit(2);
            }
            _ => kb_path = Some(PathBuf::from(arg)),
        }
    }
    let kb_path = kb_path.unwrap_or_else(|| PathBuf::from("./lepiter"));
    print_kb_info(kb_path, detail, json)
}

fn print_kb_info(kb_path: PathBuf, detail: bool, json: bool) -> Result<()> {
    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;

    let props_path = kb_path.join("lepiter.properties");
    let props = if props_path.is_file() {
        let bytes = fs::read(&props_path)
            .with_context(|| format!("failed to read {}", props_path.display()))?;
        serde_json::from_slice::<serde_json::Value>(&bytes).ok()
    } else {
        None
    };

    let db_name = props
        .as_ref()
        .and_then(|v| v.get("databaseName"))
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");
    let db_uuid = props
        .as_ref()
        .and_then(|v| v.get("uuid"))
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");
    let schema = props
        .as_ref()
        .and_then(|v| v.get("schema"))
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");
    let table_of_contents = props
        .as_ref()
        .and_then(|v| v.get("tableOfContents"))
        .and_then(|v| v.as_str())
        .unwrap_or("<none>");

    let mut min_updated = None;
    let mut max_updated = None;
    let mut tag_counts: HashMap<String, usize> = HashMap::new();
    for page in index.pages.values() {
        if let Some(ts) = page.updated_at {
            min_updated = Some(min_updated.map_or(ts, |x| if ts < x { ts } else { x }));
            max_updated = Some(max_updated.map_or(ts, |x| if ts > x { ts } else { x }));
        }
        for tag in &page.tags {
            *tag_counts.entry(tag.clone()).or_insert(0) += 1;
        }
    }

    let detailed = if detail {
        Some(compute_detailed_info(&index, table_of_contents))
    } else {
        None
    };

    let info = KbInfo {
        path: kb_path.display().to_string(),
        name: db_name.to_string(),
        uuid: db_uuid.to_string(),
        schema: schema.to_string(),
        table_of_contents: table_of_contents.to_string(),
        page_count: index.pages.len(),
        index_issues: &index.index_issues,
        min_updated,
        max_updated,
        tag_counts: &tag_counts,
        detailed: detailed.as_ref(),
        index: &index,
    };

    if json {
        print_kb_info_json(&info);
    } else {
        print_kb_info_text(&info);
    }

    Ok(())
}

struct BrokenLink {
    source_title: String,
    source_id: String,
    target: String,
}

struct DetailedInfo {
    broken_links: Vec<BrokenLink>,
    orphan_ids: Vec<PageId>,
    snippet_types: Vec<(String, usize)>,
}

struct KbInfo<'a> {
    path: String,
    name: String,
    uuid: String,
    schema: String,
    table_of_contents: String,
    page_count: usize,
    index_issues: &'a [ParseIssue],
    min_updated: Option<chrono::DateTime<chrono::FixedOffset>>,
    max_updated: Option<chrono::DateTime<chrono::FixedOffset>>,
    tag_counts: &'a HashMap<String, usize>,
    detailed: Option<&'a DetailedInfo>,
    index: &'a KnowledgeBaseIndex,
}

fn compute_detailed_info(index: &KnowledgeBaseIndex, toc_page_id: &str) -> DetailedInfo {
    use lepiter_core::{collect_node_types_in_file, extract_link_targets};
    use std::collections::HashSet;

    let mut broken_links = Vec::new();
    let mut linked_to: HashSet<PageId> = HashSet::new();
    let mut snippet_totals: HashMap<String, usize> = HashMap::new();

    for id in &index.sorted_ids {
        let meta = match index.pages.get(id) {
            Some(m) => m,
            None => continue,
        };

        // Snippet type counting from raw JSON.
        if let Ok(types) = collect_node_types_in_file(&meta.path) {
            for (typ, count) in types {
                if is_snippet_type(&typ) {
                    *snippet_totals.entry(typ).or_insert(0) += count;
                }
            }
        }

        // Link analysis.
        let page = match index.load_page(id) {
            Ok(p) => p,
            Err(_) => continue,
        };
        for target in extract_link_targets(&page.content) {
            match index.classify_link_target(&target) {
                LinkTargetKind::InternalPage(target_id) if target_id != *id => {
                    linked_to.insert(target_id);
                }
                LinkTargetKind::Unknown(_) => {
                    broken_links.push(BrokenLink {
                        source_title: meta.title.clone(),
                        source_id: id.clone(),
                        target,
                    });
                }
                _ => {}
            }
        }
    }

    // Orphan pages: not linked to by any other page.
    // The table-of-contents page is excluded — it is the root entry point and
    // is not expected to be linked to by other pages.
    let orphan_ids: Vec<PageId> = index
        .sorted_ids
        .iter()
        .filter(|id| !linked_to.contains(*id) && id.as_str() != toc_page_id)
        .cloned()
        .collect();

    // Sort snippet types by count descending.
    let mut snippet_types: Vec<(String, usize)> = snippet_totals.into_iter().collect();
    snippet_types.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    DetailedInfo {
        broken_links,
        orphan_ids,
        snippet_types,
    }
}

/// Returns true if the type string looks like a snippet type (as opposed to
/// a container type like "page" or "snippets").
fn is_snippet_type(typ: &str) -> bool {
    typ.ends_with("Snippet") || typ.ends_with("Rewrite") || typ.ends_with("snippet")
}

fn print_kb_info_text(info: &KbInfo<'_>) {
    println!("Knowledge Base");
    println!("  path: {}", info.path);
    println!("  name: {}", info.name);
    println!("  uuid: {}", info.uuid);
    println!("  schema: {}", info.schema);
    println!("  table_of_contents: {}", info.table_of_contents);
    println!("  pages: {}", info.page_count);
    println!("  unique_tags: {}", info.tag_counts.len());
    println!("  index_issues: {}", info.index_issues.len());
    match (info.min_updated, info.max_updated) {
        (Some(min), Some(max)) => {
            println!(
                "  updated_range: {} .. {}",
                min.to_rfc3339(),
                max.to_rfc3339()
            );
        }
        _ => println!("  updated_range: <none>"),
    }

    if !info.index_issues.is_empty() {
        println!("\nIndex Issues:");
        for issue in info.index_issues {
            println!("  - {}: {}", issue.path.display(), issue.message);
        }
    }

    if let Some(detail) = info.detailed {
        println!("\nBroken Links ({}):", detail.broken_links.len());
        if detail.broken_links.is_empty() {
            println!("  (none)");
        } else {
            for link in &detail.broken_links {
                println!("  - {} -> {}", link.source_title, link.target);
            }
        }

        println!("\nOrphan Pages ({}):", detail.orphan_ids.len());
        if detail.orphan_ids.is_empty() {
            println!("  (none)");
        } else {
            for id in &detail.orphan_ids {
                let title = info
                    .index
                    .pages
                    .get(id)
                    .map(|m| m.title.as_str())
                    .unwrap_or(id);
                println!("  - {title}");
            }
        }

        println!("\nTag Distribution ({}):", info.tag_counts.len());
        if info.tag_counts.is_empty() {
            println!("  (none)");
        } else {
            let mut sorted: Vec<_> = info.tag_counts.iter().collect();
            sorted.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            for (tag, count) in sorted {
                println!("  {count:>4}  {tag}");
            }
        }

        println!("\nSnippet Types ({}):", detail.snippet_types.len());
        if detail.snippet_types.is_empty() {
            println!("  (none)");
        } else {
            for (typ, count) in &detail.snippet_types {
                println!("  {count:>4}  {typ}");
            }
        }
    }
}

fn print_kb_info_json(info: &KbInfo<'_>) {
    let mut obj = serde_json::Map::new();
    obj.insert("path".into(), serde_json::json!(info.path));
    obj.insert("name".into(), serde_json::json!(info.name));
    obj.insert("uuid".into(), serde_json::json!(info.uuid));
    obj.insert("schema".into(), serde_json::json!(info.schema));
    obj.insert(
        "table_of_contents".into(),
        serde_json::json!(info.table_of_contents),
    );
    obj.insert("pages".into(), serde_json::json!(info.page_count));
    obj.insert(
        "unique_tags".into(),
        serde_json::json!(info.tag_counts.len()),
    );
    obj.insert(
        "index_issues".into(),
        serde_json::json!(info.index_issues.len()),
    );
    let updated_range = match (info.min_updated, info.max_updated) {
        (Some(min), Some(max)) => serde_json::json!({
            "min": min.to_rfc3339(),
            "max": max.to_rfc3339(),
        }),
        _ => serde_json::Value::Null,
    };
    obj.insert("updated_range".into(), updated_range);

    if let Some(detail) = info.detailed {
        let broken: Vec<serde_json::Value> = detail
            .broken_links
            .iter()
            .map(|link| {
                serde_json::json!({
                    "source_title": link.source_title,
                    "source_id": link.source_id,
                    "target": link.target,
                })
            })
            .collect();
        obj.insert("broken_links".into(), serde_json::Value::Array(broken));

        let orphans: Vec<serde_json::Value> = detail
            .orphan_ids
            .iter()
            .map(|id| {
                let title = info
                    .index
                    .pages
                    .get(id)
                    .map(|m| m.title.as_str())
                    .unwrap_or(id);
                serde_json::json!({ "id": id, "title": title })
            })
            .collect();
        obj.insert("orphan_pages".into(), serde_json::Value::Array(orphans));

        let tags: serde_json::Map<String, serde_json::Value> = info
            .tag_counts
            .iter()
            .map(|(tag, count)| (tag.clone(), serde_json::json!(count)))
            .collect();
        obj.insert("tag_distribution".into(), serde_json::Value::Object(tags));

        let snippets: serde_json::Map<String, serde_json::Value> = detail
            .snippet_types
            .iter()
            .map(|(typ, count)| (typ.clone(), serde_json::json!(count)))
            .collect();
        obj.insert("snippet_types".into(), serde_json::Value::Object(snippets));
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::Value::Object(obj)).unwrap()
    );
}

fn print_page(kb_path: PathBuf, page_id: &str, show_links: bool) -> Result<()> {
    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;
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

fn run_list(args: Vec<String>) -> Result<()> {
    let mut tsv = false;
    let mut json = false;
    let mut positional = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--tsv" => tsv = true,
            "--json" => json = true,
            _ => positional.push(arg),
        }
    }
    let kb_path = positional
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./lepiter"));
    print_page_list(kb_path, tsv, json)
}

fn print_page_list(kb_path: PathBuf, tsv: bool, json: bool) -> Result<()> {
    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;

    if json {
        let pages: Vec<&PageMeta> = index.sorted_pages();
        println!("{}", serde_json::to_string_pretty(&pages).unwrap());
        return Ok(());
    }

    if tsv {
        for meta in index.sorted_pages() {
            println!("{}\t{}", meta.title, meta.id);
        }
        return Ok(());
    }

    let title_width = index
        .sorted_pages()
        .iter()
        .map(|m| m.title.chars().count())
        .max()
        .unwrap_or(5)
        .clamp(5, 64);

    println!("{:<width$}  id", "title", width = title_width);
    println!("{:-<width$}  {:-<36}", "", "", width = title_width);
    for meta in index.sorted_pages() {
        println!(
            "{:<width$}  {}",
            truncate_chars(&meta.title, title_width),
            meta.id,
            width = title_width
        );
    }
    Ok(())
}

fn print_page_ids(kb_path: PathBuf) -> Result<()> {
    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;
    for meta in index.sorted_pages() {
        println!("{}", meta.id);
    }
    Ok(())
}

fn run_search(args: Vec<String>) -> Result<()> {
    let mut full_text = false;
    let mut tsv = false;
    let mut json = false;
    let mut positional = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--full-text" => full_text = true,
            "--tsv" => tsv = true,
            "--json" => json = true,
            _ => positional.push(arg),
        }
    }

    if positional.is_empty() {
        bail!("missing required argument: <query>");
    }

    let query = positional[0].trim().to_string();
    if query.is_empty() {
        bail!("query must not be empty");
    }

    let kb_path = positional
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./lepiter"));
    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;
    let hits = index.search_hits(&query, full_text);

    if json {
        let enriched: Vec<serde_json::Value> = hits
            .iter()
            .filter_map(|hit| {
                index.pages.get(&hit.id).map(|meta| {
                    serde_json::json!({
                        "id": hit.id,
                        "title": meta.title,
                        "kind": hit.kind,
                        "tags": meta.tags,
                    })
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&enriched).unwrap());
        return Ok(());
    }

    let hit_by_id = hits
        .into_iter()
        .map(|hit| {
            let kind = match hit.kind {
                SearchMatchKind::Title => "title",
                SearchMatchKind::Tag => "tag",
                SearchMatchKind::Content => "content",
            };
            (hit.id, kind)
        })
        .collect::<std::collections::HashMap<_, _>>();

    if tsv {
        for meta in index.sorted_pages() {
            if let Some(kind) = hit_by_id.get(&meta.id) {
                println!("{}\t{}\t{}", meta.title, meta.id, kind);
            }
        }
        return Ok(());
    }

    let title_width = index
        .sorted_pages()
        .iter()
        .map(|m| m.title.chars().count())
        .max()
        .unwrap_or(5)
        .clamp(5, 64);

    println!(
        "{:<width$}  {:<36}  match",
        "title",
        "id",
        width = title_width
    );
    println!(
        "{:-<width$}  {:-<36}  {:-<7}",
        "",
        "",
        "",
        width = title_width
    );
    for meta in index.sorted_pages() {
        if let Some(kind) = hit_by_id.get(&meta.id) {
            println!(
                "{:<width$}  {:<36}  {}",
                truncate_chars(&meta.title, title_width),
                meta.id,
                kind,
                width = title_width
            );
        }
    }

    Ok(())
}

fn run_show(args: Vec<String>) -> Result<()> {
    let mut by_id = false;
    let mut by_title = false;
    let mut open_links = false;
    let mut json = false;
    let mut positional = Vec::new();

    for arg in args {
        match arg.as_str() {
            "--id" | "-i" => by_id = true,
            "--by-title" => by_title = true,
            "--open-links" => open_links = true,
            "--json" => json = true,
            _ => positional.push(arg),
        }
    }

    if by_id && by_title {
        bail!("--id and --by-title are mutually exclusive");
    }
    if positional.is_empty() {
        bail!("missing required argument: <value>");
    }

    let value = positional[0].trim();
    if value.is_empty() {
        bail!("value must not be empty");
    }
    let kb_path = positional
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./lepiter"));
    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;

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

    print_page(kb_path, &page_id, open_links)
}

fn resolve_page_id_by_title(index: &KnowledgeBaseIndex, title: &str) -> Result<String> {
    match index.resolve_page_id_by_title(title) {
        TitleResolution::Unique(id) => Ok(id),
        TitleResolution::NotFound => bail!("no page found with title matching `{title}`"),
        TitleResolution::Ambiguous(ids) => {
            let sample = ids
                .iter()
                .take(10)
                .map(|id| {
                    if let Some(meta) = index.pages.get(id) {
                        format!("{} ({})", meta.title, meta.id)
                    } else {
                        id.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            bail!("title match is ambiguous ({} matches): {sample}", ids.len())
        }
    }
}

fn run_links(args: Vec<String>) -> Result<()> {
    let mut dot = false;
    let mut json = false;
    let mut for_page: Option<String> = None;
    let mut positional = Vec::new();

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--dot" => dot = true,
            "--json" => json = true,
            "--for" => {
                let val = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--for requires a page argument"))?;
                for_page = Some(val.clone());
            }
            _ if arg.starts_with('-') => {
                eprintln!("unknown flag: {arg}");
                std::process::exit(2);
            }
            _ => positional.push(arg.clone()),
        }
    }

    if dot && json {
        bail!("--dot and --json are mutually exclusive");
    }

    let kb_path = positional
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./lepiter"));
    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;

    let graph = index.build_link_graph();

    let ego_page_id = match &for_page {
        Some(title) => Some(resolve_page_id_by_title(&index, title)?),
        None => None,
    };

    let edges: Vec<&LinkEdge> = match &ego_page_id {
        Some(id) => graph.ego(id),
        None => graph.edges.iter().collect(),
    };

    if json {
        print_links_json(&index, &edges, ego_page_id.as_deref());
    } else if dot {
        print_links_dot(&index, &edges);
    } else {
        print_links_text(&index, &edges, ego_page_id.as_deref());
    }

    Ok(())
}

fn print_links_json(index: &KnowledgeBaseIndex, edges: &[&LinkEdge], ego_id: Option<&str>) {
    use std::collections::BTreeSet;

    let mut node_ids: BTreeSet<&str> = BTreeSet::new();
    for edge in edges {
        node_ids.insert(&edge.source);
        node_ids.insert(&edge.target);
    }
    if let Some(id) = ego_id {
        node_ids.insert(id);
    }

    let nodes: Vec<serde_json::Value> = node_ids
        .iter()
        .map(|id| {
            let title = index.pages.get(*id).map(|m| m.title.as_str()).unwrap_or(id);
            serde_json::json!({ "id": id, "title": title })
        })
        .collect();

    let edge_values: Vec<serde_json::Value> = edges
        .iter()
        .map(|e| serde_json::json!({ "source": e.source, "target": e.target }))
        .collect();

    let obj = serde_json::json!({
        "nodes": nodes,
        "edges": edge_values,
    });
    println!("{}", serde_json::to_string_pretty(&obj).unwrap());
}

fn print_links_dot(index: &KnowledgeBaseIndex, edges: &[&LinkEdge]) {
    fn dot_title(index: &KnowledgeBaseIndex, id: &str) -> String {
        index
            .pages
            .get(id)
            .map(|m| m.title.clone())
            .unwrap_or_else(|| id.to_string())
    }

    fn dot_escape(s: &str) -> String {
        s.replace('\\', "\\\\").replace('"', "\\\"")
    }

    use std::collections::BTreeSet;
    let mut node_ids = BTreeSet::new();
    for edge in edges {
        node_ids.insert(&edge.source);
        node_ids.insert(&edge.target);
    }

    println!("digraph links {{");
    println!("  rankdir=LR;");
    for id in &node_ids {
        let title = dot_title(index, id);
        println!(
            "  \"{}\" [label=\"{}\"];",
            dot_escape(id),
            dot_escape(&title)
        );
    }
    for edge in edges {
        println!(
            "  \"{}\" -> \"{}\";",
            dot_escape(&edge.source),
            dot_escape(&edge.target)
        );
    }
    println!("}}");
}

fn print_links_text(index: &KnowledgeBaseIndex, edges: &[&LinkEdge], ego_id: Option<&str>) {
    use std::collections::{BTreeMap, BTreeSet};

    let mut in_degree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut out_degree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut connected: BTreeSet<&str> = BTreeSet::new();

    for edge in edges {
        *in_degree.entry(&edge.target).or_insert(0) += 1;
        *out_degree.entry(&edge.source).or_insert(0) += 1;
        connected.insert(&edge.source);
        connected.insert(&edge.target);
    }

    let total_pages = if ego_id.is_some() {
        connected.len()
    } else {
        index.pages.len()
    };

    println!(
        "Link Graph{}",
        ego_id.map_or(String::new(), |id| {
            let title = index.pages.get(id).map(|m| m.title.as_str()).unwrap_or(id);
            format!(" (ego: {title})")
        })
    );
    println!("  pages: {total_pages}");
    println!("  links: {}", edges.len());

    // Most linked-to pages (by in-degree), top 10.
    let mut by_in: Vec<(&str, usize)> = in_degree.iter().map(|(k, v)| (*k, *v)).collect();
    by_in.sort_by(|a, b| {
        b.1.cmp(&a.1).then_with(|| {
            let ta = index
                .pages
                .get(a.0)
                .map(|m| m.title_lower.as_str())
                .unwrap_or("");
            let tb = index
                .pages
                .get(b.0)
                .map(|m| m.title_lower.as_str())
                .unwrap_or("");
            ta.cmp(tb)
        })
    });

    if !by_in.is_empty() {
        println!("\nMost Linked Pages:");
        for (id, count) in by_in.iter().take(10) {
            let title = index.pages.get(*id).map(|m| m.title.as_str()).unwrap_or(id);
            println!("  {count:>4}  {title}");
        }
    }

    // Isolated pages (not in any edge) — only in full-graph mode.
    if ego_id.is_none() {
        let isolated: Vec<&str> = index
            .sorted_ids
            .iter()
            .filter(|id| !connected.contains(id.as_str()))
            .map(String::as_str)
            .collect();
        if !isolated.is_empty() {
            println!("\nIsolated Pages ({}):", isolated.len());
            for id in &isolated {
                let title = index.pages.get(*id).map(|m| m.title.as_str()).unwrap_or(id);
                println!("  {title}");
            }
        }
    }
}

fn run_tags(args: Vec<String>) -> Result<()> {
    let mut json = false;
    let mut tsv = false;
    let mut for_tag: Option<String> = None;
    let mut positional = Vec::new();

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--tsv" => tsv = true,
            "--for" => {
                let val = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--for requires a tag argument"))?;
                for_tag = Some(val.clone());
            }
            _ if arg.starts_with('-') => {
                eprintln!("unknown flag: {arg}");
                std::process::exit(2);
            }
            _ => positional.push(arg.clone()),
        }
    }

    let kb_path = positional
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./lepiter"));
    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;

    match for_tag {
        Some(tag) => print_tag_pages(&index, &tag, json, tsv),
        None => print_tag_summary(&index, json, tsv),
    }

    Ok(())
}

fn collect_tag_counts(index: &KnowledgeBaseIndex) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for meta in index.pages.values() {
        for tag in &meta.tags {
            *counts.entry(tag.clone()).or_insert(0) += 1;
        }
    }
    let mut sorted: Vec<(String, usize)> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    sorted
}

fn print_tag_summary(index: &KnowledgeBaseIndex, json: bool, tsv: bool) {
    let tags = collect_tag_counts(index);

    if json {
        let arr: Vec<serde_json::Value> = tags
            .iter()
            .map(|(tag, count)| serde_json::json!({ "tag": tag, "count": count }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap());
        return;
    }

    if tsv {
        for (tag, count) in &tags {
            println!("{tag}\t{count}");
        }
        return;
    }

    println!("Tags ({} unique)", tags.len());
    if tags.is_empty() {
        println!("  (none)");
    } else {
        for (tag, count) in &tags {
            println!("  {count:>4}  {tag}");
        }
    }
}

fn print_tag_pages(index: &KnowledgeBaseIndex, tag: &str, json: bool, tsv: bool) {
    let needle = tag.to_lowercase();
    let pages: Vec<&PageMeta> = index
        .sorted_pages()
        .into_iter()
        .filter(|m| m.tags_lower.iter().any(|t| t == &needle))
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&pages).unwrap());
        return;
    }

    if tsv {
        for meta in &pages {
            println!("{}\t{}", meta.title, meta.id);
        }
        return;
    }

    println!("Pages tagged \"{tag}\" ({})", pages.len());
    if pages.is_empty() {
        println!("  (none)");
    } else {
        let title_width = pages
            .iter()
            .map(|m| m.title.chars().count())
            .max()
            .unwrap_or(5)
            .clamp(5, 64);
        for meta in &pages {
            println!(
                "  {:<width$}  {}",
                truncate_chars(&meta.title, title_width),
                meta.id,
                width = title_width
            );
        }
    }
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    let mut chars = input.chars();
    let mut out = String::new();
    for _ in 0..max_chars {
        let Some(c) = chars.next() else {
            return out;
        };
        out.push(c);
    }
    if chars.next().is_some() && max_chars >= 1 {
        out.pop();
        out.push('…');
    }
    out
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

fn render_page_pretty(page: &Page, colored: bool) -> String {
    let mut out = String::new();
    if colored {
        out.push_str(&format!(
            "{}\n\n",
            ansi("1;36", &format!("# {}", page.title))
        ));
    } else {
        out.push_str(&format!("# {}\n\n", page.title));
    }
    if !page.tags.is_empty() {
        let line = format!("tags: {}\n", page.tags.join(", "));
        if colored {
            out.push_str(&ansi("2", line.trim_end()));
            out.push('\n');
        } else {
            out.push_str(&line);
        }
    }
    if let Some(updated_at) = page.updated_at {
        let line = format!("updated: {}\n", updated_at.to_rfc3339());
        if colored {
            out.push_str(&ansi("2", line.trim_end()));
            out.push('\n');
        } else {
            out.push_str(&line);
        }
    }
    if colored {
        out.push_str(&format!("{}\n\n", ansi("2", &format!("id: {}", page.id))));
        out.push_str(&format!("{}\n\n", ansi("2", "---")));
    } else {
        out.push_str(&format!("id: {}\n\n", page.id));
        out.push_str("---\n\n");
    }

    let body = render_page_to_text(page);
    if colored {
        out.push_str(&render_markdown_with_ansi(body.trim()));
        out.push('\n');
    } else {
        out.push_str(body.trim());
        out.push('\n');
    }
    out
}

fn render_markdown_with_ansi(markdown: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    let mut language: Option<String> = None;

    for line in markdown.lines() {
        if let Some(rest) = line.strip_prefix("```") {
            if in_code {
                out.push_str(&ansi("90", "```"));
                out.push('\n');
                in_code = false;
                language = None;
            } else {
                language = if rest.trim().is_empty() {
                    None
                } else {
                    Some(rest.trim().to_lowercase())
                };
                out.push_str(&ansi("90", line));
                out.push('\n');
                in_code = true;
            }
            continue;
        }

        if in_code {
            out.push_str(&highlight_code_line_ansi(line, language.as_deref()));
            out.push('\n');
            continue;
        }

        if line.starts_with('#') {
            out.push_str(&ansi("1;36", line));
        } else if line.starts_with("> ") {
            out.push_str(&ansi("3;90", line));
        } else if let Some(stripped) = line.strip_prefix("- ") {
            out.push_str("- ");
            out.push_str(&style_inline_markdown_ansi(stripped));
        } else if line.starts_with("[[unknown: ") {
            out.push_str(&ansi("33", line));
        } else {
            out.push_str(&style_inline_markdown_ansi(line));
        }
        out.push('\n');
    }

    out
}

fn style_inline_markdown_ansi(text: &str) -> String {
    let elements = inline::parse_inline(text);
    let mut out = String::new();

    for elem in elements {
        match elem {
            inline::InlineElement::Styled {
                text,
                bold,
                italic,
                code,
            } => {
                if code {
                    out.push_str(&ansi("33", &text));
                } else {
                    let style = match (bold, italic) {
                        (true, true) => Some("1;3"),
                        (true, false) => Some("1"),
                        (false, true) => Some("3"),
                        (false, false) => None,
                    };
                    if let Some(style) = style {
                        out.push_str(&ansi(style, &text));
                    } else {
                        out.push_str(&text);
                    }
                }
            }
            inline::InlineElement::Link { label, target } => {
                out.push_str(&ansi("4;94", &label));
                out.push_str(&ansi("90", &format!(" ({target})")));
            }
            inline::InlineElement::WikiLink { text } => {
                out.push_str(&ansi("4;94", &text));
            }
            inline::InlineElement::Annotation { text } => {
                out.push_str(&ansi("1;35", &text));
            }
        }
    }

    out
}

fn ansi(style: &str, text: &str) -> String {
    format!("\x1b[{style}m{text}\x1b[0m")
}

fn highlight_code_line_ansi(line: &str, language: Option<&str>) -> String {
    let tokens = tokenize_code_line(line, language);
    let mut out = String::new();
    for tok in tokens {
        match tok {
            CodeToken::Comment(s) => out.push_str(&ansi("90", s)),
            CodeToken::StringLit(s) => out.push_str(&ansi("32", s)),
            CodeToken::Number(s) => out.push_str(&ansi("33", s)),
            CodeToken::Keyword(s) => out.push_str(&ansi("1;35", s)),
            CodeToken::Ident(s) => out.push_str(s),
            CodeToken::Punct(c) => out.push(c),
        }
    }
    out
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

fn collect_page_links(nodes: &[Node]) -> Vec<(String, String)> {
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
    for elem in inline::parse_inline(text) {
        match elem {
            inline::InlineElement::Link { label, target } => {
                out.push((label, target));
            }
            inline::InlineElement::WikiLink { text } => {
                out.push((text.clone(), text));
            }
            _ => {}
        }
    }
}

fn open_with_system(target: &str) -> Result<()> {
    open::that(target).with_context(|| format!("failed to open target `{target}`"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lepiter_core::Node;

    // --- is_snippet_type ---

    #[test]
    fn is_snippet_type_accepts_text_snippet() {
        assert!(is_snippet_type("textSnippet"));
    }

    #[test]
    fn is_snippet_type_accepts_pharo_snippet() {
        assert!(is_snippet_type("pharoSnippet"));
    }

    #[test]
    fn is_snippet_type_accepts_lowercase_snippet() {
        assert!(is_snippet_type("codeSnippet"));
        assert!(is_snippet_type("pythonSnippet"));
    }

    #[test]
    fn is_snippet_type_accepts_rewrite() {
        assert!(is_snippet_type("pharoRewrite"));
        assert!(is_snippet_type("someRewrite"));
    }

    #[test]
    fn is_snippet_type_rejects_container_types() {
        assert!(!is_snippet_type("page"));
        assert!(!is_snippet_type("snippets"));
        assert!(!is_snippet_type("children"));
    }

    // --- compute_detailed_info ---

    fn make_test_kb(
        pages: &[(&str, &str, &str)],
    ) -> (std::path::PathBuf, lepiter_core::KnowledgeBaseIndex) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("lepiter-tui-test-{ts}"));
        std::fs::create_dir_all(&dir).unwrap();
        for (id, title, body) in pages {
            let content = serde_json::json!({
                "uid": {"uuid": id},
                "pageType": {"title": title},
                "tags": [],
                "children": {"items": [
                    {"__type": "textSnippet", "string": body}
                ]}
            });
            let file_path = dir.join(format!("{id}.lepiter"));
            std::fs::write(&file_path, serde_json::to_vec(&content).unwrap()).unwrap();
        }
        let index = lepiter_core::KnowledgeBase::open(&dir).unwrap();
        (dir, index)
    }

    #[test]
    fn compute_detailed_info_counts_snippet_types() {
        let (dir, index) = make_test_kb(&[("p1", "Page One", "hello world")]);
        let info = compute_detailed_info(&index, "");
        // The textSnippet from the JSON should appear in snippet_types.
        assert!(info.snippet_types.iter().any(|(t, _)| t == "textSnippet"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compute_detailed_info_detects_orphan_pages() {
        // Two pages, neither links to the other — both should be orphans.
        let (dir, index) =
            make_test_kb(&[("p1", "Page One", "hello"), ("p2", "Page Two", "world")]);
        let info = compute_detailed_info(&index, "");
        assert_eq!(info.orphan_ids.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compute_detailed_info_excludes_toc_from_orphans() {
        // Two pages, neither links to the other. "p1" is the TOC page.
        let (dir, index) = make_test_kb(&[
            ("p1", "Table of Contents", "hello"),
            ("p2", "Page Two", "world"),
        ]);
        let info = compute_detailed_info(&index, "p1");
        // p1 excluded as TOC, only p2 should be orphan.
        assert_eq!(info.orphan_ids.len(), 1);
        assert_eq!(info.orphan_ids[0], "p2");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compute_detailed_info_linked_page_not_orphan() {
        // p1 links to p2 via inline markdown link.
        let (dir, index) = make_test_kb(&[
            ("p1", "Page One", "see [link](page:p2) for more"),
            ("p2", "Page Two", "target page"),
        ]);
        let info = compute_detailed_info(&index, "");
        // p2 is linked to by p1, so only p1 should be orphan.
        assert_eq!(info.orphan_ids.len(), 1);
        assert_eq!(info.orphan_ids[0], "p1");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compute_detailed_info_detects_broken_links() {
        // p1 links to a nonexistent page.
        let (dir, index) = make_test_kb(&[("p1", "Page One", "see [link](page:nonexistent) here")]);
        let info = compute_detailed_info(&index, "");
        assert_eq!(info.broken_links.len(), 1);
        assert_eq!(info.broken_links[0].target, "page:nonexistent");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compute_detailed_info_empty_kb() {
        let (dir, index) = make_test_kb(&[]);
        let info = compute_detailed_info(&index, "");
        assert!(info.broken_links.is_empty());
        assert!(info.orphan_ids.is_empty());
        assert!(info.snippet_types.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

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
