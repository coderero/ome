use crate::app::{Core, Drag, Scope};
use crate::ticket::{Kind, Quote};
use crate::view::{Gadget, Kind as Gk, Link, Side as Gadgetry, Tab, View};
use egui::{Align, Color32, CornerRadius, Label, Layout, Margin, RichText, Sense, Stroke, TextEdit, Ui, Vec2};
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{Side, TimeInForce};
use ome_protocol::{InstrumentStatus, NewsKind, OrderStatus};
use ome_ui::Theme;
use ome_ui::chart::Chart;
use ome_ui::format;
use ome_ui::ladder::LadderInput;
use ome_ui::table::{self, Align as Cell, Col, Grid, HEAD, customize};
use ome_ui::widgets::{
    badge, empty_state, filled_button, header_strip, kv_row, padded, plain_button, section, segmented, toolbar, value,
};
use ome_ui_data::{AccountView, MarketData, NewsFeed, OrderRequest};

const LADDER_ROW: f32 = 19.0;

fn plain(text: impl Into<String>, color: Color32) -> RichText {
    RichText::new(text.into()).color(color)
}

fn bold(text: impl Into<String>, color: Color32) -> RichText {
    RichText::new(text.into()).font(Theme::bold(13.0)).color(color)
}

fn signed_price(value: i64) -> String {
    format!("{}{}", if value >= 0 { "+" } else { "-" }, format::price(value.unsigned_abs()))
}

fn side_text(t: &Theme, side: Side) -> RichText {
    match side {
        Side::Buy => plain("BUY", t.up),
        Side::Sell => plain("SELL", t.down),
    }
}

fn signed_qty(side: Side, qty: u64) -> String {
    format!("{}{}", if side == Side::Buy { "+" } else { "-" }, format::qty(qty))
}

fn fold(view: &mut View, t: &Theme, ui: &mut Ui, title: &str, menu: bool, inline: impl FnOnce(&mut Ui)) -> bool {
    let mut open = view.is_open(title);
    section(ui, t, title, &mut open, menu, inline);
    view.set_open(title, open);
    open
}

fn tint(t: &Theme, side: Side) -> Option<Color32> {
    Some(if side == Side::Buy { t.buy_row } else { t.sell_row })
}

fn link_chip(ui: &mut Ui, t: &Theme, link: &mut Link) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(16.0, 16.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Symbol link"));
    ui.painter().rect_filled(rect.shrink(2.0), CornerRadius::same(2), link.color());
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "S", Theme::bold(9.5), t.base);
    ome_ui::widgets::corner_mark(ui.painter(), rect.expand(2.0), t.muted);
    let response = response.on_hover_text(format!("Linked to {}: gadgets with the same colour show the same symbol", link.label()));
    egui::Popup::menu(&response).show(|ui| {
        if let Some(chosen) = crate::desk::link_menu(ui, *link) {
            *link = chosen;
        }
    });
}

fn scrolled(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    let width = ui.available_width();
    egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show(ui, |ui| {
        ui.set_max_width(width);
        ui.spacing_mut().item_spacing.y = 0.0;
        add(ui);
    });
}

impl Core {
    pub fn sidebar(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let missing: Vec<Gadgetry> = Gadgetry::ALL.iter().copied().filter(|g| !self.view.sidebar.contains(g)).collect();
        egui::Panel::bottom("sidebar-footer")
            .resizable(false)
            .frame(egui::Frame::new().fill(t.tab_bar).inner_margin(Margin::symmetric(6, 2)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let add = ui.add_enabled(!missing.is_empty(), egui::Button::new(plain("+", t.text)).frame(false)).on_hover_text("Add gadget");
                    add.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Add gadget to the sidebar"));
                    egui::Popup::menu(&add).show(|ui| {
                        for g in &missing {
                            if ui.button(g.title()).clicked() {
                                self.view.sidebar.push(*g);
                                ui.close();
                            }
                        }
                    });
                    if ui.add(egui::Button::new(plain("⚙", t.text)).frame(false)).on_hover_text("Setup").clicked() {
                        self.open_settings();
                    }
                });
            });
        let sides = self.view.sidebar.clone();
        scrolled(ui, "sidebar", |ui| {
            for side in sides {
                let mut open = self.view.is_open(side.title());
                let inline_title = matches!(side, Gadgetry::Watchlist);
                let strip = section(ui, &t, side.title(), &mut open, true, |ui| {
                    if inline_title {
                        ui.label(plain("Main Watchlist", t.text));
                    }
                });
                self.view.set_open(side.title(), open);
                if let Some(menu) = &strip.menu {
                    egui::Popup::menu(menu).show(|ui| {
                        if ui.button("Move up").clicked() {
                            self.view.move_side(side, -1);
                            ui.close();
                        }
                        if ui.button("Move down").clicked() {
                            self.view.move_side(side, 1);
                            ui.close();
                        }
                        if ui.button("Remove gadget").clicked() {
                            self.view.sidebar.retain(|s| *s != side);
                            ui.close();
                        }
                    });
                }
                if !open {
                    continue;
                }
                match side {
                    Gadgetry::AccountInfo => self.account_info(ui),
                    Gadgetry::Watchlist => self.watchlist(ui),
                    Gadgetry::Quote => self.quote_details(ui),
                    Gadgetry::LiveNews => self.live_news(ui),
                }
            }
        });
    }

    fn account_info(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let m = self.money();
        kv_row(ui, &t, "Net Liq & Day P/L", &format!("{}   {}", format::money(m.net_liq.max(0) as u64), format::signed_money(m.day_pnl)), t.strong);
        kv_row(ui, &t, "Stock Buying Power", &format::money(m.buying_power), t.strong);
        kv_row(ui, &t, "Cash", &format::signed_money(m.cash), t.strong);
        kv_row(ui, &t, "Unsettled Cash", &format::signed_money(m.unsettled), t.tone(m.unsettled as f64));
        kv_row(ui, &t, "Long Stock Value", &format::money(m.value), t.strong);
        kv_row(ui, &t, "P/L Open", &format::signed_money(m.open_pnl), t.tone(m.open_pnl as f64));
    }

    fn quote_details(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let Some(symbol) = self.selected else {
            return empty_state(ui, &t, "No symbol selected", "Pick one in the watchlist.");
        };
        let quote = self.quote(symbol);
        let stats = self.store.stats(symbol).copied();
        let reference = self.store.instrument(symbol).map(|i| i.reference_price).unwrap_or(0);
        let last = self.store.last(symbol);
        let net = last.map(|l| l as i64 - reference as i64).unwrap_or(0);
        kv_row(ui, &t, &format!("{symbol}  Last"), &format::price_or_dash(last), t.strong);
        kv_row(ui, &t, "Net Chng", &signed_price(net), t.tone(net as f64));
        kv_row(ui, &t, "Bid / Ask", &format!("{} / {}", format::price_or_dash(quote.bid), format::price_or_dash(quote.ask)), t.strong);
        kv_row(ui, &t, "Prev Close", &format::price(reference), t.strong);
        if let Some(s) = stats {
            kv_row(ui, &t, "Open", &format::price(s.open), t.strong);
            kv_row(ui, &t, "High / Low", &format!("{} / {}", format::price(s.high), format::price(s.low)), t.strong);
            kv_row(ui, &t, "Volume", &format::qty(s.exchange_volume + s.internal_volume + s.wholesaler_volume + s.ats_volume), t.strong);
            kv_row(ui, &t, "VWAP", &format::price(s.vwap), t.strong);
        }
    }

    fn quote_cells(&self, ui: &mut Ui, t: &Theme, symbol: Symbol, last_col: usize, net_col: usize, col: usize) -> bool {
        let reference = self.store.instrument(symbol).map(|i| i.reference_price).unwrap_or(0);
        let last = self.store.last(symbol);
        let net = last.map(|l| l as i64 - reference as i64).unwrap_or(0);
        if col == last_col {
            let fill = match self.tick(symbol) {
                Some(true) => Some(t.up_fill),
                Some(false) => Some(t.tick_down_fill),
                None => None,
            };
            if let Some(color) = fill {
                table::fill(ui, color);
            }
            ui.label(plain(format::price_or_dash(last), if fill.is_some() { t.on_fill } else { t.strong }));
            return true;
        }
        if col == net_col {
            let fill = match net {
                n if n > 0 => Some(t.up_fill),
                n if n < 0 => Some(t.down_fill),
                _ => None,
            };
            if let Some(color) = fill {
                table::fill(ui, color);
            }
            ui.label(plain(signed_price(net), if fill.is_some() { t.on_fill } else { t.text }));
            return true;
        }
        false
    }

    fn watchlist(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let symbols = self.store.symbols();
        if symbols.is_empty() {
            return empty_state(ui, &t, "No instruments yet", "Listed symbols appear here.");
        }
        let selected = self.selected.and_then(|s| symbols.iter().position(|x| *x == s));
        let cols = [Col::left("Symbol", 76.0), Col::fill("Last", Cell::Right), Col::right("Net Chng", 74.0), Col::right("% Change", 70.0)];
        let chosen = Grid::new("watchlist", &cols, symbols.len()).selected(selected).show(ui, &t, |ui, row, col| {
            let symbol = symbols[row];
            if self.quote_cells(ui, &t, symbol, 1, 2, col) {
                return;
            }
            let info = self.store.instrument(symbol);
            let reference = info.map(|i| i.reference_price).unwrap_or(0);
            let last = self.store.last(symbol);
            let net = last.map(|l| l as i64 - reference as i64).unwrap_or(0);
            if col == 0 {
                let name = info.map(|i| i.name.clone()).unwrap_or_default();
                ui.add(Label::new(plain(symbol.to_string(), t.tone(net as f64))).selectable(false)).on_hover_text(name);
                if info.is_some_and(|i| i.status != InstrumentStatus::Trading) {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| badge(ui, "H", t.base, t.warning).on_hover_text("Halted"));
                }
            } else {
                let pct = last.and_then(|l| format::change_pct(reference, l)).unwrap_or(0.0);
                ui.label(plain(format::pct(pct), t.tone(pct)));
            }
        });
        if let Some(row) = chosen {
            self.select(symbols[row]);
        }
    }

    fn live_news(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let items: Vec<_> = self.store.news().into_iter().take(15).cloned().collect();
        if items.is_empty() {
            return empty_state(ui, &t, "No news yet", "Stories from the newsroom appear here.");
        }
        let cols = [Col::left("Time", 64.0), Col::fill("Headline", Cell::Left)];
        let chosen = Grid::new("live-news", &cols, items.len()).selected(None).show(ui, &t, |ui, row, col| {
            let item = &items[row];
            if col == 0 {
                ui.label(plain(&format::clock(item.time)[..5], t.muted));
            } else {
                ui.add(Label::new(plain(&item.headline, t.text)).selectable(false).truncate());
            }
        });
        if let Some(row) = chosen {
            self.news_selected = Some(items[row].seq);
            self.view.focus(Tab::MarketWatch, Gk::News);
        }
    }

    pub fn symbol_for(&self, g: &Gadget) -> Option<Symbol> {
        self.groups.get(&g.link).copied().or(self.selected)
    }

    pub fn gadget(&mut self, ui: &mut Ui, g: &mut Gadget) {
        let primary = self.selected;
        let symbol = self.symbol_for(g);
        let interval = self.interval;
        self.selected = symbol;
        self.interval = g.interval;
        self.link = g.link;
        match g.kind {
            Gk::Activity => scrolled(ui, &format!("activity-{}", g.id), |ui| self.activity(ui)),
            Gk::Statement => scrolled(ui, &format!("statement-{}", g.id), |ui| self.statement(ui)),
            Gk::AllProducts => scrolled(ui, &format!("products-{}", g.id), |ui| self.all_products(ui)),
            Gk::ActiveTrader => self.active_trader(ui, g.id),
            Gk::Chart => self.charts(ui, g.id),
            Gk::QuoteBoard => scrolled(ui, &format!("quotes-{}", g.id), |ui| self.quotes(ui)),
            Gk::News => self.news_page(ui),
            Gk::LevelTwo => scrolled(ui, &format!("l2-{}", g.id), |ui| self.level_two(ui)),
            Gk::TimeSales => scrolled(ui, &format!("ts-{}", g.id), |ui| self.time_sales(ui, 200)),
            Gk::Watchlist => scrolled(ui, &format!("wl-{}", g.id), |ui| self.watchlist(ui)),
        }
        g.interval = self.interval;
        g.link = self.link;
        let chosen = self.selected;
        self.interval = interval;
        if chosen != symbol
            && let Some(chosen) = chosen
        {
            self.groups.insert(g.link, chosen);
            self.selected = if g.link == Link::Red { Some(chosen) } else { primary };
        } else {
            self.selected = primary;
        }
    }

    fn table_section(&mut self, ui: &mut Ui, title: &str, table: &str, cols: &[Col<'_>], inline: impl FnOnce(&mut Ui)) -> bool {
        self.table_section_with(ui, title, table, cols, inline, |_| {})
    }

    fn table_section_with(
        &mut self,
        ui: &mut Ui,
        title: &str,
        table: &str,
        cols: &[Col<'_>],
        inline: impl FnOnce(&mut Ui),
        right: impl FnOnce(&mut Ui),
    ) -> bool {
        let t = self.theme;
        let mut hidden = std::mem::take(&mut self.view.hidden);
        let open = fold(&mut self.view, &t, ui, title, false, |ui| {
            inline(ui);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                customize(ui, &t, table, cols, &mut hidden);
                right(ui);
            });
        });
        self.view.hidden = hidden;
        open
    }

    fn activity(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let orders: Vec<_> = self.store.open_orders().into_iter().cloned().collect();
        let armed: Vec<_> = self.store.armed().into_iter().cloned().collect();
        let working = orders.len() + armed.len();
        let mut cancel: Option<OrderRequest> = None;
        let cols = [
            Col::left("Time Placed", 84.0),
            Col::left("Side", 52.0),
            Col::right("Qty", 74.0),
            Col::left("Symbol", 70.0),
            Col::left("Type", 56.0),
            Col::fill("Price", Cell::Left),
            Col::left("Status", 84.0),
            Col::center("", 74.0),
        ];
        let tints: Vec<Option<Color32>> = orders.iter().map(|o| tint(&t, o.side)).chain(armed.iter().map(|c| tint(&t, c.side))).collect();
        if self.table_section(ui, "Working Orders", "working", &cols, |ui| {
            ui.label(plain(format!("{working}"), t.muted));
        }) {
            if working == 0 {
                empty_state(ui, &t, "No working orders", "Orders resting in the market or waiting for a trigger appear here.");
            } else {
                Grid::new("working", &cols, working).tints(&tints).hidden(&self.view.hidden).show(ui, &t, |ui, row, col| {
                    if row < orders.len() {
                        let o = &orders[row];
                        match col {
                            0 => drop(ui.label(plain(format::clock(o.timestamp), t.text))),
                            1 => drop(ui.label(side_text(&t, o.side))),
                            2 => drop(ui.label(plain(format!("{}/{}", signed_qty(o.side, o.leaves), format::qty(o.qty)), t.text))),
                            3 => drop(ui.label(plain(o.symbol.to_string(), t.text))),
                            4 => drop(ui.label(plain("LMT", t.text))),
                            5 => drop(ui.label(plain(format::price(o.price), t.text))),
                            6 => drop(ui.label(plain("WORKING", t.accent))),
                            _ => {
                                if ui.add(egui::Button::new("Cancel").small()).clicked() {
                                    cancel = Some(OrderRequest::Cancel(o.order_id));
                                }
                            }
                        }
                    } else {
                        let c = &armed[row - orders.len()];
                        match col {
                            0 => drop(ui.label(plain("-", t.muted))),
                            1 => drop(ui.label(side_text(&t, c.side))),
                            2 => drop(ui.label(plain(signed_qty(c.side, c.qty), t.text))),
                            3 => drop(ui.label(plain(c.symbol.to_string(), t.text))),
                            4 => drop(ui.label(plain("COND", t.text))),
                            5 => drop(ui.label(plain(c.trigger.describe(), t.text))),
                            6 => drop(ui.label(plain("ARMED", t.last))),
                            _ => {
                                if ui.add(egui::Button::new("Cancel").small()).clicked() {
                                    cancel = Some(OrderRequest::CancelConditional(c.cond_id));
                                }
                            }
                        }
                    }
                });
            }
        }
        if let Some(request) = cancel {
            self.submit(request);
        }
        let notes: Vec<_> = self.store.contract_notes().into_iter().take(100).cloned().collect();
        let cols = [
            Col::left("Exec Time", 84.0),
            Col::left("Side", 52.0),
            Col::right("Qty", 74.0),
            Col::left("Symbol", 70.0),
            Col::right("Price", 80.0),
            Col::right("Charges", 80.0),
            Col::fill("Net Amount", Cell::Right),
        ];
        let tints: Vec<Option<Color32>> = notes.iter().map(|n| tint(&t, n.side)).collect();
        if self.table_section(ui, "Filled Orders", "filled", &cols, |ui| {
            ui.label(plain(format!("{}", notes.len()), t.muted));
        }) {
            if notes.is_empty() {
                empty_state(ui, &t, "No fills yet", "Every execution appears here with its charges.");
            } else {
                Grid::new("filled", &cols, notes.len()).tints(&tints).hidden(&self.view.hidden).show(ui, &t, |ui, row, col| {
                    let n = &notes[row];
                    ui.label(match col {
                        0 => plain(format::clock(n.timestamp), t.text),
                        1 => side_text(&t, n.side),
                        2 => plain(signed_qty(n.side, n.qty), t.text),
                        3 => plain(n.symbol.to_string(), t.text),
                        4 => plain(format::price(n.price), t.text),
                        5 => plain(format::money(n.charges.total().unsigned_abs()), t.text),
                        _ => plain(format::signed_money(n.net_amount), t.tone(n.net_amount as f64)),
                    });
                });
            }
        }
        let closed: Vec<_> = self.store.closed_orders().into_iter().filter(|o| o.status != OrderStatus::Filled).take(100).cloned().collect();
        let cols = [
            Col::left("Time", 84.0),
            Col::left("Side", 52.0),
            Col::right("Qty", 74.0),
            Col::left("Symbol", 70.0),
            Col::left("Status", 90.0),
            Col::fill("Reason", Cell::Left),
        ];
        let tints: Vec<Option<Color32>> = closed.iter().map(|o| tint(&t, o.side)).collect();
        if self.table_section(ui, "Cancelled Orders", "closed", &cols, |ui| {
            ui.label(plain(format!("{}", closed.len()), t.muted));
        }) {
            if closed.is_empty() {
                empty_state(ui, &t, "Nothing cancelled or rejected", "Cancelled, expired and rejected orders appear here with the reason.");
            } else {
                Grid::new("closed", &cols, closed.len()).tints(&tints).hidden(&self.view.hidden).show(ui, &t, |ui, row, col| {
                    let o = &closed[row];
                    ui.label(match col {
                        0 => plain(format::clock(o.timestamp), t.text),
                        1 => side_text(&t, o.side),
                        2 => plain(signed_qty(o.side, o.qty), t.text),
                        3 => plain(o.symbol.to_string(), t.text),
                        4 => plain(o.status.label().to_uppercase(), if o.status == OrderStatus::Rejected { t.down } else { t.muted }),
                        _ => plain(&o.reason, t.text),
                    });
                });
            }
        }
        self.position_statement(ui);
    }

    fn position_statement(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let positions: Vec<_> = self
            .store
            .account()
            .map(|a| a.positions.iter().filter(|p| p.balance.available + p.balance.reserved > 0).cloned().collect())
            .unwrap_or_default();
        let cols = [
            Col::left("Instrument", 112.0),
            Col::right("Qty", 60.0),
            Col::right("Trade Price", 86.0),
            Col::right("Mark", 76.0),
            Col::right("Net Chng", 76.0),
            Col::right("P/L Open", 92.0),
            Col::right("P/L %", 70.0),
            Col::right("P/L Day", 92.0),
            Col::fill("Mark Value", Cell::Right),
            Col::center("", 96.0),
        ];
        let mut square = None;
        let has = !positions.is_empty();
        if !self.table_section_with(ui, "Position Statement", "positions", &cols, |_| {}, |ui| {
            if ui.add_enabled(has, egui::Button::new("Square off all").min_size(Vec2::new(0.0, 20.0))).clicked() {
                square = Some(Scope::All);
            }
        }) {
            if let Some(scope) = square {
                self.ask_square_off(scope);
            }
            return;
        }
        if positions.is_empty() {
            empty_state(ui, &t, "No positions", "Shares you hold appear here with their P/L.");
            if let Some(scope) = square {
                self.ask_square_off(scope);
            }
            return;
        }
        struct Line {
            symbol: Symbol,
            qty: u64,
            avg: Price,
            mark: Price,
            net: i64,
            open: Option<i64>,
            pct: Option<f64>,
            day: Option<i64>,
            value: u64,
        }
        let lines: Vec<Line> = positions
            .iter()
            .map(|p| {
                let qty = p.balance.available + p.balance.reserved;
                let mark = self.store.last(p.symbol).unwrap_or(0);
                let reference = self.store.instrument(p.symbol).map(|i| i.reference_price).unwrap_or(mark);
                let open = (p.avg_cost > 0).then(|| qty as i64 * (mark as i64 - p.avg_cost as i64));
                Line {
                    symbol: p.symbol,
                    qty,
                    avg: p.avg_cost,
                    mark,
                    net: mark as i64 - reference as i64,
                    open,
                    pct: open.map(|o| o as f64 / (p.avg_cost * qty) as f64 * 100.0),
                    day: (p.day_cost > 0).then(|| qty as i64 * (mark as i64 - p.day_cost as i64)),
                    value: qty * mark,
                }
            })
            .collect();
        let total_open: i64 = lines.iter().filter_map(|l| l.open).sum();
        let total_day: i64 = lines.iter().filter_map(|l| l.day).sum();
        let total_value: u64 = lines.iter().map(|l| l.value).sum();
        let selected = self.selected.and_then(|s| lines.iter().position(|l| l.symbol == s));
        let signed = |v: Option<i64>| plain(v.map(format::signed_money).unwrap_or_else(|| "-".into()), t.tone(v.unwrap_or(0) as f64));
        let chosen = Grid::new("positions", &cols, lines.len() + 1).selected(selected).hidden(&self.view.hidden).show(ui, &t, |ui, row, col| {
            if row == lines.len() {
                match col {
                    0 => drop(ui.label(bold("Overall Totals", t.strong))),
                    5 => drop(ui.label(signed(Some(total_open)))),
                    7 => drop(ui.label(signed(Some(total_day)))),
                    8 => drop(ui.label(plain(format::money(total_value), t.text))),
                    _ => {}
                }
                return;
            }
            let l = &lines[row];
            match col {
                0 => drop(ui.label(plain(l.symbol.to_string(), t.tone(l.net as f64)))),
                1 => drop(ui.label(plain(format::qty(l.qty), t.text))),
                2 => drop(ui.label(plain(if l.avg > 0 { format::price(l.avg) } else { "-".into() }, t.text))),
                3 => drop(ui.label(plain(format::price(l.mark), t.text))),
                4 => drop(ui.label(plain(signed_price(l.net), t.tone(l.net as f64)))),
                5 => drop(ui.label(signed(l.open))),
                6 => drop(ui.label(plain(l.pct.map(format::pct).unwrap_or_else(|| "-".into()), t.tone(l.pct.unwrap_or(0.0))))),
                7 => drop(ui.label(signed(l.day))),
                8 => drop(ui.label(plain(format::money(l.value), t.text))),
                _ => {
                    if ui.add(egui::Button::new("Square off").small()).clicked() {
                        square = Some(Scope::One(l.symbol));
                    }
                }
            }
        });
        let online = matches!(self.store.connection, ome_ui_data::Connection::Online { .. });
        padded(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(plain("ACCOUNT STATUS:", t.text));
                ui.label(plain(if online { "OK TO TRADE" } else { "OFFLINE" }, if online { t.up } else { t.warning }));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let m = self.money();
                    ui.label(plain(format::money(m.buying_power), t.up));
                    ui.label(plain("AVAILABLE DOLLARS:", t.text));
                });
            });
        });
        if let Some(row) = chosen.filter(|r| *r < lines.len()) {
            self.select(lines[row].symbol);
        }
        if let Some(scope) = square {
            self.ask_square_off(scope);
        }
    }

    fn statement(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let account = self.store.account().map(|a| a.account_id).unwrap_or(0);
        let user = self.user.clone();
        toolbar(ui, &t, |ui| {
            ui.label(plain(format!("Statement for account {user} · {account} (Retail)"), t.text));
            ome_ui::widgets::divider(ui, &t);
            ui.label(plain("Today", t.text));
        });
        let m = self.money();
        let cash = self.store.account().map(|a| a.cash).unwrap_or_default();
        if fold(&mut self.view, &t, ui, "Cash Balance", false, |ui| {
            ui.label(bold(format::signed_money(m.cash), t.strong));
        }) {
            kv_row(ui, &t, "Available", &format::money(cash.available), t.strong);
            kv_row(ui, &t, "Reserved for working orders", &format::money(cash.reserved), t.strong);
            kv_row(ui, &t, "Unsettled (T+1)", &format::signed_money(m.unsettled), t.tone(m.unsettled as f64));
        }
        let notes: Vec<_> = self.store.contract_notes().into_iter().take(200).cloned().collect();
        let cols = [
            Col::left("Exec Time", 84.0),
            Col::left("Side", 52.0),
            Col::right("Qty", 70.0),
            Col::left("Symbol", 70.0),
            Col::right("Price", 76.0),
            Col::right("Brokerage", 82.0),
            Col::right("Exch Fee", 76.0),
            Col::right("Taxes", 70.0),
            Col::right("GST & Reg", 80.0),
            Col::fill("Net Amount", Cell::Right),
        ];
        let tints: Vec<Option<Color32>> = notes.iter().map(|n| tint(&t, n.side)).collect();
        if self.table_section(ui, "Trade History", "trade-history", &cols, |ui| {
            ui.label(plain(format!("{}", notes.len()), t.muted));
        }) {
            if notes.is_empty() {
                empty_state(ui, &t, "No trades today", "Contract notes appear here with every charge.");
            } else {
                Grid::new("trade-history", &cols, notes.len()).tints(&tints).hidden(&self.view.hidden).show(ui, &t, |ui, row, col| {
                    let n = &notes[row];
                    let c = n.charges;
                    ui.label(match col {
                        0 => plain(format::clock(n.timestamp), t.text),
                        1 => side_text(&t, n.side),
                        2 => plain(signed_qty(n.side, n.qty), t.text),
                        3 => plain(n.symbol.to_string(), t.text),
                        4 => plain(format::price(n.price), t.text),
                        5 => plain(format::money(c.commission), t.text),
                        6 => plain(format::signed_money(c.exchange_fee), t.text),
                        7 => plain(format::money(c.transaction_tax + c.stamp_duty), t.text),
                        8 => plain(format::money(c.gst + c.regulatory_fee), t.text),
                        _ => plain(format::signed_money(n.net_amount), t.tone(n.net_amount as f64)),
                    });
                });
            }
        }
        let positions: Vec<_> = self
            .store
            .account()
            .map(|a| a.positions.iter().filter(|p| p.balance.available + p.balance.reserved > 0).cloned().collect())
            .unwrap_or_default();
        if fold(&mut self.view, &t, ui, "Profits and Losses", false, |ui| {
            ui.label(bold(format::signed_money(m.open_pnl), t.tone(m.open_pnl as f64)));
            ui.label(plain("by symbol", t.text));
        }) {
            if positions.is_empty() {
                empty_state(ui, &t, "No positions", "Open P/L by symbol appears here.");
            } else {
                let cols = [Col::left("Symbol", 90.0), Col::right("Qty", 70.0), Col::right("P/L Open", 100.0), Col::right("P/L Day", 100.0), Col::fill("Mark Value", Cell::Right)];
                Grid::new("pnl", &cols, positions.len()).show(ui, &t, |ui, row, col| {
                    let p = &positions[row];
                    let qty = p.balance.available + p.balance.reserved;
                    let mark = self.store.last(p.symbol).unwrap_or(0);
                    let open = if p.avg_cost > 0 { qty as i64 * (mark as i64 - p.avg_cost as i64) } else { 0 };
                    let day = if p.day_cost > 0 { qty as i64 * (mark as i64 - p.day_cost as i64) } else { 0 };
                    ui.label(match col {
                        0 => plain(p.symbol.to_string(), t.text),
                        1 => plain(format::qty(qty), t.text),
                        2 => plain(format::signed_money(open), t.tone(open as f64)),
                        3 => plain(format::signed_money(day), t.tone(day as f64)),
                        _ => plain(format::money(qty * mark), t.text),
                    });
                });
            }
        }
        if fold(&mut self.view, &t, ui, "Account Summary", false, |_| {}) {
            let fees: i64 = notes.iter().map(|n| n.charges.total()).sum();
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 380.0).max(0.0));
                ui.vertical(|ui| {
                    ui.set_width(380.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    kv_row(ui, &t, "Net Liquidating Value", &format::money(m.net_liq.max(0) as u64), t.strong);
                    kv_row(ui, &t, "Stock Buying Power", &format::money(m.buying_power), t.strong);
                    kv_row(ui, &t, "Available Funds For Trading", &format::money(cash.available), t.strong);
                    kv_row(ui, &t, "Long Stock Value", &format::money(m.value), t.strong);
                    kv_row(ui, &t, "Cash", &format::signed_money(m.cash), t.strong);
                    kv_row(ui, &t, "Unsettled Cash", &format::signed_money(m.unsettled), t.strong);
                    kv_row(ui, &t, "Day P/L", &format::signed_money(m.day_pnl), t.tone(m.day_pnl as f64));
                    kv_row(ui, &t, "Commissions & Fees Today", &format::money(fees.unsigned_abs()), t.strong);
                });
            });
        }
    }

    fn quote_header(&mut self, ui: &mut Ui, symbol: Symbol) {
        let t = self.theme;
        let info = self.store.instrument(symbol).cloned();
        let book = self.store.book(symbol);
        let bid = book.and_then(|b| b.bids.first()).copied();
        let ask = book.and_then(|b| b.asks.first()).copied();
        let stats = self.store.stats(symbol).copied();
        let last = self.store.last(symbol);
        let reference = info.as_ref().map(|i| i.reference_price).unwrap_or(0);
        let mut link = self.link;
        toolbar(ui, &t, |ui| {
            egui::Frame::new()
                .fill(t.field)
                .stroke(Stroke::new(1.0, t.field_line))
                .inner_margin(Margin::symmetric(8, 1))
                .show(ui, |ui| {
                    ui.label(bold(symbol.to_string(), t.strong));
                });
            link_chip(ui, &t, &mut link);
            if let Some(info) = &info {
                ui.label(plain(&info.name, t.muted));
            }
            let net = last.map(|l| l as i64 - reference as i64).unwrap_or(0);
            value(ui, &t, "Last", &format::price_or_dash(last), t.strong);
            value(ui, &t, "Net Chng", &signed_price(net), t.tone(net as f64));
            value(ui, &t, "Bid", &format::price_or_dash(bid.map(|l| l.price)), t.bid);
            value(ui, &t, "Ask", &format::price_or_dash(ask.map(|l| l.price)), t.ask);
            value(ui, &t, "Size", &format!("{} × {}", bid.map_or(0, |l| l.qty), ask.map_or(0, |l| l.qty)), t.strong);
            if let Some(s) = stats {
                let volume = s.exchange_volume + s.internal_volume + s.wholesaler_volume + s.ats_volume;
                value(ui, &t, "Volume", &format::qty(volume), t.strong);
                value(ui, &t, "High", &format::price(s.high), t.strong);
                value(ui, &t, "Low", &format::price(s.low), t.strong);
                value(ui, &t, "VWAP", &format::price(s.vwap), t.strong);
            }
            if info.is_some_and(|i| i.status != InstrumentStatus::Trading) {
                badge(ui, "HALTED", t.base, t.warning);
            }
        });
        self.link = link;
    }

    fn all_products(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let Some(symbol) = self.selected else {
            return empty_state(ui, &t, "No symbol selected", "Pick one in the watchlist.");
        };
        self.quote_header(ui, symbol);
        if fold(&mut self.view, &t, ui, "Order Entry", false, |_| {}) {
            padded(ui, |ui| self.order_row(ui, symbol));
        }
        if fold(&mut self.view, &t, ui, "Level II · Time & Sales", false, |_| {}) {
            ui.columns(2, |columns| {
                self.level_two(&mut columns[0]);
                self.time_sales(&mut columns[1], 14);
            });
        }
    }

    fn level_two(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let Some(book) = self.selected.and_then(|s| self.store.book(s)).cloned() else {
            return empty_state(ui, &t, "No book yet", "Bids and offers appear here.");
        };
        let depth = book.bids.len().max(book.asks.len()).min(14);
        let cols = [Col::fill("Bid Size", Cell::Right), Col::fill("Bid", Cell::Right), Col::fill("Ask", Cell::Left), Col::fill("Ask Size", Cell::Left)];
        Grid::new("level2", &cols, depth).show(ui, &t, |ui, row, col| {
            let bid = book.bids.get(row);
            let ask = book.asks.get(row);
            ui.label(match col {
                0 => plain(bid.map(|l| format::qty(l.qty)).unwrap_or_default(), t.bid),
                1 => plain(bid.map(|l| format::price(l.price)).unwrap_or_default(), t.text),
                2 => plain(ask.map(|l| format::price(l.price)).unwrap_or_default(), t.text),
                _ => plain(ask.map(|l| format::qty(l.qty)).unwrap_or_default(), t.ask),
            });
        });
    }

    fn time_sales(&mut self, ui: &mut Ui, rows: usize) {
        let t = self.theme;
        let Some(symbol) = self.selected else { return };
        let tape: Vec<_> = self.store.tape(symbol).into_iter().take(rows).cloned().collect();
        if tape.is_empty() {
            return empty_state(ui, &t, "No trades yet", "Prints appear as they happen.");
        }
        let cols = [Col::left("Time", 76.0), Col::right("Price", 70.0), Col::right("Size", 60.0), Col::fill("Venue", Cell::Left)];
        Grid::new("tape", &cols, tape.len()).show(ui, &t, |ui, row, col| {
            let p = &tape[row];
            let color = if p.aggressor == Side::Buy { t.up } else { t.down };
            ui.label(match col {
                0 => plain(format::clock(p.timestamp), t.text),
                1 => plain(format::price(p.price), color),
                2 => plain(format::qty(p.qty), t.text),
                _ => plain(p.venue.label(), t.muted),
            });
        });
    }

    fn order_row(&mut self, ui: &mut Ui, symbol: Symbol) {
        let t = self.theme;
        let quote = self.quote(symbol);
        if self.form_symbol != Some(symbol) {
            self.form.fill_price(quote);
            self.form_symbol = Some(symbol);
        }
        let mut send = false;
        let field = |ui: &mut Ui, title: &str, width: f32, add: &mut dyn FnMut(&mut Ui, egui::Id)| {
            if ui.max_rect().right() - ui.cursor().min.x < width {
                ui.end_row();
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let id = ui.label(plain(title, t.muted)).id;
                add(ui, id);
            });
        };
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(12.0, 8.0);
            let buy = self.form.side == Side::Buy;
            field(ui, "Side", 60.0, &mut |ui, _| {
                let side = egui::Button::new(bold(if buy { "BUY" } else { "SELL" }, if buy { t.up } else { t.down })).min_size(Vec2::new(54.0, 22.0));
                if ui.add(side).on_hover_text("Click to switch side").clicked() {
                    self.form.side = if buy { Side::Sell } else { Side::Buy };
                    self.form.fill_price(quote);
                }
            });
            field(ui, "Qty", 70.0, &mut |ui, id| {
                ui.add_sized([64.0, 22.0], TextEdit::singleline(&mut self.form.qty).margin(Margin::symmetric(5, 2))).labelled_by(id);
            });
            field(ui, "Symbol", 60.0, &mut |ui, _| {
                ui.add_sized([54.0, 22.0], Label::new(bold(symbol.to_string(), t.strong)));
            });
            let mut kind = self.form.kind;
            field(ui, "Type", 112.0, &mut |ui, _| {
                egui::ComboBox::from_id_salt("order-type")
                    .selected_text(kind_label(kind))
                    .width(84.0)
                    .show_ui(ui, |ui| {
                        for k in [Kind::Market, Kind::Limit, Kind::Stop, Kind::Bracket] {
                            ui.selectable_value(&mut kind, k, kind_label(k));
                        }
                    });
            });
            if kind != self.form.kind {
                self.set_kind(kind);
            }
            if self.form.kind != Kind::Market {
                field(ui, "Price", 250.0, &mut |ui, id| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        ui.add_sized([78.0, 22.0], TextEdit::singleline(&mut self.form.price).margin(Margin::symmetric(5, 2))).labelled_by(id);
                        if ui.small_button("−").on_hover_text("Down 1¢").clicked() {
                            self.form.nudge(-1, quote);
                        }
                        if ui.small_button("+").on_hover_text("Up 1¢").clicked() {
                            self.form.nudge(1, quote);
                        }
                        if ui.small_button("Bid").clicked()
                            && let Some(p) = quote.bid
                        {
                            self.set_price(p);
                        }
                        if ui.small_button("Ask").clicked()
                            && let Some(p) = quote.ask
                        {
                            self.set_price(p);
                        }
                    });
                });
            }
            field(ui, "TIF", 100.0, &mut |ui, _| {
                if self.form.kind == Kind::Limit {
                    let mut tif = self.form.tif;
                    egui::ComboBox::from_id_salt("order-tif")
                        .selected_text(tif_label(tif))
                        .width(60.0)
                        .show_ui(ui, |ui| {
                            for v in [TimeInForce::Day, TimeInForce::Gtc] {
                                ui.selectable_value(&mut tif, v, tif_label(v));
                            }
                        });
                    self.form.tif = tif;
                } else {
                    ui.add_sized([40.0, 22.0], Label::new(plain(if self.form.kind == Kind::Market { "DAY" } else { "GTC" }, t.text)));
                }
            });
            if self.form.kind == Kind::Bracket {
                field(ui, "Target %", 50.0, &mut |ui, id| {
                    ui.add_sized([44.0, 22.0], TextEdit::singleline(&mut self.form.target_pct).margin(Margin::symmetric(5, 2))).labelled_by(id);
                });
                field(ui, "Stop %", 50.0, &mut |ui, id| {
                    ui.add_sized([44.0, 22.0], TextEdit::singleline(&mut self.form.stop_pct).margin(Margin::symmetric(5, 2))).labelled_by(id);
                });
            }
            let label = if self.prefs.confirm { "Confirm and Send" } else { "Send" };
            field(ui, "", 150.0, &mut |ui, _| {
                let button = egui::Button::new(RichText::new(label).font(Theme::bold(13.0)).color(t.on_fill))
                    .fill(t.up_fill)
                    .corner_radius(CornerRadius::same(2))
                    .min_size(Vec2::new(130.0, 22.0));
                send = ui.add(button).clicked();
            });
        });
        if let Some(error) = &self.form_error {
            ui.add_space(4.0);
            ui.label(plain(error, t.down));
        }
        if send {
            self.review(None);
        }
    }

    fn active_trader(&mut self, ui: &mut Ui, id: u32) {
        let t = self.theme;
        let Some(symbol) = self.selected else {
            return empty_state(ui, &t, "No symbol selected", "Pick one in the watchlist.");
        };
        self.quote_header(ui, symbol);
        egui::Panel::right(egui::Id::new(("active-trader-controls", id)))
            .resizable(false)
            .default_size(270.0)
            .frame(egui::Frame::new().fill(t.base).stroke(Stroke::new(1.0, t.divider)))
            .show(ui, |ui| self.trader_controls(ui, symbol));
        self.ladder(ui, symbol);
    }

    fn trader_controls(&mut self, ui: &mut Ui, symbol: Symbol) {
        let t = self.theme;
        ui.spacing_mut().item_spacing.y = 0.0;
        header_strip(ui, &t, "Order Entry");
        let mut action = None;
        padded(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.horizontal(|ui| {
                let qty = ui.label(plain("Qty", t.text));
                ui.add(TextEdit::singleline(&mut self.form.qty).desired_width(70.0).margin(Margin::symmetric(5, 2))).labelled_by(qty.id);
                ui.label(plain("TIF", t.text));
                let mut tif = self.form.tif;
                segmented(ui, &t, &mut tif, &[(TimeInForce::Day, "DAY"), (TimeInForce::Gtc, "GTC")]);
                self.form.tif = tif;
            });
            let auto = !self.prefs.confirm;
            ui.label(plain(
                if auto { "Auto send is on: clicks go out at once" } else { "Clicks open the order confirmation" },
                if auto { t.warning } else { t.muted },
            ));
            ui.columns(2, |c| {
                if { let w = c[0].available_width(); filled_button(&mut c[0], &t, "Buy MKT", t.up_fill, w) }.clicked() {
                    action = Some(0);
                }
                if { let w = c[1].available_width(); filled_button(&mut c[1], &t, "Sell MKT", t.down_fill, w) }.clicked() {
                    action = Some(1);
                }
                if { let w = c[0].available_width(); plain_button(&mut c[0], "Flatten", w) }.on_hover_text("Square off this position").clicked() {
                    action = Some(2);
                }
                if { let w = c[1].available_width(); plain_button(&mut c[1], "Cancel orders", w) }.on_hover_text("Cancel this symbol's orders").clicked() {
                    action = Some(3);
                }
            });
        });
        header_strip(ui, &t, "Position");
        let held = self.held(symbol);
        let p = self.store.account().and_then(|a| a.positions.iter().find(|p| p.symbol == symbol)).cloned();
        let mark = self.store.last(symbol).unwrap_or(0);
        let avg = p.as_ref().map(|p| p.avg_cost).unwrap_or(0);
        let open = if avg > 0 { held as i64 * (mark as i64 - avg as i64) } else { 0 };
        let working = self.store.open_orders().iter().filter(|o| o.symbol == symbol).count();
        kv_row(ui, &t, "Position", &format::qty(held), t.strong);
        kv_row(ui, &t, "Trade Price", &if avg > 0 { format::price(avg) } else { "-".into() }, t.strong);
        kv_row(ui, &t, "P/L Open", &format::signed_money(open), t.tone(open as f64));
        kv_row(ui, &t, "Working Orders", &working.to_string(), t.strong);
        match action {
            Some(0) => self.market(Side::Buy),
            Some(1) => self.market(Side::Sell),
            Some(2) => self.ask_square_off(Scope::One(symbol)),
            Some(3) => self.cancel_symbol(),
            _ => {}
        }
        if let Some(error) = &self.form_error {
            padded(ui, |ui| ui.label(plain(error, t.down)));
        }
    }

    fn ladder(&mut self, ui: &mut Ui, symbol: Symbol) {
        let t = self.theme;
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
            bids: book.map(|b| b.bids.iter().map(|l| (l.price, l.qty)).collect()).unwrap_or_default(),
            asks: book.map(|b| b.asks.iter().map(|l| (l.price, l.qty)).collect()).unwrap_or_default(),
            own_bids: mine(Side::Buy),
            own_asks: mine(Side::Sell),
            prints: self.store.tape(symbol).iter().take(200).map(|p| (p.price, p.qty)).collect(),
            last: self.store.last(symbol),
            tick: 1,
        };
        let fit = (((ui.available_height() - HEAD) / LADDER_ROW).floor() as usize).max(9);
        let rows_wanted = if fit.is_multiple_of(2) { fit - 1 } else { fit };
        let center = input.center();
        let half = (rows_wanted / 2) as u64;
        let current = self.ladder_centers.get(&symbol).copied();
        let stale = match (current, center) {
            (None, _) => true,
            (Some(c), Some(now)) => now + half.saturating_sub(3) < c || now > c + half.saturating_sub(3),
            _ => false,
        };
        if stale && let Some(c) = center {
            self.ladder_centers.insert(symbol, c);
        }
        let rows = self.ladder_centers.get(&symbol).map(|c| input.rows(*c, rows_wanted)).unwrap_or_default();
        let mut action: Option<(Side, Price)> = None;
        let mut moved: Option<(Drag, Price)> = None;
        let mut priced: Option<Price> = None;
        let cols = [
            Col::fill("Volume", Cell::Right),
            Col::fill("Buy Orders", Cell::Right),
            Col::fill("Bid Size", Cell::Right),
            Col::fill("Price", Cell::Center),
            Col::fill("Ask Size", Cell::Left),
            Col::fill("Sell Orders", Cell::Left),
        ];
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            table::builder(ui, "ladder", &cols)
                .vscroll(false)
                .header(HEAD, |mut h| {
                    for c in &cols {
                        h.col(|ui| table::head(ui, &t, c.title, c.align));
                    }
                })
                .body(|body| {
                    body.rows(LADDER_ROW, rows.len(), |mut row| {
                        let r = rows[row.index()];
                        let price = r.price;
                        let qty = |q: u64| if q == 0 { String::new() } else { format::qty(q) };
                        row.col(|ui| table::text(ui, &t, Cell::Right, plain(qty(r.traded), t.muted)));
                        row.col(|ui| own_cell(ui, &t, Side::Buy, price, r.own_bid, &mut moved));
                        row.col(|ui| {
                            if let Some(clicked) = size_cell(ui, &t, Side::Buy, price, r.bid, &mut moved) {
                                action = Some(clicked);
                            }
                        });
                        row.col(|ui| {
                            table::lines(ui, &t);
                            let rect = ui.max_rect();
                            let response = ui.allocate_rect(rect, Sense::click());
                            if r.last {
                                ui.painter().rect_stroke(rect.shrink(1.0), CornerRadius::ZERO, Stroke::new(1.0, t.last), egui::StrokeKind::Inside);
                            }
                            let font = if r.last { Theme::bold(13.0) } else { Theme::sans(13.0) };
                            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, format::price(price), font, if r.last { t.last } else { t.text });
                            if response.clicked() {
                                priced = Some(price);
                            }
                        });
                        row.col(|ui| {
                            if let Some(clicked) = size_cell(ui, &t, Side::Sell, price, r.ask, &mut moved) {
                                action = Some(clicked);
                            }
                        });
                        row.col(|ui| own_cell(ui, &t, Side::Sell, price, r.own_ask, &mut moved));
                    });
                });
        });
        if let Some(price) = priced {
            self.set_price(price);
        }
        if let Some((side, price)) = action {
            self.ladder_order(side, price);
        }
        if let Some((drag, price)) = moved {
            self.move_orders(drag, price);
        }
    }

    fn charts(&mut self, ui: &mut Ui, id: u32) {
        let t = self.theme;
        let Some(symbol) = self.selected else {
            return empty_state(ui, &t, "No symbol selected", "Pick one in the watchlist.");
        };
        let last = self.store.last(symbol);
        let reference = self.store.instrument(symbol).map(|i| i.reference_price);
        let mut link = self.link;
        toolbar(ui, &t, |ui| {
            egui::Frame::new()
                .fill(t.field)
                .stroke(Stroke::new(1.0, t.field_line))
                .inner_margin(Margin::symmetric(8, 1))
                .show(ui, |ui| {
                    ui.label(bold(symbol.to_string(), t.strong));
                });
            link_chip(ui, &t, &mut link);
            if let Some(last) = last {
                let net = last as i64 - reference.unwrap_or(last) as i64;
                ui.label(bold(format::price(last), t.strong));
                ui.label(plain(signed_price(net), t.tone(net as f64)));
            }
            ome_ui::widgets::divider(ui, &t);
            let mut interval = self.interval;
            if segmented(ui, &t, &mut interval, &[(1_000, "1s"), (5_000, "5s"), (15_000, "15s"), (60_000, "1m"), (300_000, "5m"), (900_000, "15m")]) {
                self.set_interval(interval);
            }
            ome_ui::widgets::divider(ui, &t);
            ui.label(plain("Studies", t.text));
            ui.checkbox(&mut self.studies.volume, "Volume");
            ui.checkbox(&mut self.studies.vwap, "VWAP");
            let mut sma = self.studies.sma.is_some();
            if ui.checkbox(&mut sma, "SMA 20").changed() {
                self.studies.sma = sma.then_some(20);
            }
            let mut ema = self.studies.ema.is_some();
            if ui.checkbox(&mut ema, "EMA 50").changed() {
                self.studies.ema = ema.then_some(50);
            }
        });
        self.link = link;
        let candles = self.store.candles(symbol, self.interval);
        let view = self.chart_views.entry(id).or_default();
        egui::Frame::new().inner_margin(Margin { left: 6, right: 0, top: 6, bottom: 4 }).show(ui, |ui| {
            Chart {
                candles: &candles,
                last,
                reference,
                interval: self.interval,
                studies: self.studies,
                view,
            }
            .show(ui, &t);
        });
    }

    fn quotes(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let symbols = self.store.symbols();
        if symbols.is_empty() {
            return empty_state(ui, &t, "No instruments yet", "Listed symbols appear here.");
        }
        let selected = self.selected.and_then(|s| symbols.iter().position(|x| *x == s));
        let cols = [
            Col::left("Symbol", 76.0),
            Col::fill("Description", Cell::Left),
            Col::right("Last", 80.0),
            Col::right("Net Chng", 80.0),
            Col::right("% Change", 76.0),
            Col::right("Bid", 72.0),
            Col::right("Ask", 72.0),
            Col::right("Volume", 86.0),
            Col::right("Open", 72.0),
            Col::right("High", 72.0),
            Col::right("Low", 72.0),
            Col::right("VWAP", 72.0),
        ];
        let chosen = Grid::new("quote-board", &cols, symbols.len()).selected(selected).show(ui, &t, |ui, row, col| {
            let symbol = symbols[row];
            if self.quote_cells(ui, &t, symbol, 2, 3, col) {
                return;
            }
            let info = self.store.instrument(symbol);
            let reference = info.map(|i| i.reference_price).unwrap_or(0);
            let last = self.store.last(symbol);
            let quote: Quote = self.quote(symbol);
            let stats = self.store.stats(symbol);
            let stat = |f: fn(&ome_protocol::MarketStats) -> u64| stats.map(|s| format::price(f(s))).unwrap_or_else(|| "-".into());
            let net = last.map(|l| l as i64 - reference as i64).unwrap_or(0);
            let text = match col {
                0 => plain(symbol.to_string(), t.tone(net as f64)),
                1 => plain(info.map(|i| i.name.clone()).unwrap_or_default(), t.text),
                4 => {
                    let pct = last.and_then(|l| format::change_pct(reference, l)).unwrap_or(0.0);
                    plain(format::pct(pct), t.tone(pct))
                }
                5 => plain(format::price_or_dash(quote.bid), t.bid),
                6 => plain(format::price_or_dash(quote.ask), t.ask),
                7 => plain(
                    stats
                        .map(|s| format::qty(s.exchange_volume + s.internal_volume + s.wholesaler_volume + s.ats_volume))
                        .unwrap_or_else(|| info.map(|i| format::qty(i.volume)).unwrap_or_default()),
                    t.text,
                ),
                8 => plain(stat(|s| s.open), t.text),
                9 => plain(stat(|s| s.high), t.text),
                10 => plain(stat(|s| s.low), t.text),
                _ => plain(stat(|s| s.vwap), t.text),
            };
            ui.add(Label::new(text).selectable(false).truncate());
        });
        if let Some(row) = chosen {
            self.select(symbols[row]);
        }
    }

    fn news_page(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let items: Vec<_> = self.store.news().into_iter().take(150).cloned().collect();
        if items.is_empty() {
            return empty_state(ui, &t, "No news yet", "Stories from the newsroom appear here.");
        }
        let selected = self.news_selected.and_then(|seq| items.iter().position(|i| i.seq == seq));
        let story = selected.map(|i| items[i].clone());
        let height = if story.is_some() { ui.available_height() * 0.55 } else { ui.available_height() };
        let cols = [
            Col::left("Time", 76.0),
            Col::left("About", 90.0),
            Col::fill("Headline", Cell::Left),
            Col::right("Tone", 56.0),
            Col::right("Fair Value", 84.0),
        ];
        let chosen = ui
            .allocate_ui(Vec2::new(ui.available_width(), height), |ui| {
                Grid::new("news", &cols, items.len()).selected(selected).scroll().show(ui, &t, |ui, row, col| {
                    let item = &items[row];
                    let text = match col {
                        0 => plain(format::clock(item.time), t.text),
                        1 => plain(
                            match item.kind {
                                NewsKind::Macro => "Economy".to_string(),
                                NewsKind::Sector => item.sector.clone(),
                                _ => item.symbol.map(|s| s.to_string()).unwrap_or_default(),
                            },
                            t.accent,
                        ),
                        2 => plain(&item.headline, t.text),
                        3 => plain(format!("{:+}", item.sentiment), t.tone(item.sentiment as f64)),
                        _ => plain(format!("{:+} bps", item.impact_bps), t.tone(item.impact_bps as f64)),
                    };
                    ui.add(Label::new(text).selectable(false).truncate());
                })
            })
            .inner;
        if let Some(row) = chosen {
            self.news_selected = Some(items[row].seq);
        }
        if let Some(item) = story {
            ui.spacing_mut().item_spacing.y = 0.0;
            header_strip(ui, &t, item.kind.label());
            egui::ScrollArea::vertical().id_salt("news-story").show(ui, |ui| {
                padded(ui, |ui| {
                    ui.label(RichText::new(&item.headline).font(Theme::bold(15.0)).color(t.strong));
                    ui.add_space(6.0);
                    ui.label(plain(&item.body, t.text));
                    if !item.data.is_empty() {
                        ui.add_space(6.0);
                        ui.label(plain(&item.data, t.muted));
                    }
                });
            });
        }
    }
}

fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Market => "MARKET",
        Kind::Limit => "LIMIT",
        Kind::Stop => "STOP",
        Kind::Bracket => "BRACKET",
    }
}

fn tif_label(tif: TimeInForce) -> &'static str {
    if tif == TimeInForce::Gtc { "GTC" } else { "DAY" }
}

fn own_cell(ui: &mut Ui, t: &Theme, side: Side, price: Price, own: u64, moved: &mut Option<(Drag, Price)>) {
    table::lines(ui, t);
    let rect = ui.max_rect();
    let color = if side == Side::Buy { t.bid } else { t.ask };
    let response = if own > 0 {
        let id = egui::Id::new(("own", side == Side::Buy, price));
        ui.dnd_drag_source(id, Drag { side, price }, |ui| {
            let (cell, _) = ui.allocate_exact_size(rect.size(), Sense::hover());
            ui.painter().rect_filled(cell.shrink(2.0), CornerRadius::same(2), t.button);
            ui.painter().text(cell.center(), egui::Align2::CENTER_CENTER, format::qty(own), Theme::bold(12.5), color);
        })
        .response
        .on_hover_text("Drag to another price to move, onto itself to cancel")
    } else {
        ui.allocate_rect(rect, Sense::hover())
    };
    if let Some(drag) = response.dnd_release_payload::<Drag>() {
        *moved = Some((*drag, price));
    }
}

fn size_cell(ui: &mut Ui, t: &Theme, side: Side, price: Price, qty: u64, moved: &mut Option<(Drag, Price)>) -> Option<(Side, Price)> {
    table::lines(ui, t);
    let rect = ui.max_rect();
    let response = ui.allocate_rect(rect, Sense::click());
    let (color, soft, anchor, at) = match side {
        Side::Buy => (t.bid, t.bid_soft, egui::Align2::RIGHT_CENTER, rect.right_center() - Vec2::new(6.0, 0.0)),
        Side::Sell => (t.ask, t.ask_soft, egui::Align2::LEFT_CENTER, rect.left_center() + Vec2::new(6.0, 0.0)),
    };
    if qty > 0 {
        ui.painter().rect_filled(rect.shrink2(Vec2::new(0.5, 0.5)), CornerRadius::ZERO, soft);
        ui.painter().text(at, anchor, format::qty(qty), Theme::sans(13.0), t.strong);
    }
    if response.hovered() {
        ui.painter().rect_stroke(rect.shrink(0.5), CornerRadius::ZERO, Stroke::new(1.0, color), egui::StrokeKind::Inside);
    }
    if let Some(drag) = response.dnd_release_payload::<Drag>() {
        *moved = Some((*drag, price));
    }
    response.clicked().then_some((side, price))
}
