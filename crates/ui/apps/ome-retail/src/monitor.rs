use crate::app::RetailApp;
use crate::trade::side_color;
use gpui::{Context, FontWeight, IntoElement, div, prelude::*, px};
use ome_protocol::{ConditionalReport, ExecutionReport, OrderStatus};
use ome_ui_data::{AccountView, MarketData, OrderRequest};
use ome_ui_kit::format;
use ome_ui_kit::widgets::{
    Align, Tone, body, button, gadget, metric, sub_tabs, table_header, table_row, td, td_text, th,
};

const ACTIVITY: [&str; 3] = ["Working", "Filled", "Cancelled & rejected"];

impl RetailApp {
    pub fn render_monitor(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .gap_1()
            .child(self.account_summary())
            .child(self.position_statement(cx))
            .child(self.activity(cx))
    }

    fn account_summary(&self) -> impl IntoElement {
        let t = self.theme;
        let m = self.money();
        gadget(&t, "Account Info", Some(t.links[0]))
            .flex_none()
            .child(
                div()
                    .flex()
                    .gap_8()
                    .px_3()
                    .py_2()
                    .child(metric(
                        &t,
                        "Net liquidating value",
                        format::money(m.net_liq.max(0) as u64),
                        t.strong,
                    ))
                    .child(metric(&t, "Cash", format::money(m.cash), t.text))
                    .child(metric(
                        &t,
                        "Buying power",
                        format::money(m.buying_power),
                        t.text,
                    ))
                    .child(metric(
                        &t,
                        "Market value",
                        format::money(m.market_value),
                        t.text,
                    ))
                    .child(metric(
                        &t,
                        "P/L day",
                        format::signed_money(m.day_pnl),
                        t.tone(m.day_pnl as f64),
                    ))
                    .child(metric(
                        &t,
                        "P/L open",
                        format::signed_money(m.open_pnl),
                        t.tone(m.open_pnl as f64),
                    ))
                    .child(metric(
                        &t,
                        "Unsettled cash",
                        format::signed_money(m.unsettled),
                        t.muted,
                    )),
            )
    }

    fn position_statement(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let mut rows = body().child(
            table_header(&t)
                .child(th(&t, "Instrument", 90., Align::Left))
                .child(th(&t, "Qty", 70., Align::Right))
                .child(th(&t, "Trade price", 86., Align::Right))
                .child(th(&t, "Mark", 76., Align::Right))
                .child(th(&t, "Mark chg", 76., Align::Right))
                .child(th(&t, "P/L open", 96., Align::Right))
                .child(th(&t, "P/L %", 70., Align::Right))
                .child(th(&t, "P/L day", 96., Align::Right))
                .child(th(&t, "Mark value", 110., Align::Right))
                .child(th(&t, "Unsettled", 80., Align::Right))
                .child(th(&t, "", 0., Align::Right)),
        );
        let mut totals = (0i64, 0i64, 0u64);
        let positions = self
            .store
            .account()
            .map(|a| a.positions.clone())
            .unwrap_or_default();
        for (i, p) in positions
            .iter()
            .filter(|p| p.balance.available + p.balance.reserved > 0)
            .enumerate()
        {
            let qty = p.balance.available + p.balance.reserved;
            let mark = self.store.last(p.symbol).unwrap_or(0);
            let reference = self
                .store
                .instrument(p.symbol)
                .map(|i| i.reference_price)
                .unwrap_or(mark);
            let open = (p.avg_cost > 0).then(|| qty as i64 * (mark as i64 - p.avg_cost as i64));
            let day = (p.day_cost > 0).then(|| qty as i64 * (mark as i64 - p.day_cost as i64));
            let pct = (p.avg_cost > 0).then(|| (mark as f64 / p.avg_cost as f64 - 1.0) * 100.0);
            let chg = mark as i64 - reference as i64;
            totals.0 += open.unwrap_or(0);
            totals.1 += day.unwrap_or(0);
            totals.2 += qty * mark;
            let symbol = p.symbol;
            let signed = |v: Option<i64>| v.map(format::signed_money).unwrap_or_else(|| "-".into());
            rows = rows.child(
                table_row(&t, i % 2 == 1)
                    .id(("position", i))
                    .cursor_pointer()
                    .hover(|d| d.bg(t.raised))
                    .on_click(cx.listener(move |app, _, _, cx| app.select(symbol, cx)))
                    .child(
                        td(symbol.to_string(), 90., Align::Left)
                            .font_weight(FontWeight::BOLD)
                            .text_color(t.strong),
                    )
                    .child(td(format::qty(qty), 70., Align::Right).text_color(t.strong))
                    .child(td(
                        if p.avg_cost > 0 {
                            format::price(p.avg_cost)
                        } else {
                            "-".into()
                        },
                        86.,
                        Align::Right,
                    ))
                    .child(td(format::price(mark), 76., Align::Right).text_color(t.strong))
                    .child(
                        td(
                            format!(
                                "{}{}",
                                if chg >= 0 { "+" } else { "-" },
                                format::price(chg.unsigned_abs())
                            ),
                            76.,
                            Align::Right,
                        )
                        .text_color(t.tone(chg as f64)),
                    )
                    .child(
                        td(signed(open), 96., Align::Right)
                            .text_color(t.tone(open.unwrap_or(0) as f64)),
                    )
                    .child(
                        td(
                            pct.map(format::pct).unwrap_or_else(|| "-".into()),
                            70.,
                            Align::Right,
                        )
                        .text_color(t.tone(pct.unwrap_or(0.0))),
                    )
                    .child(
                        td(signed(day), 96., Align::Right)
                            .text_color(t.tone(day.unwrap_or(0) as f64)),
                    )
                    .child(td(format::money(qty * mark), 110., Align::Right))
                    .child(td(p.unsettled.to_string(), 80., Align::Right).text_color(t.muted))
                    .child(td_text("", 0., Align::Right)),
            );
        }
        rows = rows.child(
            table_row(&t, false)
                .bg(t.header)
                .child(td_text("Total", 90., Align::Left).font_weight(FontWeight::BOLD))
                .child(div().w(px(70. + 86. + 76. + 76. + 3.0 * 12.0)))
                .child(
                    td(format::signed_money(totals.0), 96., Align::Right)
                        .text_color(t.tone(totals.0 as f64)),
                )
                .child(div().w(px(70.)))
                .child(
                    td(format::signed_money(totals.1), 96., Align::Right)
                        .text_color(t.tone(totals.1 as f64)),
                )
                .child(td(format::money(totals.2), 110., Align::Right).text_color(t.strong)),
        );
        gadget(&t, "Position Statement", Some(t.links[0]))
            .flex_1()
            .child(rows)
    }

    fn activity(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let entity = cx.entity().downgrade();
        let tabs = sub_tabs(&t, "activity", &ACTIVITY, self.activity, move |i, _, cx| {
            entity
                .update(cx, |app, cx| {
                    app.activity = i;
                    cx.notify();
                })
                .ok();
        });
        let header = |t: &ome_ui_kit::Theme, last: &'static str| {
            table_header(t)
                .child(th(t, "Time", 70., Align::Left))
                .child(th(t, "Side", 46., Align::Left))
                .child(th(t, "Qty", 70., Align::Right))
                .child(th(t, "Symbol", 70., Align::Left))
                .child(th(t, "Price / condition", 200., Align::Left))
                .child(th(t, last, 0., Align::Left))
                .child(th(t, "", 70., Align::Right))
        };
        let mut rows = body();
        match self.activity {
            0 => {
                rows = rows.child(header(&t, "Status"));
                let orders: Vec<ExecutionReport> =
                    self.store.open_orders().into_iter().cloned().collect();
                let armed: Vec<ConditionalReport> =
                    self.store.armed().into_iter().cloned().collect();
                for (i, o) in orders.iter().enumerate() {
                    let id = o.order_id;
                    rows = rows.child(
                        table_row(&t, i % 2 == 1)
                            .child(
                                td(format::clock(o.timestamp), 70., Align::Left)
                                    .text_color(t.muted),
                            )
                            .child(
                                td_text(o.side.label().to_uppercase(), 46., Align::Left)
                                    .text_color(side_color(&t, o.side)),
                            )
                            .child(td(
                                format!("{}/{}", format::qty(o.leaves), format::qty(o.qty)),
                                70.,
                                Align::Right,
                            ))
                            .child(td(o.symbol.to_string(), 70., Align::Left).text_color(t.strong))
                            .child(td(
                                format!("LMT {}", format::price(o.price)),
                                200.,
                                Align::Left,
                            ))
                            .child(
                                td_text(o.status.label().to_uppercase(), 0., Align::Left)
                                    .text_color(t.accent),
                            )
                            .child(button(
                                &t,
                                ("act-cancel", id),
                                "Cancel",
                                Tone::Plain,
                                cx.listener(move |app, _, _, cx| {
                                    app.cancel(OrderRequest::Cancel(id), cx)
                                }),
                            )),
                    );
                }
                for (i, c) in armed.iter().enumerate() {
                    let id = c.cond_id;
                    rows = rows.child(
                        table_row(&t, (orders.len() + i) % 2 == 1)
                            .child(td_text("", 70., Align::Left))
                            .child(
                                td_text(c.side.label().to_uppercase(), 46., Align::Left)
                                    .text_color(side_color(&t, c.side)),
                            )
                            .child(td(format::qty(c.qty), 70., Align::Right))
                            .child(td(c.symbol.to_string(), 70., Align::Left).text_color(t.strong))
                            .child(td(c.trigger.describe(), 200., Align::Left).text_color(t.last))
                            .child(td_text("ARMED", 0., Align::Left).text_color(t.last))
                            .child(button(
                                &t,
                                ("act-cond", id),
                                "Cancel",
                                Tone::Plain,
                                cx.listener(move |app, _, _, cx| {
                                    app.cancel(OrderRequest::CancelConditional(id), cx)
                                }),
                            )),
                    );
                }
            }
            1 => {
                rows = rows.child(
                    table_header(&t)
                        .child(th(&t, "Time", 70., Align::Left))
                        .child(th(&t, "Side", 46., Align::Left))
                        .child(th(&t, "Qty", 70., Align::Right))
                        .child(th(&t, "Symbol", 70., Align::Left))
                        .child(th(&t, "Price", 80., Align::Right))
                        .child(th(&t, "Charges", 90., Align::Right))
                        .child(th(&t, "Net amount", 110., Align::Right))
                        .child(th(&t, "", 0., Align::Left)),
                );
                for (i, n) in self.store.contract_notes().into_iter().take(80).enumerate() {
                    rows = rows.child(
                        table_row(&t, i % 2 == 1)
                            .child(
                                td(format::clock(n.timestamp), 70., Align::Left)
                                    .text_color(t.muted),
                            )
                            .child(
                                td_text(n.side.label().to_uppercase(), 46., Align::Left)
                                    .text_color(side_color(&t, n.side)),
                            )
                            .child(td(format::qty(n.qty), 70., Align::Right))
                            .child(td(n.symbol.to_string(), 70., Align::Left).text_color(t.strong))
                            .child(td(format::price(n.price), 80., Align::Right))
                            .child(
                                td(
                                    format::money(n.charges.total().unsigned_abs()),
                                    90.,
                                    Align::Right,
                                )
                                .text_color(t.muted),
                            )
                            .child(
                                td(format::signed_money(n.net_amount), 110., Align::Right)
                                    .text_color(t.tone(n.net_amount as f64)),
                            )
                            .child(td_text("", 0., Align::Left)),
                    );
                }
            }
            _ => {
                rows = rows.child(header(&t, "Reason"));
                let closed: Vec<ExecutionReport> = self
                    .store
                    .closed_orders()
                    .into_iter()
                    .filter(|o| o.status != OrderStatus::Filled)
                    .cloned()
                    .collect();
                for (i, o) in closed.iter().take(80).enumerate() {
                    rows = rows.child(
                        table_row(&t, i % 2 == 1)
                            .child(
                                td(format::clock(o.timestamp), 70., Align::Left)
                                    .text_color(t.muted),
                            )
                            .child(
                                td_text(o.side.label().to_uppercase(), 46., Align::Left)
                                    .text_color(side_color(&t, o.side)),
                            )
                            .child(td(
                                format!("{}/{}", format::qty(o.filled), format::qty(o.qty)),
                                70.,
                                Align::Right,
                            ))
                            .child(td(o.symbol.to_string(), 70., Align::Left).text_color(t.strong))
                            .child(td(
                                format!(
                                    "{} {}",
                                    o.status.label().to_uppercase(),
                                    if o.price > 0 {
                                        format::price(o.price)
                                    } else {
                                        "MKT".into()
                                    }
                                ),
                                200.,
                                Align::Left,
                            ))
                            .child(td_text(o.reason.clone(), 0., Align::Left).text_color(
                                if o.status == OrderStatus::Rejected {
                                    t.down
                                } else {
                                    t.muted
                                },
                            ))
                            .child(td_text("", 70., Align::Right)),
                    );
                }
            }
        }
        gadget(&t, "Activity and Positions", Some(t.links[0]))
            .h(px(260.))
            .flex_none()
            .child(tabs)
            .child(rows)
    }
}
