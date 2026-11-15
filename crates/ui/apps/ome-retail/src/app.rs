use crate::ticket::{Kind, Quote, Ticket};
use gpui::{
    AnyElement, Context, FocusHandle, FontWeight, KeyBinding, Task, Window, actions, div,
    prelude::*, px,
};
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::Side;
use ome_protocol::{ConditionalReport, ExecutionReport, NewsKind};
use ome_ui_data::{
    AccountView, BrokerLink, Connection, Credentials, LinkUpdate, MarketData, MarketStore,
    NewsFeed, OrderEntry, OrderRequest,
};
use ome_ui_kit::Theme;
use ome_ui_kit::chart::{ChartModel, candle_chart};
use ome_ui_kit::format;
use ome_ui_kit::ladder::{LadderInput, depth_fraction};
use ome_ui_kit::menu::{Command, CommandMenu};
use ome_ui_kit::theme::MONO;
use ome_ui_kit::widgets::{Tone, body, button, cell, header_row, panel, row, stat, tabs};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

const CONTEXT: &str = "Retail";
const LADDER_ROWS: usize = 25;
const TABS: [&str; 4] = ["Positions", "Orders", "Contract notes", "News"];

actions!(
    retail,
    [
        BuyAction,
        SellAction,
        NextSymbol,
        PreviousSymbol,
        QtyUp,
        QtyDown,
        PriceUp,
        PriceDown,
        FollowBook,
        KindMarket,
        KindLimit,
        KindStop,
        KindBracket,
        CancelSymbol,
        ShowPositions,
        ShowOrders,
        ShowNotes,
        ShowNews,
        ToggleMenu,
        Choose,
        Close
    ]
);

pub fn bind_keys(cx: &mut gpui::App) {
    let c = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("b", BuyAction, c),
        KeyBinding::new("s", SellAction, c),
        KeyBinding::new("down", NextSymbol, c),
        KeyBinding::new("j", NextSymbol, c),
        KeyBinding::new("up", PreviousSymbol, c),
        KeyBinding::new("k", PreviousSymbol, c),
        KeyBinding::new("=", QtyUp, c),
        KeyBinding::new("-", QtyDown, c),
        KeyBinding::new("]", PriceUp, c),
        KeyBinding::new("[", PriceDown, c),
        KeyBinding::new("f", FollowBook, c),
        KeyBinding::new("m", KindMarket, c),
        KeyBinding::new("l", KindLimit, c),
        KeyBinding::new("p", KindStop, c),
        KeyBinding::new("r", KindBracket, c),
        KeyBinding::new("x", CancelSymbol, c),
        KeyBinding::new("1", ShowPositions, c),
        KeyBinding::new("2", ShowOrders, c),
        KeyBinding::new("3", ShowNotes, c),
        KeyBinding::new("4", ShowNews, c),
        KeyBinding::new("ctrl-k", ToggleMenu, c),
        KeyBinding::new("enter", Choose, c),
        KeyBinding::new("escape", Close, c),
    ]);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    Buy,
    Sell,
    Kind(Kind),
    Cancel,
    Tab(usize),
    Follow,
}

pub struct RetailApp {
    theme: Theme,
    user: String,
    store: MarketStore,
    link: BrokerLink,
    updates: Receiver<LinkUpdate>,
    selected: usize,
    ticket: Ticket,
    tab: usize,
    menu: CommandMenu<Act>,
    focus: FocusHandle,
    _pump: Task<()>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl RetailApp {
    pub fn new(credentials: Credentials, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (tx, updates) = mpsc::channel();
        let user = credentials.user.clone();
        let link = BrokerLink::start(credentials, tx);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let pump = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                if this.update(cx, |app, cx| app.pump(cx)).is_err() {
                    break;
                }
            }
        });
        let command = |label: &str, keys: &str, action| Command {
            label: label.into(),
            keys: keys.into(),
            action,
        };
        let menu = CommandMenu::new(vec![
            command("Buy with the ticket", "b", Act::Buy),
            command("Sell with the ticket", "s", Act::Sell),
            command("Market order", "m", Act::Kind(Kind::Market)),
            command("Limit order (day)", "l", Act::Kind(Kind::Limit)),
            command("Stop order", "p", Act::Kind(Kind::Stop)),
            command("Bracket order", "r", Act::Kind(Kind::Bracket)),
            command("Price follows the book", "f", Act::Follow),
            command("Cancel all orders in this symbol", "x", Act::Cancel),
            command("Positions", "1", Act::Tab(0)),
            command("Open orders", "2", Act::Tab(1)),
            command("Contract notes", "3", Act::Tab(2)),
            command("News", "4", Act::Tab(3)),
        ]);
        Self {
            theme: Theme::dark(),
            user,
            store: MarketStore::default(),
            link,
            updates,
            selected: 0,
            ticket: Ticket::default(),
            tab: 0,
            menu,
            focus,
            _pump: pump,
        }
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        while let Ok(update) = self.updates.try_recv() {
            changed = true;
            match update {
                LinkUpdate::Message(message) => self.store.apply(message),
                LinkUpdate::Down(_) => self.store.disconnected(),
            }
        }
        self.store.roll(now_ms());
        if changed {
            cx.notify();
        }
    }

    fn symbol(&self) -> Option<Symbol> {
        let symbols = self.store.symbols();
        symbols
            .get(self.selected.min(symbols.len().saturating_sub(1)))
            .copied()
    }

    fn quote(&self, symbol: Symbol) -> Quote {
        let book = self.store.book(symbol);
        Quote {
            bid: book.and_then(|b| b.bids.first()).map(|l| l.price),
            ask: book.and_then(|b| b.asks.first()).map(|l| l.price),
            last: self.store.last(symbol),
        }
    }

    fn submit(&mut self, side: Side, cx: &mut Context<Self>) {
        if let Some(symbol) = self.symbol()
            && let Some(request) = self.ticket.request(symbol, side, self.quote(symbol))
        {
            self.link.submit(request);
        }
        cx.notify();
    }

    fn place_limit(&mut self, price: Price, side: Side, cx: &mut Context<Self>) {
        if let Some(symbol) = self.symbol() {
            self.link.submit(OrderRequest::Limit {
                symbol,
                side,
                price,
                qty: self.ticket.qty(),
                tif: ome_core::types::TimeInForce::Day,
            });
        }
        cx.notify();
    }

    fn act(&mut self, act: Act, cx: &mut Context<Self>) {
        match act {
            Act::Buy => self.submit(Side::Buy, cx),
            Act::Sell => self.submit(Side::Sell, cx),
            Act::Kind(kind) => self.ticket.kind = kind,
            Act::Follow => self.ticket.follow_book(),
            Act::Tab(tab) => self.tab = tab,
            Act::Cancel => {
                self.link.submit(OrderRequest::CancelAll(self.symbol()));
                let armed: Vec<u64> = self
                    .store
                    .armed()
                    .iter()
                    .filter(|c| Some(c.symbol) == self.symbol())
                    .map(|c| c.cond_id)
                    .collect();
                for id in armed {
                    self.link.submit(OrderRequest::CancelConditional(id));
                }
            }
        }
        cx.notify();
    }

    fn step_symbol(&mut self, delta: i64, cx: &mut Context<Self>) {
        if self.menu.open {
            self.menu.step(delta);
        } else {
            let n = self.store.symbols().len() as i64;
            if n > 0 {
                self.selected = (self.selected as i64 + delta).rem_euclid(n) as usize;
                self.ticket.follow_book();
            }
        }
        cx.notify();
    }

    fn top_bar(&self) -> impl IntoElement {
        let t = &self.theme;
        let (status, color) = match self.store.connection {
            Connection::Connecting => ("connecting".to_string(), t.warning),
            Connection::Online { account, role } => (
                format!("{} · {} #{account}", self.user, role.label()),
                t.buy,
            ),
            Connection::Offline => ("offline, reconnecting".to_string(), t.sell),
        };
        let phase = self.store.phase().map_or("-", |p| p.label()).to_string();
        let (cash, equity, day_pnl, unsettled) = self.money();
        div()
            .flex()
            .items_center()
            .gap_6()
            .px_3()
            .h(px(44.))
            .bg(t.panel)
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .text_color(t.accent)
                    .child("OME RETAIL"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(color))
                    .child(status),
            )
            .child(stat(t, "market", phase, t.text))
            .child(stat(t, "cash", format::money(cash), t.text))
            .child(stat(t, "equity", format::money(equity), t.text))
            .child(stat(
                t,
                "day P&L",
                format::signed_money(day_pnl),
                t.tone(day_pnl as f64),
            ))
            .child(stat(
                t,
                "unsettled",
                format::signed_money(unsettled),
                t.muted,
            ))
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(t.muted)
                    .child("ctrl-k commands · b/s buy/sell · ↑↓ symbol · -/= qty · [ ] price"),
            )
    }

    fn money(&self) -> (u64, u64, i64, i64) {
        let Some(account) = self.store.account() else {
            return (0, 0, 0, 0);
        };
        let cash = account.cash.available + account.cash.reserved;
        let stock: u64 = account
            .positions
            .iter()
            .map(|p| {
                (p.balance.available + p.balance.reserved) * self.store.last(p.symbol).unwrap_or(0)
            })
            .sum();
        (
            account.cash.available,
            cash + stock,
            account.day_pnl,
            account.unsettled_cash,
        )
    }

    fn watchlist(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let mut list = body().child(
            header_row(&t)
                .child(cell("symbol", 56., false))
                .child(cell("last", 64., true))
                .child(cell("chg", 60., true)),
        );
        for (i, symbol) in self.store.symbols().into_iter().enumerate() {
            let info = self.store.instrument(symbol);
            let last = self.store.last(symbol);
            let change = info
                .zip(last)
                .and_then(|(info, last)| format::change_pct(info.reference_price, last))
                .unwrap_or(0.0);
            let halted = info.is_some_and(|i| !i.halt_reason.is_empty());
            let selected = Some(symbol) == self.symbol();
            list = list.child(
                row()
                    .id(("watch", i))
                    .h(px(26.))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(t.raised))
                    .hover(|d| d.bg(t.raised))
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.selected = i;
                        app.ticket.follow_book();
                        cx.notify();
                    }))
                    .child(
                        cell(symbol.to_string(), 56., false)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if halted { t.warning } else { t.text }),
                    )
                    .child(cell(format::price_or_dash(last), 64., true))
                    .child(cell(format::pct(change), 60., true).text_color(t.tone(change))),
            );
        }
        panel(&t, "Watchlist", Some(t.links[0]))
            .w(px(236.))
            .child(list)
    }

    fn chart(&self, symbol: Option<Symbol>) -> impl IntoElement {
        let t = self.theme;
        let Some(symbol) = symbol else {
            return panel(&t, "Chart", Some(t.links[0])).flex_1();
        };
        let info = self.store.instrument(symbol);
        let last = self.store.last(symbol);
        let change = info
            .zip(last)
            .and_then(|(i, l)| format::change_pct(i.reference_price, l))
            .unwrap_or(0.0);
        let stats = self.store.stats(symbol);
        let title = format!(
            "{symbol} · {}",
            info.map(|i| i.name.clone()).unwrap_or_default()
        );
        let header = div()
            .flex()
            .items_center()
            .gap_4()
            .px_2()
            .py_1()
            .text_xs()
            .child(
                div()
                    .font_family(MONO)
                    .text_xl()
                    .text_color(t.tone(change))
                    .child(format::price_or_dash(last)),
            )
            .child(div().text_color(t.tone(change)).child(format::pct(change)))
            .children(stats.map(|s| {
                div().text_color(t.muted).child(format!(
                    "O {}  H {}  L {}  VWAP {}  vol {}",
                    format::price(s.open),
                    format::price(s.high),
                    format::price(s.low),
                    format::price(s.vwap),
                    format::qty(
                        s.exchange_volume + s.internal_volume + s.wholesaler_volume + s.ats_volume
                    )
                ))
            }))
            .children(info.filter(|i| !i.halt_reason.is_empty()).map(|i| {
                div()
                    .text_color(t.warning)
                    .child(format!("HALTED: {}", i.halt_reason))
            }));
        let model = ChartModel {
            candles: self.store.candles(symbol),
            last,
            fair: None,
        };
        panel(&t, title, Some(t.links[0]))
            .flex_1()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_2()
                    .pb_2()
                    .child(candle_chart(model, t).size_full()),
            )
    }

    fn ladder(&self, symbol: Option<Symbol>, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let mut ladder = body().child(
            header_row(&t)
                .gap_0()
                .child(cell("mine", 40., true))
                .child(cell("bid", 64., true).pr_1())
                .child(cell("price", 64., true))
                .child(cell("ask", 64., false).pl_2())
                .child(cell("mine", 40., false))
                .child(cell("traded", 52., true)),
        );
        if let Some(symbol) = symbol {
            let book = self.store.book(symbol);
            let mine = |side: Side| -> Vec<(Price, u64)> {
                self.store
                    .open_orders()
                    .iter()
                    .filter(|o| o.symbol == symbol && o.side == side)
                    .map(|o| (o.price, o.leaves))
                    .collect()
            };
            let input = LadderInput {
                bids: book
                    .map(|b| b.bids.iter().map(|l| (l.price, l.qty)).collect())
                    .unwrap_or_default(),
                asks: book
                    .map(|b| b.asks.iter().map(|l| (l.price, l.qty)).collect())
                    .unwrap_or_default(),
                own_bids: mine(Side::Buy),
                own_asks: mine(Side::Sell),
                prints: self
                    .store
                    .tape(symbol)
                    .iter()
                    .take(100)
                    .map(|p| (p.price, p.qty))
                    .collect(),
                last: self.store.last(symbol),
                tick: 1,
            };
            let rows = input
                .center()
                .map(|c| input.rows(c, LADDER_ROWS))
                .unwrap_or_default();
            for (i, r) in rows.iter().enumerate() {
                let price = r.price;
                let bid_w = depth_fraction(r.bid, &rows) * 64.0;
                let ask_w = depth_fraction(r.ask, &rows) * 64.0;
                let qty = |q: u64| {
                    if q == 0 {
                        String::new()
                    } else {
                        format::qty(q)
                    }
                };
                ladder = ladder.child(
                    row()
                        .h(px(18.))
                        .gap_0()
                        .when(r.last, |d| d.bg(t.raised))
                        .child(cell(qty(r.own_bid), 40., true).text_color(t.accent))
                        .child(
                            div()
                                .id(("bid", i))
                                .relative()
                                .w(px(64.))
                                .h_full()
                                .cursor_pointer()
                                .hover(|d| d.bg(t.buy_soft))
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    app.place_limit(price, Side::Buy, cx)
                                }))
                                .child(
                                    div()
                                        .absolute()
                                        .right_0()
                                        .top_0()
                                        .h_full()
                                        .w(px(bid_w))
                                        .bg(t.buy_soft),
                                )
                                .child(cell(qty(r.bid), 64., true).pr_1().text_color(t.buy)),
                        )
                        .child(cell(format::price(price), 64., true).text_color(if r.last {
                            t.accent
                        } else {
                            t.text
                        }))
                        .child(
                            div()
                                .id(("ask", i))
                                .relative()
                                .w(px(64.))
                                .h_full()
                                .cursor_pointer()
                                .hover(|d| d.bg(t.sell_soft))
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    app.place_limit(price, Side::Sell, cx)
                                }))
                                .child(
                                    div()
                                        .absolute()
                                        .left_0()
                                        .top_0()
                                        .h_full()
                                        .w(px(ask_w))
                                        .bg(t.sell_soft),
                                )
                                .child(cell(qty(r.ask), 64., false).pl_2().text_color(t.sell)),
                        )
                        .child(cell(qty(r.own_ask), 40., false).text_color(t.accent))
                        .child(cell(qty(r.traded), 52., true).text_color(t.muted)),
                );
            }
        }
        panel(
            &t,
            "Ladder · click a bid to buy, an ask to sell",
            Some(t.links[0]),
        )
        .flex_1()
        .child(ladder)
    }

    fn ticket(&self, symbol: Option<Symbol>, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let mut kinds = div().flex().gap_1();
        for kind in Kind::ALL {
            kinds = kinds.child(button(
                &t,
                ("kind", kind as usize),
                kind.label(),
                Tone::Plain,
                self.ticket.kind == kind,
                cx.listener(move |app, _, _, cx| {
                    app.ticket.kind = kind;
                    cx.notify();
                }),
            ));
        }
        let quote = symbol.map(|s| self.quote(s));
        let price_text = match (self.ticket.price, quote) {
            (Some(p), _) => format::price(p),
            (None, _) => "follow book".to_string(),
        };
        let summary = |side| {
            symbol
                .zip(quote)
                .map(|(s, q)| self.ticket.describe(s, side, q))
                .unwrap_or_default()
        };
        let line = |label: &str, value: String| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .text_xs()
                .child(div().text_color(t.muted).child(label.to_string()))
                .child(div().font_family(MONO).child(value))
        };
        let qty_row = div()
            .flex()
            .items_center()
            .gap_1()
            .child(button(
                &t,
                "qty-down",
                "−",
                Tone::Plain,
                false,
                cx.listener(|app, _, _, cx| {
                    app.ticket.step_qty(false);
                    cx.notify();
                }),
            ))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .justify_center()
                    .font_family(MONO)
                    .child(format!("qty {}", format::qty(self.ticket.qty()))),
            )
            .child(button(
                &t,
                "qty-up",
                "+",
                Tone::Plain,
                false,
                cx.listener(|app, _, _, cx| {
                    app.ticket.step_qty(true);
                    cx.notify();
                }),
            ));
        let price_row = div()
            .flex()
            .items_center()
            .gap_1()
            .child(button(
                &t,
                "px-down",
                "−1¢",
                Tone::Plain,
                false,
                cx.listener(move |app, _, _, cx| {
                    if let Some(q) = quote {
                        app.ticket.nudge(-1, q);
                    }
                    cx.notify();
                }),
            ))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .justify_center()
                    .font_family(MONO)
                    .child(price_text),
            )
            .child(button(
                &t,
                "px-up",
                "+1¢",
                Tone::Plain,
                false,
                cx.listener(move |app, _, _, cx| {
                    if let Some(q) = quote {
                        app.ticket.nudge(1, q);
                    }
                    cx.notify();
                }),
            ))
            .child(button(
                &t,
                "px-follow",
                "book",
                Tone::Accent,
                self.ticket.price.is_none(),
                cx.listener(|app, _, _, cx| {
                    app.ticket.follow_book();
                    cx.notify();
                }),
            ));
        let sides = div()
            .flex()
            .gap_2()
            .child(
                button(
                    &t,
                    "buy",
                    "BUY  (b)",
                    Tone::Buy,
                    false,
                    cx.listener(|app, _, _, cx| app.submit(Side::Buy, cx)),
                )
                .flex_1()
                .h(px(32.)),
            )
            .child(
                button(
                    &t,
                    "sell",
                    "SELL  (s)",
                    Tone::Sell,
                    false,
                    cx.listener(|app, _, _, cx| app.submit(Side::Sell, cx)),
                )
                .flex_1()
                .h(px(32.)),
            );
        panel(&t, "Order ticket", Some(t.links[0])).child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .child(kinds)
                .child(qty_row)
                .when(self.ticket.kind != Kind::Market, |d| d.child(price_row))
                .when(
                    matches!(self.ticket.kind, Kind::Stop | Kind::Bracket),
                    |d| {
                        d.child(line(
                            "stop distance",
                            format!("{:.1}%", self.ticket.stop_bps as f64 / 100.0),
                        ))
                    },
                )
                .when(self.ticket.kind == Kind::Bracket, |d| {
                    d.child(line(
                        "target distance",
                        format!("{:.1}%", self.ticket.target_bps as f64 / 100.0),
                    ))
                })
                .child(sides)
                .child(div().text_xs().text_color(t.buy).child(summary(Side::Buy)))
                .child(
                    div()
                        .text_xs()
                        .text_color(t.sell)
                        .child(summary(Side::Sell)),
                ),
        )
    }

    fn tape(&self, symbol: Option<Symbol>) -> impl IntoElement {
        let t = self.theme;
        let mut tape = body().child(
            header_row(&t)
                .child(cell("time UTC", 64., false))
                .child(cell("price", 60., true))
                .child(cell("qty", 50., true))
                .child(cell("venue", 80., false)),
        );
        if let Some(symbol) = symbol {
            for print in self.store.tape(symbol).into_iter().take(40) {
                let color = match print.aggressor {
                    Side::Buy => t.buy,
                    Side::Sell => t.sell,
                };
                tape = tape.child(
                    row()
                        .h(px(17.))
                        .child(cell(format::clock(print.timestamp), 64., false).text_color(t.muted))
                        .child(cell(format::price(print.price), 60., true).text_color(color))
                        .child(cell(format::qty(print.qty), 50., true))
                        .child(cell(print.venue.label(), 80., false).text_color(t.muted)),
                );
            }
        }
        panel(&t, "Time & sales", Some(t.links[0]))
            .h(px(190.))
            .child(tape)
    }

    fn positions(&self) -> AnyElement {
        let t = self.theme;
        let mut table = body().child(
            header_row(&t)
                .child(cell("symbol", 70., false))
                .child(cell("qty", 70., true))
                .child(cell("reserved", 70., true))
                .child(cell("last", 70., true))
                .child(cell("value", 100., true))
                .child(cell("unsettled", 80., true)),
        );
        if let Some(account) = self.store.account() {
            for p in account
                .positions
                .iter()
                .filter(|p| p.balance.available + p.balance.reserved > 0 || p.unsettled != 0)
            {
                let last = self.store.last(p.symbol).unwrap_or(0);
                let total = p.balance.available + p.balance.reserved;
                table = table.child(
                    row()
                        .child(cell(p.symbol.to_string(), 70., false))
                        .child(cell(format::qty(total), 70., true))
                        .child(cell(format::qty(p.balance.reserved), 70., true).text_color(t.muted))
                        .child(cell(format::price(last), 70., true))
                        .child(cell(format::money(total * last), 100., true))
                        .child(cell(p.unsettled.to_string(), 80., true).text_color(t.muted)),
                );
            }
        }
        table.into_any_element()
    }

    fn orders(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let mut table = body().child(
            header_row(&t)
                .child(cell("order", 70., false))
                .child(cell("symbol", 60., false))
                .child(cell("side", 40., false))
                .child(cell("price / trigger", 150., true))
                .child(cell("left", 60., true))
                .child(cell("", 70., false)),
        );
        let side_color = |side: Side| if side == Side::Buy { t.buy } else { t.sell };
        let orders: Vec<ExecutionReport> = self.store.open_orders().into_iter().cloned().collect();
        for o in orders {
            let id = o.order_id;
            table = table.child(
                row()
                    .child(cell(format!("#{id}"), 70., false).text_color(t.muted))
                    .child(cell(o.symbol.to_string(), 60., false))
                    .child(cell(o.side.label(), 40., false).text_color(side_color(o.side)))
                    .child(cell(format::price(o.price), 150., true))
                    .child(cell(format::qty(o.leaves), 60., true))
                    .child(button(
                        &t,
                        ("cancel", id),
                        "cancel",
                        Tone::Plain,
                        false,
                        cx.listener(move |app, _, _, cx| {
                            app.link.submit(OrderRequest::Cancel(id));
                            cx.notify();
                        }),
                    )),
            );
        }
        let armed: Vec<ConditionalReport> = self.store.armed().into_iter().cloned().collect();
        for c in armed {
            let id = c.cond_id;
            table = table.child(
                row()
                    .child(cell(format!("cond {id}"), 70., false).text_color(t.accent))
                    .child(cell(c.symbol.to_string(), 60., false))
                    .child(cell(c.side.label(), 40., false).text_color(side_color(c.side)))
                    .child(cell(c.trigger.describe(), 150., true))
                    .child(cell(format::qty(c.qty), 60., true))
                    .child(button(
                        &t,
                        ("cancel-cond", id),
                        "cancel",
                        Tone::Plain,
                        false,
                        cx.listener(move |app, _, _, cx| {
                            app.link.submit(OrderRequest::CancelConditional(id));
                            cx.notify();
                        }),
                    )),
            );
        }
        table.into_any_element()
    }

    fn notes(&self) -> AnyElement {
        let t = self.theme;
        let mut table = body().child(
            header_row(&t)
                .child(cell("time", 60., false))
                .child(cell("trade", 150., false))
                .child(cell("charges", 70., true))
                .child(cell("net", 100., true)),
        );
        for note in self.store.contract_notes().into_iter().take(30) {
            table = table.child(
                row()
                    .child(cell(format::clock(note.timestamp), 60., false).text_color(t.muted))
                    .child(cell(
                        format!(
                            "{} {} {} @ {}",
                            note.side.label(),
                            note.qty,
                            note.symbol,
                            format::price(note.price)
                        ),
                        150.,
                        false,
                    ))
                    .child(
                        cell(
                            format::money(note.charges.total().unsigned_abs()),
                            70.,
                            true,
                        )
                        .text_color(t.muted),
                    )
                    .child(
                        cell(format::signed_money(note.net_amount), 100., true)
                            .text_color(t.tone(note.net_amount as f64)),
                    ),
            );
        }
        table.into_any_element()
    }

    fn news(&self) -> AnyElement {
        let t = self.theme;
        let mut list = body();
        for item in self.store.news().into_iter().take(40) {
            let tag = match item.kind {
                NewsKind::Macro => "macro".to_string(),
                NewsKind::Sector => item.sector.clone(),
                _ => item.symbol.map(|s| s.to_string()).unwrap_or_default(),
            };
            list = list.child(
                row()
                    .h(px(20.))
                    .child(cell(format::clock(item.time), 60., false).text_color(t.muted))
                    .child(cell(tag, 70., false).text_color(t.accent))
                    .child(
                        cell(format!("{:+}", item.sentiment), 34., true)
                            .text_color(t.tone(item.sentiment as f64)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(item.headline.clone()),
                    ),
            );
        }
        list.into_any_element()
    }

    fn bottom(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let content = match self.tab {
            0 => self.positions(),
            1 => self.orders(cx),
            2 => self.notes(),
            _ => self.news(),
        };
        let entity = cx.entity().downgrade();
        panel(&t, "Account", Some(t.links[0]))
            .h(px(230.))
            .child(tabs(
                &t,
                "account-tabs",
                &TABS,
                self.tab,
                move |i, _, cx| {
                    entity
                        .update(cx, |app, cx| {
                            app.tab = i;
                            cx.notify();
                        })
                        .ok();
                },
            ))
            .child(content)
    }

    fn notices(&self) -> impl IntoElement {
        let t = self.theme;
        let latest = self.store.notices().into_iter().next().cloned();
        div()
            .h(px(22.))
            .px_3()
            .flex()
            .items_center()
            .text_xs()
            .bg(t.panel)
            .border_t_1()
            .border_color(t.border)
            .children(latest.map(|n| {
                div()
                    .text_color(if n.error { t.sell } else { t.muted })
                    .child(n.text)
            }))
    }

    fn menu_overlay(&self) -> Option<impl IntoElement> {
        let t = self.theme;
        if !self.menu.open {
            return None;
        }
        let mut list = div()
            .w(px(420.))
            .flex()
            .flex_col()
            .bg(t.raised)
            .border_1()
            .border_color(t.accent)
            .rounded(px(6.))
            .p_1()
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(t.muted)
                    .child("↑↓ choose · enter run · esc close"),
            );
        for (i, command) in self.menu.commands.iter().enumerate() {
            let chosen = i == self.menu.selected;
            list = list.child(
                div()
                    .flex()
                    .justify_between()
                    .px_2()
                    .h(px(24.))
                    .items_center()
                    .rounded(px(3.))
                    .text_sm()
                    .when(chosen, |d| d.bg(t.accent).text_color(t.background))
                    .child(command.label.clone())
                    .child(
                        div()
                            .font_family(MONO)
                            .text_xs()
                            .child(command.keys.clone()),
                    ),
            );
        }
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .justify_center()
                .pt(px(90.))
                .bg(gpui::rgba(0x00000088))
                .child(list),
        )
    }
}

impl Render for RetailApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let symbol = self.symbol();
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(t.background)
            .text_color(t.text)
            .text_sm()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|app, _: &BuyAction, _, cx| app.act(Act::Buy, cx)))
            .on_action(cx.listener(|app, _: &SellAction, _, cx| app.act(Act::Sell, cx)))
            .on_action(cx.listener(|app, _: &NextSymbol, _, cx| app.step_symbol(1, cx)))
            .on_action(cx.listener(|app, _: &PreviousSymbol, _, cx| app.step_symbol(-1, cx)))
            .on_action(cx.listener(|app, _: &QtyUp, _, cx| {
                app.ticket.step_qty(true);
                cx.notify();
            }))
            .on_action(cx.listener(|app, _: &QtyDown, _, cx| {
                app.ticket.step_qty(false);
                cx.notify();
            }))
            .on_action(cx.listener(|app, _: &PriceUp, _, cx| {
                if let Some(s) = app.symbol() {
                    let q = app.quote(s);
                    app.ticket.nudge(1, q);
                }
                cx.notify();
            }))
            .on_action(cx.listener(|app, _: &PriceDown, _, cx| {
                if let Some(s) = app.symbol() {
                    let q = app.quote(s);
                    app.ticket.nudge(-1, q);
                }
                cx.notify();
            }))
            .on_action(cx.listener(|app, _: &FollowBook, _, cx| app.act(Act::Follow, cx)))
            .on_action(
                cx.listener(|app, _: &KindMarket, _, cx| app.act(Act::Kind(Kind::Market), cx)),
            )
            .on_action(cx.listener(|app, _: &KindLimit, _, cx| app.act(Act::Kind(Kind::Limit), cx)))
            .on_action(cx.listener(|app, _: &KindStop, _, cx| app.act(Act::Kind(Kind::Stop), cx)))
            .on_action(
                cx.listener(|app, _: &KindBracket, _, cx| app.act(Act::Kind(Kind::Bracket), cx)),
            )
            .on_action(cx.listener(|app, _: &CancelSymbol, _, cx| app.act(Act::Cancel, cx)))
            .on_action(cx.listener(|app, _: &ShowPositions, _, cx| app.act(Act::Tab(0), cx)))
            .on_action(cx.listener(|app, _: &ShowOrders, _, cx| app.act(Act::Tab(1), cx)))
            .on_action(cx.listener(|app, _: &ShowNotes, _, cx| app.act(Act::Tab(2), cx)))
            .on_action(cx.listener(|app, _: &ShowNews, _, cx| app.act(Act::Tab(3), cx)))
            .on_action(cx.listener(|app, _: &ToggleMenu, _, cx| {
                app.menu.toggle();
                cx.notify();
            }))
            .on_action(cx.listener(|app, _: &Choose, _, cx| {
                if let Some(act) = app.menu.choose() {
                    app.act(act, cx);
                }
                cx.notify();
            }))
            .on_action(cx.listener(|app, _: &Close, _, cx| {
                app.menu.close();
                cx.notify();
            }))
            .child(self.top_bar())
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap_2()
                    .p_2()
                    .child(self.watchlist(cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .child(self.chart(symbol))
                            .child(self.bottom(cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .w(px(352.))
                            .gap_2()
                            .child(self.ladder(symbol, cx))
                            .child(self.ticket(symbol, cx))
                            .child(self.tape(symbol)),
                    ),
            )
            .child(self.notices())
            .children(self.menu_overlay())
    }
}
