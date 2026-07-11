//! Terminal UI for `gh6 status tui`.
//!
//! Uses ratatui + crossterm to provide a live crawl-monitoring dashboard
//! with five sections:
//!
//! 1. Done — completed crawl events (table with FOLLOWING / FOLLOWERS / NEW)
//! 2. Queue — discovery events (table with VIA)
//! 3. Upcoming — queue preview (normal | hub | retry columns)
//! 4. Workers — currently crawling
//! 5. Stats — counters + API quota

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::Paragraph,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use unicode_width::UnicodeWidthStr;

use crate::display;
use gh6::types::{CrawlEvent, QueueItem, ServerResponse, StatusData};

/// Maximum number of event lines to keep per buffer.
const MAX_EVENTS: usize = 9999;
/// Poll interval for keyboard input (milliseconds).
const TICK_MS: u64 = 100;
/// Maximum rows for the Done panel.
const DONE_MAX_ROWS: usize = 15;

// ── Focus ────────────────────────────────────────────────────────────────

/// Which scrollable panel has keyboard focus.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Done,
    Queue,
}

// ── App state ────────────────────────────────────────────────────────────

/// Mutable TUI application state shared between the socket reader and the
/// render loop via `Arc<Mutex<App>>`.
pub struct App {
    /// Ring buffer of completed crawl events (UserDone), newest at the back.
    done_events: VecDeque<CrawlEvent>,
    /// Ring buffer of queued discovery events (UserQueued), newest at the back.
    queue_events: VecDeque<CrawlEvent>,
    /// Latest status snapshot (replaced on each `Ok` response).
    status: Option<StatusData>,
    /// Scroll offset for Done panel (0 = bottom, larger = scrolled up).
    done_scroll: usize,
    /// Scroll offset for Queue panel.
    queue_scroll: usize,
    /// Which panel receives keyboard input.
    focus: Panel,
    /// Set to true to signal the render loop to exit.
    quit: bool,
}

impl App {
    fn new() -> Self {
        Self {
            done_events: VecDeque::with_capacity(MAX_EVENTS),
            queue_events: VecDeque::with_capacity(MAX_EVENTS),
            status: None,
            done_scroll: 0,
            queue_scroll: 0,
            focus: Panel::Done,
            quit: false,
        }
    }

    fn push_done(&mut self, event: CrawlEvent) {
        if self.done_events.len() >= MAX_EVENTS {
            self.done_events.pop_front();
        }
        self.done_events.push_back(event);
    }

    fn push_queue(&mut self, event: CrawlEvent) {
        if self.queue_events.len() >= MAX_EVENTS {
            self.queue_events.pop_front();
        }
        self.queue_events.push_back(event);
    }

    fn update_status(&mut self, s: StatusData) {
        self.status = Some(s);
    }

    /// Handle a key press. Returns `true` if the TUI should exit.
    fn handle_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.quit = true;
                true
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Panel::Done => Panel::Queue,
                    Panel::Queue => Panel::Done,
                };
                false
            }
            KeyCode::Char('j') | KeyCode::Down => {
                match self.focus {
                    Panel::Done if self.done_scroll > 0 => self.done_scroll -= 1,
                    Panel::Queue if self.queue_scroll > 0 => self.queue_scroll -= 1,
                    _ => {}
                }
                false
            }
            KeyCode::Char('k') | KeyCode::Up => {
                match self.focus {
                    Panel::Done => {
                        let max = self.done_events.len().saturating_sub(1);
                        if self.done_scroll < max {
                            self.done_scroll += 1;
                        }
                    }
                    Panel::Queue => {
                        let max = self.queue_events.len().saturating_sub(1);
                        if self.queue_scroll < max {
                            self.queue_scroll += 1;
                        }
                    }
                }
                false
            }
            KeyCode::PageDown => {
                match self.focus {
                    Panel::Done => self.done_scroll = self.done_scroll.saturating_sub(10),
                    Panel::Queue => self.queue_scroll = self.queue_scroll.saturating_sub(10),
                }
                false
            }
            KeyCode::PageUp => {
                match self.focus {
                    Panel::Done => {
                        let max = self.done_events.len().saturating_sub(1);
                        self.done_scroll = (self.done_scroll + 10).min(max);
                    }
                    Panel::Queue => {
                        let max = self.queue_events.len().saturating_sub(1);
                        self.queue_scroll = (self.queue_scroll + 10).min(max);
                    }
                }
                false
            }
            KeyCode::Char('g') => {
                match self.focus {
                    Panel::Done => {
                        self.done_scroll = self.done_events.len().saturating_sub(1);
                    }
                    Panel::Queue => {
                        self.queue_scroll = self.queue_events.len().saturating_sub(1);
                    }
                }
                false
            }
            KeyCode::Char('G') => {
                match self.focus {
                    Panel::Done => self.done_scroll = 0,
                    Panel::Queue => self.queue_scroll = 0,
                }
                false
            }
            _ => false,
        }
    }
}

// ── Entry point ──────────────────────────────────────────────────────────

/// Connect to the daemon via Unix socket and run the TUI loop.
///
/// Spawns a background task to read socket messages, then enters the
/// ratatui render loop on the calling thread.  Returns when the user
/// presses `q` / `Esc`, the daemon sends `Bye`, or the socket closes.
pub async fn run(socket_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    // ── Connect & send watch command ──
    let mut stream = UnixStream::connect(&socket_path)
        .await
        .map_err(|_| "gh6d daemon is not running.")?;

    let cmd = serde_json::json!({"cmd": "status", "watch": true});
    let mut line = serde_json::to_string(&cmd)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;

    let (reader, _writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);

    let app = Arc::new(Mutex::new(App::new()));

    // ── Spawn socket reader ──
    let app_reader = Arc::clone(&app);
    tokio::spawn(async move {
        let mut buffer = String::new();
        loop {
            buffer.clear();
            match buf_reader.read_line(&mut buffer).await {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = buffer.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Ok(resp) = serde_json::from_str::<ServerResponse>(trimmed) {
                        let mut app = app_reader.lock().unwrap();
                        handle_response(&mut app, resp);
                        if app.quit {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        let mut app = app_reader.lock().unwrap();
        app.quit = true;
    });

    // ── Run TUI ──
    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, &app);
    ratatui::restore();

    result
}

// ── Socket message dispatch ──────────────────────────────────────────────

fn handle_response(app: &mut App, resp: ServerResponse) {
    match resp {
        ServerResponse::Ok { data: Some(data) } => {
            if let Ok(s) = serde_json::from_value::<StatusData>(data) {
                app.update_status(s);
            }
        }
        ServerResponse::Event { data } => match &data {
            CrawlEvent::UserDone { .. } => {
                app.push_done(data);
                if app.done_scroll > 0 {
                    app.done_scroll += 1;
                    let max = app.done_events.len().saturating_sub(1);
                    if app.done_scroll > max {
                        app.done_scroll = max;
                    }
                }
            }
            CrawlEvent::UserQueued { .. } => {
                app.push_queue(data);
                if app.queue_scroll > 0 {
                    app.queue_scroll += 1;
                    let max = app.queue_events.len().saturating_sub(1);
                    if app.queue_scroll > max {
                        app.queue_scroll = max;
                    }
                }
            }
        },
        ServerResponse::Bye => {
            app.quit = true;
        }
        _ => {}
    }
}

// ── Render loop ──────────────────────────────────────────────────────────

fn run_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    app: &Arc<Mutex<App>>,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let quit;
        {
            let app = app.lock().unwrap();
            terminal.draw(|f| render(f, &app))?;
            quit = app.quit;
        }
        if quit {
            return Ok(());
        }

        if event::poll(Duration::from_millis(TICK_MS))?
            && let Event::Key(key) = event::read()?
        {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                return Ok(());
            }
            let mut app = app.lock().unwrap();
            if app.handle_key(key.code) {
                return Ok(());
            }
        }
    }
}

// ── Layout ───────────────────────────────────────────────────────────────

fn render(f: &mut Frame, app: &App) {
    let area = f.area();
    let total_h = area.height as usize;

    // Fixed regions: upcoming(6) + workers(1) + stats(1) = 8
    let fixed = 8usize;
    let flex = total_h.saturating_sub(fixed);

    // Done gets at most DONE_MAX_ROWS (header + data), queue gets the rest.
    let done_rows = (app.done_events.len() + 1).min(DONE_MAX_ROWS);
    let done_h = done_rows.min(flex.saturating_sub(1));
    let queue_h = flex.saturating_sub(done_h);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(done_h as u16),
            Constraint::Length(queue_h.max(1) as u16),
            Constraint::Length(6),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    // Compute shared DEG / LOGIN column widths across both tables.
    let (deg_w, login_w) = shared_col_widths(app);

    render_done(f, layout[0], app, deg_w, login_w);
    render_queue(f, layout[1], app, deg_w, login_w);
    render_upcoming(f, layout[2], app);
    render_workers(f, layout[3], app);
    render_stats(f, layout[4], app);
}

// ── Shared column widths ─────────────────────────────────────────────────

/// Compute the maximum DEG and LOGIN display widths across all visible
/// Done and Queue events, so columns align in both panels.
fn shared_col_widths(app: &App) -> (usize, usize) {
    let mut max_deg = 2; // minimum "N°"
    let mut max_login = 0;

    for event in app.done_events.iter().chain(app.queue_events.iter()) {
        let (degree, login) = match event {
            CrawlEvent::UserDone { login, degree, .. }
            | CrawlEvent::UserQueued { login, degree, .. } => (*degree, login.as_str()),
        };
        let deg_s = format!("{degree}°");
        max_deg = max_deg.max(UnicodeWidthStr::width(deg_s.as_str()));
        max_login = max_login.max(UnicodeWidthStr::width(login));
    }
    (max_deg, max_login)
}

// ── Done panel ───────────────────────────────────────────────────────────

fn render_done(f: &mut Frame, area: Rect, app: &App, deg_w: usize, login_w: usize) {
    if area.height == 0 {
        return;
    }
    let term_w = area.width as usize;

    let mut lines: Vec<Line<'_>> = Vec::with_capacity(area.height as usize);

    // Header
    lines.push(done_header(deg_w, login_w, term_w));

    // Data rows (bottom-aligned in the remaining space)
    let data_h = area.height.saturating_sub(1) as usize;
    let total = app.done_events.len();
    let end = total.saturating_sub(app.done_scroll);
    let start = end.saturating_sub(data_h);
    let count = end - start;
    let top_pad = data_h.saturating_sub(count);
    for _ in 0..top_pad {
        lines.push(Line::from(""));
    }
    for event in app.done_events.iter().skip(start).take(count) {
        if let CrawlEvent::UserDone {
            login,
            degree,
            new_connections,
            following_count,
            followers_count,
        } = event
        {
            lines.push(format_done_line(&DoneFmt {
                degree: *degree,
                login,
                new_connections: *new_connections,
                following_count: *following_count,
                followers_count: *followers_count,
                deg_w,
                login_w,
                term_w,
            }));
        }
    }

    // Focus indicator
    let style = if app.focus == Panel::Done {
        Style::new().bold()
    } else {
        Style::new()
    };

    f.render_widget(Paragraph::new(lines).style(style), area);
}

fn done_header(deg_w: usize, login_w: usize, term_w: usize) -> Line<'static> {
    let left = format!(
        "  {}  {}",
        pad_right("DEG", deg_w),
        pad_right("LOGIN", login_w)
    );
    let left_w = 2 + deg_w + 2 + login_w;
    let fw = 9usize;
    let new_w = 3usize; // "NEW"
    let right_w = fw + 2 + fw + 2 + new_w;
    let gap = term_w.saturating_sub(left_w + right_w).max(1);

    let line_str = format!(
        "{left}{}  {:>fw$}  {:>fw$}  {:>new_w$}",
        " ".repeat(gap),
        "FOLLOWING",
        "FOLLOWERS",
        "NEW",
        fw = fw,
        new_w = new_w,
    );
    Line::from(line_str.dim().bold())
}

/// Layout parameters for formatting a done-line row.
struct DoneFmt<'a> {
    degree: i32,
    login: &'a str,
    new_connections: usize,
    following_count: i64,
    followers_count: i64,
    deg_w: usize,
    login_w: usize,
    term_w: usize,
}

fn format_done_line(cfg: &DoneFmt<'_>) -> Line<'static> {
    let deg_s = pad_left(&format!("{}°", cfg.degree), cfg.deg_w);
    let login_s = pad_right(cfg.login, cfg.login_w);
    let left_w = 2 + cfg.deg_w + 2 + cfg.login_w;

    let following_s = display::num(cfg.following_count as u64);
    let followers_s = display::num(cfg.followers_count as u64);
    let new_s = format!("+{}", cfg.new_connections);

    let fw = 9usize;
    let new_w = UnicodeWidthStr::width(new_s.as_str());
    let right_w = fw + 2 + fw + 2 + new_w;
    let gap = cfg.term_w.saturating_sub(left_w + right_w).max(1);

    Line::from(vec![
        Span::raw("  "),
        Span::styled(deg_s, Style::new().cyan()),
        Span::raw("  "),
        Span::styled(login_s, Style::new().blue()),
        Span::raw(" ".repeat(gap)),
        Span::styled(format!("{:>fw$}", following_s, fw = fw), Style::new().dim()),
        Span::raw("  "),
        Span::styled(format!("{:>fw$}", followers_s, fw = fw), Style::new().dim()),
        Span::raw("  "),
        Span::styled(new_s, Style::new().green()),
    ])
}

// ── Queue panel ──────────────────────────────────────────────────────────

fn render_queue(f: &mut Frame, area: Rect, app: &App, deg_w: usize, login_w: usize) {
    if area.height == 0 {
        return;
    }
    let term_w = area.width as usize;

    let mut lines: Vec<Line<'_>> = Vec::with_capacity(area.height as usize);

    // Header
    lines.push(queue_header(deg_w, login_w, term_w));

    // Data rows
    let data_h = area.height.saturating_sub(1) as usize;
    let total = app.queue_events.len();
    let end = total.saturating_sub(app.queue_scroll);
    let start = end.saturating_sub(data_h);
    let count = end - start;
    let top_pad = data_h.saturating_sub(count);
    for _ in 0..top_pad {
        lines.push(Line::from(""));
    }
    for event in app.queue_events.iter().skip(start).take(count) {
        if let CrawlEvent::UserQueued {
            login,
            degree,
            parent_login,
        } = event
        {
            lines.push(format_queue_line(
                *degree,
                login,
                parent_login,
                deg_w,
                login_w,
                term_w,
            ));
        }
    }

    let style = if app.focus == Panel::Queue {
        Style::new().bold()
    } else {
        Style::new()
    };

    f.render_widget(Paragraph::new(lines).style(style), area);
}

fn queue_header(deg_w: usize, login_w: usize, term_w: usize) -> Line<'static> {
    let left = format!(
        "  {}  {}",
        pad_right("DEG", deg_w),
        pad_right("LOGIN", login_w)
    );
    let left_w = 2 + deg_w + 2 + login_w;
    let via_w = 3usize;
    let right_w = via_w;
    let gap = term_w.saturating_sub(left_w + right_w).max(1);

    let line_str = format!("{left}{}VIA", " ".repeat(gap),);
    Line::from(line_str.dim().bold())
}

fn format_queue_line(
    degree: i32,
    login: &str,
    parent_login: &str,
    deg_w: usize,
    login_w: usize,
    term_w: usize,
) -> Line<'static> {
    let deg_s = pad_left(&format!("{}°", degree), deg_w);
    let login_s = pad_right(login, login_w);
    let left_w = 2 + deg_w + 2 + login_w;

    let via_s = format!("via {parent_login}");
    let via_w = UnicodeWidthStr::width(via_s.as_str());
    let gap = term_w.saturating_sub(left_w + via_w).max(1);

    Line::from(vec![
        Span::raw("  "),
        Span::styled(deg_s, Style::new().cyan()),
        Span::raw("  "),
        Span::styled(login_s, Style::new().blue()),
        Span::raw(" ".repeat(gap)),
        Span::styled(via_s, Style::new().dim()),
    ])
}

// ── Upcoming panel ───────────────────────────────────────────────────────

fn render_upcoming(f: &mut Frame, area: Rect, app: &App) {
    let status = match &app.status {
        Some(s) => s,
        None => {
            f.render_widget(Paragraph::new("connecting...".dim()), area);
            return;
        }
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    // Title row
    let title = Line::from(vec![
        "  Upcoming  ".dim(),
        Span::styled(
            format!("normal:{}", status.pending_normal_count),
            Style::new().green(),
        ),
        "  ".dim(),
        Span::styled(
            format!("hub:{}", status.pending_hub_count),
            Style::new().yellow(),
        ),
        "  ".dim(),
        Span::styled(
            format!("retry:{}", status.pending_retry_count),
            Style::new().red(),
        ),
    ]);
    f.render_widget(Paragraph::new(title), chunks[0]);

    // Three-column body: normal | hub | retry
    let body_area = chunks[1];
    let col_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
        ])
        .split(body_area);

    render_upcoming_col(f, col_chunks[0], &status.pending_normal, "normal");
    render_upcoming_col(f, col_chunks[1], &status.pending_hub, "hub");
    render_upcoming_col(f, col_chunks[2], &status.pending_retry, "retry");
}

fn render_upcoming_col(f: &mut Frame, area: Rect, items: &[QueueItem], _label: &str) {
    let visible = area.height as usize;
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(visible);

    for item in items.iter().take(visible) {
        let text = if area.width >= 12 {
            format!("{} ({}°)", item.login, item.degree)
        } else {
            display::truncate_str(&item.login, area.width as usize)
        };

        let style = match item.priority.as_str() {
            "normal" => Style::new(),
            "low" => Style::new().yellow(),
            "retry" => Style::new().red(),
            _ => Style::new(),
        };

        lines.push(Line::from(Span::styled(text, style)));
    }

    // Fill remaining with "—"
    let remaining = visible.saturating_sub(items.len());
    for _ in 0..remaining {
        lines.push(Line::from("—".dim()));
    }

    f.render_widget(Paragraph::new(lines), area);
}

// ── Workers line ─────────────────────────────────────────────────────────

fn render_workers(f: &mut Frame, area: Rect, app: &App) {
    let status = match &app.status {
        Some(s) => s,
        None => {
            f.render_widget(Paragraph::new("connecting...".dim()), area);
            return;
        }
    };

    if status.paused {
        let line = Line::from(vec![
            "⏸ ".yellow(),
            "paused — run ".dim(),
            "gh6 run".bold(),
            " to resume".dim(),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }

    if status.currently_crawling.is_empty() {
        f.render_widget(Paragraph::new("idle".dim()), area);
        return;
    }

    let term_width = area.width as usize;
    let max_show = 5;
    let total = status.currently_crawling.len();
    let overflow = total.saturating_sub(max_show);

    let mut text = String::from("crawling ");
    for (i, w) in status.currently_crawling.iter().take(max_show).enumerate() {
        if i > 0 {
            text.push_str("  ");
        }
        text.push_str(&w.login);
        text.push_str(&format!(" ({}°)", w.degree));
    }
    if overflow > 0 {
        text.push_str(&format!("  +{overflow} more"));
    }

    if UnicodeWidthStr::width(text.as_str()) <= term_width {
        let mut spans: Vec<Span<'_>> = vec![Span::styled("crawling ", Style::new().dim())];
        for (i, w) in status.currently_crawling.iter().take(max_show).enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(w.login.clone(), Style::new().blue()));
            spans.push(Span::styled(
                format!(" ({}°)", w.degree),
                Style::new().cyan(),
            ));
        }
        if overflow > 0 {
            spans.push(Span::styled(
                format!("  +{overflow} more"),
                Style::new().dim(),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    } else {
        let logins: Vec<String> = status
            .currently_crawling
            .iter()
            .take(max_show)
            .map(|w| format!("{} ({}°)", w.login, w.degree))
            .collect();
        let mut compact = format!("crawling {}", logins.join(", "));
        if overflow > 0 {
            compact.push_str(&format!(", +{overflow} more"));
        }
        if UnicodeWidthStr::width(compact.as_str()) > term_width {
            compact = display::truncate_str(&compact, term_width);
        }
        f.render_widget(Paragraph::new(compact.dim()), area);
    }
}

// ── Stats line ───────────────────────────────────────────────────────────

fn render_stats(f: &mut Frame, area: Rect, app: &App) {
    let status = match &app.status {
        Some(s) => s,
        None => return,
    };

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // ── Left half: crawl counters ──
    let left = Line::from(vec![
        "crawled ".dim(),
        Span::styled(display::num(status.users_crawled), Style::new().green()),
        "  queue ".dim(),
        Span::styled(display::num(status.users_queued), Style::new().dim()),
        "  retry ".dim(),
        Span::styled(display::num(status.users_retry), Style::new().yellow()),
        "  error ".dim(),
        Span::styled(display::num(status.users_error), Style::new().red()),
    ]);
    f.render_widget(Paragraph::new(left), chunks[0]);

    // ── Right half: uptime + API ──
    let api_str = format!(
        "{}/{}",
        display::num(status.api_remaining as u64),
        display::num(status.api_limit as u64)
    );
    let api_style = if status.api_remaining >= 1000 {
        Style::new().green()
    } else if status.api_remaining >= 100 {
        Style::new().yellow()
    } else {
        Style::new().red()
    };

    let right = Line::from(vec![
        "up ".dim(),
        Span::styled(display::fmt_uptime(status.uptime_secs), Style::new().dim()),
        "  API ".dim(),
        Span::styled(api_str, api_style),
    ]);
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), chunks[1]);
}

// ── Padding helpers ──────────────────────────────────────────────────────

/// Right-pad a string to `width` display columns.
fn pad_right(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - w))
    }
}

/// Left-pad a string to `width` display columns.
fn pad_left(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{}{}", " ".repeat(width - w), s)
    }
}
