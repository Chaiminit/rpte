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

/// 按引擎精度格式化 Decimal（右对齐，定宽 + 动态小数位）
fn fmt_val(val: &Decimal, prec: u8, width: usize) -> String {
    format!("{:<width$.prec$}", val, width = width, prec = prec as usize)
}

/// 按引擎精度格式化 Decimal（无宽度填充）
fn fmt_val_nopad(val: &Decimal, prec: u8) -> String {
    format!("{:.prec$}", val, prec = prec as usize)
}

// ─── View modes ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
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
    refresh_interval: u64,      // steps between pairs/balance cache refreshes
    logged_in_account: Option<usize>, // bound account for make/swap/orders/bal
    cmd_history: Vec<String>,   // previously executed commands (newest last)
    history_idx: Option<usize>, // current position browsing history (None = new input)
    quit_requested: bool,       // set by :q command, checked in main loop
    /// Step counter for throttling cache refreshes
    refresh_counter: u64,
    /// View navigation history (newest last)
    view_history: Vec<View>,
    /// Current position browsing view history (None = at latest view)
    view_history_idx: Option<usize>,
    /// Pairs info cached for rendering
    pairs_cache: Vec<(usize, usize, usize, Decimal)>,
    /// Balance cache: (token_name, available, equity, converted_to_quote)
    balance_cache: Vec<(String, Decimal, Decimal, Decimal)>,
    /// K-line data cached for rendering
    candle_cache: VecDeque<crate::pair::CandleData>,
    live_candle_cache: Option<crate::pair::CandleData>,
    current_price_cache: Option<(Decimal, usize, usize)>,
}

impl AppState {
    fn new(interval: u64, max_candles: usize, refresh_interval: u64) -> Self {
        Self {
            view: View::Pairs,
            cmd_buf: String::new(),
            cmd_cursor: 0,
            cmd_mode: false,
            msg: String::new(),
            interval,
            max_candles,
            refresh_interval,
            logged_in_account: None,
            cmd_history: Vec::new(),
            history_idx: None,
            quit_requested: false,
            refresh_counter: 0,
            view_history: Vec::new(),
            view_history_idx: None,
            pairs_cache: Vec::new(),
            balance_cache: Vec::new(),
            candle_cache: VecDeque::new(),
            live_candle_cache: None,
            current_price_cache: None,
        }
    }

    fn set_msg(&mut self, msg: impl Into<String>) {
        self.msg = msg.into();
    }

    /// Switch to a view and record in navigation history.
    fn switch_view(&mut self, new_view: View) {
        // Don't duplicate consecutive same views
        if self.view == new_view {
            return;
        }
        self.view_history.push(self.view.clone());
        if self.view_history.len() > 10 {
            self.view_history.remove(0);
        }
        self.view = new_view;
        self.view_history_idx = None;
    }

    /// Navigate backward in view history (Left arrow).
    fn view_history_back(&mut self) -> bool {
        if self.view_history.is_empty() {
            return false;
        }
        let idx = match self.view_history_idx {
            Some(i) => {
                if i == 0 { return false; }
                i - 1
            }
            None => {
                // Save current view as latest
                self.view_history.push(self.view.clone());
                self.view_history.len() - 2
            }
        };
        self.view_history_idx = Some(idx);
        self.view = self.view_history[idx].clone();
        true
    }

    /// Navigate forward in view history (Right arrow).
    fn view_history_forward(&mut self) -> bool {
        match self.view_history_idx {
            Some(i) => {
                let next = i + 1;
                if next >= self.view_history.len() {
                    // Past the end → back to latest view
                    self.view_history_idx = None;
                } else {
                    self.view_history_idx = Some(next);
                    self.view = self.view_history[next].clone();
                }
                true
            }
            None => false,
        }
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
    refresh_interval: u64,
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

    let mut state = AppState::new(interval, max_candles, refresh_interval);
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
                        KeyCode::Left => {
                            if state.view_history_back() {
                                state.set_msg("");
                            }
                        }
                        KeyCode::Right => {
                            if state.view_history_forward() {
                                state.set_msg("");
                            }
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
    state.refresh_counter += 1;
    let interval = state.refresh_interval.max(1);

    if state.refresh_counter % interval == 0 {
        // pairs 缓存
        state.pairs_cache = engine.get_all_pairs_info();

        // balance 缓存（仅在已登录时更新）
        if let Some(account_id) = state.logged_in_account {
            let tokens = engine.get_all_tokens();
            let quote_token = engine.get_global_quote_token();
            let mut cache: Vec<(String, Decimal, Decimal, Decimal)> = Vec::new();
            for &token in &tokens {
                let bal = engine.get_node_balance(account_id, token).unwrap_or(Decimal::ZERO);
                let equity = engine.get_account_equity_token(account_id, token).unwrap_or(Decimal::ZERO);
                if equity.is_zero() {
                    continue;
                }
                let name = engine.get_token_name(token).unwrap_or("?").to_string();
                let converted = if token == quote_token {
                    equity
                } else {
                    engine.convert_value(token, quote_token, equity)
                };
                cache.push((name, bal, equity, converted));
            }
            state.balance_cache = cache;
        }
    }

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
    let prec = engine.get_precision();
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
        Span::styled("ID    ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled("Quote      ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled("Base      ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled("            Price", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(Span::styled(
        "─".repeat(inner.width as usize),
        Style::default().fg(Color::DarkGray),
    )));

    for (pair_id, qt, bt, price) in &state.pairs_cache {
        let qname = engine.get_token_name(*qt).unwrap_or("?");
        let bname = engine.get_token_name(*bt).unwrap_or("?");
        let price_str = fmt_val_nopad(price, prec);
        lines.push(Line::from(vec![
            Span::styled(format!("{:<5} ", pair_id), Style::default().fg(Color::White)),
            Span::styled(format!("{:<10}", qname), Style::default().fg(Color::Green)),
            Span::styled("  ", Style::default()),
            Span::styled(format!("{:<10}", bname), Style::default().fg(Color::Green)),
            Span::styled(format!("  {:>16}", price_str), Style::default().fg(Color::Yellow)),
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

    let prec = engine.get_precision();
    // Build simplified title: only real-time price + previous candle volume
    let price_str = state
        .current_price_cache
        .map(|(p, _, _)| fmt_val_nopad(&p, prec))
        .unwrap_or_else(|| "—".to_string());
    let vol_str = if state.candle_cache.len() >= 2 {
        let prev = &state.candle_cache[state.candle_cache.len() - 2];
        fmt_val_nopad(&prev.volume, prec)
    } else if state.candle_cache.len() == 1 {
        fmt_val_nopad(&state.candle_cache[0].volume, prec)
    } else {
        "—".to_string()
    };
    let title = format!("{}:{}  Price: {}  Prev Vol: {}", src_name, dst_name, price_str, vol_str);
    let title = format!("{}   [←→] views  [:] cmd", title);

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

    // 使用缓存的 balance 数据，避免每帧读引擎
    if state.balance_cache.is_empty() {
        let para = Paragraph::new("Loading…")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(para, inner);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("Token           Available           Equity            Converted", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(Span::styled(
        "─".repeat(inner.width as usize),
        Style::default().fg(Color::DarkGray),
    )));

    let prec = engine.get_precision();
    let quote_name = engine.get_token_name(engine.get_global_quote_token()).unwrap_or("?").to_string();
    let mut total_converted = Decimal::ZERO;

    for (name, bal, equity, converted) in &state.balance_cache {
        total_converted += converted;
        lines.push(Line::from(vec![
            Span::styled(format!("{:<16}", name), Style::default().fg(Color::Green)),
            Span::styled(fmt_val(bal, prec, 18), Style::default().fg(Color::White)),
            Span::styled(fmt_val(equity, prec, 18), Style::default().fg(Color::Yellow)),
            Span::styled(fmt_val(converted, prec, 18), Style::default().fg(Color::Cyan)),
        ]));
    }

    // 总权益（按全局计价 token 换算）
    if !total_converted.is_zero() {
        lines.push(Line::from(Span::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<52}", format_args!("Total ({})", quote_name)),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                fmt_val_nopad(&total_converted, prec),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        ]));
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

    let prec = engine.get_precision();
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("ID    Dir     Pair                Volume            Price", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
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
                    Span::styled(format!("{:<5} ", brief.id), Style::default().fg(Color::White)),
                    Span::styled(format!("{:<6} ", dir_str), Style::default().fg(dir_color)),
                    Span::styled(format!("{:<18} ", pair_str), Style::default().fg(Color::Green)),
                    Span::styled(fmt_val(&brief.src_volume, prec, 16) + " ", Style::default().fg(Color::Yellow)),
                    Span::styled(fmt_val_nopad(&brief.price, prec), Style::default().fg(Color::Yellow)),
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

/// Parse a volume argument.  If the input starts with `/`, treat as `equity / N`.
fn parse_vol(input: &str, engine: &mut Rpte, account_id: usize, src_token: usize) -> Result<Decimal, String> {
    if let Some(rest) = input.strip_prefix('/') {
        let divisor: u64 = rest.parse().map_err(|_| format!("Invalid fraction: {}", input))?;
        if divisor == 0 {
            return Err("Division by zero".to_string());
        }
        let equity = engine
            .get_account_equity_token(account_id, src_token)
            .map_err(|e| format!("Equity query failed: {}", e))?;
        Ok(engine.round(equity / Decimal::from(divisor)))
    } else {
        input.parse().map_err(|_| format!("Invalid volume: {}", input))
    }
}

fn execute_cmd(cmd: &str, state: &mut AppState, engine: &mut Rpte) {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return;
    }

    match parts[0] {
        "pairs" | "p" => {
            state.switch_view(View::Pairs);
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
                state.switch_view(View::Kline { src, dst });
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
                state.switch_view(View::Kline { src, dst });
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
            state.balance_cache.clear();
            state.set_msg(format!("Logged in as account #{}", id));
        }

        "bal" | "b" => {
            if state.logged_in_account.is_none() {
                state.set_msg("Not logged in. Use :login <account_id> first");
                return;
            }
            state.switch_view(View::Balance);
            state.set_msg("Showing balances");
        }

        "orders" | "o" => {
            if state.logged_in_account.is_none() {
                state.set_msg("Not logged in. Use :login <account_id> first");
                return;
            }
            state.switch_view(View::Orders);
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
            let volume: Decimal = match parse_vol(parts[3], engine, account_id, src_token) {
                Ok(n) => n,
                Err(e) => {
                    state.set_msg(e);
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

            if !engine.is_swap_allowed(src_token, dst_token) {
                state.set_msg(format!("Trade not allowed: {} ↔ {} (whitelist restriction)", parts[1], parts[2]));
                return;
            }

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
            let volume: Decimal = match parse_vol(parts[3], engine, account_id, src_token) {
                Ok(n) => n,
                Err(e) => {
                    state.set_msg(e);
                    return;
                }
            };

            if !engine.is_swap_allowed(src_token, dst_token) {
                state.set_msg(format!("Trade not allowed: {} ↔ {} (whitelist restriction)", parts[1], parts[2]));
                return;
            }

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
            state.switch_view(View::Help);
            state.set_msg("");
        }

        _ => {
            state.set_msg(format!("Unknown command: {}. Type :help", parts[0]));
        }
    }
}
