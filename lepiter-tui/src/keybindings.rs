use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::edit::{delete_char_at, delete_char_before, insert_char_at, move_cursor_vertical};
use crate::{App, Mode};

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
                    self.rebuild_visible_ids();
                }
                KeyCode::Char(c) => {
                    self.search.push(c);
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
                Mode::Page => {
                    let half = crossterm::terminal::size()
                        .map(|(_, h)| (h.saturating_sub(6) / 2).max(1) as isize)
                        .unwrap_or(10);
                    self.scroll_page(-half);
                }
                Mode::List | Mode::Search | Mode::Edit => {}
            },
            KeyCode::PageDown => match self.mode {
                Mode::Page => {
                    let half = crossterm::terminal::size()
                        .map(|(_, h)| (h.saturating_sub(6) / 2).max(1) as isize)
                        .unwrap_or(10);
                    self.scroll_page(half);
                }
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
