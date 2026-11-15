use crate::app::{RetailApp, Tab, now_ms};
use gpui::{AnyElement, Context, FontWeight, IntoElement, Window, div, prelude::*, px};
use ome_core::types::Side;
use ome_protocol::{InstrumentStatus, MarketPhase, NewsKind};
use ome_ui_data::{AccountView, Connection, MarketData, NewsFeed};
use ome_ui_kit::format;
use ome_ui_kit::theme::MONO;
use ome_ui_kit::widgets::{
    Align, Tone, badge, body, button, dialog, gadget, kv, main_tabs, metric, overlay, table_header,
    table_row, td, td_text, th,
};

pub struct Money {
    pub net_liq: i64,
    pub cash: u64,
    pub buying_power: u64,
    pub day_pnl: i64,
    pub open_pnl: i64,
    pub unsettled: i64,
    pub market_value: u64,
}

impl RetailApp {
    pub fn money(&self) -> Money {
        let Some(account) = self.store.account() else {
            return Money {
                net_liq: 0,
                cash: 0,
                buying_power: 0,
                day_pnl: 0,
                open_pnl: 0,
                unsettled: 0,
                market_value: 0,
            };
        };
        let mut market_value = 0u64;
        let mut open_pnl = 0i64;
        for p in &account.positions {
            let qty = p.balance.available + p.balance.reserved;
            let mark = self.store.last(p.symbol).unwrap_or(0);
            market_value += qty * mark;
            if p.avg_cost > 0 {
                open_pnl += qty as i64 * (mark as i64 - p.avg_cost as i64);
            }
        }
        let cash = account.cash.available + account.cash.reserved;
        Money {
            net_liq: (cash + market_value) as i64,
            cash,
            buying_power: account.cash.available,
            day_pnl: account.day_pnl,
            open_pnl,
            unsettled: account.unsettled_cash,
            market_value,
        }
    }

    pub fn render_trading(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        let content = match self.tab {
            Tab::Monitor => self.render_monitor(cx).into_any_element(),
            Tab::Trade => self.render_trade(cx).into_any_element(),
            Tab::Charts => self.render_charts(cx).into_any_element(),
            Tab::News => self.render_news(cx).into_any_element(),
        };
        let labels: Vec<&'static str> = Tab::ALL.iter().map(|t| t.label()).collect();
        let active = Tab::ALL.iter().position(|x| *x == self.tab).unwrap_or(0);
        let entity = cx.entity().downgrade();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.header(cx))
            .child(main_tabs(&t, &labels, active, move |i, _, cx| {
                entity.update(cx, |app, cx| app.show(Tab::ALL[i], cx)).ok();
            }))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap_1()
                    .p_1()
                    .child(self.sidebar(cx))
                    .child(div().flex().flex_col().flex_1().min_w_0().child(content)),
            )
            .child(self.status_bar())
            .children(self.confirm_dialog(window, cx))
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let m = self.money();
        let (account, dot) = match self.store.connection {
            Connection::Online { account, role } => (
                format!("{} · {} · #{account}", self.user, role.label()),
                t.up,
            ),
            Connection::Connecting => (self.user.clone(), t.warning),
            Connection::Offline => (format!("{} · reconnecting", self.user), t.down),
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_5()
            .px_3()
            .h(px(46.))
            .bg(t.panel)
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(24.))
                            .h(px(24.))
                            .rounded(px(3.))
                            .bg(t.selection)
                            .flex()
                            .items_center()
                            .justify_center()
                            .font_weight(FontWeight::BOLD)
                            .text_color(t.strong)
                            .child("O"),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(t.strong)
                            .child("OME Trader"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(px(26.))
                    .rounded(px(3.))
                    .bg(t.field)
                    .border_1()
                    .border_color(t.border)
                    .child(div().w(px(7.)).h(px(7.)).rounded_full().bg(dot))
                    .child(div().text_size(px(12.)).child(account)),
            )
            .child(metric(
                &t,
                "Net liq",
                format::signed_money(m.net_liq)
                    .trim_start_matches('+')
                    .to_string(),
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
                "Day P/L",
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
                "Unsettled",
                format::signed_money(m.unsettled),
                t.muted,
            ))
            .child(div().flex_1())
            .child(self.phase_badge())
            .child(button(
                &t,
                "logout",
                "Log out",
                Tone::Plain,
                cx.listener(|app, _, window, cx| app.log_out(window, cx)),
            ))
    }

    fn phase_badge(&self) -> impl IntoElement {
        let t = self.theme;
        let Some(status) = self.store.status else {
            return badge("NO SESSION", t.muted, t.raised);
        };
        let (label, fg, bg) = match status.phase {
            MarketPhase::Open => ("MARKET OPEN", t.background, t.up),
            MarketPhase::PreOpen => ("PRE-OPEN", t.background, t.warning),
            MarketPhase::ClosingAuction => ("CLOSING AUCTION", t.background, t.warning),
            MarketPhase::Closed => ("CLOSED", t.strong, t.raised),
        };
        let left = status
            .next_change_at
            .checked_sub(now_ms())
            .filter(|_| status.next_change_at > 0)
            .map(|ms| format!(" · {}s", ms / 1_000))
            .unwrap_or_default();
        badge(format!("{label}{left}"), fg, bg)
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(318.))
            .gap_1()
            .child(self.watchlist(cx))
            .child(self.time_and_sales())
            .child(self.live_news(cx))
    }

    fn watchlist(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let mut rows = body().child(
            table_header(&t)
                .child(th(&t, "Symbol", 50., Align::Left))
                .child(th(&t, "Last", 60., Align::Right))
                .child(th(&t, "Net chg", 52., Align::Right))
                .child(th(&t, "% chg", 56., Align::Right))
                .child(th(&t, "", 0., Align::Right)),
        );
        for (i, symbol) in self.store.symbols().into_iter().enumerate() {
            let info = self.store.instrument(symbol);
            let last = self.store.last(symbol);
            let reference = info.map(|i| i.reference_price).unwrap_or(0);
            let net = last.map(|l| l as i64 - reference as i64).unwrap_or(0);
            let pct = last
                .and_then(|l| format::change_pct(reference, l))
                .unwrap_or(0.0);
            let selected = Some(symbol) == self.selected;
            let halted = info.is_some_and(|i| i.status != InstrumentStatus::Trading);
            let flash = self.flash(symbol);
            rows = rows.child(
                table_row(&t, i % 2 == 1)
                    .id(("watch", i))
                    .h(px(24.))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(t.selection))
                    .hover(|d| d.bg(t.raised))
                    .on_click(cx.listener(move |app, _, _, cx| app.select(symbol, cx)))
                    .child(
                        td(symbol.to_string(), 50., Align::Left)
                            .font_weight(FontWeight::BOLD)
                            .text_color(t.strong),
                    )
                    .child(
                        td(format::price_or_dash(last), 60., Align::Right)
                            .px_1()
                            .when_some(flash, |d, up| {
                                d.bg(if up { t.up_flash } else { t.down_flash })
                            })
                            .text_color(t.strong),
                    )
                    .child(
                        td(
                            format!(
                                "{}{}",
                                if net >= 0 { "+" } else { "-" },
                                format::price(net.unsigned_abs())
                            ),
                            52.,
                            Align::Right,
                        )
                        .text_color(t.tone(net as f64)),
                    )
                    .child(td(format::pct(pct), 56., Align::Right).text_color(t.tone(pct)))
                    .child(
                        td_text(if halted { "HALT" } else { "" }, 0., Align::Right)
                            .text_size(px(10.))
                            .text_color(t.warning),
                    ),
            );
        }
        gadget(&t, "Watchlist", Some(t.links[0]))
            .h(px(170.))
            .child(rows)
    }

    fn time_and_sales(&self) -> impl IntoElement {
        let t = self.theme;
        let title = format!(
            "Time & Sales{}",
            self.selected.map(|s| format!(" · {s}")).unwrap_or_default()
        );
        let mut rows = body().child(
            table_header(&t)
                .child(th(&t, "Time", 62., Align::Left))
                .child(th(&t, "Price", 64., Align::Right))
                .child(th(&t, "Size", 52., Align::Right))
                .child(th(&t, "Venue", 0., Align::Left)),
        );
        if let Some(symbol) = self.selected {
            for (i, print) in self.store.tape(symbol).into_iter().take(60).enumerate() {
                let color = match print.aggressor {
                    Side::Buy => t.up,
                    Side::Sell => t.down,
                };
                rows = rows.child(
                    table_row(&t, i % 2 == 1)
                        .h(px(18.))
                        .child(
                            td(format::clock(print.timestamp), 62., Align::Left)
                                .text_color(t.muted),
                        )
                        .child(td(format::price(print.price), 64., Align::Right).text_color(color))
                        .child(td(format::qty(print.qty), 52., Align::Right).text_color(t.strong))
                        .child(td_text(print.venue.label(), 0., Align::Left).text_color(t.muted)),
                );
            }
        }
        gadget(&t, title, Some(t.links[0])).flex_1().child(rows)
    }

    fn live_news(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let mut rows = body();
        for (i, item) in self.store.news().into_iter().take(30).enumerate() {
            let seq = item.seq;
            let tag = match item.kind {
                NewsKind::Macro => "ECON".to_string(),
                NewsKind::Sector => item.sector.to_uppercase(),
                _ => item.symbol.map(|s| s.to_string()).unwrap_or_default(),
            };
            rows = rows.child(
                div()
                    .id(("side-news", i))
                    .flex()
                    .flex_col()
                    .gap_px()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(t.grid)
                    .cursor_pointer()
                    .hover(|d| d.bg(t.raised))
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.news_selected = Some(seq);
                        app.show(Tab::News, cx);
                    }))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .text_size(px(10.5))
                            .child(
                                div()
                                    .font_family(MONO)
                                    .text_color(t.muted)
                                    .child(format::clock(item.time)),
                            )
                            .child(
                                div()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(t.accent)
                                    .child(tag),
                            )
                            .child(
                                div()
                                    .font_family(MONO)
                                    .text_color(t.tone(item.sentiment as f64))
                                    .child(format!("{:+}", item.sentiment)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(t.strong)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(item.headline.clone()),
                    ),
            );
        }
        gadget(&t, "Live News", Some(t.links[1]))
            .h(px(240.))
            .child(rows)
    }

    fn status_bar(&self) -> impl IntoElement {
        let t = self.theme;
        let notice = self.store.notices().into_iter().next().cloned();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_4()
            .px_3()
            .h(px(22.))
            .bg(t.panel)
            .border_t_1()
            .border_color(t.border)
            .text_size(px(11.))
            .children(notice.map(|n| {
                div()
                    .text_color(if n.error { t.down } else { t.muted })
                    .child(n.text)
            }))
            .child(div().flex_1())
            .child(
                div()
                    .text_color(t.faint)
                    .child("F1–F4 tabs · ↑↓ symbol · B/S ticket · Enter confirm · Esc cancel · C recenter ladder"),
            )
            .child(
                div()
                    .font_family(MONO)
                    .text_color(t.muted)
                    .child(format!("{} UTC", format::clock(now_ms()))),
            )
    }

    fn confirm_dialog(&self, _window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.theme;
        let pending = self.pending.clone()?;
        let estimate = self
            .store
            .estimate
            .filter(|e| e.side == pending.side && e.qty == pending.qty && e.price == pending.price);
        let side_color = if pending.side == Side::Buy {
            t.up
        } else {
            t.down
        };
        let principal = pending.price * pending.qty;
        let mut sheet = div()
            .flex()
            .flex_col()
            .px_4()
            .py_3()
            .child(
                div()
                    .text_size(px(14.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(side_color)
                    .font_family(MONO)
                    .child(pending.text.clone()),
            )
            .child(div().h(px(10.)))
            .child(kv(
                &t,
                "Price used for the estimate",
                format::price(pending.price),
                t.text,
            ))
            .child(kv(&t, "Quantity", format::qty(pending.qty), t.text))
            .child(kv(&t, "Principal", format::money(principal), t.strong));
        match estimate {
            Some(e) => {
                let c = e.charges;
                sheet = sheet
                    .child(kv(&t, "Brokerage", format::money(c.commission), t.text))
                    .child(kv(
                        &t,
                        "Exchange fee (estimate)",
                        format::signed_money(c.exchange_fee),
                        t.text,
                    ))
                    .child(kv(
                        &t,
                        "Transaction tax",
                        format::money(c.transaction_tax),
                        t.text,
                    ))
                    .child(kv(&t, "Stamp duty", format::money(c.stamp_duty), t.text))
                    .child(kv(
                        &t,
                        "Regulatory fee",
                        format::money(c.regulatory_fee),
                        t.text,
                    ))
                    .child(kv(&t, "GST", format::money(c.gst), t.text))
                    .child(kv(
                        &t,
                        "Total charges",
                        format::money(c.total().unsigned_abs()),
                        t.strong,
                    ))
                    .child(
                        kv(
                            &t,
                            "Cash effect",
                            format::signed_money(e.cash_effect),
                            t.tone(e.cash_effect as f64),
                        )
                        .mt_1()
                        .border_t_1()
                        .border_color(t.border),
                    )
                    .child(kv(
                        &t,
                        "Buying power after",
                        format::signed_money(
                            self.money().buying_power as i64 + e.cash_effect.min(0),
                        ),
                        t.text,
                    ));
            }
            None => {
                sheet = sheet.child(
                    div()
                        .py_2()
                        .text_color(t.muted)
                        .child("Requesting brokerage and tax estimate…"),
                );
            }
        }
        let request = pending.request;
        let actions = div()
            .flex()
            .justify_end()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(t.border)
            .child(button(
                &t,
                "edit-order",
                "Edit (Esc)",
                Tone::Plain,
                cx.listener(|app, _, _, cx| {
                    app.pending = None;
                    cx.notify();
                }),
            ))
            .child(button(
                &t,
                "send-order",
                "Send (Enter)",
                if pending.side == Side::Buy {
                    Tone::Buy
                } else {
                    Tone::Sell
                },
                cx.listener(move |app, _, _, cx| app.send(request, cx)),
            ));
        Some(
            overlay(&t)
                .child(
                    dialog(&t, "Order Confirmation", 460.)
                        .child(sheet)
                        .child(actions),
                )
                .into_any_element(),
        )
    }
}
