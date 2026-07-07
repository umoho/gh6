//! Terminal UI for `gh6 status tui`.
//!
//! Uses ratatui + crossterm to provide a live crawl-monitoring dashboard.
//! Replaces the old `gh6 status --watch --progress` ANSI escape-code approach.

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

use gh6::display;
use gh6::types::{CrawlEvent, ServerResponse, StatusData};

/// Maximum number of event lines to keep in memory.
const MAX_EVENTS: usize = 9999;
/// Poll interval for keyboard input (milliseconds).
const TICK_MS: u64 = 100;

// ── App state ────────────────────────────────────────────────────────────

/// Mutable TUI application state shared between the socket reader and the
/// render loop via `Arc<Mutex<App>>`.
pub struct App {
    /// Ring buffer of crawl events, newest at the back.
    events: VecDeque<CrawlEvent>,
    /// Latest status snapshot (replaced on each `Ok` response).
    status: Option<StatusData>,
    /// How many lines we have scrolled back from the bottom (0 = at bottom).
    scroll: usize,
    /// Set to true to signal the render loop to exit.
    quit: bool,
}

impl App {
    fn new() -> Self {
        Self {
            events: VecDeque::with_capacity(MAX_EVENTS),
            status: None,
            scroll: 0,
            quit: false,
        }
    }

    /// Push a new event, evicting the oldest if at capacity.
    fn push_event(&mut self, event: CrawlEvent) {
        if self.events.len() >= MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(event);
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
            KeyCode::Char('j') | KeyCode::Down => {
                if self.scroll > 0 {
                    self.scroll -= 1;
                }
                false
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let max = self.events.len().saturating_sub(1);
                if self.scroll < max {
                    self.scroll += 1;
                }
                false
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(10);
                false
            }
            KeyCode::PageUp => {
                let max = self.events.len().saturating_sub(1);
                self.scroll = (self.scroll + 10).min(max);
                false
            }
            KeyCode::Char('g') => {
                self.scroll = self.events.len().saturating_sub(1);
                false
            }
            KeyCode::Char('G') => {
                self.scroll = 0;
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
        // Socket closed or errored — signal quit.
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
        ServerResponse::Event { data } => {
            app.push_event(data);
            // When viewing history, maintain relative scroll position.
            if app.scroll > 0 {
                app.scroll += 1;
                // Cap at buffer length.
                let max = app.events.len().saturating_sub(1);
                if app.scroll > max {
                    app.scroll = max;
                }
            }
        }
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
        // Draw — release the lock before polling for input.
        let quit;
        {
            let app = app.lock().unwrap();
            terminal.draw(|f| render(f, &app))?;
            quit = app.quit;
        }
        if quit {
            return Ok(());
        }

        // Wait for keyboard input with a short timeout so we redraw
        // regularly even when nothing happens.
        if event::poll(Duration::from_millis(TICK_MS))?
            && let Event::Key(key) = event::read()?
        {
            // Ctrl+C → quit.
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
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // event log — takes remaining space
            Constraint::Length(1), // workers line
            Constraint::Length(1), // stats line
        ])
        .split(f.area());

    render_events(f, layout[0], app);
    render_workers(f, layout[1], app);
    render_stats(f, layout[2], app);
}

// ── Event log ────────────────────────────────────────────────────────────

fn render_events(f: &mut Frame, area: Rect, app: &App) {
    let visible = area.height as usize;
    if visible == 0 {
        return;
    }

    let total = app.events.len();
    let end = total.saturating_sub(app.scroll);
    let start = end.saturating_sub(visible);
    let count = end - start;

    let width = area.width as usize;

    let mut lines: Vec<Line<'_>> = Vec::with_capacity(visible);

    // Pad top so events are bottom-aligned when there are fewer than
    // the visible area (matching the natural scroll-up feel).
    let top_pad = visible.saturating_sub(count);
    for _ in 0..top_pad {
        lines.push(Line::from(""));
    }

    for event in app.events.iter().skip(start).take(count) {
        lines.push(format_event_line(event, width));
    }

    f.render_widget(Paragraph::new(lines), area);
}

/// Build one coloured event line with left (degree + login) and
/// right (status) parts, padded with spaces to fill `term_width`.
fn format_event_line(event: &CrawlEvent, term_width: usize) -> Line<'static> {
    match event {
        CrawlEvent::UserDone {
            login,
            degree,
            new_connections,
        } => {
            let left = format!("[{}°] {}", degree, login);
            let left_w = UnicodeWidthStr::width(left.as_str());
            let right = format!("done  +{} connections", new_connections);
            let right_w = UnicodeWidthStr::width(right.as_str());
            let pad = term_width.saturating_sub(left_w + right_w).max(1);

            Line::from(vec![
                Span::styled(format!("[{}°]", degree), Style::new().cyan()),
                Span::raw(" "),
                Span::styled(login.clone(), Style::new().blue()),
                Span::raw(" ".repeat(pad)),
                Span::styled(right, Style::new().green()),
            ])
        }
        CrawlEvent::UserQueued { login, degree } => {
            let left = format!("[{}°] {}", degree, login);
            let left_w = UnicodeWidthStr::width(left.as_str());
            let right = "queued";
            let right_w = UnicodeWidthStr::width(right);
            let pad = term_width.saturating_sub(left_w + right_w).max(1);

            Line::from(vec![
                Span::styled(format!("[{}°]", degree), Style::new().cyan()),
                Span::raw(" "),
                Span::styled(login.clone(), Style::new().blue()),
                Span::raw(" ".repeat(pad)),
                Span::styled(right.to_string(), Style::new().dim()),
            ])
        }
    }
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

    let cc = status.currently_crawling.as_deref().unwrap_or("-");
    let deg = status.current_degree;
    let line = Line::from(vec![
        "crawling ".dim(),
        Span::styled(cc.to_string(), Style::new().blue()),
        Span::styled(format!(" ({deg}°)"), Style::new().cyan()),
    ]);
    f.render_widget(Paragraph::new(line), area);
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
