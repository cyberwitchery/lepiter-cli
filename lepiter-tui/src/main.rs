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
//! - `LEPITER_TUI_PARSED_CACHE` / `LEPITER_TUI_RENDERED_CACHE`: cache sizes
//! - `LEPITER_EDIT_AUTOSAVE_MS`: edit autosave delay
//! - `LEPITER_PLUGIN_CONFIG`: external snippet renderer config
//!
//! # docs
//!
//! - tui behavior: `docs/tui.md`
//! - editor: `docs/editor.md`
//! - plugins: `docs/plugins.md`
mod edit;
mod plugins;
mod render;
mod util;

use std::collections::HashMap;
use std::collections::VecDeque;
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use crate::edit::{
    EditState, apply_cursor_marker, collect_snippets, delete_char_at, delete_char_before,
    ensure_scroll, insert_char_at, load_raw_page, move_cursor_vertical, render_edit_page,
    wrap_lines_with_cursor_and_highlight,
};
use crate::plugins::PluginManager;
use crate::render::{
    RenderedPage, highlight_selected_link_markers, keywords_for_language, render_page,
    sanitize_for_terminal,
};
use crate::util::cache_limit_from_env;
use anyhow::{Context, Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use lepiter_core::{
    KnowledgeBase, KnowledgeBaseIndex, LinkTargetKind, Node, Page, PageId, SearchMatchKind,
    TitleResolution, render_page_to_text,
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    List,
    Search,
    Page,
    Edit,
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
    parsed_cache: HashMap<PageId, Page>,
    rendered_cache: HashMap<PageId, RenderedPage>,
    parsed_lru: VecDeque<PageId>,
    rendered_lru: VecDeque<PageId>,
    max_parsed_cache: usize,
    max_rendered_cache: usize,
    page_scroll: usize,
    selected_link: usize,
    search: String,
    search_hit_kind: HashMap<PageId, SearchMatchKind>,
    text_index: HashMap<PageId, IndexedPageText>,
    text_index_queue: VecDeque<PageId>,
    history: Vec<PageId>,
    mode: Mode,
    status: String,
}

impl App {
    fn new(index: KnowledgeBaseIndex) -> Self {
        let plugins = PluginManager::from_env();
        let max_parsed_cache = cache_limit_from_env("LEPITER_TUI_PARSED_CACHE", 128);
        let max_rendered_cache = cache_limit_from_env("LEPITER_TUI_RENDERED_CACHE", 128);
        let mut app = Self {
            index,
            plugins,
            edit: None,
            visible_ids: Vec::new(),
            selected: 0,
            opened: None,
            parsed_cache: HashMap::new(),
            rendered_cache: HashMap::new(),
            parsed_lru: VecDeque::new(),
            rendered_lru: VecDeque::new(),
            max_parsed_cache,
            max_rendered_cache,
            page_scroll: 0,
            selected_link: 0,
            search: String::new(),
            search_hit_kind: HashMap::new(),
            text_index: HashMap::new(),
            text_index_queue: VecDeque::new(),
            history: Vec::new(),
            mode: Mode::List,
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

    fn rebuild_visible_ids(&mut self) {
        let query = self.search.trim();
        if query.is_empty() {
            self.visible_ids = self
                .index
                .sorted_pages_by_title()
                .into_iter()
                .map(|m| m.id.clone())
                .collect();
            self.search_hit_kind.clear();
            self.reset_text_index_queue();
        } else {
            let needle = query.to_lowercase();
            let mut hit_kind = HashMap::new();
            for id in self.index.filter_page_ids(query) {
                hit_kind.insert(id, SearchMatchKind::Meta);
            }
            for (id, text) in &self.text_index {
                if hit_kind.contains_key(id) {
                    continue;
                }
                if text.lower.contains(&needle) {
                    hit_kind.insert(id.clone(), SearchMatchKind::Content);
                }
            }

            let mut ids = Vec::new();
            for meta in self.index.sorted_pages_by_title() {
                if hit_kind.contains_key(&meta.id) {
                    ids.push(meta.id.clone());
                }
            }
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
        if self.rendered_cache.contains_key(id) {
            touch_lru(&mut self.rendered_lru, id);
        } else {
            let page = match self.get_or_load_page(id) {
                Ok(page) => page,
                Err(err) => {
                    self.status = format!("failed to load page: {err:#}");
                    return;
                }
            };
            let rendered = render_page(&page, &mut self.plugins);
            insert_lru(
                &mut self.rendered_cache,
                &mut self.rendered_lru,
                id.to_string(),
                rendered,
                self.max_rendered_cache,
            );
        }

        if from_link && let Some(current) = self.opened.as_ref() {
            self.history.push(current.clone());
        }

        self.opened = Some(id.to_string());
        self.page_scroll = 0;
        self.selected_link = 0;
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

    fn refresh_after_edit(&mut self, id: &str) {
        if let Ok(page) = self.index.load_page(id) {
            insert_lru(
                &mut self.parsed_cache,
                &mut self.parsed_lru,
                id.to_string(),
                page.clone(),
                self.max_parsed_cache,
            );
            let rendered = render_page(&page, &mut self.plugins);
            insert_lru(
                &mut self.rendered_cache,
                &mut self.rendered_lru,
                id.to_string(),
                rendered,
                self.max_rendered_cache,
            );
            let raw = render_page_to_text(&page);
            self.text_index.insert(
                id.to_string(),
                IndexedPageText {
                    raw: raw.clone(),
                    lower: raw.to_lowercase(),
                },
            );
        }
    }

    fn get_or_load_page(&mut self, id: &str) -> Result<Page> {
        if self.parsed_cache.contains_key(id) {
            touch_lru(&mut self.parsed_lru, id);
            if let Some(page) = self.parsed_cache.get(id) {
                return Ok(page.clone());
            }
        }

        let page = self.index.load_page(id)?;
        insert_lru(
            &mut self.parsed_cache,
            &mut self.parsed_lru,
            id.to_string(),
            page.clone(),
            self.max_parsed_cache,
        );
        Ok(page)
    }

    fn reset_text_index_queue(&mut self) {
        self.text_index_queue = self
            .index
            .sorted_pages_by_title()
            .into_iter()
            .map(|m| m.id.clone())
            .collect();
    }

    fn back_to_list(&mut self) {
        self.mode = Mode::List;
    }

    fn back_in_history(&mut self) {
        if let Some(prev) = self.history.pop() {
            self.open_page(&prev, false);
        } else {
            self.mode = Mode::List;
        }
    }

    fn scroll_page(&mut self, delta: isize) {
        let next = self.page_scroll as isize + delta;
        self.page_scroll = next.max(0) as usize;
    }

    fn current_rendered_page(&self) -> Option<&RenderedPage> {
        let id = self.opened.as_ref()?;
        self.rendered_cache.get(id)
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
        let query = self.search.trim();
        if query.is_empty() {
            return;
        }

        let needle = query.to_lowercase();
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
            if lower.contains(&needle) {
                changed = true;
            }
            self.text_index.insert(id, IndexedPageText { raw, lower });
        }

        if changed {
            self.rebuild_visible_ids();
        }
    }

    fn snippet_for(&self, id: &str, query: &str) -> Option<String> {
        let text = self.text_index.get(id)?;
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return None;
        }
        let idx = text.lower.find(&needle)?;
        let start = idx.saturating_sub(40);
        let end = (idx + needle.len() + 80).min(text.raw.len());
        let fragment = text.raw.get(start..end).unwrap_or("").replace('\n', " ");
        let fragment = fragment.trim();
        if fragment.is_empty() {
            None
        } else {
            Some(truncate_chars(fragment, 120))
        }
    }

    fn jump_to_search_match(&mut self, id: &str) {
        let query = self.search.trim().to_lowercase();
        if query.is_empty() {
            return;
        }
        let Some(page) = self.rendered_cache.get(id) else {
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
            if text.contains(&query) {
                line_idx = idx;
                break;
            }
        }
        self.page_scroll = line_idx;
    }

    fn handle_edit_key(&mut self, key: KeyEvent) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };

        match key.code {
            KeyCode::Esc => {
                self.exit_edit_mode();
                return;
            }
            KeyCode::PageUp => {
                edit.preview_scroll = edit.preview_scroll.saturating_sub(10);
                edit.follow_cursor = false;
                edit.snapshot_pending = true;
                return;
            }
            KeyCode::PageDown => {
                edit.preview_scroll = edit.preview_scroll.saturating_add(10);
                edit.follow_cursor = false;
                edit.snapshot_pending = true;
                return;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let mut saved_page = None;
                if edit.undo() {
                    if let Err(err) = edit.save_to_disk() {
                        self.status = format!("edit save failed: {err:#}");
                    } else {
                        saved_page = Some(edit.page_id.clone());
                    }
                }
                if let Some(id) = saved_page {
                    self.refresh_after_edit(&id);
                }
                return;
            }
            KeyCode::Tab => {
                self.switch_edit_snippet(1);
                return;
            }
            KeyCode::BackTab => {
                self.switch_edit_snippet(-1);
                return;
            }
            _ => {}
        }

        if !edit.is_editable() {
            return;
        }

        match key.code {
            KeyCode::Left => {
                edit.cursor = edit.cursor.saturating_sub(1);
                edit.snapshot_pending = true;
            }
            KeyCode::Right => {
                let len = edit.buffer.chars().count();
                if edit.cursor < len {
                    edit.cursor += 1;
                }
                edit.snapshot_pending = true;
            }
            KeyCode::Up => {
                move_cursor_vertical(edit, -1);
                edit.snapshot_pending = true;
            }
            KeyCode::Down => {
                move_cursor_vertical(edit, 1);
                edit.snapshot_pending = true;
            }
            KeyCode::Home => {
                edit.cursor = 0;
                edit.snapshot_pending = true;
            }
            KeyCode::End => {
                edit.cursor = edit.buffer.chars().count();
                edit.snapshot_pending = true;
            }
            KeyCode::Backspace => {
                if edit.cursor > 0 {
                    edit.maybe_push_undo_snapshot();
                }
                if delete_char_before(&mut edit.buffer, &mut edit.cursor) {
                    edit.commit_buffer();
                }
            }
            KeyCode::Delete => {
                let len = edit.buffer.chars().count();
                if edit.cursor < len {
                    edit.maybe_push_undo_snapshot();
                }
                if delete_char_at(&mut edit.buffer, &mut edit.cursor) {
                    edit.commit_buffer();
                }
            }
            KeyCode::Enter => {
                edit.maybe_push_undo_snapshot();
                insert_char_at(&mut edit.buffer, &mut edit.cursor, '\n');
                edit.commit_buffer();
            }
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    return;
                }
                edit.maybe_push_undo_snapshot();
                insert_char_at(&mut edit.buffer, &mut edit.cursor, c);
                edit.commit_buffer();
            }
            _ => {}
        }
    }

    fn switch_edit_snippet(&mut self, delta: isize) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        edit.commit_buffer();
        let mut saved_page = None;
        if edit.dirty {
            if let Err(err) = edit.save_to_disk() {
                self.status = format!("edit save failed: {err:#}");
            } else {
                saved_page = Some(edit.page_id.clone());
            }
        }

        let total = edit.snippets.len() as isize;
        if total == 0 {
            return;
        }
        let next = (edit.selected as isize + delta).clamp(0, total - 1) as usize;
        edit.selected = next;
        if let Some(entry) = edit.current() {
            edit.set_buffer(entry.text.clone());
        }
        edit.follow_cursor = true;
        if let Some(id) = saved_page {
            self.refresh_after_edit(&id);
        }
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
            let kb_path = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("./lepiter"));
            print_kb_info(kb_path)
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
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        other => {
            let maybe_path = PathBuf::from(other);
            if maybe_path.is_dir() {
                print_kb_info(maybe_path)
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
        "lepiter-cli <subcommand|kb-path> [args]\n\nsubcommands:\n  tui [kb-path]                                      launch the terminal reader (default path: ./lepiter)\n  info [kb-path]                                     print knowledge base metadata summary\n  list [--tsv] [kb-path]                             list pages (pretty columns by default)\n  ids [kb-path]                                      print page ids only (sorted by title)\n  search [--full-text] [--tsv] <query> [kb-path]     search by title/id/tags, optionally page content\n  show [--id|--by-title] [--open-links] <value> [kb-path]  render one page (default: title lookup)\n\nIf the first argument is a directory path, `info` mode is used implicitly."
    );
}

fn print_kb_info(kb_path: PathBuf) -> Result<()> {
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
    let mut tag_cardinality = std::collections::HashSet::new();
    for page in index.pages.values() {
        if let Some(ts) = page.updated_at {
            min_updated = Some(min_updated.map_or(ts, |x| if ts < x { ts } else { x }));
            max_updated = Some(max_updated.map_or(ts, |x| if ts > x { ts } else { x }));
        }
        for tag in &page.tags {
            tag_cardinality.insert(tag.clone());
        }
    }

    println!("Knowledge Base");
    println!("  path: {}", kb_path.display());
    println!("  name: {db_name}");
    println!("  uuid: {db_uuid}");
    println!("  schema: {schema}");
    println!("  table_of_contents: {table_of_contents}");
    println!("  pages: {}", index.pages.len());
    println!("  unique_tags: {}", tag_cardinality.len());
    println!("  index_issues: {}", index.index_issues.len());
    match (min_updated, max_updated) {
        (Some(min), Some(max)) => {
            println!(
                "  updated_range: {} .. {}",
                min.to_rfc3339(),
                max.to_rfc3339()
            );
        }
        _ => println!("  updated_range: <none>"),
    }

    if !index.index_issues.is_empty() {
        println!("\nIndex Issues:");
        for issue in &index.index_issues {
            println!("  - {}: {}", issue.path.display(), issue.message);
        }
    }

    Ok(())
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
    let mut positional = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--tsv" => tsv = true,
            _ => positional.push(arg),
        }
    }
    let kb_path = positional
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./lepiter"));
    print_page_list(kb_path, tsv)
}

fn print_page_list(kb_path: PathBuf, tsv: bool) -> Result<()> {
    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;
    if tsv {
        for meta in index.sorted_pages_by_title() {
            println!("{}\t{}", meta.title, meta.id);
        }
        return Ok(());
    }

    let title_width = index
        .sorted_pages_by_title()
        .iter()
        .map(|m| m.title.chars().count())
        .max()
        .unwrap_or(5)
        .clamp(5, 64);

    println!("{:<width$}  id", "title", width = title_width);
    println!("{:-<width$}  {:-<36}", "", "", width = title_width);
    for meta in index.sorted_pages_by_title() {
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
    for meta in index.sorted_pages_by_title() {
        println!("{}", meta.id);
    }
    Ok(())
}

fn run_search(args: Vec<String>) -> Result<()> {
    let mut full_text = false;
    let mut tsv = false;
    let mut positional = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--full-text" => full_text = true,
            "--tsv" => tsv = true,
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
    let hit_by_id = hits
        .into_iter()
        .map(|hit| {
            let kind = match hit.kind {
                SearchMatchKind::Meta => "meta",
                SearchMatchKind::Content => "content",
            };
            (hit.id, kind)
        })
        .collect::<std::collections::HashMap<_, _>>();

    if tsv {
        for meta in index.sorted_pages_by_title() {
            if let Some(kind) = hit_by_id.get(&meta.id) {
                println!("{}\t{}\t{}", meta.title, meta.id, kind);
            }
        }
        return Ok(());
    }

    let title_width = index
        .sorted_pages_by_title()
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
    for meta in index.sorted_pages_by_title() {
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
    let mut positional = Vec::new();

    for arg in args {
        match arg.as_str() {
            "--id" | "-i" => by_id = true,
            "--by-title" => by_title = true,
            "--open-links" => open_links = true,
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
    let start_char = lower_text[..pos].chars().count();
    let match_chars = lower_needle.chars().count();
    let end_char = start_char + match_chars;
    let start_byte = text
        .char_indices()
        .nth(start_char)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    let end_byte = text
        .char_indices()
        .nth(end_char)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
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
    let chars = text.chars().collect::<Vec<_>>();
    let mut i = 0usize;
    let mut out = String::new();
    let mut buf = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut code = false;

    let push_buf = |out: &mut String, buf: &mut String, bold: bool, italic: bool, code: bool| {
        if buf.is_empty() {
            return;
        }
        let s = std::mem::take(buf);
        if code {
            out.push_str(&ansi("33", &s));
            return;
        }
        let style = match (bold, italic) {
            (true, true) => Some("1;3"),
            (true, false) => Some("1"),
            (false, true) => Some("3"),
            (false, false) => None,
        };
        if let Some(style) = style {
            out.push_str(&ansi(style, &s));
        } else {
            out.push_str(&s);
        }
    };

    while i < chars.len() {
        if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '*' {
            push_buf(&mut out, &mut buf, bold, italic, code);
            bold = !bold;
            i += 2;
            continue;
        }
        if chars[i] == '*' {
            push_buf(&mut out, &mut buf, bold, italic, code);
            italic = !italic;
            i += 1;
            continue;
        }
        if chars[i] == '`' {
            push_buf(&mut out, &mut buf, bold, italic, code);
            code = !code;
            i += 1;
            continue;
        }
        if chars[i] == '[' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != ']' {
                j += 1;
            }
            if j + 1 < chars.len() && chars[j] == ']' && chars[j + 1] == '(' {
                let mut k = j + 2;
                while k < chars.len() && chars[k] != ')' {
                    k += 1;
                }
                if k < chars.len() {
                    push_buf(&mut out, &mut buf, bold, italic, code);
                    let label = chars[i + 1..j].iter().collect::<String>();
                    let target = chars[j + 2..k].iter().collect::<String>();
                    out.push_str(&ansi("4;94", &label));
                    out.push_str(&ansi("90", &format!(" ({target})")));
                    i = k + 1;
                    continue;
                }
            }
        }
        buf.push(chars[i]);
        i += 1;
    }

    push_buf(&mut out, &mut buf, bold, italic, code);
    out
}

fn ansi(style: &str, text: &str) -> String {
    format!("\x1b[{style}m{text}\x1b[0m")
}

fn highlight_code_line_ansi(line: &str, language: Option<&str>) -> String {
    let keywords = keywords_for_language(language.unwrap_or_default());
    let mut out = String::new();
    let mut i = 0usize;
    let chars = line.chars().collect::<Vec<_>>();

    while i < chars.len() {
        let c = chars[i];

        if (language == Some("python") || language == Some("shell") || language == Some("bash"))
            && c == '#'
        {
            let rest = chars[i..].iter().collect::<String>();
            out.push_str(&ansi("90", &rest));
            break;
        }
        if language == Some("javascript") && i + 1 < chars.len() && c == '/' && chars[i + 1] == '/'
        {
            let rest = chars[i..].iter().collect::<String>();
            out.push_str(&ansi("90", &rest));
            break;
        }
        if c == '"' || c == '\'' {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == quote && chars[i.saturating_sub(1)] != '\\' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let s = chars[start..i].iter().collect::<String>();
            out.push_str(&ansi("32", &s));
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let s = chars[start..i].iter().collect::<String>();
            out.push_str(&ansi("33", &s));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word = chars[start..i].iter().collect::<String>();
            if keywords.contains(&word.as_str()) {
                out.push_str(&ansi("1;35", &word));
            } else {
                out.push_str(&word);
            }
            continue;
        }

        out.push(c);
        i += 1;
    }

    out
}

fn touch_lru(order: &mut VecDeque<PageId>, id: &str) {
    if let Some(pos) = order.iter().position(|x| x == id) {
        order.remove(pos);
    }
    order.push_back(id.to_string());
}

fn insert_lru<T>(
    map: &mut HashMap<PageId, T>,
    order: &mut VecDeque<PageId>,
    id: PageId,
    value: T,
    max_entries: usize,
) {
    if map.contains_key(&id) {
        map.insert(id.clone(), value);
        touch_lru(order, &id);
        return;
    }

    if max_entries > 0
        && map.len() >= max_entries
        && let Some(oldest) = order.pop_front()
    {
        map.remove(&oldest);
    }

    order.push_back(id.clone());
    map.insert(id, value);
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

        if app.mode == Mode::Search {
            match key.code {
                KeyCode::Esc => app.mode = Mode::List,
                KeyCode::Enter => {
                    app.mode = Mode::List;
                    app.open_selected_page();
                }
                KeyCode::Up => app.move_selection(-1),
                KeyCode::Down => app.move_selection(1),
                KeyCode::Backspace => {
                    app.search.pop();
                    app.rebuild_visible_ids();
                }
                KeyCode::Char(c) => {
                    app.search.push(c);
                    app.rebuild_visible_ids();
                }
                _ => {}
            }
            app.advance_full_text_index(4);
            continue;
        }

        if app.mode == Mode::Edit {
            app.handle_edit_key(key);
            app.tick();
            continue;
        }

        match key.code {
            KeyCode::Char('q') => break,
            KeyCode::Char('/') => {
                app.search.clear();
                app.rebuild_visible_ids();
                app.mode = Mode::Search;
            }
            KeyCode::Esc => app.mode = Mode::List,
            KeyCode::Enter => match app.mode {
                Mode::List => {
                    app.mode = Mode::List;
                    app.open_selected_page();
                }
                Mode::Page => app.follow_selected_link(),
                Mode::Search | Mode::Edit => {}
            },
            KeyCode::Up | KeyCode::Char('k') => match app.mode {
                Mode::List => app.move_selection(-1),
                Mode::Page => app.scroll_page(-1),
                Mode::Search | Mode::Edit => {}
            },
            KeyCode::Down | KeyCode::Char('j') => match app.mode {
                Mode::List => app.move_selection(1),
                Mode::Page => app.scroll_page(1),
                Mode::Search | Mode::Edit => {}
            },
            KeyCode::Char('g') => match app.mode {
                Mode::Page => app.page_scroll = 0,
                Mode::List | Mode::Search | Mode::Edit => {}
            },
            KeyCode::Char('G') => match app.mode {
                Mode::Page => app.page_scroll = usize::MAX / 2,
                Mode::List | Mode::Search | Mode::Edit => {}
            },
            KeyCode::Char('b') => {
                if app.mode == Mode::Page {
                    app.back_to_list();
                }
            }
            KeyCode::Char('e') => {
                if app.mode == Mode::Page {
                    app.enter_edit_mode();
                }
            }
            KeyCode::Char('h') => {
                if app.mode == Mode::Page {
                    app.back_in_history();
                }
            }
            KeyCode::Tab => {
                if app.mode == Mode::Page {
                    app.move_link_selection(1);
                }
            }
            KeyCode::BackTab => {
                if app.mode == Mode::Page {
                    app.move_link_selection(-1);
                }
            }
            _ => {}
        }

        app.tick();
    }

    Ok(())
}

fn ui(frame: &mut Frame, app: &mut App) {
    match app.mode {
        Mode::List | Mode::Search => render_list_view(frame, app),
        Mode::Page => render_page_view(frame, app),
        Mode::Edit => render_edit_view(frame, app),
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

    let items = app
        .visible_ids
        .iter()
        .map(|id| {
            let meta = &app.index.pages[id];
            let mut text = format!("{}  [{}]", meta.title, meta.id);
            if let Some(kind) = app.search_hit_kind.get(id)
                && *kind == SearchMatchKind::Content
                && let Some(snippet) = app.snippet_for(id, &app.search)
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
        "matches: {} | index: {}/{} | cache p/r: {}/{} | j/k or up/down move | enter open | / search | q quit",
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
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
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
        let lines = if page.links.is_empty() {
            page.lines.clone()
        } else {
            highlight_selected_link_markers(&page.lines, app.selected_link + 1)
        };
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
        .scroll((app.page_scroll as u16, 0));
    frame.render_widget(paragraph, chunks[1]);

    let mut footer = String::from(
        "j/k scroll | tab/backtab select link | enter follow link | h back-link | b list | q quit",
    );
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

fn collect_page_links(nodes: &[Node]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            Node::Link { text, url } => out.push((text.clone(), url.clone())),
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

fn open_with_system(target: &str) -> Result<()> {
    open::that(target).with_context(|| format!("failed to open target `{target}`"))?;
    Ok(())
}
