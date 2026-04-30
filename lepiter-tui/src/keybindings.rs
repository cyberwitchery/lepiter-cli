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

        if self.mode == Mode::Edit {
            self.handle_edit_key(key);
            return KeyResult::Tick;
        }

        match key.code {
            KeyCode::Char('q') => return KeyResult::Quit,
            KeyCode::Char('?') => {
                self.show_help = true;
            }
            KeyCode::Char('/') => {
                self.search.clear();
                self.update_search_needle();
                self.rebuild_visible_ids();
                self.mode = Mode::Search;
            }
            KeyCode::Esc => self.mode = Mode::List,
            KeyCode::Enter => match self.mode {
                Mode::List => {
                    self.mode = Mode::List;
                    self.open_selected_page();
                }
                Mode::Page => self.follow_selected_link(),
                Mode::Search | Mode::Edit => {}
            },
            KeyCode::Up | KeyCode::Char('k') => match self.mode {
                Mode::List => self.move_selection(-1),
                Mode::Page => self.scroll_page(-1),
                Mode::Search | Mode::Edit => {}
            },
            KeyCode::Down | KeyCode::Char('j') => match self.mode {
                Mode::List => self.move_selection(1),
                Mode::Page => self.scroll_page(1),
                Mode::Search | Mode::Edit => {}
            },
            KeyCode::PageUp => match self.mode {
                Mode::Page => self.scroll_page(-page_scroll_half()),
                Mode::List | Mode::Search | Mode::Edit => {}
            },
            KeyCode::PageDown => match self.mode {
                Mode::Page => self.scroll_page(page_scroll_half()),
                Mode::List | Mode::Search | Mode::Edit => {}
            },
            KeyCode::Char('g') => match self.mode {
                Mode::Page => self.page_scroll = 0,
                Mode::List | Mode::Search | Mode::Edit => {}
            },
            KeyCode::Char('G') => match self.mode {
                Mode::Page => self.page_scroll = usize::MAX / 2,
                Mode::List | Mode::Search | Mode::Edit => {}
            },
            KeyCode::Char('b') if self.mode == Mode::Page => {
                self.back_to_list();
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
        App {
            index: lepiter_core::KnowledgeBase::open(fixtures_dir()).unwrap(),
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
            text_index: HashMap::new(),
            text_index_queue: VecDeque::new(),
            history: Vec::new(),
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

    // ── Help mode ──────────────────────────────────────────────────

    #[test]
    fn help_mode_question_mark_dismisses() {
        let mut app = make_app();
        app.show_help = true;
        let result = app.handle_key(key(KeyCode::Char('?')));
        assert!(!app.show_help);
        assert!(matches!(result, KeyResult::Continue));
    }

    #[test]
    fn help_mode_esc_dismisses() {
        let mut app = make_app();
        app.show_help = true;
        let result = app.handle_key(key(KeyCode::Esc));
        assert!(!app.show_help);
        assert!(matches!(result, KeyResult::Continue));
    }

    #[test]
    fn help_mode_ignores_other_keys() {
        let mut app = make_app();
        app.show_help = true;
        let result = app.handle_key(key(KeyCode::Char('q')));
        // 'q' should NOT quit while help is showing.
        assert!(app.show_help);
        assert!(matches!(result, KeyResult::Continue));
    }

    #[test]
    fn help_mode_ignores_enter() {
        let mut app = make_app();
        app.show_help = true;
        let result = app.handle_key(key(KeyCode::Enter));
        assert!(app.show_help);
        assert!(matches!(result, KeyResult::Continue));
    }

    // ── List mode ──────────────────────────────────────────────────

    #[test]
    fn list_q_returns_quit() {
        let mut app = make_app();
        let result = app.handle_key(key(KeyCode::Char('q')));
        assert!(matches!(result, KeyResult::Quit));
    }

    #[test]
    fn list_question_mark_shows_help() {
        let mut app = make_app();
        assert!(!app.show_help);
        app.handle_key(key(KeyCode::Char('?')));
        assert!(app.show_help);
    }

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
    fn list_j_moves_selection_down() {
        let mut app = make_app();
        app.rebuild_visible_ids();
        if app.visible_ids.len() < 2 {
            return; // need at least 2 pages for this test
        }
        app.selected = 0;
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn list_k_moves_selection_up() {
        let mut app = make_app();
        app.rebuild_visible_ids();
        if app.visible_ids.len() < 2 {
            return;
        }
        app.selected = 1;
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn list_down_arrow_moves_selection() {
        let mut app = make_app();
        app.rebuild_visible_ids();
        if app.visible_ids.len() < 2 {
            return;
        }
        app.selected = 0;
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn list_up_arrow_moves_selection() {
        let mut app = make_app();
        app.rebuild_visible_ids();
        if app.visible_ids.len() < 2 {
            return;
        }
        app.selected = 1;
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn list_selection_clamps_at_zero() {
        let mut app = make_app();
        app.rebuild_visible_ids();
        app.selected = 0;
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected, 0);
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

    #[test]
    fn list_returns_tick() {
        let mut app = make_app();
        let result = app.handle_key(key(KeyCode::Esc));
        assert!(matches!(result, KeyResult::Tick));
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
        // With pages present, Enter opens the selected page (Page mode).
        // The code sets mode=List then calls open_selected_page which
        // transitions to Page if a page is available.
        if app.visible_ids.is_empty() {
            assert_eq!(app.mode, Mode::List);
        } else {
            assert_eq!(app.mode, Mode::Page);
        }
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
        let mut app = make_app();
        app.mode = Mode::Search;
        let result = app.handle_key(key(KeyCode::Char('x')));
        assert!(matches!(result, KeyResult::SearchTick));
    }

    #[test]
    fn search_esc_returns_search_tick() {
        let mut app = make_app();
        app.mode = Mode::Search;
        let result = app.handle_key(key(KeyCode::Esc));
        assert!(matches!(result, KeyResult::SearchTick));
    }

    #[test]
    fn search_unrecognized_key_returns_search_tick() {
        let mut app = make_app();
        app.mode = Mode::Search;
        let result = app.handle_key(key(KeyCode::F(1)));
        assert!(matches!(result, KeyResult::SearchTick));
    }

    // ── Page mode ──────────────────────────────────────────────────

    #[test]
    fn page_b_returns_to_list() {
        let mut app = make_app_on_page();
        app.handle_key(key(KeyCode::Char('b')));
        assert_eq!(app.mode, Mode::List);
    }

    #[test]
    fn page_j_scrolls_down() {
        let mut app = make_app_on_page();
        app.page_scroll = 0;
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.page_scroll, 1);
    }

    #[test]
    fn page_k_scrolls_up() {
        let mut app = make_app_on_page();
        app.page_scroll = 5;
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.page_scroll, 4);
    }

    #[test]
    fn page_k_clamps_at_zero() {
        let mut app = make_app_on_page();
        app.page_scroll = 0;
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.page_scroll, 0);
    }

    #[test]
    fn page_g_scrolls_to_top() {
        let mut app = make_app_on_page();
        app.page_scroll = 50;
        app.handle_key(key(KeyCode::Char('g')));
        assert_eq!(app.page_scroll, 0);
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
    fn page_q_quits() {
        let mut app = make_app_on_page();
        let result = app.handle_key(key(KeyCode::Char('q')));
        assert!(matches!(result, KeyResult::Quit));
    }

    #[test]
    fn page_question_mark_shows_help() {
        let mut app = make_app_on_page();
        app.handle_key(key(KeyCode::Char('?')));
        assert!(app.show_help);
    }

    #[test]
    fn page_slash_enters_search() {
        let mut app = make_app_on_page();
        app.handle_key(key(KeyCode::Char('/')));
        assert_eq!(app.mode, Mode::Search);
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
        let mut app = make_app_on_page();
        let initial = app.selected_link;
        app.handle_key(key(KeyCode::Tab));
        // If there are links, selection should advance (or stay at 0 if no links).
        let page = app.current_rendered_page();
        if let Some(p) = page
            && !p.links.is_empty()
        {
            assert!(app.selected_link >= initial);
        }
    }

    #[test]
    fn page_backtab_cycles_link_selection_backward() {
        let mut app = make_app_on_page();
        // Set link selection to 1 to test backward.
        app.selected_link = 1;
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(app.selected_link, 0);
    }

    #[test]
    fn page_down_arrow_scrolls() {
        let mut app = make_app_on_page();
        app.page_scroll = 0;
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.page_scroll, 1);
    }

    #[test]
    fn page_up_arrow_scrolls() {
        let mut app = make_app_on_page();
        app.page_scroll = 3;
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.page_scroll, 2);
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
    fn edit_pageup_decreases_scroll() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.preview_scroll = 15;
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::PageUp));
        assert_eq!(app.edit.as_ref().unwrap().preview_scroll, 5);
    }

    #[test]
    fn edit_pagedown_increases_scroll() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.preview_scroll = 0;
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.edit.as_ref().unwrap().preview_scroll, 10);
    }

    #[test]
    fn edit_pageup_clamps_at_zero() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.preview_scroll = 3;
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::PageUp));
        assert_eq!(app.edit.as_ref().unwrap().preview_scroll, 0);
    }

    #[test]
    fn edit_left_arrow_moves_cursor() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.cursor = 5;
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::Left));
        assert_eq!(app.edit.as_ref().unwrap().cursor, 4);
    }

    #[test]
    fn edit_right_arrow_moves_cursor() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.cursor = 0;
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::Right));
        assert_eq!(app.edit.as_ref().unwrap().cursor, 1);
    }

    #[test]
    fn edit_left_clamps_at_zero() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.cursor = 0;
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::Left));
        assert_eq!(app.edit.as_ref().unwrap().cursor, 0);
    }

    #[test]
    fn edit_right_clamps_at_buffer_len() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        let len = edit.buffer.chars().count();
        edit.cursor = len;
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::Right));
        assert_eq!(app.edit.as_ref().unwrap().cursor, len);
    }

    #[test]
    fn edit_home_moves_cursor_to_start() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.cursor = 5;
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::Home));
        assert_eq!(app.edit.as_ref().unwrap().cursor, 0);
    }

    #[test]
    fn edit_end_moves_cursor_to_end() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        let len = edit.buffer.chars().count();
        edit.cursor = 0;
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::End));
        assert_eq!(app.edit.as_ref().unwrap().cursor, len);
    }

    #[test]
    fn edit_char_inserts_into_buffer() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.cursor = 0;
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::Char('X')));
        assert!(app.edit.as_ref().unwrap().buffer.starts_with('X'));
    }

    #[test]
    fn edit_enter_inserts_newline() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.cursor = 5;
        app.edit = Some(edit);
        let before = app.edit.as_ref().unwrap().buffer.clone();
        app.handle_key(key(KeyCode::Enter));
        assert!(app.edit.as_ref().unwrap().buffer.contains('\n'));
        assert!(app.edit.as_ref().unwrap().buffer.len() > before.len());
    }

    #[test]
    fn edit_backspace_deletes_before_cursor() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.cursor = 1;
        app.edit = Some(edit);
        let before_len = app.edit.as_ref().unwrap().buffer.len();
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.edit.as_ref().unwrap().buffer.len(), before_len - 1);
        assert_eq!(app.edit.as_ref().unwrap().cursor, 0);
    }

    #[test]
    fn edit_delete_removes_char_at_cursor() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.cursor = 0;
        let first_char = edit.buffer.chars().next().unwrap();
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::Delete));
        assert!(!app.edit.as_ref().unwrap().buffer.starts_with(first_char));
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
        app.handle_key(key_ctrl(KeyCode::Char('a')));
        assert_eq!(app.edit.as_ref().unwrap().buffer, before);
    }

    #[test]
    fn edit_tab_switches_snippet_forward() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let edit = make_edit_state();
        assert_eq!(edit.selected, 0);
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.edit.as_ref().unwrap().selected, 1);
    }

    #[test]
    fn edit_backtab_switches_snippet_backward() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        edit.selected = 1;
        if let Some(entry) = edit.snippets.get(1) {
            edit.set_buffer(entry.text.clone());
        }
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(app.edit.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn edit_tab_clamps_at_last_snippet() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let mut edit = make_edit_state();
        let last = edit.snippets.len() - 1;
        edit.selected = last;
        if let Some(entry) = edit.snippets.get(last) {
            edit.set_buffer(entry.text.clone());
        }
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.edit.as_ref().unwrap().selected, last);
    }

    #[test]
    fn edit_backtab_clamps_at_first_snippet() {
        let mut app = make_app();
        app.mode = Mode::Edit;
        let edit = make_edit_state();
        assert_eq!(edit.selected, 0);
        app.edit = Some(edit);
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(app.edit.as_ref().unwrap().selected, 0);
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
    fn list_to_search_to_list_roundtrip() {
        let mut app = make_app();
        assert_eq!(app.mode, Mode::List);

        app.handle_key(key(KeyCode::Char('/')));
        assert_eq!(app.mode, Mode::Search);

        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::List);
    }

    #[test]
    fn list_to_page_to_list_via_b() {
        let mut app = make_app();
        app.rebuild_visible_ids();
        if app.visible_ids.is_empty() {
            return;
        }
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Page);

        app.handle_key(key(KeyCode::Char('b')));
        assert_eq!(app.mode, Mode::List);
    }

    #[test]
    fn page_to_edit_to_page_roundtrip() {
        let mut app = make_app_on_page();
        // 'e' enters edit mode.
        app.handle_key(key(KeyCode::Char('e')));
        // Whether we enter edit depends on whether the page has editable snippets.
        if app.mode == Mode::Edit {
            app.handle_key(key(KeyCode::Esc));
            assert_eq!(app.mode, Mode::Page);
        }
    }

    // ── Unrecognized keys ──────────────────────────────────────────

    #[test]
    fn list_unrecognized_key_returns_tick() {
        let mut app = make_app();
        let result = app.handle_key(key(KeyCode::F(12)));
        assert!(matches!(result, KeyResult::Tick));
    }

    #[test]
    fn page_unrecognized_key_returns_tick() {
        let mut app = make_app_on_page();
        let result = app.handle_key(key(KeyCode::F(12)));
        assert!(matches!(result, KeyResult::Tick));
    }

    // ── Page mode: keys that should NOT work ───────────────────────

    #[test]
    fn page_j_k_dont_change_list_selection() {
        let mut app = make_app_on_page();
        let before = app.selected;
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected, before); // j scrolls page, not selection
    }

    #[test]
    fn list_g_does_not_change_scroll() {
        let mut app = make_app();
        app.page_scroll = 42;
        app.handle_key(key(KeyCode::Char('g')));
        // 'g' only works in Page mode; in List, it's a no-op.
        assert_eq!(app.page_scroll, 42);
    }
}
