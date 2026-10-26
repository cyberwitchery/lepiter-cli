//! edit-mode support: snippet collection, buffer edits, cursor positioning,
//! autosave, undo, and rendering helpers for the inline editor view.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use lepiter_core::{
    Node, PageId, extract_type, is_code_snippet, normalize_text, parse_heading, parse_node_from_raw,
};

use crate::plugins::PluginManager;
use crate::render::{
    heading_style, parse_inline_annotations, render_code_block, render_node, sanitize_for_terminal,
};

pub struct EditState {
    pub page_id: PageId,
    pub path: PathBuf,
    pub raw: Value,
    pub snippets: Vec<SnippetEntry>,
    pub selected: usize,
    pub buffer: String,
    pub cursor: usize,
    pub preview_scroll: usize,
    pub follow_cursor: bool,
    pub last_snapshot: Option<Instant>,
    pub snapshot_pending: bool,
    pub dirty: bool,
    pub last_edit: Option<Instant>,
}

pub struct SnippetEntry {
    pub path: Vec<usize>,
    pub typ: String,
    pub field: Option<String>,
    pub text: String,
    pub editable: bool,
    pub undo: VecDeque<UndoEntry>,
}

pub struct UndoEntry {
    pub text: String,
    pub cursor: usize,
}

impl EditState {
    pub fn new(
        page_id: PageId,
        path: PathBuf,
        raw: Value,
        snippets: Vec<SnippetEntry>,
        buffer: String,
    ) -> Self {
        let buffer = normalize_text(&buffer);
        let cursor = buffer.chars().count();
        Self {
            page_id,
            path,
            raw,
            snippets,
            selected: 0,
            buffer,
            cursor,
            preview_scroll: 0,
            follow_cursor: true,
            last_snapshot: None,
            snapshot_pending: true,
            dirty: false,
            last_edit: None,
        }
    }

    pub fn current(&self) -> Option<&SnippetEntry> {
        self.snippets.get(self.selected)
    }

    pub fn current_mut(&mut self) -> Option<&mut SnippetEntry> {
        self.snippets.get_mut(self.selected)
    }

    pub fn is_editable(&self) -> bool {
        self.current().map(|s| s.editable).unwrap_or(false)
    }

    pub fn set_buffer(&mut self, text: String) {
        self.buffer = normalize_text(&text);
        self.cursor = self.buffer.chars().count();
        self.last_snapshot = None;
        self.snapshot_pending = true;
    }

    pub fn commit_buffer(&mut self) {
        let buffer = self.buffer.clone();
        let (path, field) = {
            let Some(entry) = self.current_mut() else {
                return;
            };
            if !entry.editable || entry.text == buffer {
                return;
            }
            entry.text = buffer.clone();
            (entry.path.clone(), entry.field.clone())
        };
        if let Some(field) = field
            && let Some(item) = snippet_at_path_mut(&mut self.raw, &path)
            && let Some(obj) = item.as_object_mut()
        {
            obj.insert(field, Value::String(buffer));
        }
        self.dirty = true;
        self.last_edit = Some(Instant::now());
    }

    pub fn push_undo_snapshot(&mut self) {
        let snapshot = UndoEntry {
            text: self.buffer.clone(),
            cursor: self.cursor,
        };
        let Some(entry) = self.current_mut() else {
            return;
        };
        if !entry.editable {
            return;
        }
        if entry.undo.len() >= 200 {
            entry.undo.pop_front();
        }
        entry.undo.push_back(snapshot);
    }

    pub fn maybe_push_undo_snapshot(&mut self) {
        let now = Instant::now();
        let elapsed_ok = self
            .last_snapshot
            .map(|last| now.duration_since(last) >= Duration::from_millis(750))
            .unwrap_or(true);
        if self.snapshot_pending || elapsed_ok {
            self.push_undo_snapshot();
            self.last_snapshot = Some(now);
            self.snapshot_pending = false;
        }
    }

    pub fn undo(&mut self) -> bool {
        let (path, field, prev) = {
            let Some(entry) = self.current_mut() else {
                return false;
            };
            if !entry.editable {
                return false;
            }
            let Some(prev) = entry.undo.pop_back() else {
                return false;
            };
            entry.text = prev.text.clone();
            (entry.path.clone(), entry.field.clone(), prev)
        };
        self.buffer = prev.text;
        self.cursor = prev.cursor.min(self.buffer.chars().count());
        if let Some(field) = field
            && let Some(item) = snippet_at_path_mut(&mut self.raw, &path)
            && let Some(obj) = item.as_object_mut()
        {
            obj.insert(field, Value::String(self.buffer.clone()));
        }
        self.dirty = true;
        self.last_edit = Some(Instant::now());
        self.last_snapshot = Some(Instant::now());
        self.snapshot_pending = true;
        true
    }

    pub fn maybe_autosave(&mut self) -> bool {
        if !self.dirty {
            return false;
        }
        let Some(last) = self.last_edit else {
            return false;
        };
        let delay = Duration::from_millis(
            std::env::var("LEPITER_EDIT_AUTOSAVE_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(500),
        );
        last.elapsed() >= delay
    }

    pub fn append_text_snippet(&mut self) {
        let new_snippet = serde_json::json!({
            "__type": "textSnippet",
            "string": ""
        });

        let items = self
            .raw
            .get_mut("children")
            .and_then(|v| v.get_mut("items"))
            .and_then(Value::as_array_mut);
        let Some(items) = items else {
            return;
        };
        items.push(new_snippet);

        self.commit_buffer();

        let mut snippets = Vec::new();
        collect_snippets(&self.raw, Vec::new(), &mut snippets);
        let new_idx = snippets.len().saturating_sub(1);
        self.snippets = snippets;
        self.selected = new_idx;
        if let Some(entry) = self.current() {
            let text = entry.text.clone();
            self.set_buffer(text);
        }
        self.dirty = true;
        self.last_edit = Some(Instant::now());
    }

    pub fn save_to_disk(&mut self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.raw)?;

        serde_json::from_slice::<Value>(&bytes)
            .context("BUG: serialized page JSON failed re-parse validation")?;

        // Write to a temporary file in the same directory (same filesystem),
        // then atomically rename over the target path.  This avoids corruption
        // if the process crashes or the disk fills mid-write.
        let dir = self
            .path
            .parent()
            .context("page path has no parent directory")?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)
            .with_context(|| format!("failed to create temp file in {}", dir.display()))?;
        tmp.write_all(&bytes)
            .with_context(|| format!("failed to write temp file for {}", self.path.display()))?;
        tmp.as_file()
            .sync_all()
            .with_context(|| format!("failed to fsync temp file for {}", self.path.display()))?;
        tmp.persist(&self.path)
            .with_context(|| format!("failed to persist temp file to {}", self.path.display()))?;

        self.dirty = false;
        Ok(())
    }
}

pub fn load_raw_page(path: &PathBuf) -> Result<Value> {
    let file =
        File::open(path).with_context(|| format!("failed to open page file {}", path.display()))?;
    let reader = BufReader::new(file);
    let raw: Value =
        serde_json::from_reader(reader).with_context(|| "failed to decode page JSON")?;
    Ok(raw)
}

pub fn collect_snippets(raw: &Value, path: Vec<usize>, out: &mut Vec<SnippetEntry>) {
    let Some(items) = raw
        .get("children")
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
    else {
        return;
    };

    for (idx, item) in items.iter().enumerate() {
        let mut current_path = path.clone();
        current_path.push(idx);
        let typ = item
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or("<missing-type>")
            .to_string();
        let (editable, field) = editable_field(&typ, item);
        let text = if let Some(field) = field.as_ref() {
            item.get(field)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        } else {
            snippet_preview(item)
        };
        let text = normalize_text(&text);
        out.push(SnippetEntry {
            path: current_path.clone(),
            typ,
            field,
            text,
            editable,
            undo: VecDeque::new(),
        });

        collect_snippets(item, current_path, out);
    }
}

/// Field names that may hold text-snippet content, in priority order.
const TEXT_FIELDS: &[&str] = &["string", "text", "content"];
/// Field names that may hold code-snippet content, in priority order.
const CODE_FIELDS: &[&str] = &["code", "source"];

pub fn editable_field(typ: &str, item: &Value) -> (bool, Option<String>) {
    if typ == "textSnippet" {
        let field = pick_first_field(item, TEXT_FIELDS).unwrap_or("string");
        return (true, Some(field.to_string()));
    }

    if is_code_snippet(typ) {
        let field = pick_first_field(item, CODE_FIELDS).unwrap_or("code");
        return (true, Some(field.to_string()));
    }

    (false, None)
}

fn pick_first_field<'a>(item: &'a Value, fields: &[&'a str]) -> Option<&'a str> {
    for field in fields {
        if item.get(*field).and_then(Value::as_str).is_some() {
            return Some(*field);
        }
    }
    None
}

fn snippet_preview(item: &Value) -> String {
    pick_first_field(item, TEXT_FIELDS)
        .or_else(|| pick_first_field(item, CODE_FIELDS))
        .and_then(|f| item.get(f).and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

fn snippet_at_path_mut<'a>(raw: &'a mut Value, path: &[usize]) -> Option<&'a mut Value> {
    let mut current = raw;
    for idx in path {
        current = current
            .get_mut("children")?
            .get_mut("items")?
            .as_array_mut()?
            .get_mut(*idx)?;
    }
    Some(current)
}

pub fn insert_char_at(buf: &mut String, cursor: &mut usize, ch: char) {
    let ch = if ch == '\r' { '\n' } else { ch };
    let idx = char_to_byte_idx(buf, *cursor);
    buf.insert(idx, ch);
    *cursor += 1;
}

pub fn delete_char_before(buf: &mut String, cursor: &mut usize) -> bool {
    if *cursor == 0 {
        return false;
    }
    let idx = char_to_byte_idx(buf, *cursor - 1);
    buf.remove(idx);
    *cursor -= 1;
    true
}

pub fn delete_char_at(buf: &mut String, cursor: &mut usize) -> bool {
    let len = buf.chars().count();
    if *cursor >= len {
        return false;
    }
    let idx = char_to_byte_idx(buf, *cursor);
    buf.remove(idx);
    true
}

pub fn char_to_byte_idx(buf: &str, char_idx: usize) -> usize {
    buf.char_indices()
        .nth(char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(buf.len())
}

pub fn cursor_line_col_display(text: &str, cursor: usize) -> (usize, usize) {
    let mut line = 0usize;
    let mut col = 0usize;
    for (idx, ch) in text.chars().enumerate() {
        if idx == cursor {
            return (line, col);
        }
        if ch == '\n' || ch == '\r' {
            line += 1;
            col = 0;
        } else if ch == '\t' {
            col += 4;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn display_col_to_char_idx(line: &str, target_col: usize) -> usize {
    let mut col = 0usize;
    let mut idx = 0usize;
    for ch in line.chars() {
        let width = if ch == '\t' { 4 } else { 1 };
        if col + width > target_col {
            return idx;
        }
        col += width;
        idx += 1;
    }
    idx
}

pub fn move_cursor_vertical(edit: &mut EditState, delta: isize) {
    debug_assert!(
        !edit.buffer.contains('\r'),
        "buffer must be normalized (no CR characters)"
    );
    let (line, col) = cursor_line_col_display(&edit.buffer, edit.cursor);
    let lines = edit.buffer.split('\n').collect::<Vec<_>>();
    if lines.is_empty() {
        return;
    }
    let next_line = if delta.is_negative() {
        line.saturating_sub(delta.unsigned_abs())
    } else {
        (line + delta as usize).min(lines.len().saturating_sub(1))
    };
    let target_idx = display_col_to_char_idx(lines[next_line], col);
    let mut idx = 0usize;
    for (i, l) in lines.iter().enumerate() {
        if i == next_line {
            idx += target_idx;
            break;
        }
        idx += l.chars().count() + 1;
    }
    edit.cursor = idx;
}

pub fn ensure_scroll(cursor_line: usize, scroll: usize, view_height: usize) -> usize {
    if view_height == 0 {
        return scroll;
    }
    if cursor_line < scroll {
        return cursor_line;
    }
    if cursor_line >= scroll + view_height {
        return cursor_line.saturating_sub(view_height.saturating_sub(1));
    }
    scroll
}

fn wrap_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![line.clone()];
    }
    let mut out = Vec::new();
    let mut spans = Vec::new();
    let mut current_width = 0usize;
    for span in &line.spans {
        for ch in span.content.chars() {
            let ch_width = if ch == '\t' { 4 } else { 1 };
            if current_width + ch_width > width && !spans.is_empty() {
                let mut new_line = Line::from(std::mem::take(&mut spans));
                new_line.style = line.style;
                out.push(new_line);
                current_width = 0;
            }
            let text = if ch == '\t' {
                "    ".to_string()
            } else {
                ch.to_string()
            };
            spans.push(Span::styled(text, span.style));
            current_width += ch_width;
        }
    }
    let mut new_line = Line::from(spans);
    new_line.style = line.style;
    out.push(new_line);
    out
}

pub fn wrap_lines_with_cursor_and_highlight(
    lines: &[Line<'static>],
    width: usize,
    cursor: Option<(usize, usize)>,
    highlight_line: Option<usize>,
) -> (Vec<Line<'static>>, Option<(usize, usize)>, Option<usize>) {
    if width == 0 {
        return (lines.to_vec(), cursor, highlight_line);
    }

    let mut out = Vec::new();
    let mut cursor_out = None;
    let mut highlight_out = None;
    let (cursor_line, cursor_col) = cursor.unwrap_or((usize::MAX, 0));

    for (idx, line) in lines.iter().enumerate() {
        let start = out.len();
        let segments = wrap_line(line, width);
        out.extend(segments.iter().cloned());

        if Some(idx) == highlight_line && highlight_out.is_none() {
            highlight_out = Some(start);
        }

        if idx == cursor_line {
            let width_line = line_width(line);
            if width_line == 0 {
                cursor_out = Some((start, 0));
            } else {
                let target_col = cursor_col.min(width_line.saturating_sub(1));
                let mut wrapped_offset = target_col / width;
                if wrapped_offset >= segments.len() {
                    wrapped_offset = segments.len().saturating_sub(1);
                }
                let wrapped_col = target_col % width;
                cursor_out = Some((start + wrapped_offset, wrapped_col));
            }
        }
    }

    (out, cursor_out, highlight_out)
}

fn is_list_snippet(item: &Value) -> bool {
    matches!(extract_type(item), Some("listSnippet"))
}

fn render_edit_text_lines(
    text: &str,
    _links: &mut Vec<crate::render::LinkTarget>,
    out: &mut Vec<Line<'static>>,
) {
    let mut has_line = false;
    for raw_line in normalize_text(text).lines() {
        has_line = true;
        let line = sanitize_for_terminal(raw_line);
        if let Some((level, _)) = parse_heading(&line) {
            out.push(Line::from(Span::styled(line, heading_style(level))));
            continue;
        }
        if let Some(stripped) = line.strip_prefix("> ") {
            out.push(Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::DarkGray)),
                Span::styled(stripped.to_string(), Style::default().fg(Color::Gray)),
            ]));
            continue;
        }
        out.push(parse_inline_annotations(&line));
    }
    if !has_line {
        out.push(Line::raw(""));
    }
    out.push(Line::raw(""));
}

pub fn highlight_lines(lines: &mut [Line<'static>]) {
    let style = Style::default().bg(Color::White).fg(Color::Black);
    for line in lines {
        line.style = style;
    }
}

pub fn highlight_readonly_lines(lines: &mut [Line<'static>]) {
    let style = Style::default().bg(Color::Rgb(255, 250, 205));
    for line in lines {
        line.style = line.style.patch(style);
    }
}

fn line_width(line: &Line<'static>) -> usize {
    line.spans
        .iter()
        .map(|span| span.content.chars().count())
        .sum()
}

pub fn apply_cursor_marker(lines: &mut [Line<'static>], line_idx: usize, col: usize) {
    if line_idx >= lines.len() {
        return;
    }
    let style = Style::default()
        .bg(Color::Yellow)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);
    let line = &mut lines[line_idx];
    let width = line_width(line);
    if width == 0 {
        line.spans = vec![Span::styled(" ".to_string(), style)];
        return;
    }
    let mut remaining = col.min(width.saturating_sub(1));
    let mut spans = Vec::new();
    let mut placed = false;

    for span in line.spans.iter() {
        let content = span.content.to_string();
        let len = content.chars().count();
        if placed {
            spans.push(span.clone());
            continue;
        }
        if remaining >= len {
            remaining = remaining.saturating_sub(len);
            spans.push(span.clone());
            continue;
        }

        let mut iter = content.chars();
        let left = iter.by_ref().take(remaining).collect::<String>();
        let rest = iter.collect::<String>();
        let mut rest_iter = rest.chars();
        let cursor_char = rest_iter
            .next()
            .map(|c| c.to_string())
            .unwrap_or_else(|| " ".to_string());
        let right = rest_iter.collect::<String>();

        if !left.is_empty() {
            spans.push(Span::styled(left, span.style));
        }
        spans.push(Span::styled(cursor_char, style));
        if !right.is_empty() {
            spans.push(Span::styled(right, span.style));
        }
        placed = true;
    }

    if !placed {
        spans.push(Span::styled(" ".to_string(), style));
    }

    line.spans = spans;
}

pub fn render_edit_page(
    edit: &EditState,
    plugins: &mut PluginManager,
) -> (Vec<Line<'static>>, Option<(usize, usize)>, Option<usize>) {
    let mut lines = Vec::new();
    let mut links = Vec::new();
    let mut cursor = None;
    let mut highlight_start = None;
    let highlight_path = edit.current().map(|s| s.path.clone());

    let mut ctx = RenderCtx {
        out: &mut lines,
        links: &mut links,
        plugins,
        cursor: &mut cursor,
        highlight_start: &mut highlight_start,
    };

    if let Some(items) = edit
        .raw
        .get("children")
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
    {
        for (idx, item) in items.iter().enumerate() {
            let mut path = vec![idx];
            render_raw_item_with_highlight(
                item,
                &mut path,
                highlight_path.as_deref(),
                edit,
                &mut ctx,
            );
        }
    }

    (lines, cursor, highlight_start)
}

struct RenderCtx<'a> {
    out: &'a mut Vec<Line<'static>>,
    links: &'a mut Vec<crate::render::LinkTarget>,
    plugins: &'a mut PluginManager,
    cursor: &'a mut Option<(usize, usize)>,
    highlight_start: &'a mut Option<usize>,
}

fn render_raw_item_with_highlight(
    item: &Value,
    path: &mut Vec<usize>,
    highlight_path: Option<&[usize]>,
    edit: &EditState,
    ctx: &mut RenderCtx<'_>,
) {
    let is_highlight = highlight_path
        .map(|highlight| highlight == path.as_slice())
        .unwrap_or(false);
    let start = ctx.out.len();
    let mut cursor_offset = 0usize;
    let typ = item
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<missing-type>");
    let (editable, _) = editable_field(typ, item);

    if is_highlight && ctx.highlight_start.is_none() {
        *ctx.highlight_start = Some(start);
    }

    if is_highlight && edit.current().map(|s| s.editable).unwrap_or(false) {
        let current = edit.current();
        let typ = current
            .map(|s| s.typ.as_str())
            .or_else(|| extract_type(item))
            .unwrap_or("");
        if typ == "textSnippet" {
            render_edit_text_lines(&edit.buffer, ctx.links, ctx.out);
        } else if is_code_snippet(typ) {
            let mut patched = item.clone();
            if let Some(field) = current.and_then(|s| s.field.as_ref())
                && let Some(obj) = patched.as_object_mut()
            {
                obj.insert(field.clone(), Value::String(edit.buffer.clone()));
            }
            match parse_node_from_raw(&patched) {
                Node::Code { language, code } => {
                    render_code_block(language.as_deref(), &code, ctx.out);
                    cursor_offset = 1;
                }
                node => {
                    render_node(&node, ctx.out, ctx.links, ctx.plugins);
                }
            }
        } else {
            let node = parse_node_from_raw(item);
            render_node(&node, ctx.out, ctx.links, ctx.plugins);
        }
    } else {
        let node = parse_node_from_raw(item);
        render_node(&node, ctx.out, ctx.links, ctx.plugins);
    }

    let end = ctx.out.len();
    if is_highlight {
        if editable {
            highlight_lines(&mut ctx.out[start..end]);
        } else {
            highlight_readonly_lines(&mut ctx.out[start..end]);
        }
        if ctx.cursor.is_none()
            && let Some(current) = edit.current()
            && current.editable
        {
            let (cursor_line, cursor_col) = cursor_line_col_display(&edit.buffer, edit.cursor);
            let buffer_lines = normalize_text(&edit.buffer).lines().count().max(1);
            let rel_line = cursor_line.min(buffer_lines.saturating_sub(1));
            let mut abs_line = start + cursor_offset + rel_line;
            if abs_line >= end {
                abs_line = end.saturating_sub(1);
            }
            let width = line_width(&ctx.out[abs_line]);
            let col = cursor_col.min(width);
            *ctx.cursor = Some((abs_line, col));
        }
    }
    if !is_highlight && !editable {
        highlight_readonly_lines(&mut ctx.out[start..end]);
    }

    if is_list_snippet(item) {
        return;
    }
    if let Some(children) = item
        .get("children")
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
    {
        for (idx, child) in children.iter().enumerate() {
            path.push(idx);
            render_raw_item_with_highlight(child, path, highlight_path, edit, ctx);
            path.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn cursor_tracks_multiline_text_snippet() {
        let raw = json!({
            "children": {
                "items": [
                    {
                        "__type": "textSnippet",
                        "string": "#Behavior\rA button widget is composed of multiple view models"
                    }
                ]
            }
        });
        let mut snippets = Vec::new();
        collect_snippets(&raw, Vec::new(), &mut snippets);
        let buffer = snippets[0].text.clone();
        let mut edit = EditState::new(
            "page".into(),
            PathBuf::from("/tmp/page.json"),
            raw,
            snippets,
            buffer,
        );
        let newline_pos = edit.buffer.chars().take_while(|c| *c != '\n').count() + 1;
        edit.cursor = newline_pos;

        let (line, col) = cursor_line_col_display(&edit.buffer, edit.cursor);
        assert_eq!((line, col), (1, 0));

        let (lines, cursor, _) = render_edit_page(&edit, &mut PluginManager::empty());
        assert_eq!(cursor, Some((1, 0)));
        assert!(lines.len() >= 2);
    }

    // ── insert_char_at ──────────────────────────────────────────────

    #[test]
    fn insert_char_at_inserts_and_advances_the_cursor() {
        let cases = [
            ("hello", 2, 'X', "heXllo", 3),
            ("abc", 0, 'Z', "Zabc", 1),
            ("abc", 3, '!', "abc!", 4),
            ("café", 4, '☕', "café☕", 5),
            ("αβγ", 1, 'δ', "αδβγ", 2),
            // the editor's line break: a typed CR is stored as LF.
            ("ab", 1, '\r', "a\nb", 2),
            ("", 0, 'x', "x", 1),
        ];
        for (start, at, ch, expected, cursor_after) in cases {
            let mut buf = start.to_string();
            let mut cursor = at;
            insert_char_at(&mut buf, &mut cursor, ch);
            assert_eq!(buf, expected, "{start:?} at {at} with {ch:?}");
            assert_eq!(cursor, cursor_after, "{start:?} at {at} with {ch:?}");
        }
    }

    // ── delete_char_before ──────────────────────────────────────────

    #[test]
    fn delete_char_before_removes_the_char_left_of_the_cursor() {
        let cases = [
            ("hello", 0, false, "hello", 0),
            ("hello", 3, true, "helo", 2),
            ("abc", 3, true, "ab", 2),
            ("café", 4, true, "caf", 3),
            ("x", 1, true, "", 0),
        ];
        for (start, at, deleted, expected, cursor_after) in cases {
            let mut buf = start.to_string();
            let mut cursor = at;
            assert_eq!(
                delete_char_before(&mut buf, &mut cursor),
                deleted,
                "{start:?} at {at}"
            );
            assert_eq!(buf, expected, "{start:?} at {at}");
            assert_eq!(cursor, cursor_after, "{start:?} at {at}");
        }
    }

    // ── delete_char_at ──────────────────────────────────────────────

    #[test]
    fn delete_char_at_removes_the_char_under_the_cursor() {
        let cases = [
            ("hello", 5, false, "hello", 5),
            ("hello", 0, true, "ello", 0),
            ("hello", 2, true, "helo", 2),
            ("α☕β", 1, true, "αβ", 1),
            ("", 0, false, "", 0),
        ];
        for (start, at, deleted, expected, cursor_after) in cases {
            let mut buf = start.to_string();
            let mut cursor = at;
            assert_eq!(
                delete_char_at(&mut buf, &mut cursor),
                deleted,
                "{start:?} at {at}"
            );
            assert_eq!(buf, expected, "{start:?} at {at}");
            assert_eq!(cursor, cursor_after, "{start:?} at {at}");
        }
    }

    // ── char_to_byte_idx ────────────────────────────────────────────

    #[test]
    fn char_to_byte_idx_ascii() {
        assert_eq!(char_to_byte_idx("hello", 0), 0);
        assert_eq!(char_to_byte_idx("hello", 2), 2);
        assert_eq!(char_to_byte_idx("hello", 5), 5); // past end
    }

    #[test]
    fn char_to_byte_idx_multibyte() {
        // 'é' is 2 bytes in UTF-8
        assert_eq!(char_to_byte_idx("café", 0), 0);
        assert_eq!(char_to_byte_idx("café", 3), 3); // 'é'
        assert_eq!(char_to_byte_idx("café", 4), 5); // past end
    }

    #[test]
    fn char_to_byte_idx_emoji() {
        // '☕' is 3 bytes
        let s = "a☕b";
        assert_eq!(char_to_byte_idx(s, 0), 0); // 'a'
        assert_eq!(char_to_byte_idx(s, 1), 1); // '☕'
        assert_eq!(char_to_byte_idx(s, 2), 4); // 'b'
    }

    // ── cursor_line_col_display ─────────────────────────────────────

    #[test]
    fn cursor_line_col_single_line() {
        assert_eq!(cursor_line_col_display("hello", 0), (0, 0));
        assert_eq!(cursor_line_col_display("hello", 3), (0, 3));
        assert_eq!(cursor_line_col_display("hello", 5), (0, 5)); // at end
    }

    #[test]
    fn cursor_line_col_multiline() {
        let text = "ab\ncd\nef";
        assert_eq!(cursor_line_col_display(text, 0), (0, 0)); // 'a'
        assert_eq!(cursor_line_col_display(text, 2), (0, 2)); // '\n'
        assert_eq!(cursor_line_col_display(text, 3), (1, 0)); // 'c'
        assert_eq!(cursor_line_col_display(text, 5), (1, 2)); // second '\n'
        assert_eq!(cursor_line_col_display(text, 6), (2, 0)); // 'e'
        assert_eq!(cursor_line_col_display(text, 8), (2, 2)); // at end
    }

    #[test]
    fn cursor_line_col_with_tab() {
        let text = "a\tb";
        assert_eq!(cursor_line_col_display(text, 0), (0, 0));
        assert_eq!(cursor_line_col_display(text, 1), (0, 1)); // '\t'
        assert_eq!(cursor_line_col_display(text, 2), (0, 5)); // 'b' (tab=4)
    }

    #[test]
    fn cursor_line_col_cr_treated_as_newline() {
        let text = "a\rb";
        assert_eq!(cursor_line_col_display(text, 0), (0, 0));
        assert_eq!(cursor_line_col_display(text, 1), (0, 1)); // '\r'
        assert_eq!(cursor_line_col_display(text, 2), (1, 0)); // 'b'
    }

    // ── display_col_to_char_idx ─────────────────────────────────────

    #[test]
    fn display_col_to_char_idx_basic() {
        assert_eq!(display_col_to_char_idx("hello", 0), 0);
        assert_eq!(display_col_to_char_idx("hello", 3), 3);
        assert_eq!(display_col_to_char_idx("hello", 10), 5); // past end
    }

    #[test]
    fn display_col_to_char_idx_with_tab() {
        let line = "a\tb";
        assert_eq!(display_col_to_char_idx(line, 0), 0); // 'a'
        assert_eq!(display_col_to_char_idx(line, 1), 1); // '\t' starts
        assert_eq!(display_col_to_char_idx(line, 4), 1); // still within tab
        assert_eq!(display_col_to_char_idx(line, 5), 2); // 'b'
    }

    #[test]
    fn display_col_to_char_idx_empty() {
        assert_eq!(display_col_to_char_idx("", 0), 0);
        assert_eq!(display_col_to_char_idx("", 5), 0);
    }

    // ── move_cursor_vertical ────────────────────────────────────────

    fn make_edit(text: &str) -> EditState {
        let raw = json!({
            "children": {
                "items": [
                    {
                        "__type": "textSnippet",
                        "string": text
                    }
                ]
            }
        });
        let mut snippets = Vec::new();
        collect_snippets(&raw, Vec::new(), &mut snippets);
        let buffer = snippets[0].text.clone();
        EditState::new(
            "p".into(),
            PathBuf::from("/tmp/t.json"),
            raw,
            snippets,
            buffer,
        )
    }

    #[test]
    fn move_cursor_vertical_keeps_the_column_within_bounds() {
        let cases = [
            // 'b' on line 0 down to 'e'.
            ("abc\ndef\nghi", 1, 1, 5),
            // 'e' on line 1 up to 'b'.
            ("abc\ndef\nghi", 5, -1, 1),
            ("abc\ndef", 1, -1, 1),
            ("abc\ndef", 5, 1, 5),
            // col 5 clamps to the end of the two-char line, (1, 2).
            ("abcdef\nxy", 5, 1, 9),
            ("hello", 2, 1, 2),
        ];
        for (text, start, delta, expected) in cases {
            let mut edit = make_edit(text);
            edit.cursor = start;
            move_cursor_vertical(&mut edit, delta);
            assert_eq!(edit.cursor, expected, "{text:?} from {start} by {delta}");
        }
    }

    // ── collect_snippets ────────────────────────────────────────────

    #[test]
    fn collect_snippets_flat() {
        let raw = json!({
            "children": {
                "items": [
                    {"__type": "textSnippet", "string": "hello"},
                    {"__type": "pictureSnippet", "url": "img.png"}
                ]
            }
        });
        let mut out = Vec::new();
        collect_snippets(&raw, Vec::new(), &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].typ, "textSnippet");
        assert!(out[0].editable);
        assert_eq!(out[0].text, "hello");
        assert_eq!(out[0].path, vec![0]);
        assert_eq!(out[1].typ, "pictureSnippet");
        assert!(!out[1].editable);
        assert_eq!(out[1].path, vec![1]);
    }

    #[test]
    fn collect_snippets_nested() {
        let raw = json!({
            "children": {
                "items": [
                    {
                        "__type": "textSnippet",
                        "string": "parent",
                        "children": {
                            "items": [
                                {"__type": "pharoSnippet", "code": "1 + 2"}
                            ]
                        }
                    }
                ]
            }
        });
        let mut out = Vec::new();
        collect_snippets(&raw, Vec::new(), &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].path, vec![0]);
        assert_eq!(out[0].text, "parent");
        assert_eq!(out[1].path, vec![0, 0]);
        assert_eq!(out[1].typ, "pharoSnippet");
        assert!(out[1].editable);
        assert_eq!(out[1].text, "1 + 2");
    }

    #[test]
    fn collect_snippets_empty() {
        let raw = json!({"children": {"items": []}});
        let mut out = Vec::new();
        collect_snippets(&raw, Vec::new(), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn collect_snippets_no_children_key() {
        let raw = json!({"other": "data"});
        let mut out = Vec::new();
        collect_snippets(&raw, Vec::new(), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn collect_snippets_normalizes_cr() {
        let raw = json!({
            "children": {
                "items": [
                    {"__type": "textSnippet", "string": "line1\rline2"}
                ]
            }
        });
        let mut out = Vec::new();
        collect_snippets(&raw, Vec::new(), &mut out);
        assert_eq!(out[0].text, "line1\nline2");
    }

    // ── editable_field ──────────────────────────────────────────────

    #[test]
    fn editable_field_picks_the_documented_field() {
        let cases = [
            (
                json!({"__type": "textSnippet", "string": "hi"}),
                true,
                Some("string"),
            ),
            (
                json!({"__type": "textSnippet", "text": "hi"}),
                true,
                Some("text"),
            ),
            (
                json!({"__type": "pharoSnippet", "code": "1+2"}),
                true,
                Some("code"),
            ),
            (
                json!({"__type": "pythonSnippet", "source": "print(1)"}),
                true,
                Some("source"),
            ),
            (json!({"__type": "pictureSnippet"}), false, None),
        ];
        for (item, editable, field) in cases {
            let typ = item["__type"].as_str().unwrap();
            assert_eq!(
                editable_field(typ, &item),
                (editable, field.map(str::to_string)),
                "{item}"
            );
        }
    }

    // ── undo/redo push/pop ──────────────────────────────────────────

    #[test]
    fn undo_on_empty_stack_returns_false() {
        let mut edit = make_edit("hello");
        assert!(!edit.undo());
    }

    #[test]
    fn push_undo_caps_at_200() {
        let mut edit = make_edit("initial");
        for i in 0..210 {
            edit.buffer = format!("v{i}");
            edit.cursor = edit.buffer.chars().count();
            edit.push_undo_snapshot();
        }
        let entry = edit.current().unwrap();
        assert_eq!(entry.undo.len(), 200);
        assert_eq!(entry.undo[0].text, "v10");
        assert_eq!(entry.undo[199].text, "v209");
    }

    #[test]
    fn push_undo_on_non_editable_is_noop() {
        let raw = json!({
            "children": {
                "items": [
                    {"__type": "pictureSnippet", "url": "img.png"}
                ]
            }
        });
        let mut snippets = Vec::new();
        collect_snippets(&raw, Vec::new(), &mut snippets);
        let mut edit = EditState::new(
            "p".into(),
            PathBuf::from("/tmp/t.json"),
            raw,
            snippets,
            String::new(),
        );
        edit.push_undo_snapshot();
        let entry = edit.current().unwrap();
        assert!(entry.undo.is_empty());
    }

    #[test]
    fn undo_clamps_cursor_to_buffer_length() {
        let mut edit = make_edit("hi");
        edit.cursor = 2;
        edit.push_undo_snapshot();
        // Manually set cursor beyond buffer length in the snapshot
        edit.current_mut().unwrap().undo.back_mut().unwrap().cursor = 100;
        edit.buffer = "longer text".to_string();
        edit.commit_buffer();
        let undone = edit.undo();
        assert!(undone);
        // cursor is clamped to chars().count() of "hi" = 2
        assert_eq!(edit.buffer, "hi");
        assert_eq!(edit.cursor, 2);
    }

    // ── ensure_scroll ───────────────────────────────────────────────

    #[test]
    fn ensure_scroll_keeps_the_cursor_line_in_view() {
        // (cursor_line, scroll, view_height) -> scroll
        let cases = [
            ((5, 3, 0), 3),
            ((5, 3, 10), 3),
            ((2, 5, 10), 2),
            ((15, 3, 10), 6),
            ((3, 3, 10), 3),
            ((12, 3, 10), 3),
            ((13, 3, 10), 4),
            ((5, 5, 1), 5),
            ((6, 5, 1), 6),
            ((4, 5, 1), 4),
        ];
        for ((line, scroll, height), expected) in cases {
            assert_eq!(
                ensure_scroll(line, scroll, height),
                expected,
                "line {line}, scroll {scroll}, height {height}"
            );
        }
    }

    // ── snippet_at_path_mut ─────────────────────────────────────────

    #[test]
    fn snippet_at_path_mut_finds_nested() {
        let mut raw = json!({
            "children": {
                "items": [
                    {
                        "__type": "textSnippet",
                        "string": "top",
                        "children": {
                            "items": [
                                {"__type": "textSnippet", "string": "nested"}
                            ]
                        }
                    }
                ]
            }
        });
        let item = snippet_at_path_mut(&mut raw, &[0, 0]).unwrap();
        assert_eq!(item.get("string").unwrap().as_str().unwrap(), "nested");
    }

    #[test]
    fn snippet_at_path_mut_invalid_returns_none() {
        let mut raw = json!({"children": {"items": []}});
        assert!(snippet_at_path_mut(&mut raw, &[99]).is_none());
    }

    // ── commit_buffer ───────────────────────────────────────────────

    #[test]
    fn commit_buffer_updates_raw_json() {
        let mut edit = make_edit("original");
        edit.buffer = "updated".to_string();
        edit.commit_buffer();
        assert!(edit.dirty);
        let snippet_text = edit
            .raw
            .get("children")
            .unwrap()
            .get("items")
            .unwrap()
            .get(0)
            .unwrap()
            .get("string")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(snippet_text, "updated");
    }

    #[test]
    fn commit_buffer_noop_when_unchanged() {
        let mut edit = make_edit("same");
        edit.commit_buffer();
        assert!(!edit.dirty);
    }

    // ── set_buffer ──────────────────────────────────────────────────

    #[test]
    fn set_buffer_normalizes_and_sets_cursor() {
        let mut edit = make_edit("old");
        edit.set_buffer("new\rtext".to_string());
        assert_eq!(edit.buffer, "new\ntext");
        assert_eq!(edit.cursor, 8); // chars count
        assert!(edit.snapshot_pending);
    }

    // ── is_editable ─────────────────────────────────────────────────

    #[test]
    fn is_editable_true_for_text() {
        let edit = make_edit("hi");
        assert!(edit.is_editable());
    }

    #[test]
    fn is_editable_false_for_picture() {
        let raw = json!({
            "children": {
                "items": [
                    {"__type": "pictureSnippet", "url": "img.png"}
                ]
            }
        });
        let mut snippets = Vec::new();
        collect_snippets(&raw, Vec::new(), &mut snippets);
        let edit = EditState::new(
            "p".into(),
            PathBuf::from("/tmp/t.json"),
            raw,
            snippets,
            String::new(),
        );
        assert!(!edit.is_editable());
    }

    // ── append_text_snippet ────────────────────────────────────────

    #[test]
    fn append_text_snippet_adds_an_editable_text_snippet_to_raw_json() {
        let mut edit = make_edit("hello");
        edit.append_text_snippet();
        let items = edit
            .raw
            .get("children")
            .unwrap()
            .get("items")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[1].get("__type").unwrap().as_str().unwrap(),
            "textSnippet"
        );
        assert_eq!(items[1].get("string").unwrap().as_str().unwrap(), "");
        let entry = edit.current().unwrap();
        assert!(entry.editable);
        assert_eq!(entry.typ, "textSnippet");
        assert_eq!(entry.field, Some("string".to_string()));
    }

    #[test]
    fn append_text_snippet_to_empty_page() {
        let raw = json!({"children": {"items": []}});
        let snippets = Vec::new();
        let mut edit = EditState::new(
            "p".into(),
            PathBuf::from("/tmp/t.json"),
            raw,
            snippets,
            String::new(),
        );
        edit.append_text_snippet();
        assert_eq!(edit.snippets.len(), 1);
        assert_eq!(edit.selected, 0);
        assert!(edit.dirty);
    }
}
