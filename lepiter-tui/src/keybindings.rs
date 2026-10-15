use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::edit::{delete_char_at, delete_char_before, insert_char_at, move_cursor_vertical};
use crate::{App, Mode};

/// Half the usable terminal height, for PageUp / PageDown scrolling.
fn page_scroll_half() -> isize {
    crossterm::terminal::size()
        .map(|(_, h)| (h.saturating_sub(6) / 2).max(1) as isize)
        .unwrap_or(10)
}

/// Result from processing a key event, telling the event loop what to do next.
pub(crate) enum KeyResult {
    /// Exit the application.
    Quit,
    /// Continue without any tick processing.
    Continue,
    /// Run a full tick (text index + autosave).
    Tick,
    /// Advance the full-text search index only.
    SearchTick,
}

impl App {
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> KeyResult {
        if self.show_help {
            match key.code {
                KeyCode::Char('?') | KeyCode::Esc => self.show_help = false,
                _ => {}
            }
            return KeyResult::Continue;
        }

        if self.mode == Mode::Search {
            match key.code {
                KeyCode::Esc => {
                    self.search.clear();
                    self.update_search_needle();
                    self.rebuild_visible_ids();
                    self.mode = Mode::List;
                }
                KeyCode::Enter => {
                    self.mode = Mode::List;
                    self.open_selected_page();
                }
                KeyCode::Up => self.move_selection(-1),
                KeyCode::Down => self.move_selection(1),
                KeyCode::Backspace => {
                    self.search.pop();
                    self.update_search_needle();
                    self.rebuild_visible_ids();
                }
                KeyCode::Char(c) => {
                    self.search.push(c);
                    self.update_search_needle();
                    self.rebuild_visible_ids();
                }
                _ => {}
            }
            return KeyResult::SearchTick;
        }

        if self.mode == Mode::PageSearch {
            match key.code {
                KeyCode::Esc => {
                    self.clear_page_search();
                    self.mode = Mode::Page;
                }
                KeyCode::Enter => {
                    self.mode = Mode::Page;
                    if !self.page_search_match_lines.is_empty() {
                        self.page_scroll = self.page_search_match_lines[self.page_search_current];
                    }
                }
                KeyCode::Backspace => {
                    self.page_search.pop();
                    self.update_page_search_needle();
                    self.rebuild_page_search_matches();
                    if let Some(&line) = self.page_search_match_lines.first() {
                        self.page_scroll = line;
                    }
                }
                KeyCode::Char(c) => {
                    self.page_search.push(c);
                    self.update_page_search_needle();
                    self.rebuild_page_search_matches();
                    if let Some(&line) = self.page_search_match_lines.first() {
                        self.page_scroll = line;
                    }
                }
                _ => {}
            }
            return KeyResult::Continue;
        }

        if self.mode == Mode::NewPageTitle {
            match key.code {
                KeyCode::Esc => {
                    self.new_page_title.clear();
                    self.mode = Mode::List;
                }
                KeyCode::Enter => {
                    let title = self.new_page_title.trim().to_string();
                    self.new_page_title.clear();
                    if title.is_empty() {
                        self.mode = Mode::List;
                    } else {
                        self.create_page(&title);
                    }
                }
                KeyCode::Backspace => {
                    self.new_page_title.pop();
                }
                KeyCode::Char(c) => {
                    self.new_page_title.push(c);
                }
                _ => {}
            }
            return KeyResult::Tick;
        }

        if self.mode == Mode::Edit {
            self.handle_edit_key(key);
            return KeyResult::Tick;
        }

        if self.mode == Mode::Backlinks {
            match key.code {
                KeyCode::Char('q') => return KeyResult::Quit,
                KeyCode::Char('?') => self.show_help = true,
                KeyCode::Esc => self.mode = Mode::Page,
                KeyCode::Enter => self.open_selected_backlink(),
                KeyCode::Up | KeyCode::Char('k') => self.move_backlink_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_backlink_selection(1),
                _ => {}
            }
            return KeyResult::Tick;
        }

        match key.code {
            KeyCode::Char('q') => return KeyResult::Quit,
            KeyCode::Char('?') => {
                self.show_help = true;
            }
            KeyCode::Char('/') => match self.mode {
                Mode::Page => {
                    self.page_search.clear();
                    self.update_page_search_needle();
                    self.rebuild_page_search_matches();
                    self.mode = Mode::PageSearch;
                }
                _ => {
                    self.search.clear();
                    self.update_search_needle();
                    self.rebuild_visible_ids();
                    self.mode = Mode::Search;
                }
            },
            KeyCode::Esc => match self.mode {
                Mode::Page if !self.page_search_needle.is_empty() => {
                    self.clear_page_search();
                }
                _ => self.mode = Mode::List,
            },
            KeyCode::Enter => match self.mode {
                Mode::List => {
                    self.mode = Mode::List;
                    self.open_selected_page();
                }
                Mode::Page => self.follow_selected_link(),
                Mode::Search
                | Mode::PageSearch
                | Mode::Edit
                | Mode::Backlinks
                | Mode::NewPageTitle => {}
            },
            KeyCode::Char('n') if self.mode == Mode::Page => {
                self.page_search_next();
            }
            KeyCode::Char('N') if self.mode == Mode::Page => {
                self.page_search_prev();
            }
            KeyCode::Up | KeyCode::Char('k') => match self.mode {
                Mode::List => self.move_selection(-1),
                Mode::Page => self.scroll_page(-1),
                Mode::Search
                | Mode::PageSearch
                | Mode::Edit
                | Mode::Backlinks
                | Mode::NewPageTitle => {}
            },
            KeyCode::Down | KeyCode::Char('j') => match self.mode {
                Mode::List => self.move_selection(1),
                Mode::Page => self.scroll_page(1),
                Mode::Search
                | Mode::PageSearch
                | Mode::Edit
                | Mode::Backlinks
                | Mode::NewPageTitle => {}
            },
            KeyCode::PageUp => match self.mode {
                Mode::Page => self.scroll_page(-page_scroll_half()),
                Mode::List
                | Mode::Search
                | Mode::PageSearch
                | Mode::Edit
                | Mode::Backlinks
                | Mode::NewPageTitle => {}
            },
            KeyCode::PageDown => match self.mode {
                Mode::Page => self.scroll_page(page_scroll_half()),
                Mode::List
                | Mode::Search
                | Mode::PageSearch
                | Mode::Edit
                | Mode::Backlinks
                | Mode::NewPageTitle => {}
            },
            KeyCode::Char('g') => match self.mode {
                Mode::Page => self.page_scroll = 0,
                Mode::List
                | Mode::Search
                | Mode::PageSearch
                | Mode::Edit
                | Mode::Backlinks
                | Mode::NewPageTitle => {}
            },
            KeyCode::Char('G') => match self.mode {
                Mode::Page => self.page_scroll = usize::MAX / 2,
                Mode::List
                | Mode::Search
                | Mode::PageSearch
                | Mode::Edit
                | Mode::Backlinks
                | Mode::NewPageTitle => {}
            },
            KeyCode::Char('n') if self.mode == Mode::List => {
                self.new_page_title.clear();
                self.mode = Mode::NewPageTitle;
            }
            KeyCode::Char('b') if self.mode == Mode::Page => {
                self.back_to_list();
            }
            KeyCode::Char('B') if self.mode == Mode::Page => {
                self.show_backlinks();
            }
            KeyCode::Char('e') if self.mode == Mode::Page => {
                self.enter_edit_mode();
            }
            KeyCode::Char('h') if self.mode == Mode::Page => {
                self.back_in_history();
            }
            KeyCode::Char('O') if self.mode == Mode::Page => {
                self.open_externally();
            }
            KeyCode::Tab if self.mode == Mode::Page => {
                self.move_link_selection(1);
            }
            KeyCode::BackTab if self.mode == Mode::Page => {
                self.move_link_selection(-1);
            }
            _ => {}
        }

        KeyResult::Tick
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
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                edit.append_text_snippet();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{EditState, collect_snippets};
    use crate::plugins::PluginManager;
    use crate::util::LruCache;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use serde_json::json;
    use std::collections::{HashMap, VecDeque};
    use std::path::PathBuf;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn key_ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[allow(dead_code)]
    fn key_shift(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    /// Build a minimal App with no pages for testing key routing logic.
    fn make_app() -> App {
        let mut index = lepiter_core::KnowledgeBase::open(fixtures_dir()).unwrap();
        index.build_backlinks();
        App {
            index,
            plugins: PluginManager::empty(),
            edit: None,
            visible_ids: Vec::new(),
            selected: 0,
            opened: None,
            parsed_cache: LruCache::new(16),
            rendered_cache: LruCache::new(16),
            page_scroll: 0,
            selected_link: 0,
            search: String::new(),
            search_needle: String::new(),
            search_hit_kind: HashMap::new(),
            text_index: LruCache::new(16),
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
        }
    }

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("lepiter-core")
            .join("tests")
            .join("fixtures")
            .join("corpus")
    }

    /// Build an App that has a page opened in Page mode with some links.
    fn make_app_on_page() -> App {
        let mut app = make_app();
        app.rebuild_visible_ids();
        // Open the first page so we're in Page mode.
        if let Some(id) = app.visible_ids.first().cloned() {
            app.open_page(&id, false);
        }
        assert_eq!(app.mode, Mode::Page);
        app
    }

    fn make_edit_state() -> EditState {
        let raw = json!({
            "children": {
                "items": [
                    {"__type": "textSnippet", "string": "hello world"},
                    {"__type": "textSnippet", "string": "second snippet"}
                ]
            }
        });
        let mut snippets = Vec::new();
        collect_snippets(&raw, Vec::new(), &mut snippets);
        let buffer = snippets[0].text.clone();
        EditState::new(
            "test-page".into(),
            PathBuf::from("/tmp/test.json"),
            raw,
            snippets,
            buffer,
        )
    }

    /// An App in `mode`: Page mode has a page open, the others start empty.
    fn make_app_in(mode: Mode) -> App {
        if mode == Mode::Page {
            return make_app_on_page();
        }
        let mut app = make_app();
        app.mode = mode;
        app
    }

    /// An App over a one-page knowledge base whose page has two links.
    fn make_app_on_page_with_two_links() -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        let page = json!({
            "uid": { "uuid": "two-links" },
            "pageType": { "title": "two links" },
            "children": { "items": [
                { "__type": "textSnippet", "string": "[a](https://a.example) and [b](https://b.example)" }
            ] }
        });
        std::fs::write(dir.path().join("two-links.lepiter"), page.to_string()).unwrap();
        let mut app = make_app();
        app.index = lepiter_core::KnowledgeBase::open(dir.path()).unwrap();
        app.open_page("two-links", false);
        assert_eq!(app.mode, Mode::Page);
        assert_eq!(app.current_rendered_page().unwrap().links.len(), 2);
        (dir, app)
    }

    // ── Help mode ──────────────────────────────────────────────────

    #[test]
    fn help_mode_dismisses_only_on_question_mark_or_esc() {
        let cases = [
            (KeyCode::Char('?'), false),
            (KeyCode::Esc, false),
            // 'q' must NOT quit while help is showing.
            (KeyCode::Char('q'), true),
            (KeyCode::Enter, true),
        ];
        for (code, still_shown) in cases {
            let mut app = make_app();
            app.show_help = true;
            let result = app.handle_key(key(code));
            assert_eq!(app.show_help, still_shown, "{code:?}");
            assert!(matches!(result, KeyResult::Continue), "{code:?}");
        }
    }

    // ── q and ? per mode ───────────────────────────────────────────

    #[test]
    fn q_quits_and_question_mark_shows_help_in_every_browsing_mode() {
        let cases = [
            (Mode::List, 'q', true),
            (Mode::Page, 'q', true),
            (Mode::Backlinks, 'q', true),
            (Mode::List, '?', false),
            (Mode::Page, '?', false),
            (Mode::Backlinks, '?', false),
        ];
        for (mode, c, quits) in cases {
            let mut app = make_app_in(mode);
            assert!(!app.show_help);
            let result = app.handle_key(key(KeyCode::Char(c)));
            assert_eq!(matches!(result, KeyResult::Quit), quits, "{mode:?} {c}");
            assert_eq!(app.show_help, !quits, "{mode:?} {c}");
        }
    }

    // ── List mode ──────────────────────────────────────────────────

    #[test]
    fn list_slash_enters_search_mode() {
        let mut app = make_app();
        app.handle_key(key(KeyCode::Char('/')));
        assert_eq!(app.mode, Mode::Search);
        assert!(app.search.is_empty());
    }

    #[test]
    fn list_esc_stays_in_list() {
        let mut app = make_app();
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::List);
    }

    #[test]
    fn list_movement_keys_move_the_selection() {
        let cases = [
            (KeyCode::Char('j'), 0, 1),
            (KeyCode::Char('k'), 1, 0),
            (KeyCode::Down, 0, 1),
            (KeyCode::Up, 1, 0),
            (KeyCode::Char('k'), 0, 0),
        ];
        for (code, start, expected) in cases {
            let mut app = make_app();
            app.rebuild_visible_ids();
            assert!(app.visible_ids.len() >= 2);
            app.selected = start;
            app.handle_key(key(code));
            assert_eq!(app.selected, expected, "{code:?} from {start}");
        }
    }

    #[test]
    fn list_enter_opens_page() {
        let mut app = make_app();
        app.rebuild_visible_ids();
        if app.visible_ids.is_empty() {
            return;
        }
        app.selected = 0;
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Page);
        assert!(app.opened.is_some());
    }

    // ── Search mode ────────────────────────────────────────────────

    #[test]
    fn search_typing_accumulates_chars() {
        let mut app = make_app();
        app.mode = Mode::Search;
        app.handle_key(key(KeyCode::Char('a')));
        app.handle_key(key(KeyCode::Char('b')));
        app.handle_key(key(KeyCode::Char('c')));
        assert_eq!(app.search, "abc");
    }

    #[test]
    fn search_backspace_removes_last_char() {
        let mut app = make_app();
        app.mode = Mode::Search;
        app.search = "abc".to_string();
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.search, "ab");
    }

    #[test]
    fn search_backspace_on_empty_is_noop() {
        let mut app = make_app();
        app.mode = Mode::Search;
        app.handle_key(key(KeyCode::Backspace));
        assert!(app.search.is_empty());
    }

    #[test]
    fn search_esc_clears_and_returns_to_list() {
        let mut app = make_app();
        app.mode = Mode::Search;
        app.search = "query".to_string();
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::List);
        assert!(app.search.is_empty());
    }

    #[test]
    fn search_enter_opens_selected_page() {
        let mut app = make_app();
        app.rebuild_visible_ids();
        app.mode = Mode::Search;
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Page);
        assert_eq!(app.opened.as_deref(), Some(app.visible_ids[0].as_str()));
    }

    #[test]
    fn search_up_down_navigate_results() {
        let mut app = make_app();
        app.rebuild_visible_ids();
        app.mode = Mode::Search;
        if app.visible_ids.len() < 2 {
            return;
        }
        app.selected = 0;
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.selected, 1);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn search_always_returns_search_tick() {
        for code in [KeyCode::Char('x'), KeyCode::Esc, KeyCode::F(1)] {
            let mut app = make_app();
            app.mode = Mode::Search;
            let result = app.handle_key(key(code));
            assert!(matches!(result, KeyResult::SearchTick), "{code:?}");
        }
    }

    // ── Page mode ──────────────────────────────────────────────────

    #[test]
    fn page_b_returns_to_list() {
        let mut app = make_app_on_page();
        app.handle_key(key(KeyCode::Char('b')));
        assert_eq!(app.mode, Mode::List);
    }

    #[test]
    fn page_scroll_keys_scroll_the_page_not_the_list() {
        let cases = [
            (KeyCode::Char('j'), 0, 1),
            (KeyCode::Char('k'), 5, 4),
            (KeyCode::Char('k'), 0, 0),
            (KeyCode::Down, 0, 1),
            (KeyCode::Up, 3, 2),
            (KeyCode::Char('g'), 50, 0),
        ];
        for (code, start, expected) in cases {
            let mut app = make_app_on_page();
            let selected = app.selected;
            app.page_scroll = start;
            app.handle_key(key(code));
            assert_eq!(app.page_scroll, expected, "{code:?} from {start}");
            assert_eq!(app.selected, selected, "{code:?} moved the list selection");
        }
    }

    #[test]
    fn page_big_g_scrolls_to_bottom() {
        let mut app = make_app_on_page();
        app.handle_key(key(KeyCode::Char('G')));
        assert!(app.page_scroll > 0);
    }

    #[test]
    fn page_esc_returns_to_list() {
        let mut app = make_app_on_page();
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::List);
    }

    #[test]
    fn page_slash_enters_page_search() {
        let mut app = make_app_on_page();
        app.handle_key(key(KeyCode::Char('/')));
        assert_eq!(app.mode, Mode::PageSearch);
    }

    #[test]
    fn page_h_goes_back_in_history() {
        let mut app = make_app_on_page();
        // No history — should go to List.
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.mode, Mode::List);
    }

    #[test]
    fn page_h_with_history_opens_previous() {
        let mut app = make_app();
        app.rebuild_visible_ids();
        if app.visible_ids.len() < 2 {
            return;
        }
        // Open first page, then open second as a link follow.
        let first = app.visible_ids[0].clone();
        let second = app.visible_ids[1].clone();
        app.open_page(&first, false);
        app.open_page(&second, true); // pushes first to history
        assert_eq!(app.opened.as_deref(), Some(second.as_str()));

        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.opened.as_deref(), Some(first.as_str()));
        assert_eq!(app.mode, Mode::Page);
    }

    #[test]
    fn page_tab_cycles_link_selection_forward() {
        let (_dir, mut app) = make_app_on_page_with_two_links();
        assert_eq!(app.selected_link, 0);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.selected_link, 1);
    }

    #[test]
    fn page_backtab_cycles_link_selection_backward() {
        let (_dir, mut app) = make_app_on_page_with_two_links();
        app.selected_link = 1;
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(app.selected_link, 0);
    }

    #[test]
    fn page_tab_on_a_page_without_links_resets_link_selection() {
        for code in [KeyCode::Tab, KeyCode::BackTab] {
            let mut app = make_app_on_page();
            app.selected_link = 3;
            app.handle_key(key(code));
            assert_eq!(app.selected_link, 0, "{code:?}");
        }
    }

    #[test]
    fn page_pageup_scrolls_half_page() {
        let mut app = make_app_on_page();
        app.page_scroll = 20;
        app.handle_key(key(KeyCode::PageUp));
        assert!(app.page_scroll < 20);
    }

    #[test]
    fn page_pagedown_scrolls_half_page() {
        let mut app = make_app_on_page();
        app.page_scroll = 0;
        app.handle_key(key(KeyCode::PageDown));
        assert!(app.page_scroll > 0);
    }

    // ── Edit mode ──────────────────────────────────────────────────

    #[test]
    fn edit_mode_returns_tick() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        app.edit = Some(make_edit_state());
        let result = app.handle_key(key(KeyCode::Char('x')));
        assert!(matches!(result, KeyResult::Tick));
    }

    #[test]
    fn edit_esc_exits_to_page_mode() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        app.edit = Some(make_edit_state());
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Page);
        assert!(app.edit.is_none());
    }

    #[test]
    fn edit_page_keys_scroll_the_preview() {
        let cases = [
            (KeyCode::PageUp, 15, 5),
            (KeyCode::PageDown, 0, 10),
            (KeyCode::PageUp, 3, 0),
        ];
        for (code, start, expected) in cases {
            let mut app = make_app();
            app.mode = Mode::Edit;
            let mut edit = make_edit_state();
            edit.preview_scroll = start;
            app.edit = Some(edit);
            app.handle_key(key(code));
            assert_eq!(
                app.edit.as_ref().unwrap().preview_scroll,
                expected,
                "{code:?} from {start}"
            );
        }
    }

    #[test]
    fn edit_cursor_keys_move_the_cursor() {
        // The buffer is "hello world", 11 chars.
        let cases = [
            (KeyCode::Left, 5, 4),
            (KeyCode::Right, 0, 1),
            (KeyCode::Left, 0, 0),
            (KeyCode::Right, 11, 11),
            (KeyCode::Home, 5, 0),
            (KeyCode::End, 0, 11),
        ];
        for (code, start, expected) in cases {
            let mut app = make_app();
            app.mode = Mode::Edit;
            let mut edit = make_edit_state();
            assert_eq!(edit.buffer, "hello world");
            edit.cursor = start;
            app.edit = Some(edit);
            app.handle_key(key(code));
            assert_eq!(
                app.edit.as_ref().unwrap().cursor,
                expected,
                "{code:?} from {start}"
            );
        }
    }

    #[test]
    fn edit_char_inserts_into_buffer() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.cursor = 0;
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::Char('X')));
        assert_eq!(app.edit.as_ref().unwrap().buffer, "Xhello world");
        assert_eq!(app.edit.as_ref().unwrap().cursor, 1);
    }

    #[test]
    fn edit_enter_inserts_newline() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.cursor = 5;
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.edit.as_ref().unwrap().buffer, "hello\n world");
        assert_eq!(app.edit.as_ref().unwrap().cursor, 6);
    }

    #[test]
    fn edit_backspace_deletes_before_cursor() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.cursor = 1;
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.edit.as_ref().unwrap().buffer, "ello world");
        assert_eq!(app.edit.as_ref().unwrap().cursor, 0);
    }

    #[test]
    fn edit_delete_removes_char_at_cursor() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.cursor = 0;
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::Delete));
        assert_eq!(app.edit.as_ref().unwrap().buffer, "ello world");
        assert_eq!(app.edit.as_ref().unwrap().cursor, 0);
    }

    #[test]
    fn edit_ctrl_u_undoes() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        // Push a snapshot, then modify.
        edit.push_undo_snapshot();
        let original = edit.buffer.clone();
        edit.buffer = "modified".to_string();
        edit.cursor = 8;
        edit.commit_buffer();
        app.edit = Some(edit);

        app.handle_key(key_ctrl(KeyCode::Char('u')));
        assert_eq!(app.edit.as_ref().unwrap().buffer, original);
    }

    #[test]
    fn edit_ctrl_char_is_ignored() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let edit = make_edit_state();
        let before = edit.buffer.clone();
        app.edit = Some(edit);
        app.handle_key(key_ctrl(KeyCode::Char('b')));
        assert_eq!(app.edit.as_ref().unwrap().buffer, before);
    }

    #[test]
    fn edit_tab_and_backtab_switch_snippets_within_bounds() {
        // make_edit_state has two snippets.
        let cases = [
            (0, KeyCode::Tab, 1),
            (1, KeyCode::BackTab, 0),
            (1, KeyCode::Tab, 1),
            (0, KeyCode::BackTab, 0),
        ];
        for (start, code, expected) in cases {
            let mut app = make_app();
            app.mode = Mode::Edit;
            let mut edit = make_edit_state();
            assert_eq!(edit.snippets.len(), 2);
            edit.selected = start;
            let text = edit.snippets[start].text.clone();
            edit.set_buffer(text);
            app.edit = Some(edit);
            app.handle_key(key(code));
            assert_eq!(
                app.edit.as_ref().unwrap().selected,
                expected,
                "{code:?} from {start}"
            );
        }
    }

    // ── Open externally ────────────────────────────────────────────

    #[test]
    fn page_shift_o_triggers_open_externally() {
        let mut app = make_app_on_page();
        app.handle_key(key(KeyCode::Char('O')));
        // Without LEPITER_OPEN_CMD set, the method prompts the user.
        assert!(app.status.contains("LEPITER_OPEN_CMD"));
    }

    #[test]
    fn list_shift_o_is_noop() {
        let mut app = make_app();
        app.handle_key(key(KeyCode::Char('O')));
        assert!(app.status.is_empty());
    }

    // ── Mode transitions ───────────────────────────────────────────

    #[test]
    fn page_to_edit_to_page_roundtrip() {
        let mut app = make_app_on_page();
        app.handle_key(key(KeyCode::Char('e')));
        assert_eq!(app.mode, Mode::Edit);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Page);
    }

    // ── Unrecognized keys ──────────────────────────────────────────

    #[test]
    fn list_and_page_keys_return_tick() {
        let cases = [
            (Mode::List, KeyCode::Esc),
            (Mode::List, KeyCode::F(12)),
            (Mode::Page, KeyCode::F(12)),
        ];
        for (mode, code) in cases {
            let mut app = make_app_in(mode);
            let result = app.handle_key(key(code));
            assert!(matches!(result, KeyResult::Tick), "{mode:?} {code:?}");
        }
    }

    // ── Page mode: keys that should NOT work ───────────────────────

    #[test]
    fn list_g_does_not_change_scroll() {
        let mut app = make_app();
        app.page_scroll = 42;
        app.handle_key(key(KeyCode::Char('g')));
        // 'g' only works in Page mode; in List, it's a no-op.
        assert_eq!(app.page_scroll, 42);
    }

    // ── Page search mode ──────────────────────────────────────────

    #[test]
    fn page_search_accumulates_chars() {
        let mut app = make_app_on_page();
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('h')));
        app.handle_key(key(KeyCode::Char('e')));
        assert_eq!(app.page_search, "he");
        assert_eq!(app.page_search_needle, "he");
    }

    #[test]
    fn page_search_backspace_removes_char() {
        let mut app = make_app_on_page();
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('a')));
        app.handle_key(key(KeyCode::Char('b')));
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.page_search, "a");
    }

    #[test]
    fn page_search_esc_clears_and_returns_to_page() {
        let mut app = make_app_on_page();
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('x')));
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Page);
        assert!(app.page_search.is_empty());
        assert!(app.page_search_needle.is_empty());
    }

    #[test]
    fn page_search_enter_returns_to_page_with_search_active() {
        let mut app = make_app_on_page();
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('t')));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Page);
        // Search needle should be preserved after Enter.
        assert_eq!(app.page_search_needle, "t");
    }

    #[test]
    fn page_search_returns_continue() {
        let mut app = make_app_on_page();
        app.handle_key(key(KeyCode::Char('/')));
        let result = app.handle_key(key(KeyCode::Char('x')));
        assert!(matches!(result, KeyResult::Continue));
    }

    #[test]
    fn page_esc_clears_active_search_before_going_to_list() {
        let mut app = make_app_on_page();
        // Enter page search and confirm with Enter
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('t')));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Page);
        assert!(!app.page_search_needle.is_empty());
        // First Esc clears the search but stays in Page mode.
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Page);
        assert!(app.page_search_needle.is_empty());
        // Second Esc goes to list.
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::List);
    }

    #[test]
    fn page_n_without_search_is_noop() {
        let mut app = make_app_on_page();
        let scroll_before = app.page_scroll;
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.page_scroll, scroll_before);
    }

    #[test]
    fn page_search_n_cycles_forward() {
        let mut app = make_app_on_page();
        // Set up a fake match list to test cycling.
        app.page_search_needle = "test".to_string();
        app.page_search_match_lines = vec![5, 10, 20];
        app.page_search_current = 0;
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.page_search_current, 1);
        assert_eq!(app.page_scroll, 10);
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.page_search_current, 2);
        assert_eq!(app.page_scroll, 20);
        // Wraps around.
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.page_search_current, 0);
        assert_eq!(app.page_scroll, 5);
    }

    #[test]
    fn page_search_shift_n_cycles_backward() {
        let mut app = make_app_on_page();
        app.page_search_needle = "test".to_string();
        app.page_search_match_lines = vec![5, 10, 20];
        app.page_search_current = 0;
        // N (shift-n) goes backward, wrapping to end.
        app.handle_key(key(KeyCode::Char('N')));
        assert_eq!(app.page_search_current, 2);
        assert_eq!(app.page_scroll, 20);
        app.handle_key(key(KeyCode::Char('N')));
        assert_eq!(app.page_search_current, 1);
        assert_eq!(app.page_scroll, 10);
    }

    #[test]
    fn open_page_clears_page_search() {
        let mut app = make_app_on_page();
        app.page_search = "something".to_string();
        app.page_search_needle = "something".to_string();
        app.page_search_match_lines = vec![1, 2];
        app.page_search_current = 1;
        let id = app.visible_ids[1].clone();
        app.open_page(&id, false);
        assert!(app.page_search.is_empty());
        assert!(app.page_search_needle.is_empty());
        assert!(app.page_search_match_lines.is_empty());
        assert_eq!(app.page_search_current, 0);
    }

    // ── Backlinks mode ─────────────────────────────────────────────

    #[test]
    fn page_shift_b_shows_backlinks_or_status() {
        let mut app = make_app_on_page();
        assert_eq!(app.opened.as_deref(), Some("fixture-page-alpha"));
        app.handle_key(key(KeyCode::Char('B')));
        assert_eq!(app.mode, Mode::Backlinks);
        assert_eq!(app.backlink_ids, vec!["fixture-page-beta".to_string()]);
    }

    #[test]
    fn backlinks_esc_returns_to_page() {
        let mut app = make_app_on_page();
        app.mode = Mode::Backlinks;
        app.backlink_ids = vec!["dummy".to_string()];
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Page);
    }

    #[test]
    fn backlinks_j_k_navigate() {
        let mut app = make_app();
        app.mode = Mode::Backlinks;
        app.backlink_ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        app.backlink_selected = 0;

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.backlink_selected, 1);

        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.backlink_selected, 0);
    }

    #[test]
    fn backlinks_selection_clamps() {
        let mut app = make_app();
        app.mode = Mode::Backlinks;
        app.backlink_ids = vec!["a".to_string()];
        app.backlink_selected = 0;

        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.backlink_selected, 0);

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.backlink_selected, 0); // only 1 item
    }

    #[test]
    fn backlinks_returns_tick() {
        let mut app = make_app();
        app.mode = Mode::Backlinks;
        let result = app.handle_key(key(KeyCode::Char('j')));
        assert!(matches!(result, KeyResult::Tick));
    }

    // ── New page title mode ───────────────────────────────────────

    #[test]
    fn list_n_enters_new_page_title_mode() {
        let mut app = make_app();
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.mode, Mode::NewPageTitle);
        assert!(app.new_page_title.is_empty());
    }

    #[test]
    fn new_page_title_typing_accumulates() {
        let mut app = make_app();
        app.mode = Mode::NewPageTitle;
        app.handle_key(key(KeyCode::Char('H')));
        app.handle_key(key(KeyCode::Char('i')));
        assert_eq!(app.new_page_title, "Hi");
    }

    #[test]
    fn new_page_title_backspace_removes_char() {
        let mut app = make_app();
        app.mode = Mode::NewPageTitle;
        app.new_page_title = "abc".to_string();
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.new_page_title, "ab");
    }

    #[test]
    fn new_page_title_esc_cancels() {
        let mut app = make_app();
        app.mode = Mode::NewPageTitle;
        app.new_page_title = "something".to_string();
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::List);
        assert!(app.new_page_title.is_empty());
    }

    #[test]
    fn new_page_title_empty_enter_returns_to_list() {
        let mut app = make_app();
        app.mode = Mode::NewPageTitle;
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::List);
    }

    #[test]
    fn new_page_title_returns_tick() {
        let mut app = make_app();
        app.mode = Mode::NewPageTitle;
        let result = app.handle_key(key(KeyCode::Char('x')));
        assert!(matches!(result, KeyResult::Tick));
    }

    // ── Ctrl+A append snippet ─────────────────────────────────────

    #[test]
    fn edit_ctrl_a_appends_snippet() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let edit = make_edit_state();
        assert_eq!(edit.snippets.len(), 2);
        app.edit = Some(edit);
        app.handle_key(key_ctrl(KeyCode::Char('a')));
        let edit = app.edit.as_ref().unwrap();
        assert_eq!(edit.snippets.len(), 3);
        assert_eq!(edit.selected, 2);
        assert!(edit.buffer.is_empty());
        assert!(edit.dirty);
    }
}
