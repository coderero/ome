use crate::app::{Drag, RetailApp};
use crate::ticket::Kind;
use gpui::{
    Context, FontWeight, IntoElement, MouseButton, MouseMoveEvent, ScrollWheelEvent, div,
    prelude::*, px,
};
use ome_core::price::Price;
use ome_core::types::{Side, TimeInForce};
use ome_protocol::{ConditionalReport, ExecutionReport, InstrumentStatus};
use ome_ui_data::{AccountView, MarketData};
use ome_ui_kit::chart::{ChartModel, candle_chart};
use ome_ui_kit::format;
use ome_ui_kit::input::field;
use ome_ui_kit::ladder::{LadderInput, depth_fraction};
use ome_ui_kit::theme::MONO;
use ome_ui_kit::widgets::{
    Align, Tone, badge, body, button, gadget, segmented, table_header, table_row, td, td_text, th,
};

const LADDER_ROWS: usize = 29;
const W_VOL: f32 = 58.;
const W_ORD: f32 = 62.;
const W_SIZE: f32 = 66.;
const W_PRICE: f32 = 70.;

impl RetailApp {
    pub fn render_trade(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .gap_1()
            .child(self.quote_header())
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap_1()
                    .child(self.active_trader(cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(self.mini_chart(cx))
                            .child(self.level_two()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .h(px(150.))
                    .gap_1()
                    .child(self.order_entry(cx))
                    .child(self.working_orders(cx)),
            )
    }

    fn quote_header(&self) -> impl IntoElement {
        let t = self.theme;
        let Some(symbol) = self.selected else {
            return div().h(px(52.));
        };
        let info = self.store.instrument(symbol);
        let quote = self.quote(symbol);
        let book = self.store.book(symbol);
        let reference = info.map(|i| i.reference_price).unwrap_or(0);
        let last = quote.last.unwrap_or(reference);
        let net = last as i64 - reference as i64;
        let pct = format::change_pct(reference, last).unwrap_or(0.0);
        let stats = self.store.stats(symbol);
        let flash = self.flash(symbol);
        let size_at = |levels: Option<&Vec<ome_core::order_book::Level>>| {
            levels
                .and_then(|l| l.first())
                .map(|l| format::qty(l.qty))
                .unwrap_or_default()
        };
        let pair = |label: &str, value: String| {
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(t.muted)
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .font_family(MONO)
                        .text_size(px(13.))
                        .text_color(t.strong)
                        .child(value),
                )
        };
        let status = match info.map(|i| i.status) {
            Some(InstrumentStatus::Trading) => badge("TRADING", t.background, t.up),
            Some(_) => badge(
                format!(
                    "HALTED {}",
                    info.map(|i| i.halt_reason.clone()).unwrap_or_default()
                ),
                t.background,
                t.warning,
            ),
            None => badge("-", t.muted, t.raised),
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_6()
            .px_3()
            .h(px(52.))
            .bg(t.panel)
            .border_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(t.strong)
                            .child(symbol.to_string()),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(t.muted)
                            .child(info.map(|i| i.name.clone()).unwrap_or_default()),
                    ),
            )
            .child(
                div()
                    .px_2()
                    .rounded(px(2.))
                    .when_some(flash, |d, up| {
                        d.bg(if up { t.up_flash } else { t.down_flash })
                    })
                    .font_family(MONO)
                    .text_size(px(24.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.strong)
                    .child(format::price(last)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .font_family(MONO)
                    .text_color(t.tone(net as f64))
                    .child(div().text_size(px(14.)).child(format!(
                        "{}{}",
                        if net >= 0 { "+" } else { "-" },
                        format::price(net.unsigned_abs())
                    )))
                    .child(div().text_size(px(12.)).child(format::pct(pct))),
            )
            .child(pair(
                "BID",
                format!(
                    "{} × {}",
                    format::price_or_dash(quote.bid),
                    size_at(book.map(|b| &b.bids))
                ),
            ))
            .child(pair(
                "ASK",
                format!(
                    "{} × {}",
                    format::price_or_dash(quote.ask),
                    size_at(book.map(|b| &b.asks))
                ),
            ))
            .children(stats.map(|s| {
                pair(
                    "VOLUME",
                    format::qty(
                        s.exchange_volume + s.internal_volume + s.wholesaler_volume + s.ats_volume,
                    ),
                )
            }))
            .children(stats.map(|s| pair("OPEN", format::price(s.open))))
            .children(stats.map(|s| pair("HIGH", format::price(s.high))))
            .children(stats.map(|s| pair("LOW", format::price(s.low))))
            .children(stats.map(|s| pair("VWAP", format::price(s.vwap))))
            .child(div().flex_1())
            .child(status)
    }

    fn ladder_input(&self) -> Option<LadderInput> {
        let symbol = self.selected?;
        let book = self.store.book(symbol);
        let mine = |side: Side| -> Vec<(Price, u64)> {
            self.store
                .open_orders()
                .iter()
                .filter(|o| o.symbol == symbol && o.side == side)
                .map(|o| (o.price, o.leaves))
                .collect()
        };
        Some(LadderInput {
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
                .take(200)
                .map(|p| (p.price, p.qty))
                .collect(),
            last: self.store.last(symbol),
            tick: 1,
        })
    }

    fn active_trader(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let input = self.ladder_input();
        let center = input.as_ref().and_then(|i| i.center());
        let half = (LADDER_ROWS / 2) as u64;
        let needs_center = match (self.ladder_center, center) {
            (None, _) => true,
            (Some(c), Some(now)) => now + half - 3 < c || now > c + half - 3,
            _ => false,
        };
        if needs_center {
            self.ladder_center = center;
        }
        let rows = match (&input, self.ladder_center) {
            (Some(input), Some(c)) => input.rows(c, LADDER_ROWS),
            _ => Vec::new(),
        };
        let drag = self.drag;
        let header = table_header(&t)
            .gap_0()
            .child(th(&t, "Volume", W_VOL, Align::Right).pr_1())
            .child(th(&t, "Buy orders", W_ORD, Align::Center))
            .child(th(&t, "Bid size", W_SIZE, Align::Right).pr_1())
            .child(th(&t, "Price", W_PRICE, Align::Center))
            .child(th(&t, "Ask size", W_SIZE, Align::Left).pl_1())
            .child(th(&t, "Sell orders", W_ORD, Align::Center));
        let mut ladder = body().child(header);
        let qty = |q: u64| {
            if q == 0 {
                String::new()
            } else {
                format::qty(q)
            }
        };
        for (i, r) in rows.iter().enumerate() {
            let price = r.price;
            let bid_w = depth_fraction(r.bid, &rows) * W_SIZE;
            let ask_w = depth_fraction(r.ask, &rows) * W_SIZE;
            let quote_last = r.last;
            let drop_target = |side: Side| drag.is_some_and(|d| d.side == side && d.price != price);
            ladder = ladder.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(18.))
                    .px_2()
                    .text_size(px(12.))
                    .border_b_1()
                    .border_color(t.grid)
                    .child(
                        td(qty(r.traded), W_VOL, Align::Right)
                            .pr_1()
                            .text_color(t.muted),
                    )
                    .child(
                        div()
                            .id(("buy-orders", i))
                            .w(px(W_ORD))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .font_family(MONO)
                            .when(r.own_bid > 0, |d| {
                                d.bg(t.bid_cell)
                                    .text_color(t.bid)
                                    .font_weight(FontWeight::BOLD)
                                    .cursor_grab()
                            })
                            .when(drop_target(Side::Buy), |d| d.hover(|h| h.bg(t.selection)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |app, _, _, cx| {
                                    if r_own(&app.store, Side::Buy, price) {
                                        app.drag = Some(Drag {
                                            side: Side::Buy,
                                            price,
                                        });
                                        cx.notify();
                                    }
                                }),
                            )
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |app, _, _, cx| app.drop_on(Side::Buy, price, cx)),
                            )
                            .child(qty(r.own_bid)),
                    )
                    .child(
                        div()
                            .id(("bid-size", i))
                            .relative()
                            .w(px(W_SIZE))
                            .h_full()
                            .cursor_pointer()
                            .hover(|d| d.bg(t.bid_cell))
                            .when(drop_target(Side::Buy), |d| d.hover(|h| h.bg(t.selection)))
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.ladder_order(Side::Buy, price, cx)
                            }))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |app, _, _, cx| {
                                    if app.drag.is_some() {
                                        app.drop_on(Side::Buy, price, cx)
                                    }
                                }),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .right_0()
                                    .top_0()
                                    .h_full()
                                    .w(px(bid_w))
                                    .bg(t.bid_cell),
                            )
                            .child(
                                td(qty(r.bid), W_SIZE, Align::Right)
                                    .pr_1()
                                    .text_color(t.bid),
                            ),
                    )
                    .child(
                        div()
                            .id(("price", i))
                            .w(px(W_PRICE))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .font_family(MONO)
                            .cursor_pointer()
                            .bg(if quote_last { t.raised } else { t.panel })
                            .text_color(if quote_last { t.last } else { t.strong })
                            .when(quote_last, |d| d.font_weight(FontWeight::BOLD))
                            .on_click(cx.listener(move |app, _, _, cx| app.set_price(price, cx)))
                            .child(format::price(price)),
                    )
                    .child(
                        div()
                            .id(("ask-size", i))
                            .relative()
                            .w(px(W_SIZE))
                            .h_full()
                            .cursor_pointer()
                            .hover(|d| d.bg(t.ask_cell))
                            .when(drop_target(Side::Sell), |d| d.hover(|h| h.bg(t.selection)))
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.ladder_order(Side::Sell, price, cx)
                            }))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |app, _, _, cx| {
                                    if app.drag.is_some() {
                                        app.drop_on(Side::Sell, price, cx)
                                    }
                                }),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .h_full()
                                    .w(px(ask_w))
                                    .bg(t.ask_cell),
                            )
                            .child(td(qty(r.ask), W_SIZE, Align::Left).pl_1().text_color(t.ask)),
                    )
                    .child(
                        div()
                            .id(("sell-orders", i))
                            .w(px(W_ORD))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .font_family(MONO)
                            .when(r.own_ask > 0, |d| {
                                d.bg(t.ask_cell)
                                    .text_color(t.ask)
                                    .font_weight(FontWeight::BOLD)
                                    .cursor_grab()
                            })
                            .when(drop_target(Side::Sell), |d| d.hover(|h| h.bg(t.selection)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |app, _, _, cx| {
                                    if r_own(&app.store, Side::Sell, price) {
                                        app.drag = Some(Drag {
                                            side: Side::Sell,
                                            price,
                                        });
                                        cx.notify();
                                    }
                                }),
                            )
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |app, _, _, cx| {
                                    app.drop_on(Side::Sell, price, cx)
                                }),
                            )
                            .child(qty(r.own_ask)),
                    ),
            );
        }
        let auto = self.auto_send;
        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_2()
            .h(px(28.))
            .border_b_1()
            .border_color(t.border)
            .child(div().text_size(px(11.)).text_color(t.muted).child("Qty"))
            .child(
                div()
                    .w(px(64.))
                    .child(field(&t, "ladder-qty", &self.fields.qty, "qty")),
            )
            .child(segmented(
                &t,
                "auto-send",
                &[(false, "Confirm"), (true, "Auto send")],
                auto,
                {
                    let entity = cx.entity().downgrade();
                    move |value, _, cx| {
                        entity
                            .update(cx, |app, cx| {
                                app.auto_send = value;
                                cx.notify();
                            })
                            .ok();
                    }
                },
            ))
            .child(div().flex_1())
            .child(button(
                &t,
                "recenter",
                "Center (C)",
                Tone::Quiet,
                cx.listener(|app, _, _, cx| {
                    app.ladder_center = None;
                    cx.notify();
                }),
            ));
        let hint = div()
            .flex_none()
            .px_2()
            .h(px(20.))
            .flex()
            .items_center()
            .text_size(px(10.5))
            .text_color(t.faint)
            .child(if drag.is_some() {
                "Release on a new price to move · same price cancels"
            } else {
                "Click bid/ask size to trade · drag your order to move it"
            });
        gadget(&t, "Active Trader", Some(t.links[0]))
            .w(px(W_VOL + 2.0 * W_ORD + 2.0 * W_SIZE + W_PRICE + 18.0))
            .flex_none()
            .child(toolbar)
            .child(ladder)
            .child(hint)
    }

    fn mini_chart(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let Some(symbol) = self.selected else {
            return gadget(&t, "Chart", Some(t.links[0])).flex_1();
        };
        let model = ChartModel {
            candles: self.store.candles(symbol, self.interval),
            last: self.store.last(symbol),
            reference: self.store.instrument(symbol).map(|i| i.reference_price),
            interval: self.interval,
            studies: self.studies,
            hover: self.hover,
            slot: self.slot,
        };
        gadget(
            &t,
            format!("{symbol} · {}", interval_label(self.interval)),
            Some(t.links[0]),
        )
        .flex_1()
        .child(self.chart_area(model, cx))
    }

    pub fn chart_area(&self, model: ChartModel, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        div()
            .id("chart-area")
            .flex_1()
            .min_h_0()
            .p_1()
            .on_mouse_move(cx.listener(|app, event: &MouseMoveEvent, _, cx| {
                app.hover = Some(event.position);
                cx.notify();
            }))
            .on_hover(cx.listener(|app, hovered: &bool, _, cx| {
                if !hovered {
                    app.hover = None;
                    cx.notify();
                }
            }))
            .on_scroll_wheel(cx.listener(|app, event: &ScrollWheelEvent, _, cx| {
                let dy = f32::from(event.delta.pixel_delta(px(16.)).y);
                app.slot = (app.slot * if dy > 0.0 { 1.1 } else { 0.9 }).clamp(3.0, 24.0);
                cx.notify();
            }))
            .child(candle_chart(model, t).size_full())
    }

    fn level_two(&self) -> impl IntoElement {
        let t = self.theme;
        let book = self.selected.and_then(|s| self.store.book(s));
        let mut rows = body().child(
            table_header(&t)
                .child(th(&t, "Bid size", 70., Align::Right))
                .child(th(&t, "Bid", 66., Align::Right))
                .child(th(&t, "Orders", 46., Align::Right))
                .child(div().w(px(16.)))
                .child(th(&t, "Ask", 66., Align::Right))
                .child(th(&t, "Ask size", 70., Align::Right))
                .child(th(&t, "Orders", 46., Align::Right)),
        );
        if let Some(book) = book {
            for i in 0..8 {
                let bid = book.bids.get(i);
                let ask = book.asks.get(i);
                if bid.is_none() && ask.is_none() {
                    break;
                }
                rows = rows.child(
                    table_row(&t, i % 2 == 1)
                        .child(
                            td(
                                bid.map(|l| format::qty(l.qty)).unwrap_or_default(),
                                70.,
                                Align::Right,
                            )
                            .text_color(t.bid),
                        )
                        .child(
                            td(
                                bid.map(|l| format::price(l.price)).unwrap_or_default(),
                                66.,
                                Align::Right,
                            )
                            .text_color(t.strong),
                        )
                        .child(
                            td(
                                bid.map(|l| l.orders.to_string()).unwrap_or_default(),
                                46.,
                                Align::Right,
                            )
                            .text_color(t.muted),
                        )
                        .child(div().w(px(16.)))
                        .child(
                            td(
                                ask.map(|l| format::price(l.price)).unwrap_or_default(),
                                66.,
                                Align::Right,
                            )
                            .text_color(t.strong),
                        )
                        .child(
                            td(
                                ask.map(|l| format::qty(l.qty)).unwrap_or_default(),
                                70.,
                                Align::Right,
                            )
                            .text_color(t.ask),
                        )
                        .child(
                            td(
                                ask.map(|l| l.orders.to_string()).unwrap_or_default(),
                                46.,
                                Align::Right,
                            )
                            .text_color(t.muted),
                        ),
                );
            }
        }
        gadget(&t, "Level II", Some(t.links[0]))
            .h(px(196.))
            .flex_none()
            .child(rows)
    }

    fn order_entry(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let entity = cx.entity().downgrade();
        let side = self.form.side;
        let kind = self.form.kind;
        let tif = self.form.tif;
        let labeled = |label: &'static str, control: gpui::AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(10.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.muted)
                        .child(label),
                )
                .child(control)
        };
        let symbol = self.selected.map(|s| s.to_string()).unwrap_or_default();
        let quote = self.selected.map(|s| self.quote(s));
        let side_control = {
            let entity = entity.clone();
            segmented(
                &t,
                "side",
                &[(Side::Buy, "BUY"), (Side::Sell, "SELL")],
                side,
                move |value, _, cx| {
                    entity
                        .update(cx, |app, cx| {
                            app.form.side = value;
                            cx.notify();
                        })
                        .ok();
                },
            )
        };
        let kind_control = {
            let entity = entity.clone();
            segmented(
                &t,
                "kind",
                &[
                    (Kind::Market, "MKT"),
                    (Kind::Limit, "LMT"),
                    (Kind::Stop, "STP"),
                    (Kind::Bracket, "BRACKET"),
                ],
                kind,
                move |value, _, cx| {
                    entity.update(cx, |app, cx| app.set_kind(value, cx)).ok();
                },
            )
        };
        let tif_control = {
            let entity = entity.clone();
            segmented(
                &t,
                "tif",
                &[(TimeInForce::Day, "DAY"), (TimeInForce::Gtc, "GTC")],
                tif,
                move |value, _, cx| {
                    entity
                        .update(cx, |app, cx| {
                            app.form.tif = value;
                            cx.notify();
                        })
                        .ok();
                },
            )
        };
        let price_label = match kind {
            Kind::Stop => "STOP PRICE",
            Kind::Bracket => "ENTRY PRICE",
            _ => "LIMIT PRICE",
        };
        let price_control = div()
            .flex()
            .gap_1()
            .child(
                div()
                    .w(px(84.))
                    .child(field(&t, "entry-price", &self.fields.price, "price")),
            )
            .child(button(
                &t,
                "px-dn",
                "−",
                Tone::Plain,
                cx.listener(move |app, _, _, cx| {
                    app.sync_form(cx);
                    if let Some(q) = quote {
                        app.form.nudge(-1, q);
                        let text = app.form.price.clone();
                        app.fields.price.update(cx, |s, cx| s.emplace(&text, cx));
                    }
                    cx.notify();
                }),
            ))
            .child(button(
                &t,
                "px-up",
                "+",
                Tone::Plain,
                cx.listener(move |app, _, _, cx| {
                    app.sync_form(cx);
                    if let Some(q) = quote {
                        app.form.nudge(1, q);
                        let text = app.form.price.clone();
                        app.fields.price.update(cx, |s, cx| s.emplace(&text, cx));
                    }
                    cx.notify();
                }),
            ))
            .child(button(
                &t,
                "px-bid",
                "BID",
                Tone::Quiet,
                cx.listener(move |app, _, _, cx| {
                    if let Some(p) = quote.and_then(|q| q.bid) {
                        app.set_price(p, cx);
                    }
                }),
            ))
            .child(button(
                &t,
                "px-ask",
                "ASK",
                Tone::Quiet,
                cx.listener(move |app, _, _, cx| {
                    if let Some(p) = quote.and_then(|q| q.ask) {
                        app.set_price(p, cx);
                    }
                }),
            ));
        let mut row = div()
            .flex()
            .items_end()
            .gap_3()
            .child(labeled("SIDE", side_control.into_any_element()))
            .child(labeled(
                "QTY",
                div()
                    .w(px(72.))
                    .child(field(&t, "entry-qty", &self.fields.qty, "qty"))
                    .into_any_element(),
            ))
            .child(labeled(
                "SYMBOL",
                div()
                    .h(px(24.))
                    .px_2()
                    .flex()
                    .items_center()
                    .font_family(MONO)
                    .font_weight(FontWeight::BOLD)
                    .text_color(t.strong)
                    .child(symbol)
                    .into_any_element(),
            ))
            .child(labeled("ORDER TYPE", kind_control.into_any_element()));
        if kind != Kind::Market {
            row = row.child(labeled(price_label, price_control.into_any_element()));
        }
        if kind == Kind::Limit {
            row = row.child(labeled("TIF", tif_control.into_any_element()));
        }
        if kind == Kind::Bracket {
            row = row
                .child(labeled(
                    "TARGET %",
                    div()
                        .w(px(56.))
                        .child(field(&t, "entry-target", &self.fields.target, "3"))
                        .into_any_element(),
                ))
                .child(labeled(
                    "STOP %",
                    div()
                        .w(px(56.))
                        .child(field(&t, "entry-stop", &self.fields.stop, "2"))
                        .into_any_element(),
                ));
        }
        let send = button(
            &t,
            "confirm-send",
            "Confirm and Send",
            if side == Side::Buy {
                Tone::Buy
            } else {
                Tone::Sell
            },
            cx.listener(|app, _, _, cx| app.open_ticket(None, cx)),
        )
        .h(px(30.))
        .px_4();
        gadget(&t, "Order Entry", Some(t.links[0]))
            .flex_1()
            .min_w_0()
            .child(
                div().flex().flex_col().gap_2().p_2().child(row).child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(send)
                        .children(
                            self.form_error
                                .clone()
                                .map(|e| div().text_color(t.down).child(e)),
                        )
                        .when(self.form_error.is_none(), |d| {
                            d.child(div().text_color(t.faint).child(
                                "Enter or B / S opens the confirmation with brokerage and taxes",
                            ))
                        }),
                ),
            )
    }

    fn working_orders(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let symbol = self.selected;
        let orders: Vec<ExecutionReport> = self
            .store
            .open_orders()
            .into_iter()
            .filter(|o| Some(o.symbol) == symbol)
            .cloned()
            .collect();
        let armed: Vec<ConditionalReport> = self
            .store
            .armed()
            .into_iter()
            .filter(|c| Some(c.symbol) == symbol)
            .cloned()
            .collect();
        let mut rows = body().child(
            table_header(&t)
                .child(th(&t, "Side", 40., Align::Left))
                .child(th(&t, "Qty", 56., Align::Right))
                .child(th(&t, "Type", 0., Align::Left))
                .child(th(&t, "", 54., Align::Right)),
        );
        for (i, o) in orders.iter().enumerate() {
            let id = o.order_id;
            rows = rows.child(
                table_row(&t, i % 2 == 1)
                    .child(
                        td_text(o.side.label().to_uppercase(), 40., Align::Left)
                            .text_color(side_color(&t, o.side)),
                    )
                    .child(td(format::qty(o.leaves), 56., Align::Right))
                    .child(td(
                        format!("LMT {}", format::price(o.price)),
                        0.,
                        Align::Left,
                    ))
                    .child(button(
                        &t,
                        ("wo-cancel", id),
                        "Cancel",
                        Tone::Plain,
                        cx.listener(move |app, _, _, cx| {
                            app.cancel(ome_ui_data::OrderRequest::Cancel(id), cx)
                        }),
                    )),
            );
        }
        for (i, c) in armed.iter().enumerate() {
            let id = c.cond_id;
            rows = rows.child(
                table_row(&t, (orders.len() + i) % 2 == 1)
                    .child(
                        td_text(c.side.label().to_uppercase(), 40., Align::Left)
                            .text_color(side_color(&t, c.side)),
                    )
                    .child(td(format::qty(c.qty), 56., Align::Right))
                    .child(td(c.trigger.describe(), 0., Align::Left).text_color(t.last))
                    .child(button(
                        &t,
                        ("wc-cancel", id),
                        "Cancel",
                        Tone::Plain,
                        cx.listener(move |app, _, _, cx| {
                            app.cancel(ome_ui_data::OrderRequest::CancelConditional(id), cx)
                        }),
                    )),
            );
        }
        gadget(&t, "Working orders", Some(t.links[0]))
            .w(px(380.))
            .flex_none()
            .child(rows)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .justify_end()
                    .p_1()
                    .border_t_1()
                    .border_color(t.border)
                    .child(button(
                        &t,
                        "cancel-all",
                        "Cancel all in symbol",
                        Tone::Plain,
                        cx.listener(|app, _, _, cx| app.cancel_symbol(cx)),
                    )),
            )
    }
}

fn r_own(store: &ome_ui_data::MarketStore, side: Side, price: Price) -> bool {
    store
        .orders
        .values()
        .any(|o| o.side == side && o.price == price)
}

pub fn side_color(t: &ome_ui_kit::Theme, side: Side) -> gpui::Rgba {
    match side {
        Side::Buy => t.up,
        Side::Sell => t.down,
    }
}

pub fn interval_label(interval: u64) -> String {
    match interval {
        i if i < 60_000 => format!("{}s", i / 1_000),
        i if i < 3_600_000 => format!("{}m", i / 60_000),
        i => format!("{}h", i / 3_600_000),
    }
}
