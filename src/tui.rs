//! Command-driven TUI monitor for Rpte engine.
//!
//! Use `:login <id>` to bind an account, then all operations target it.
//!
//! Commands:
//!   - `pairs`              — list all trading pairs with real-time prices
//!   - `kline <Q> <B>`      — OHLC chart for a pair
//!   - `bal`                — token balances of the logged-in account
//!   - `orders`             — open orders of the logged-in account
//!   - `make <Q> <B> <vol> <price>` — place a limit order
//!   - `swap <Q> <B> <vol>`         — place a market order
//!   - `login <id>`         — bind to an account
//!   - `help`               — show command help

use std::collections::VecDeque;
use std::io::stdout;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui_plt::prelude::*;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::Rpte;

// ─── View modes ──────────────────────────────────────────────────────────────

#[derive(Clone)]
enum View {
    Pairs,
    Kline { src: usize, dst: usize },
    Balance,
    Orders,
    Help,
}

// ─── App state ───────────────────────────────────────────────────────────────

struct AppState {
    view: View,
    cmd_buf: String,
    cmd_cursor: usize,          // cursor position within cmd_buf
    cmd_mode: bool,             // true → typing a command
    msg: String,                // status / error message
    interval: u64,
    max_candles: usize,
    logged_in_account: Option<usize>, // bound account for make/swap/orders/bal
    cmd_history: Vec<String>,   // previously executed commands (newest last)
    history_idx: Option<usize>, // current position browsing history (None = new input)
    quit_requested: bool,       // set by :q command, checked in main loop
    /// Pairs info cached for rendering
    pairs_cache: Vec<(usize, usize, usize, Decimal)>,
    /// K-line data cached for rendering
    candle_cache: VecDeque<crate::pair::CandleData>,
    live_candle_cache: Option<crate::pair::CandleData>,
    current_price_cache: Option<(Decimal, usize, usize)>,
}

impl AppState {
    fn new(interval: u64, max_candles: usize) -> Self {
        Self {
            view: View::Pairs,
            cmd_buf: String::new(),
            cmd_cursor: 0,
            cmd_mode: false,
            msg: String::new(),
            interval,
            max_candles,
            logged_in_account: None,
            cmd_history: Vec::new(),
            history_idx: None,
            quit_requested: false,
            pairs_cache: Vec::new(),
            candle_cache: VecDeque::new(),
            live_candle_cache: None,
            current_price_cache: None,
        }
    }

    fn set_msg(&mut self, msg: impl Into<String>) {
        self.msg = msg.into();
    }

    /// Navigate backward in history (Up). Returns true if a history entry was loaded.
    fn history_back(&mut self) -> bool {
        if self.cmd_history.is_empty() {
            return false;
        }
        let idx = match self.history_idx {
            Some(i) => {
                if i == 0 {
                    return false; // already at oldest
                }
                i - 1
            }
            None => {
                // Save current buffer before navigating
                self.cmd_history.len() - 1
            }
        };
        self.history_idx = Some(idx);
        self.cmd_buf = self.cmd_history[idx].clone();
        self.cmd_cursor = self.cmd_buf.len();
        true
    }

    /// Navigate forward in history (Down). Returns true if a history entry was loaded.
    fn history_forward(&mut self) -> bool {
        match self.history_idx {
            Some(i) => {
                let next = i + 1;
                if next >= self.cmd_history.len() {
                    // Reached end of history → clear buffer
                    self.history_idx = None;
                    self.cmd_buf.clear();
                    self.cmd_cursor = 0;
                } else {
                    self.history_idx = Some(next);
                    self.cmd_buf = self.cmd_history[next].clone();
                    self.cmd_cursor = self.cmd_buf.len();
                }
                true
            }
            None => false,
        }
    }

    /// Push a command into history.
    fn push_history(&mut self, cmd: &str) {
        if cmd.is_empty() {
            return;
        }
        // Avoid consecutive duplicates
        if self.cmd_history.last().map(|s| s.as_str()) == Some(cmd) {
            return;
        }
        self.cmd_history.push(cmd.to_string());
        if self.cmd_history.len() > 50 {
            self.cmd_history.remove(0);
        }
    }
}

// ─── Public entry point ──────────────────────────────────────────────────────

pub fn run_tui<F>(
    engine: &mut Rpte,
    interval: u64,
    max_candles: usize,
    login_id: Option<usize>,
    mut frame_callback: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnMut(&mut Rpte),
{
    enable_raw_mode()?;
    let mut out = stdout();
    out.execute(EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut state = AppState::new(interval, max_candles);
    if let Some(id) = login_id {
        state.logged_in_account = Some(id);
        state.set_msg(format!("Logged in as account #{}  |  :help for commands", id));
    } else {
        state.set_msg("Type :help for commands");
    }

    'main: loop {
        if state.quit_requested {
            break 'main;
        }
        // Step engine
        frame_callback(engine);
        engine.step();
        refresh_caches(engine, &mut state);

        terminal.draw(|f| {
            let area = f.area();
            let (content_area, cmd_area) = layout(area);
            render_content(f, content_area, engine, &mut state);
            render_cmd_bar(f, cmd_area, &mut state);
        })?;

        // Input handling
        if event::poll(Duration::from_millis(10))? {
            if let Event::Key(key) = event::read()? {
                if state.cmd_mode {
                    handle_cmd_input(key, &mut state, engine, &mut frame_callback);
                } else {
                    match key.code {
                        KeyCode::Char(':') => {
                            state.cmd_mode = true;
                            state.cmd_buf.clear();
                            state.cmd_cursor = 0;
                        }
                        KeyCode::Up => {
                            state.cmd_mode = true;
                            state.history_back();
                        }
                        KeyCode::Down => {
                            state.cmd_mode = true;
                            state.history_forward();
                        }
                        KeyCode::Esc => break 'main,
                        _ => {}
                    }
                }
            }
        }

        // Process queued messages from engine (CloseOrder etc.)
        engine.step();
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

// ─── Cache refresh ───────────────────────────────────────────────────────────

fn refresh_caches(engine: &mut Rpte, state: &mut AppState) {
    state.pairs_cache = engine.get_all_pairs_info();

    if let View::Kline { src, dst } = state.view {
        state.candle_cache = engine
            .get_candle_data(src, dst, state.interval)
            .unwrap_or_default();
        while state.candle_cache.len() > state.max_candles {
            state.candle_cache.pop_front();
        }
        state.current_price_cache = engine.get_current_price(src, dst).ok();
        state.live_candle_cache = engine.latest_candle(src, dst, state.interval).ok().flatten();
    }
}

// ─── Layout ──────────────────────────────────────────────────────────────────

fn layout(area: Rect) -> (Rect, Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    (chunks[0], chunks[1])
}

// ─── Content rendering ───────────────────────────────────────────────────────

fn render_content(f: &mut ratatui::Frame, area: Rect, engine: &mut Rpte, state: &mut AppState) {
    match &state.view {
        View::Pairs => render_pairs(f, area, engine, state),
        View::Kline { src, dst } => render_kline(f, area, engine, state, *src, *dst),
        View::Balance => render_balance(f, area, engine, state),
        View::Orders => render_orders(f, area, engine, state),
        View::Help => render_help(f, area),
    }
}

// ─── Pairs list ──────────────────────────────────────────────────────────────

fn render_pairs(f: &mut ratatui::Frame, area: Rect, engine: &mut Rpte, state: &AppState) {
    let title = format!("Trading Pairs  ({})", state.pairs_cache.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);

    // Render block background
    f.render_widget(block, area);

    if state.pairs_cache.is_empty() {
        let para = Paragraph::new("No trading pairs yet. Place an order to create one.")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(para, inner);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    // Header
    lines.push(Line::from(vec![
        Span::styled("ID  ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled("Quote", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled("  ", Style::default()),
        Span::styled("Base", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled("        Price", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(Span::styled(
        "─".repeat(inner.width as usize),
        Style::default().fg(Color::DarkGray),
    )));

    for (pair_id, qt, bt, price) in &state.pairs_cache {
        let qname = engine.get_token_name(*qt).unwrap_or("?");
        let bname = engine.get_token_name(*bt).unwrap_or("?");
        let price_str = format!("{:.5}", price);
        lines.push(Line::from(vec![
            Span::styled(format!("{:<3} ", pair_id), Style::default().fg(Color::White)),
            Span::styled(format!("{:<5}", qname), Style::default().fg(Color::Green)),
            Span::styled("  ", Style::default()),
            Span::styled(format!("{:<5}", bname), Style::default().fg(Color::Green)),
            Span::styled(format!("  {:>12}", price_str), Style::default().fg(Color::Yellow)),
        ]));
    }

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
}

// ─── K-line chart ────────────────────────────────────────────────────────────

fn render_kline(
    f: &mut ratatui::Frame,
    area: Rect,
    engine: &mut Rpte,
    state: &AppState,
    src: usize,
    dst: usize,
) {
    let src_name = engine.get_token_name(src).unwrap_or("?");
    let dst_name = engine.get_token_name(dst).unwrap_or("?");

    // Build title
    let price_str = state
        .current_price_cache
        .map(|(p, _, _)| format!("{:.5}", p))
        .unwrap_or_else(|| "—".to_string());
    let title = match &state.live_candle_cache {
        Some(c) => format!(
            "{}:{}  {}  O:{:.5} H:{:.5} L:{:.5} C:{:.5}",
            src_name, dst_name, price_str, c.open, c.high, c.low, c.close,
        ),
        None => format!("{}:{}  {}", src_name, dst_name, price_str),
    };
    // Append key hint
    let title = format!("{}   [:] cmd  [q] quit", title);

    // Convert candle data
    let data: Vec<(f64, f64)> = state
        .candle_cache
        .iter()
        .map(|c| (c.step_count as f64, c.close.to_f64().unwrap_or(0.0)))
        .collect();
    let err_low: Vec<f64> = state
        .candle_cache
        .iter()
        .map(|c| c.close.to_f64().unwrap_or(0.0) - c.low.to_f64().unwrap_or(0.0))
        .collect();
    let err_high: Vec<f64> = state
        .candle_cache
        .iter()
        .map(|c| c.high.to_f64().unwrap_or(0.0) - c.close.to_f64().unwrap_or(0.0))
        .collect();

    let close_series = Series::new("")
        .data(data)
        .color(Color::Cyan)
        .y_err_asymmetric(err_low, err_high);

    let plot = LinePlot::new()
        .series(close_series)
        .title(&title)
        .show_legend(false)
        .x_axis(Axis::new().label(""))
        .y_axis(Axis::new().label(""));

    f.render_widget(&plot, area);
}

// ─── Balance view ────────────────────────────────────────────────────────────

fn render_balance(f: &mut ratatui::Frame, area: Rect, engine: &mut Rpte, state: &mut AppState) {
    let account_id = match state.logged_in_account {
        Some(id) => id,
        None => {
            let para = Paragraph::new("Not logged in. Use :login <account_id>")
                .style(Style::default().fg(Color::Red));
            f.render_widget(para, area);
            return;
        }
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("Account #{} — Balances", account_id))
        .style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let tokens = engine.get_all_tokens();
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("Token       Available     Equity       Locked", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(Span::styled(
        "─".repeat(inner.width as usize),
        Style::default().fg(Color::DarkGray),
    )));

    let mut has_balance = false;
    for &token in &tokens {
        let bal = engine.get_node_balance(account_id, token).unwrap_or(Decimal::ZERO);
        let equity = engine.get_account_equity_token(account_id, token).unwrap_or(Decimal::ZERO);
        if equity.is_zero() {
            continue;
        }
        has_balance = true;
        let name = engine.get_token_name(token).unwrap_or("?");
        let locked = equity - bal;
        lines.push(Line::from(vec![
            Span::styled(format!("{:<12}", name), Style::default().fg(Color::Green)),
            Span::styled(format!("{:<14.5}", bal), Style::default().fg(Color::White)),
            Span::styled(format!("{:<14.5}", equity), Style::default().fg(Color::Yellow)),
            Span::styled(format!("{:.5}", locked), Style::default().fg(Color::DarkGray)),
        ]));
    }

    if !has_balance {
        lines.push(Line::from(
            Span::styled("(no balances)", Style::default().fg(Color::DarkGray)),
        ));
    }

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
}

// ─── Orders view ─────────────────────────────────────────────────────────────

fn render_orders(f: &mut ratatui::Frame, area: Rect, engine: &mut Rpte, state: &mut AppState) {
    let account_id = match state.logged_in_account {
        Some(id) => id,
        None => {
            let para = Paragraph::new("Not logged in. Use :login <account_id>")
                .style(Style::default().fg(Color::Red));
            f.render_widget(para, area);
            return;
        }
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("Orders for Account #{}", account_id))
        .style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let order_ids: Vec<usize> = match engine.get_account_orders(account_id) {
        Ok(set) => set.iter().copied().collect(),
        Err(e) => {
            let para = Paragraph::new(format!("Error: {}", e))
                .style(Style::default().fg(Color::Red));
            f.render_widget(para, inner);
            return;
        }
    };

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("ID  Dir   Pair          Volume       Price       ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(Span::styled(
        "─".repeat(inner.width as usize),
        Style::default().fg(Color::DarkGray),
    )));

    if order_ids.is_empty() {
        lines.push(Line::from(
            Span::styled("(no open orders)", Style::default().fg(Color::DarkGray)),
        ));
    } else {
        for &oid in &order_ids {
            if let Ok(brief) = engine.get_order_brief(oid) {
                let dir_str = match brief.direction {
                    crate::node::Drt::Buy => "BUY ",
                    crate::node::Drt::Sell => "SELL",
                };
                let src_name = engine.get_token_name(brief.src_token).unwrap_or("?");
                let dst_name = engine.get_token_name(brief.dst_token).unwrap_or("?");
                let pair_str = format!("{}/{}", src_name, dst_name);

                let dir_color = match brief.direction {
                    crate::node::Drt::Buy => Color::Green,
                    crate::node::Drt::Sell => Color::Red,
                };

                lines.push(Line::from(vec![
                    Span::styled(format!("{:<3} ", brief.id), Style::default().fg(Color::White)),
                    Span::styled(format!("{:<5} ", dir_str), Style::default().fg(dir_color)),
                    Span::styled(format!("{:<13} ", pair_str), Style::default().fg(Color::Green)),
                    Span::styled(format!("{:<12.5} ", brief.src_volume), Style::default().fg(Color::Yellow)),
                    Span::styled(format!("{:<.5}", brief.price), Style::default().fg(Color::Yellow)),
                ]));
            }
        }
    }

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
}

// ─── Help view ───────────────────────────────────────────────────────────────

fn render_help(f: &mut ratatui::Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Help")
        .style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let help_lines = vec![
        Line::from(Span::styled("First bind an account, then operate:", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
        Line::from(Span::raw("")),
        Line::from(vec![
            Span::styled("  login <id>          ", Style::default().fg(Color::Green)),
            Span::styled("Bind to an account (required first)", Style::default().fg(Color::White)),
        ]),
        Line::from(Span::raw("")),
        Line::from(Span::styled("Views:", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
        Line::from(Span::raw("")),
        Line::from(vec![
            Span::styled("  pairs               ", Style::default().fg(Color::Green)),
            Span::styled("List all trading pairs", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  kline <Q> <B>       ", Style::default().fg(Color::Green)),
            Span::styled("Show K-line chart (or :k <pair_id>)", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  bal                 ", Style::default().fg(Color::Green)),
            Span::styled("Show balances of logged-in account", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  orders              ", Style::default().fg(Color::Green)),
            Span::styled("Show open orders of logged-in account", Style::default().fg(Color::White)),
        ]),
        Line::from(Span::raw("")),
        Line::from(Span::styled("Trading:", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
        Line::from(Span::raw("")),
        Line::from(vec![
            Span::styled("  make <Q> <B> <vol>  ", Style::default().fg(Color::Green)),
            Span::styled("<price>", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("                      ", Style::default()),
            Span::styled("Place a limit order for logged-in account", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  swap <Q> <B> <vol>  ", Style::default().fg(Color::Green)),
            Span::styled("Place a market order for logged-in account", Style::default().fg(Color::White)),
        ]),
        Line::from(Span::raw("")),
        Line::from(Span::styled("Key shortcuts:", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
        Line::from(Span::raw("  :        Enter command mode")),
        Line::from(Span::raw("  Enter    Execute command")),
        Line::from(Span::raw("  Esc      Cancel / exit command mode")),
        Line::from(Span::raw("  q        Quit")),
    ];

    let para = Paragraph::new(help_lines);
    f.render_widget(para, inner);
}

// ─── Command bar ─────────────────────────────────────────────────────────────

fn render_cmd_bar(f: &mut ratatui::Frame, area: Rect, state: &mut AppState) {
    let (left, mid, right) = {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(3), Constraint::Length(18), Constraint::Length(60)])
            .split(area);
        (chunks[0], chunks[1], chunks[2])
    };

    let prefix = if state.cmd_mode { ": " } else { "  " };
    let prompt = format!("{}{}", prefix, state.cmd_buf);

    let style = if state.cmd_mode {
        Style::default().fg(Color::Yellow).bg(Color::Black)
    } else {
        Style::default().fg(Color::DarkGray).bg(Color::Black)
    };

    let para = Paragraph::new(Line::from(vec![
        Span::styled(&prompt, style),
    ]));
    f.render_widget(para, left);

    // Logged-in account indicator
    let acct_str = match state.logged_in_account {
        Some(id) => format!(" acct #{} ", id),
        None => " no login ".to_string(),
    };
    let acct_color = if state.logged_in_account.is_some() {
        Color::Green
    } else {
        Color::DarkGray
    };
    let acct_para = Paragraph::new(Line::from(vec![
        Span::styled(&acct_str, Style::default().fg(acct_color).bg(Color::Black)),
    ]));
    f.render_widget(acct_para, mid);

    // Status message on the right
    let msg_para = Paragraph::new(Line::from(vec![
        Span::styled(&state.msg, Style::default().fg(Color::Green).bg(Color::Black)),
    ]));
    f.render_widget(msg_para, right);
}

// ─── Command input handling ──────────────────────────────────────────────────

fn handle_cmd_input<F: FnMut(&mut Rpte)>(
    key: event::KeyEvent,
    state: &mut AppState,
    engine: &mut Rpte,
    _frame_callback: &mut F,
) {
    match key.code {
        KeyCode::Enter => {
            let cmd = state.cmd_buf.trim().to_string();
            state.cmd_mode = false;
            if !cmd.is_empty() {
                state.push_history(&cmd);
                execute_cmd(&cmd, state, engine);
            }
            state.history_idx = None;
            state.cmd_buf.clear();
            state.cmd_cursor = 0;
        }
        KeyCode::Esc => {
            state.cmd_mode = false;
            state.cmd_buf.clear();
            state.cmd_cursor = 0;
            state.history_idx = None;
        }
        KeyCode::Up => {
            state.history_back();
        }
        KeyCode::Down => {
            state.history_forward();
        }
        KeyCode::Char(c) => {
            if c == ':' && state.cmd_buf.is_empty() && !state.cmd_mode {
                state.cmd_mode = true;
            } else {
                state.history_idx = None; // typing breaks history navigation
                state.cmd_buf.insert(state.cmd_cursor, c);
                state.cmd_cursor += 1;
            }
        }
        KeyCode::Backspace => {
            if state.cmd_cursor > 0 {
                state.cmd_cursor -= 1;
                state.cmd_buf.remove(state.cmd_cursor);
            }
        }
        KeyCode::Delete => {
            if state.cmd_cursor < state.cmd_buf.len() {
                state.cmd_buf.remove(state.cmd_cursor);
            }
        }
        KeyCode::Left => {
            if state.cmd_cursor > 0 {
                state.cmd_cursor -= 1;
            }
        }
        KeyCode::Right => {
            if state.cmd_cursor < state.cmd_buf.len() {
                state.cmd_cursor += 1;
            }
        }
        KeyCode::Home => state.cmd_cursor = 0,
        KeyCode::End => state.cmd_cursor = state.cmd_buf.len(),
        _ => {}
    }
}

// ─── Command execution ───────────────────────────────────────────────────────

fn execute_cmd(cmd: &str, state: &mut AppState, engine: &mut Rpte) {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return;
    }

    match parts[0] {
        "pairs" | "p" => {
            state.view = View::Pairs;
            state.set_msg("Switched to pairs list");
        }

        "kline" | "k" => {
            if parts.len() == 2 {
                // kline <pair_id>
                let pair_id: usize = match parts[1].parse() {
                    Ok(n) => n,
                    Err(_) => {
                        state.set_msg("Usage: kline <pair_id>  or  kline <quote_token> <base_token>");
                        return;
                    }
                };
                let src = match engine.get_pair_quote_token(pair_id) {
                    Ok(t) => t,
                    Err(_) => {
                        state.set_msg(format!("Pair #{} not found", pair_id));
                        return;
                    }
                };
                let dst = match engine.get_pair_base_token(pair_id) {
                    Ok(t) => t,
                    Err(_) => {
                        state.set_msg(format!("Pair #{} not found", pair_id));
                        return;
                    }
                };
                let src_name = engine.get_token_name(src).unwrap_or("?");
                let dst_name = engine.get_token_name(dst).unwrap_or("?");
                state.view = View::Kline { src, dst };
                state.set_msg(format!("Showing K-line: {}/{} (#{})", src_name, dst_name, pair_id));
            } else if parts.len() >= 3 {
                // kline <quote_token> <base_token>
                let q_name = parts[1];
                let b_name = parts[2];
                let src = match engine.get_token_by_name(q_name) {
                    Some(id) => id,
                    None => {
                        state.set_msg(format!("Token not found: {}", q_name));
                        return;
                    }
                };
                let dst = match engine.get_token_by_name(b_name) {
                    Some(id) => id,
                    None => {
                        state.set_msg(format!("Token not found: {}", b_name));
                        return;
                    }
                };
                state.view = View::Kline { src, dst };
                state.set_msg(format!("Showing K-line: {}/{}", q_name, b_name));
            } else {
                state.set_msg("Usage: kline <pair_id>  or  kline <quote_token> <base_token>");
            }
        }

        "login" | "l" => {
            if parts.len() < 2 {
                state.set_msg("Usage: login <account_id>");
                return;
            }
            let id: usize = match parts[1].parse() {
                Ok(n) => n,
                Err(_) => {
                    state.set_msg("Invalid account ID (must be a number)");
                    return;
                }
            };
            state.logged_in_account = Some(id);
            state.set_msg(format!("Logged in as account #{}", id));
        }

        "bal" | "b" => {
            if state.logged_in_account.is_none() {
                state.set_msg("Not logged in. Use :login <account_id> first");
                return;
            }
            state.view = View::Balance;
            state.set_msg("Showing balances");
        }

        "orders" | "o" => {
            if state.logged_in_account.is_none() {
                state.set_msg("Not logged in. Use :login <account_id> first");
                return;
            }
            state.view = View::Orders;
            state.set_msg("Showing orders");
        }

        "make" | "m" => {
            let account_id = match state.logged_in_account {
                Some(id) => id,
                None => {
                    state.set_msg("Not logged in. Use :login <account_id> first");
                    return;
                }
            };
            // make <src_token> <dst_token> <volume> <price>
            if parts.len() < 5 {
                state.set_msg("Usage: make <src_token> <dst_token> <volume> <price>");
                return;
            }
            let src_token = match engine.get_token_by_name(parts[1]) {
                Some(id) => id,
                None => {
                    state.set_msg(format!("Token not found: {}", parts[1]));
                    return;
                }
            };
            let dst_token = match engine.get_token_by_name(parts[2]) {
                Some(id) => id,
                None => {
                    state.set_msg(format!("Token not found: {}", parts[2]));
                    return;
                }
            };
            let volume: Decimal = match parts[3].parse() {
                Ok(n) => n,
                Err(_) => {
                    state.set_msg("Invalid volume");
                    return;
                }
            };
            let price: Decimal = match parts[4].parse() {
                Ok(n) => n,
                Err(_) => {
                    state.set_msg("Invalid price");
                    return;
                }
            };

            engine.make(account_id, src_token, dst_token, volume, price);
            state.set_msg(format!(
                "Limit order placed: account #{} {}→{} vol={} price={}",
                account_id, parts[1], parts[2], volume, price
            ));
        }

        "swap" | "s" => {
            let account_id = match state.logged_in_account {
                Some(id) => id,
                None => {
                    state.set_msg("Not logged in. Use :login <account_id> first");
                    return;
                }
            };
            // swap <src_token> <dst_token> <volume>
            if parts.len() < 4 {
                state.set_msg("Usage: swap <src_token> <dst_token> <volume>");
                return;
            }
            let src_token = match engine.get_token_by_name(parts[1]) {
                Some(id) => id,
                None => {
                    state.set_msg(format!("Token not found: {}", parts[1]));
                    return;
                }
            };
            let dst_token = match engine.get_token_by_name(parts[2]) {
                Some(id) => id,
                None => {
                    state.set_msg(format!("Token not found: {}", parts[2]));
                    return;
                }
            };
            let volume: Decimal = match parts[3].parse() {
                Ok(n) => n,
                Err(_) => {
                    state.set_msg("Invalid volume");
                    return;
                }
            };

            engine.swap(account_id, src_token, dst_token, volume);
            state.set_msg(format!(
                "Market order placed: account #{} {}→{} vol={}",
                account_id, parts[1], parts[2], volume
            ));
        }

        "q" | "quit" => {
            // Will be handled by the caller checking state.quit_requested
            state.quit_requested = true;
        }

        "help" | "h" | "?" => {
            state.view = View::Help;
            state.set_msg("");
        }

        _ => {
            state.set_msg(format!("Unknown command: {}. Type :help", parts[0]));
        }
    }
}
