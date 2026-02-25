use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use lepiter_core::{KnowledgeBase, KnowledgeBaseIndex, Node, Page, PageId};
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
}

#[derive(Debug, Clone)]
struct LinkTarget {
    label: String,
    target: String,
}

#[derive(Debug, Clone)]
struct RenderedPage {
    id: PageId,
    title: String,
    lines: Vec<Line<'static>>,
    links: Vec<LinkTarget>,
}

struct App {
    index: KnowledgeBaseIndex,
    visible_ids: Vec<PageId>,
    selected: usize,
    opened: Option<PageId>,
    rendered_cache: HashMap<PageId, RenderedPage>,
    page_scroll: usize,
    selected_link: usize,
    search: String,
    history: Vec<PageId>,
    mode: Mode,
    status: String,
}

impl App {
    fn new(index: KnowledgeBaseIndex) -> Self {
        let mut app = Self {
            index,
            visible_ids: Vec::new(),
            selected: 0,
            opened: None,
            rendered_cache: HashMap::new(),
            page_scroll: 0,
            selected_link: 0,
            search: String::new(),
            history: Vec::new(),
            mode: Mode::List,
            status: String::new(),
        };
        app.rebuild_visible_ids();
        app
    }

    fn rebuild_visible_ids(&mut self) {
        let needle = self.search.to_lowercase();
        let mut metas = self.index.sorted_pages_by_title();
        if !needle.is_empty() {
            metas.retain(|m| {
                m.title.to_lowercase().contains(&needle)
                    || m.id.to_lowercase().contains(&needle)
                    || m.tags.iter().any(|t| t.to_lowercase().contains(&needle))
            });
        }
        self.visible_ids = metas.into_iter().map(|m| m.id.clone()).collect();
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
    }

    fn open_page(&mut self, id: &str, from_link: bool) {
        if !self.rendered_cache.contains_key(id) {
            match self.index.load_page(id) {
                Ok(page) => {
                    let rendered = render_page(&page);
                    self.rendered_cache.insert(id.to_string(), rendered);
                }
                Err(err) => {
                    self.status = format!("failed to load page: {err:#}");
                    return;
                }
            }
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

        if let Some(target_id) = self.resolve_internal_target(&link.target) {
            self.open_page(&target_id, true);
        } else {
            self.status = format!("external link not supported in TUI: {}", link.target);
        }
    }

    fn resolve_internal_target(&self, raw: &str) -> Option<PageId> {
        let target = raw.trim();

        if self.index.pages.contains_key(target) {
            return Some(target.to_string());
        }

        if let Some(rest) = target.strip_prefix("page:") {
            let id = rest.trim();
            if self.index.pages.contains_key(id) {
                return Some(id.to_string());
            }
        }
        if let Some(rest) = target.strip_prefix("title:") {
            return self.resolve_page_by_title(rest.trim());
        }

        if let Some(uuid) = extract_uuid_like(target)
            && self.index.pages.contains_key(uuid)
        {
            return Some(uuid.to_string());
        }

        self.resolve_page_by_title(target)
    }

    fn resolve_page_by_title(&self, title: &str) -> Option<PageId> {
        let target = title.trim().to_lowercase();
        self.index
            .pages
            .values()
            .find(|m| m.title.to_lowercase() == target)
            .map(|m| m.id.clone())
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
        "lepiter-cli <subcommand|kb-path> [args]\n\nsubcommands:\n  tui [kb-path]    launch the terminal reader (default path: ./lepiter)\n  info [kb-path]   print knowledge base metadata summary\n\nIf the first argument is a directory path, `info` mode is used implicitly."
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

fn run_app(terminal: &mut DefaultTerminal, mut app: App) -> Result<()> {
    loop {
        terminal.draw(|f| ui(f, &app))?;

        if !event::poll(Duration::from_millis(100))? {
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
                Mode::Search => {}
            },
            KeyCode::Up | KeyCode::Char('k') => match app.mode {
                Mode::List => app.move_selection(-1),
                Mode::Page => app.scroll_page(-1),
                Mode::Search => {}
            },
            KeyCode::Down | KeyCode::Char('j') => match app.mode {
                Mode::List => app.move_selection(1),
                Mode::Page => app.scroll_page(1),
                Mode::Search => {}
            },
            KeyCode::Char('g') => match app.mode {
                Mode::Page => app.page_scroll = 0,
                Mode::List | Mode::Search => {}
            },
            KeyCode::Char('G') => match app.mode {
                Mode::Page => app.page_scroll = usize::MAX / 2,
                Mode::List | Mode::Search => {}
            },
            KeyCode::Char('b') => {
                if app.mode == Mode::Page {
                    app.back_to_list();
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
    }

    Ok(())
}

fn ui(frame: &mut Frame, app: &App) {
    match app.mode {
        Mode::List | Mode::Search => render_list_view(frame, app),
        Mode::Page => render_page_view(frame, app),
    }
}

fn render_list_view(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
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
        "matches: {} | j/k or up/down move | enter open | / search | q quit",
        app.visible_ids.len()
    );
    if !app.status.is_empty() {
        status.push_str(" | ");
        status.push_str(&app.status);
    }
    frame.render_widget(
        Paragraph::new(status).style(Style::default().fg(Color::Gray)),
        chunks[2],
    );
}

fn render_page_view(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(2),
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
    frame.render_widget(Paragraph::new(footer), chunks[2]);
}

fn render_page(page: &Page) -> RenderedPage {
    let mut lines = Vec::new();
    let mut links = Vec::new();

    for node in &page.content {
        render_node(node, &mut lines, &mut links);
    }

    RenderedPage {
        id: page.id.clone(),
        title: page.title.clone(),
        lines,
        links,
    }
}

fn render_node(node: &Node, out: &mut Vec<Line<'static>>, links: &mut Vec<LinkTarget>) {
    match node {
        Node::Heading { level, text } => {
            let style = match *level {
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
            out.push(Line::from(Span::styled(
                format!(
                    "{} {}",
                    "#".repeat((*level).max(1) as usize),
                    sanitize_for_terminal(text)
                ),
                style,
            )));
            out.push(Line::raw(""));
        }
        Node::Paragraph { text } | Node::Text { text } => {
            out.push(parse_inline_markdown(&sanitize_for_terminal(text), links));
            out.push(Line::raw(""));
        }
        Node::Quote { text } => {
            out.push(Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    sanitize_for_terminal(text),
                    Style::default()
                        .fg(Color::Gray)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
            out.push(Line::raw(""));
        }
        Node::Code { language, code } => {
            let title = language.clone().unwrap_or_else(|| "code".to_string());
            out.push(Line::from(Span::styled(
                format!("```{title}"),
                Style::default().fg(Color::DarkGray),
            )));
            for line in normalize_text(code).lines() {
                out.push(highlight_code_line(
                    &sanitize_for_terminal(line),
                    language.as_deref(),
                ));
            }
            out.push(Line::from(Span::styled(
                "```".to_string(),
                Style::default().fg(Color::DarkGray),
            )));
            out.push(Line::raw(""));
        }
        Node::List { items } => {
            for item in items {
                let mut rendered = Vec::new();
                for n in item {
                    render_node(n, &mut rendered, links);
                }
                if let Some(first) = rendered.first() {
                    let mut spans = vec![Span::styled(
                        "- ".to_string(),
                        Style::default().fg(Color::DarkGray),
                    )];
                    spans.extend(first.spans.iter().cloned());
                    out.push(Line::from(spans));
                } else {
                    out.push(Line::from(Span::raw("-")));
                }
            }
            out.push(Line::raw(""));
        }
        Node::Link { text, url } => {
            links.push(LinkTarget {
                label: sanitize_for_terminal(text),
                target: sanitize_for_terminal(url),
            });
            let idx = links.len();
            out.push(Line::from(vec![
                Span::styled(
                    format!("[{idx}] "),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    sanitize_for_terminal(text),
                    Style::default()
                        .fg(Color::LightBlue)
                        .add_modifier(Modifier::UNDERLINED),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("({})", sanitize_for_terminal(url)),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            out.push(Line::raw(""));
        }
        Node::Unknown { typ, .. } => {
            out.push(Line::from(Span::styled(
                format!("[[unknown: {}]]", sanitize_for_terminal(typ)),
                Style::default().fg(Color::Yellow),
            )));
            out.push(Line::raw(""));
        }
    }
}

fn parse_inline_markdown(text: &str, links: &mut Vec<LinkTarget>) -> Line<'static> {
    let mut spans = Vec::new();
    let chars = text.chars().collect::<Vec<_>>();
    let mut i = 0usize;
    let mut buf = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut code = false;

    let push_buf =
        |spans: &mut Vec<Span<'static>>, buf: &mut String, bold: bool, italic: bool, code: bool| {
            if buf.is_empty() {
                return;
            }
            let mut style = Style::default();
            if bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if code {
                style = style.fg(Color::Yellow).bg(Color::Black);
            }
            spans.push(Span::styled(std::mem::take(buf), style));
        };

    while i < chars.len() {
        if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '*' {
            push_buf(&mut spans, &mut buf, bold, italic, code);
            bold = !bold;
            i += 2;
            continue;
        }
        if chars[i] == '*' {
            push_buf(&mut spans, &mut buf, bold, italic, code);
            italic = !italic;
            i += 1;
            continue;
        }
        if chars[i] == '`' {
            push_buf(&mut spans, &mut buf, bold, italic, code);
            code = !code;
            i += 1;
            continue;
        }
        if chars[i] == '[' {
            if i + 1 < chars.len() && chars[i + 1] == '[' {
                let mut j = i + 2;
                while j + 1 < chars.len() {
                    if chars[j] == ']' && chars[j + 1] == ']' {
                        break;
                    }
                    j += 1;
                }
                if j + 1 < chars.len() && chars[j] == ']' && chars[j + 1] == ']' {
                    push_buf(&mut spans, &mut buf, bold, italic, code);
                    let link_text = chars[i + 2..j].iter().collect::<String>();
                    links.push(LinkTarget {
                        label: link_text.clone(),
                        target: link_text.clone(),
                    });
                    let idx = links.len();
                    spans.push(Span::styled(
                        link_text,
                        Style::default()
                            .fg(Color::LightBlue)
                            .add_modifier(Modifier::UNDERLINED),
                    ));
                    spans.push(Span::styled(
                        format!("[{idx}]"),
                        Style::default().fg(Color::Yellow),
                    ));
                    i = j + 2;
                    continue;
                }
            }

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
                    push_buf(&mut spans, &mut buf, bold, italic, code);
                    let link_text = chars[i + 1..j].iter().collect::<String>();
                    let link_target = chars[j + 2..k].iter().collect::<String>();
                    links.push(LinkTarget {
                        label: link_text.clone(),
                        target: link_target.clone(),
                    });
                    let idx = links.len();
                    spans.push(Span::styled(
                        link_text,
                        Style::default()
                            .fg(Color::LightBlue)
                            .add_modifier(Modifier::UNDERLINED),
                    ));
                    spans.push(Span::styled(
                        format!("[{idx}]"),
                        Style::default().fg(Color::Yellow),
                    ));
                    i = k + 1;
                    continue;
                }
            }
        }
        buf.push(chars[i]);
        i += 1;
    }

    push_buf(&mut spans, &mut buf, bold, italic, code);
    Line::from(spans)
}

fn highlight_code_line(line: &str, language: Option<&str>) -> Line<'static> {
    let keywords: &[&str] = match language.unwrap_or_default() {
        "python" => &[
            "def", "class", "if", "else", "elif", "for", "while", "return", "import", "from", "as",
            "try", "except", "with", "lambda",
        ],
        "javascript" => &[
            "function", "const", "let", "var", "if", "else", "for", "while", "return", "class",
            "import", "from", "export", "new", "async", "await",
        ],
        "pharo" | "smalltalk" => &["self", "super", "true", "false", "nil", "^"],
        "shell" | "shellcommand" | "bash" => &["if", "then", "fi", "for", "do", "done", "echo"],
        _ => &[],
    };

    let mut spans = Vec::new();
    let mut i = 0usize;
    let chars = line.chars().collect::<Vec<_>>();
    while i < chars.len() {
        let c = chars[i];

        if (language == Some("python") || language == Some("shell") || language == Some("bash"))
            && c == '#'
        {
            let rest = chars[i..].iter().collect::<String>();
            spans.push(Span::styled(rest, Style::default().fg(Color::DarkGray)));
            break;
        }
        if language == Some("javascript") && i + 1 < chars.len() && c == '/' && chars[i + 1] == '/'
        {
            let rest = chars[i..].iter().collect::<String>();
            spans.push(Span::styled(rest, Style::default().fg(Color::DarkGray)));
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
            spans.push(Span::styled(s, Style::default().fg(Color::Green)));
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let s = chars[start..i].iter().collect::<String>();
            spans.push(Span::styled(s, Style::default().fg(Color::Yellow)));
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
                spans.push(Span::styled(
                    word,
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::raw(word));
            }
            continue;
        }

        spans.push(Span::raw(c.to_string()));
        i += 1;
    }

    Line::from(spans)
}

fn highlight_search_match(text: &str, needle: &str) -> Line<'static> {
    if needle.is_empty() {
        return Line::from(Span::raw(text.to_string()));
    }
    let lower = text.to_lowercase();
    let needle_lower = needle.to_lowercase();
    if let Some(idx) = lower.find(&needle_lower) {
        let end = idx + needle.len().min(text.len().saturating_sub(idx));
        let before = text.get(..idx).unwrap_or("");
        let mid = text.get(idx..end).unwrap_or("");
        let after = text.get(end..).unwrap_or("");
        Line::from(vec![
            Span::raw(before.to_string()),
            Span::styled(
                mid.to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(after.to_string()),
        ])
    } else {
        Line::from(Span::raw(text.to_string()))
    }
}

fn normalize_text(input: &str) -> String {
    input.replace("\r\n", "\n").replace('\r', "\n")
}

fn sanitize_for_terminal(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\t' => out.push_str("    "),
            '\u{001b}' => {}
            c if c.is_control() && c != '\n' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn highlight_selected_link_markers(
    lines: &[Line<'static>],
    selected_idx: usize,
) -> Vec<Line<'static>> {
    let marker = format!("[{selected_idx}]");
    let marker_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let mut spans = Vec::new();
        for span in &line.spans {
            let text = span.content.as_ref();
            let mut rest = text;
            while let Some(pos) = rest.find(&marker) {
                if pos > 0 {
                    spans.push(Span::styled(rest[..pos].to_string(), span.style));
                }
                spans.push(Span::styled(marker.clone(), marker_style));
                rest = &rest[pos + marker.len()..];
            }
            if !rest.is_empty() {
                spans.push(Span::styled(rest.to_string(), span.style));
            }
        }
        out.push(Line::from(spans));
    }
    out
}

fn extract_uuid_like(input: &str) -> Option<&str> {
    let bytes = input.as_bytes();
    if bytes.len() < 36 {
        return None;
    }

    for i in 0..=bytes.len() - 36 {
        let cand = &input[i..i + 36];
        let ok = cand.chars().enumerate().all(|(idx, c)| match idx {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        });
        if ok {
            return Some(cand);
        }
    }
    None
}
