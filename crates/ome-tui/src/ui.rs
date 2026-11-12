use crate::app::{App, LogLevel, MarketView, side_symbol};
use crate::candle_chart::CandleChart;
use crate::command::ChartMode;
use ome_core::candles::Candle;
use ome_core::price::{Price, format_price, to_f64};
use ome_core::types::{Side, Timestamp};
use ome_protocol::{InstrumentStatus, Role};
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Clear, Dataset, GraphType, Paragraph, Row, Table};
use std::collections::HashSet;

const UP: Color = Color::Green;
const DOWN: Color = Color::Red;
const MUTED: Color = Color::DarkGray;
const ACCENT: Color = Color::Cyan;
const WARN: Color = Color::Yellow;

pub fn draw(frame: &mut Frame, app: &App) {
    let [header, tabs, body, bottom, input, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(12),
        Constraint::Length(11),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let [chart, book] =
        Layout::horizontal([Constraint::Min(40), Constraint::Length(36)]).areas(body);

    draw_header(frame, header, app);
    draw_tabs(frame, tabs, app);
    draw_chart(frame, chart, app);
    draw_book(frame, book, app);

    if app.is_admin() {
        let [tape, instruments, accounts, log] = Layout::horizontal([
            Constraint::Percentage(22),
            Constraint::Percentage(28),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .areas(bottom);
        draw_tape(frame, tape, app);
        draw_instruments(frame, instruments, app);
        draw_accounts(frame, accounts, app);
        draw_log(frame, log, app);
    } else {
        let [tape, orders, portfolio, log] = Layout::horizontal([
            Constraint::Percentage(22),
            Constraint::Percentage(28),
            Constraint::Percentage(20),
            Constraint::Percentage(30),
        ])
        .areas(bottom);
        draw_tape(frame, tape, app);
        draw_orders(frame, orders, app);
        draw_portfolio(frame, portfolio, app);
        draw_log(frame, log, app);
    }
    draw_input(frame, input, app);
    draw_footer(frame, footer, app);
    if app.show_help {
        draw_help(frame, app);
    }
}

fn panel(title: &str) -> Block<'static> {
    Block::bordered()
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(ACCENT).bold(),
        ))
        .border_style(Style::default().fg(MUTED))
}

fn format_time(ms: Timestamp) -> String {
    let s = (ms / 1_000) % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

fn role_badge(role: Role) -> Span<'static> {
    let (text, color) = match role {
        Role::Admin => (" ADMIN ", Color::Red),
        Role::Institutional => (" INSTITUTIONAL ", Color::Blue),
        Role::Retail => (" RETAIL ", Color::Green),
        Role::Bot => (" BOT ", Color::Magenta),
    };
    Span::styled(text, Style::default().fg(Color::Black).bg(color).bold())
}

fn change_pct(last: Price, reference: Price) -> f64 {
    if reference == 0 {
        0.0
    } else {
        (last as f64 / reference as f64 - 1.0) * 100.0
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![
        Span::styled(" OME ", Style::default().fg(Color::Black).bg(ACCENT).bold()),
        Span::raw(" "),
        role_badge(app.role),
        Span::styled(
            format!(" {}@{} ", app.username, app.addr),
            Style::default().fg(Color::White),
        ),
        if app.connected {
            " ● connected ".green().bold()
        } else {
            " ● disconnected ".red().bold()
        },
        Span::styled(
            format!(" {} UTC ", format_time(app.server_time)),
            Style::default().fg(MUTED),
        ),
    ];
    if let Some(info) = app.selected() {
        let last = info.last_price.unwrap_or(info.reference_price);
        let change = change_pct(last, info.reference_price);
        let color = if change >= 0.0 { UP } else { DOWN };
        spans.push(Span::styled(
            format!(" {} ", info.symbol),
            Style::default().fg(Color::White).bold(),
        ));
        spans.push(Span::styled(
            format!("{} ", format_price(last)),
            Style::default().fg(color).bold(),
        ));
        spans.push(Span::styled(
            format!("{change:+.2}% "),
            Style::default().fg(color),
        ));
        spans.push(Span::styled(
            format!(
                " ref {}  breaker {}-{}  vol {}",
                format_price(info.reference_price),
                format_price(info.low_limit),
                format_price(info.high_limit),
                info.volume
            ),
            Style::default().fg(MUTED),
        ));
    }
    frame.render_widget(Line::from(spans), area);
}

fn draw_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let selected = app.view.as_ref().map(|v| v.symbol);
    let mut spans = vec![Span::styled(" ◀ ▶ ", Style::default().fg(MUTED))];
    for info in app.instruments.values() {
        let last = info.last_price.unwrap_or(info.reference_price);
        let change = change_pct(last, info.reference_price);
        let halted = info.status == InstrumentStatus::Halted;
        let label = format!(
            " {}{} {} {:+.1}% ",
            if halted { "⏸ " } else { "" },
            info.symbol,
            format_price(last),
            change
        );
        let color = if halted {
            WARN
        } else if change >= 0.0 {
            UP
        } else {
            DOWN
        };
        let style = if Some(info.symbol) == selected {
            Style::default().fg(Color::Black).bg(color).bold()
        } else {
            Style::default().fg(color)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    if app.instruments.is_empty() {
        spans.push(Span::styled(
            "no instruments listed yet (admin: ipo <SYMBOL> <price> <shares> <issuer> <name>)",
            Style::default().fg(MUTED).italic(),
        ));
    }
    frame.render_widget(Line::from(spans), area);
}

fn draw_chart(frame: &mut Frame, area: Rect, app: &App) {
    let Some(view) = &app.view else {
        frame.render_widget(panel("Price"), area);
        return;
    };
    let info = app.instruments.get(&view.symbol);
    let mut title = match app.chart_mode {
        ChartMode::Candles => format!(
            "{} · {}s candles",
            view.symbol,
            view.candles.interval() / 1_000
        ),
        ChartMode::Line => format!("{} · line", view.symbol),
    };
    if let Some(info) = info {
        title = format!("{title} · {}", info.name);
    }
    let mut block = panel(&title);
    if let Some(info) = info.filter(|i| i.status == InstrumentStatus::Halted) {
        block = block
            .title_bottom(Span::styled(
                format!(" ⏸ HALTED: {} ", info.halt_reason),
                Style::default().fg(Color::Black).bg(WARN).bold(),
            ))
            .border_style(Style::default().fg(WARN));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let candles = view.candles.candles();
    match app.chart_mode {
        ChartMode::Candles => {
            let take = CandleChart::capacity(inner.width);
            let visible: Vec<Candle> = candles
                .iter()
                .skip(candles.len().saturating_sub(take))
                .copied()
                .collect();
            let last = info.and_then(|i| i.last_price);
            frame.render_widget(CandleChart::new(&visible, last), inner);
        }
        ChartMode::Line => {
            draw_line_chart(frame, inner, &candles.iter().copied().collect::<Vec<_>>())
        }
    }
}

fn draw_line_chart(frame: &mut Frame, area: Rect, candles: &[Candle]) {
    let take = (area.width as usize).saturating_sub(10) * 2;
    let visible = &candles[candles.len().saturating_sub(take)..];
    if visible.len() < 2 {
        return;
    }
    let points: Vec<(f64, f64)> = visible
        .iter()
        .enumerate()
        .map(|(i, c)| (i as f64, to_f64(c.close)))
        .collect();
    let low = visible.iter().map(|c| c.low).min().unwrap_or(0);
    let high = visible.iter().map(|c| c.high).max().unwrap_or(1);
    let padding = ((high - low) / 10).max(2);
    let (low, high) = (to_f64(low.saturating_sub(padding)), to_f64(high + padding));
    let rising = visible.last().map(|c| c.close) >= visible.first().map(|c| c.open);

    let dataset = Dataset::default()
        .marker(Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(if rising { UP } else { DOWN }))
        .data(&points);
    let chart = Chart::new(vec![dataset])
        .x_axis(
            Axis::default()
                .style(Style::default().fg(MUTED))
                .bounds([0.0, (visible.len() - 1) as f64])
                .labels([format!("-{}s", visible.len()), "now".to_string()]),
        )
        .y_axis(
            Axis::default()
                .style(Style::default().fg(MUTED))
                .bounds([low, high])
                .labels([
                    format!("{low:.2}"),
                    format!("{:.2}", (low + high) / 2.0),
                    format!("{high:.2}"),
                ]),
        );
    frame.render_widget(chart, area);
}

fn draw_book(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel("Order Book");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(view) = &app.view else {
        return;
    };

    let per_side = (inner.height.saturating_sub(1) / 2) as usize;
    let asks: Vec<_> = view.asks.iter().take(per_side).collect();
    let bids: Vec<_> = view.bids.iter().take(per_side).collect();
    let mine: HashSet<Price> = app
        .orders
        .values()
        .filter(|o| o.symbol == view.symbol)
        .map(|o| o.price)
        .collect();
    let max_qty = asks
        .iter()
        .chain(&bids)
        .map(|l| l.qty)
        .max()
        .unwrap_or(1)
        .max(1);
    let bar_width = inner.width.saturating_sub(20) as u64;

    let row = |price: Price, qty: u64, color: Color| {
        let bar = "█".repeat((qty * bar_width).div_ceil(max_qty) as usize);
        Line::from(vec![
            Span::styled(
                if mine.contains(&price) { "◆" } else { " " },
                Style::default().fg(WARN),
            ),
            Span::styled(
                format!("{:>9} ", format_price(price)),
                Style::default().fg(color),
            ),
            Span::styled(format!("{qty:>8} "), Style::default().fg(Color::White)),
            Span::styled(bar, Style::default().fg(color).add_modifier(Modifier::DIM)),
        ])
    };

    let mut lines: Vec<Line> = vec![Line::raw(""); per_side.saturating_sub(asks.len())];
    lines.extend(asks.iter().rev().map(|l| row(l.price, l.qty, DOWN)));
    lines.push(spread_line(view, inner.width));
    lines.extend(bids.iter().map(|l| row(l.price, l.qty, UP)));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn spread_line(view: &MarketView, width: u16) -> Line<'static> {
    let text = match (view.bids.first(), view.asks.first()) {
        (Some(bid), Some(ask)) => {
            let spread = ask.price.saturating_sub(bid.price);
            let bps = spread as f64 / ((ask.price + bid.price) as f64 / 2.0) * 10_000.0;
            format!(" spread {} · {bps:.1} bps ", format_price(spread))
        }
        _ => " one-sided book ".to_string(),
    };
    Line::styled(
        format!("{text:─^width$}", width = width as usize),
        Style::default().fg(MUTED),
    )
}

fn draw_tape(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel("Trades");
    let inner = block.inner(area);
    let lines: Vec<Line> = app
        .view
        .iter()
        .flat_map(|v| v.tape.iter())
        .take(inner.height as usize)
        .map(|t| {
            let color = if t.aggressor == Side::Buy { UP } else { DOWN };
            Line::from(vec![
                Span::styled(
                    format!("{} ", format_time(t.timestamp)),
                    Style::default().fg(MUTED),
                ),
                Span::styled(
                    format!(
                        "{} {:>9} {:>6}",
                        side_symbol(t.aggressor),
                        format_price(t.price),
                        t.qty
                    ),
                    Style::default().fg(color),
                ),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_orders(frame: &mut Frame, area: Rect, app: &App) {
    let rows = app.orders.values().map(|o| {
        let color = if o.side == Side::Buy { UP } else { DOWN };
        Row::new(vec![
            format!("#{}", o.order_id),
            o.symbol.to_string(),
            o.side.label().to_uppercase(),
            format_price(o.price),
            format!("{}/{}", o.leaves, o.qty),
        ])
        .style(Style::default().fg(color))
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(9),
            Constraint::Length(6),
            Constraint::Length(5),
            Constraint::Length(9),
            Constraint::Min(7),
        ],
    )
    .header(
        Row::new(["ID", "SYM", "SIDE", "PRICE", "LEFT"]).style(Style::default().fg(MUTED).bold()),
    )
    .block(panel(&format!("My Orders ({})", app.orders.len())));
    frame.render_widget(table, area);
}

fn draw_portfolio(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines = Vec::new();
    if let Some(account) = &app.account {
        let mut equity = account.cash.total();
        lines.push(Line::from(vec![
            Span::styled("cash ", Style::default().fg(MUTED)),
            Span::styled(
                format_price(account.cash.available),
                Style::default().fg(Color::White).bold(),
            ),
        ]));
        lines.push(Line::styled(
            format!("held {}", format_price(account.cash.reserved)),
            Style::default().fg(MUTED),
        ));
        for p in &account.positions {
            let last = app
                .instruments
                .get(&p.symbol)
                .map(|i| i.last_price.unwrap_or(i.reference_price))
                .unwrap_or(0);
            let value = last * p.balance.total();
            equity += value;
            lines.push(Line::from(vec![
                Span::styled(format!("{:<6}", p.symbol), Style::default().fg(ACCENT)),
                Span::raw(format!("{:>6} ", p.balance.total())),
                Span::styled(format_price(value), Style::default().fg(MUTED)),
            ]));
        }
        lines.insert(
            0,
            Line::from(vec![
                Span::styled("equity ", Style::default().fg(MUTED)),
                Span::styled(format_price(equity), Style::default().fg(WARN).bold()),
            ]),
        );
    }
    frame.render_widget(Paragraph::new(lines).block(panel("Portfolio")), area);
}

fn draw_instruments(frame: &mut Frame, area: Rect, app: &App) {
    let rows = app.instruments.values().map(|i| {
        let halted = i.status == InstrumentStatus::Halted;
        let last = i.last_price.unwrap_or(i.reference_price);
        Row::new(vec![
            i.symbol.to_string(),
            if halted { "HALT".into() } else { "LIVE".into() },
            format_price(last),
            format!("{:+.1}%", change_pct(last, i.reference_price)),
            i.volume.to_string(),
        ])
        .style(Style::default().fg(if halted { WARN } else { Color::White }))
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Length(5),
            Constraint::Length(9),
            Constraint::Length(7),
            Constraint::Min(6),
        ],
    )
    .header(
        Row::new(["SYM", "STATE", "LAST", "CHG", "VOL"]).style(Style::default().fg(MUTED).bold()),
    )
    .block(panel(&format!("Instruments ({})", app.instruments.len())));
    frame.render_widget(table, area);
}

fn draw_accounts(frame: &mut Frame, area: Rect, app: &App) {
    let rows = app.accounts.iter().map(|a| {
        Row::new(vec![
            a.account_id.to_string(),
            a.username.clone(),
            a.role.label().chars().take(5).collect(),
            format_price(a.cash.total()),
            a.open_orders.to_string(),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Length(11),
            Constraint::Length(5),
            Constraint::Length(12),
            Constraint::Min(4),
        ],
    )
    .header(
        Row::new(["ID", "USER", "ROLE", "CASH", "OPEN"]).style(Style::default().fg(MUTED).bold()),
    )
    .block(panel(&format!("Accounts ({})", app.accounts.len())));
    frame.render_widget(table, area);
}

fn draw_log(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel("Log");
    let inner = block.inner(area);
    let skip = app.log.len().saturating_sub(inner.height as usize);
    let lines: Vec<Line> = app
        .log
        .iter()
        .skip(skip)
        .map(|entry| {
            let color = match entry.level {
                LogLevel::Info => Color::Gray,
                LogLevel::Success => UP,
                LogLevel::Warning => WARN,
                LogLevel::Error => DOWN,
            };
            Line::from(vec![
                Span::styled(
                    format!("{} ", format_time(entry.time)),
                    Style::default().fg(MUTED),
                ),
                Span::styled(entry.text.as_str(), Style::default().fg(color)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_input(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered()
        .title(Span::styled(
            " Command ",
            Style::default().fg(ACCENT).bold(),
        ))
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(area);
    let placeholder = if app.is_admin() {
        "ipo ZEN 12.50 50000 issuer Zen Robotics · halt ACME news · cash alice 5000 · help"
    } else {
        "buy 25.10 10 · sell mkt 5 · cancel 42 · sym NOVA · help"
    };
    let content = if app.input.is_empty() {
        Line::from(vec![
            "› ".fg(ACCENT),
            Span::styled(
                placeholder,
                Style::default().fg(MUTED).add_modifier(Modifier::ITALIC),
            ),
        ])
    } else {
        Line::from(vec!["› ".fg(ACCENT), Span::raw(app.input.as_str())])
    };
    frame.render_widget(Paragraph::new(content).block(block), area);
    if !app.show_help {
        let x = inner.x + 2 + app.input.chars().count() as u16;
        frame.set_cursor_position((x.min(inner.right().saturating_sub(1)), inner.y));
    }
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let keys = [
        ("Enter", "run"),
        ("↑↓", "history"),
        ("◀ ▶", "symbol"),
        ("Tab", "chart"),
        ("F1/?", "help"),
        ("Ctrl-L", "clear"),
        ("Ctrl-C", "quit"),
    ];
    let mut spans: Vec<Span> = keys
        .iter()
        .flat_map(|(key, action)| {
            [
                Span::styled(
                    format!(" {key} "),
                    Style::default().fg(Color::Black).bg(MUTED),
                ),
                Span::styled(format!(" {action}  "), Style::default().fg(MUTED)),
            ]
        })
        .collect();
    if !app.is_admin() {
        spans.push(Span::styled("◆ = your order", Style::default().fg(WARN)));
    }
    frame.render_widget(Line::from(spans), area);
}

fn draw_help(frame: &mut Frame, app: &App) {
    let heading = |text: &'static str| Line::from(text.fg(ACCENT).bold());
    let item = |cmd: &'static str, what: &'static str| {
        Line::from(vec![
            Span::styled(
                format!("  {cmd:<34}"),
                Style::default().fg(Color::White).bold(),
            ),
            Span::styled(what, Style::default().fg(Color::Gray)),
        ])
    };
    let mut lines = vec![heading("Everyone")];
    lines.extend([
        item(
            "sym <SYMBOL>",
            "switch instrument (or ◀ ▶ with empty input)",
        ),
        item("chart candles | line", "switch chart (Tab)"),
        item("clear", "clear the log (Ctrl-L)"),
        item("quit", "exit (Ctrl-C)"),
        Line::raw(""),
    ]);
    if app.is_admin() {
        lines.extend([
            heading("Admin"),
            item(
                "create <user> <secret> <role> <cash>",
                "role: retail | institutional | bot | admin",
            ),
            item(
                "cash <account> <amount>",
                "set available cash (id or username)",
            ),
            item("shares <account> <SYMBOL> <qty>", "set available shares"),
            item(
                "ipo <SYM> <price> <shares> <issuer> <name>",
                "list; issuer offers all shares at IPO price",
            ),
            item(
                "halt [SYMBOL] [reason]",
                "halt trading (current symbol if omitted)",
            ),
            item(
                "resume [SYMBOL]",
                "resume trading, reference resets to last",
            ),
            item("accounts", "refresh the accounts table"),
        ]);
    } else {
        lines.extend([
            heading("Trading (current symbol)"),
            item("buy <price> <qty>", "limit buy (alias: b)"),
            item("sell <price> <qty>", "limit sell (alias: s)"),
            item(
                "buy mkt <qty> / sell mkt <qty>",
                "market order, fills inside the price band",
            ),
            item("cancel <id> | cancel all", "cancel orders (alias: c)"),
        ]);
    }
    lines.extend([
        Line::raw(""),
        heading("Market rules"),
        Line::raw("  orders must be within ±10% of the last trade; cash and shares are reserved"),
        Line::raw("  up front (no shorting, no margin); self-trades cancel your resting order;"),
        Line::raw("  a move over 30% from the reference price halts trading for 30 seconds"),
        Line::raw("  limits: retail 1,000 shares/order, bot 10,000, institutional 1,000,000"),
        Line::raw(""),
        Line::from("  press any key to close".fg(MUTED).italic()),
    ]);

    let height = (lines.len() as u16 + 2).min(frame.area().height);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(frame.area());
    let [area] = Layout::horizontal([Constraint::Length(86)])
        .flex(Flex::Center)
        .areas(area);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(panel("Help")), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{info, populated};
    use crate::command::Command;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn maybe_print(screen: &str) {
        if std::env::var("OME_PRINT").is_ok() {
            println!("{screen}\n");
        }
    }

    #[test]
    fn trader_screen_has_trading_panels() {
        let (mut app, _) = populated(Role::Retail);
        for mode in [ChartMode::Candles, ChartMode::Line] {
            app.execute(Command::Chart(mode));
            let screen = render(&app, 150, 44);
            maybe_print(&screen);
            for panel in [
                "RETAIL",
                "ACME",
                "NOVA",
                "Order Book",
                "Trades",
                "My Orders",
                "Portfolio",
                "Log",
            ] {
                assert!(screen.contains(panel), "missing {panel}");
            }
        }
    }

    #[test]
    fn admin_screen_has_admin_panels_and_halt_banner() {
        let (mut app, _) = populated(Role::Admin);
        app.apply(ome_protocol::ServerMessage::Instrument(info(
            "ACME",
            InstrumentStatus::Halted,
        )));
        let screen = render(&app, 150, 44);
        maybe_print(&screen);
        for panel in [
            "ADMIN",
            "Instruments",
            "Accounts",
            "HALTED: circuit breaker",
            "alice",
        ] {
            assert!(screen.contains(panel), "missing {panel}");
        }
        assert!(!screen.contains("My Orders"));
    }

    #[test]
    fn help_and_tiny_terminals_render() {
        for role in [Role::Admin, Role::Retail] {
            let (mut app, _) = populated(role);
            app.execute(Command::Help);
            assert!(render(&app, 150, 44).contains("Market rules"));
            render(&app, 20, 8);
        }
    }
}
