//! K-line TUI monitor for Rpte engine.
//!
//! Uses `ratatui` + `ratatui-plt`'s `LinePlot` with asymmetric error bars
//! to display live OHLC candles (close line + high-low wicks) from the trading engine.

use std::io::stdout;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui_plt::prelude::*;
use rust_decimal::prelude::ToPrimitive;

use crate::Rpte;

/// Run the K-line TUI monitor.
///
/// Steps the engine via `frame_callback`, fetches candle data for the given
/// token pair at `interval` step-granularity, and renders a live chart with
/// close-price line and high-low error bars (wicks).
///
/// Press `q` to quit.
pub fn run_tui<F>(
    engine: &mut Rpte,
    src_token: usize,
    dst_token: usize,
    interval: u64,
    max_candles: usize,
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

    let src_name = engine
        .get_token_name(src_token)
        .unwrap_or("?")
        .to_string();
    let dst_name = engine
        .get_token_name(dst_token)
        .unwrap_or("?")
        .to_string();

    'main: loop {
        // Step the engine one frame
        frame_callback(engine);
        engine.step();

        // Fetch candle data from the engine
        let mut candle_data = engine
            .get_candle_data(src_token, dst_token, interval)
            .unwrap_or_default();
        // Get real-time current price (updates every step)
        let current_price = engine
            .get_current_price(src_token, dst_token)
            .ok()
            .map(|(p, _, _)| p);
        // Get the current (unfinished) candle for OHLC info
        let live_candle = engine
            .latest_candle(src_token, dst_token, interval)
            .ok()
            .flatten();
        // Trim to max_candles (drop oldest)
        while candle_data.len() > max_candles {
            candle_data.pop_front();
        }

        terminal.draw(|f| {
            let area = f.area();
            let (chart_area, status_area) = layout(area);

            // Build title: real-time price + OHLC from current candle
            let price_str = current_price
                .map(|p| format!("{:.5}", p))
                .unwrap_or_else(|| "—".to_string());
            let title = match &live_candle {
                Some(c) => format!(
                    "{}:{}  {}  O:{:.5} H:{:.5} L:{:.5} C:{:.5}",
                    src_name, dst_name, price_str, c.open, c.high, c.low, c.close,
                ),
                None => format!("{}:{}  {}", src_name, dst_name, price_str),
            };

            // Convert CandleData → LinePlot series with high-low error bars
            let data: Vec<(f64, f64)> = candle_data
                .iter()
                .map(|c| (c.step_count as f64, c.close.to_f64().unwrap_or(0.0)))
                .collect();
            let err_low: Vec<f64> = candle_data
                .iter()
                .map(|c| c.close.to_f64().unwrap_or(0.0) - c.low.to_f64().unwrap_or(0.0))
                .collect();
            let err_high: Vec<f64> = candle_data
                .iter()
                .map(|c| c.high.to_f64().unwrap_or(0.0) - c.close.to_f64().unwrap_or(0.0))
                .collect();

            let close_series = Series::new("")
                .data(data)
                .color(ratatui::style::Color::Cyan)
                .y_err_asymmetric(err_low, err_high);

            let plot = LinePlot::new()
                .series(close_series)
                .title(&title)
                .show_legend(false)
                .x_axis(Axis::new().label(""))
                .y_axis(Axis::new().label(""));

            f.render_widget(&plot, chart_area);

            // Status bar at the bottom
            let status = Paragraph::new(" [q] quit  engine running")
                .style(ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray));
            f.render_widget(status, status_area);
        })?;

        // Handle keyboard input (non-blocking poll)
        if event::poll(Duration::from_millis(5))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break 'main,
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

/// Split the terminal area into chart + status bar.
fn layout(area: Rect) -> (Rect, Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    (chunks[0], chunks[1])
}
