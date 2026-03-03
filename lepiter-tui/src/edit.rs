//! edit-mode support: snippet collection, buffer edits, cursor positioning,
//! autosave, undo, and rendering helpers for the inline editor view.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use lepiter_core::{Node, PageId, parse_node_from_raw};

use crate::plugins::PluginManager;
use crate::render::{
    highlight_code_line, normalize_text, parse_inline_annotations, render_node,
    sanitize_for_terminal,
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
    pub undo: Vec<UndoEntry>,
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
            entry.undo.remove(0);
        }
        entry.undo.push(snapshot);
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
            let Some(prev) = entry.undo.pop() else {
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

    pub fn save_to_disk(&mut self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.raw)?;
        std::fs::write(&self.path, bytes)
            .with_context(|| format!("failed to save {}", self.path.display()))?;
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
            undo: Vec::new(),
        });

        collect_snippets(item, current_path, out);
    }
}

pub fn editable_field(typ: &str, item: &Value) -> (bool, Option<String>) {
    if typ == "textSnippet" {
        let field = pick_first_field(item, &["string", "text", "content"]).unwrap_or("string");
        return (true, Some(field.to_string()));
    }

    if matches!(
        typ,
        "pharoSnippet"
            | "pythonSnippet"
            | "javascriptSnippet"
            | "shellCommandSnippet"
            | "gemstoneSnippet"
    ) {
        let field = pick_first_field(item, &["code", "source"]).unwrap_or("code");
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
    item.get("string")
        .or_else(|| item.get("text"))
        .or_else(|| item.get("content"))
        .or_else(|| item.get("code"))
        .or_else(|| item.get("source"))
        .and_then(Value::as_str)
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
    let (line, col) = cursor_line_col_display(&edit.buffer, edit.cursor);
    let normalized = normalize_text(&edit.buffer);
    let lines = normalized.split('\n').collect::<Vec<_>>();
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

fn extract_type_raw(item: &Value) -> Option<&str> {
    item.get("type")
        .and_then(Value::as_str)
        .or_else(|| item.get("__type").and_then(Value::as_str))
}

fn is_list_snippet(item: &Value) -> bool {
    matches!(extract_type_raw(item), Some("listSnippet"))
}

fn is_code_snippet(typ: &str) -> bool {
    matches!(
        typ,
        "pharoSnippet"
            | "pythonSnippet"
            | "javascriptSnippet"
            | "shellCommandSnippet"
            | "gemstoneSnippet"
    )
}

fn parse_heading_line(input: &str) -> Option<u8> {
    let trimmed = input.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes == 0 {
        return None;
    }
    let rest = trimmed[hashes..].trim_start();
    if rest.is_empty() {
        return None;
    }
    Some(hashes.min(6) as u8)
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
        if let Some(level) = parse_heading_line(&line) {
            let style = match level {
                1 => Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                2 => Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                _ => Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            };
            out.push(Line::from(Span::styled(line, style)));
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

fn render_code_block(language: Option<&str>, code: &str, out: &mut Vec<Line<'static>>) {
    let title = language.unwrap_or("code");
    out.push(Line::from(Span::styled(
        format!("```{title}"),
        Style::default().fg(Color::DarkGray),
    )));
    for line in normalize_text(code).lines() {
        out.push(highlight_code_line(&sanitize_for_terminal(line), language));
    }
    out.push(Line::from(Span::styled(
        "```".to_string(),
        Style::default().fg(Color::DarkGray),
    )));
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
            .or_else(|| extract_type_raw(item))
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
        let cursor = cursor.expect("cursor should be set");
        assert_eq!(cursor.0, 1);
        assert!(lines.len() >= 2);
    }
}
