use crate::app::{RetailApp, Scope};
use crate::prefs::SCALES;
use egui::{Align, Layout, Margin, RichText, TextEdit, Ui, Vec2};
use ome_core::types::{Side, TimeInForce};
use ome_ui::format;
use ome_ui::theme::Mode;
use ome_ui::widgets::{dialog, dialog_body, filled_button, header_strip, kv, plain_button, segmented};
use ome_ui::Theme;

fn buttons(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(12.0);
    ui.with_layout(Layout::right_to_left(Align::Center), add);
}

fn modal<R>(ui: &Ui, id: &str, t: &Theme, title: &str, width: f32, add: impl FnOnce(&mut Ui) -> R) -> (R, bool) {
    let response = egui::Modal::new(egui::Id::new(id)).frame(dialog(t)).show(ui.ctx(), |ui| {
        ui.set_width(width);
        header_strip(ui, t, title);
        dialog_body(ui, add)
    });
    let close = response.should_close();
    (response.inner, close)
}

impl RetailApp {
    pub fn confirm(&mut self, ui: &mut Ui) {
        let t = self.core.theme;
        let Some(pending) = self.core.pending.clone() else { return };
        let estimate = self
            .core
            .store
            .estimate
            .filter(|e| e.side == pending.side && e.qty == pending.qty && e.price == pending.price);
        let buy = pending.side == Side::Buy;
        let side_color = if buy { t.up } else { t.down };
        let buying_power = self.core.money().buying_power as i64;
        let (choice, closed) = modal(ui, "confirm-order", &t, "Order confirmation", 420.0, |ui| {
            ui.label(RichText::new(&pending.text).font(Theme::bold(14.0)).color(side_color));
            ui.add_space(8.0);
            let principal = pending.price * pending.qty;
            kv(ui, &t, "Estimate price", &format::price(pending.price), t.text);
            kv(ui, &t, "Quantity", &format::qty(pending.qty), t.text);
            kv(ui, &t, "Principal", &format::money(principal), t.strong);
            ui.separator();
            match estimate {
                Some(e) => {
                    let c = e.charges;
                    kv(ui, &t, "Brokerage", &format::money(c.commission), t.text);
                    kv(ui, &t, "Exchange fee", &format::signed_money(c.exchange_fee), t.text);
                    kv(ui, &t, "Taxes and duty", &format::money(c.transaction_tax + c.stamp_duty), t.text);
                    kv(ui, &t, "Regulatory fee and GST", &format::money(c.regulatory_fee + c.gst), t.text);
                    kv(ui, &t, "Total charges", &format::money(c.total().unsigned_abs()), t.strong);
                    ui.separator();
                    kv(ui, &t, "Cash effect", &format::signed_money(e.cash_effect), t.tone(e.cash_effect as f64));
                    kv(ui, &t, "Buying power after", &format::money((buying_power + e.cash_effect.min(0)).max(0) as u64), t.text);
                }
                None => {
                    ui.label(RichText::new("Fetching brokerage and taxes from the broker…").color(t.muted));
                }
            }
            let mut choice = None;
            buttons(ui, |ui| {
                let label = if buy { "Send buy order" } else { "Send sell order" };
                if filled_button(ui, &t, label, if buy { t.up_fill } else { t.down_fill }, 150.0).clicked() {
                    choice = Some(true);
                }
                if plain_button(ui, "Edit", 90.0).clicked() {
                    choice = Some(false);
                }
            });
            choice
        });
        match choice {
            Some(true) => self.core.send_pending(),
            Some(false) => self.core.pending = None,
            None if closed => self.core.pending = None,
            None => {}
        }
    }

    pub fn square_dialog(&mut self, ui: &mut Ui) {
        let t = self.core.theme;
        let Some(scope) = self.core.square_confirm else { return };
        let legs = self.core.square_legs(scope);
        if legs.is_empty() {
            self.core.square_confirm = None;
            return;
        }
        let title = match scope {
            Scope::One(symbol) => format!("Square off {symbol}"),
            Scope::All => "Square off all positions".to_string(),
        };
        let orders: usize = legs.iter().map(|l| l.orders).sum();
        let conditionals: usize = legs.iter().map(|l| l.conditionals).sum();
        let value: u64 = legs.iter().filter_map(|l| l.value()).sum();
        let (choice, closed) = modal(ui, "square-off", &t, &title, 460.0, |ui| {
            ui.label(RichText::new("Every share below is sold at market.").color(t.text));
            ui.add_space(6.0);
            egui::Frame::new().stroke(egui::Stroke::new(1.0, t.divider)).inner_margin(Margin::same(6)).show(ui, |ui| {
                egui::Grid::new("square-legs").num_columns(4).min_col_width(90.0).spacing([12.0, 4.0]).show(ui, |ui| {
                    for title in ["Symbol", "Sell qty", "Bid", "Est. value"] {
                        ui.label(RichText::new(title).font(Theme::bold(11.5)).color(t.muted));
                    }
                    ui.end_row();
                    for leg in &legs {
                        ui.label(RichText::new(leg.symbol.to_string()).font(Theme::bold(12.5)).color(t.strong));
                        ui.label(RichText::new(format::qty(leg.qty)).color(t.down));
                        ui.label(RichText::new(format::price_or_dash(leg.bid)).color(t.text));
                        ui.label(RichText::new(leg.value().map(format::money).unwrap_or_else(|| "-".into())).color(t.text));
                        ui.end_row();
                    }
                });
            });
            ui.add_space(6.0);
            kv(ui, &t, "Estimated proceeds at the bid", &format::money(value), t.strong);
            if orders + conditionals > 0 {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "First cancels {orders} working order{} and {conditionals} armed conditional{} in {}.",
                        if orders == 1 { "" } else { "s" },
                        if conditionals == 1 { "" } else { "s" },
                        if legs.len() == 1 { "this symbol" } else { "these symbols" },
                    ))
                    .color(t.warning),
                );
            }
            ui.label(RichText::new("Market orders fill at the best available prices, which can be worse than the bid shown.").color(t.muted));
            let mut choice = None;
            buttons(ui, |ui| {
                if filled_button(ui, &t, "Confirm square off", t.down_fill, 150.0).clicked() {
                    choice = Some(true);
                }
                if plain_button(ui, "Keep positions", 120.0).clicked() {
                    choice = Some(false);
                }
            });
            choice
        });
        match choice {
            Some(true) => self.core.square_off(scope),
            Some(false) => self.core.square_confirm = None,
            None if closed => self.core.square_confirm = None,
            None => {}
        }
    }

    pub fn settings_dialog(&mut self, ui: &mut Ui) {
        let t = self.core.theme;
        let Some(mut draft) = self.core.settings.clone() else { return };
        let (choice, closed) = modal(ui, "settings", &t, "Settings", 420.0, |ui| {
            let mut choice = None;
            let row = |ui: &mut Ui, label: &str, add: &mut dyn FnMut(&mut Ui)| {
                ui.horizontal(|ui| {
                    ui.add_sized(Vec2::new(150.0, 24.0), egui::Label::new(RichText::new(label).color(t.muted)));
                    add(ui);
                });
            };
            ui.label(RichText::new("Appearance").font(Theme::bold(12.5)).color(t.strong));
            row(ui, "Theme", &mut |ui| {
                segmented(ui, &t, &mut draft.mode, &[(Mode::Dark, "Dark"), (Mode::Light, "Light")]);
            });
            let scales: Vec<(u32, String)> = SCALES.iter().map(|s| (*s, format!("{s}%"))).collect();
            let options: Vec<(u32, &str)> = scales.iter().map(|(s, l)| (*s, l.as_str())).collect();
            row(ui, "Text size", &mut |ui| {
                segmented(ui, &t, &mut draft.scale, &options);
            });
            ui.add_space(8.0);
            ui.label(RichText::new("Orders").font(Theme::bold(12.5)).color(t.strong));
            row(ui, "Confirm before sending", &mut |ui| {
                let label = if draft.confirm { "On" } else { "Off" };
                ui.checkbox(&mut draft.confirm, label);
            });
            row(ui, "Default quantity", &mut |ui| {
                ui.add(TextEdit::singleline(&mut draft.qty).desired_width(90.0).margin(Margin::symmetric(6, 3)));
            });
            row(ui, "Default time in force", &mut |ui| {
                segmented(ui, &t, &mut draft.tif, &[(TimeInForce::Day, "Day"), (TimeInForce::Gtc, "GTC")]);
            });
            if !draft.confirm {
                ui.label(RichText::new("Orders and ladder clicks go out at once, without the charges review.").color(t.warning));
            }
            if let Some(error) = &draft.error {
                ui.label(RichText::new(error).color(t.down));
            }
            buttons(ui, |ui| {
                if plain_button(ui, "Save", 90.0).clicked() {
                    choice = Some(true);
                }
                if plain_button(ui, "Cancel", 90.0).clicked() {
                    choice = Some(false);
                }
            });
            choice
        });
        match choice {
            Some(true) => {
                self.core.settings = Some(draft);
                self.core.save_settings();
            }
            Some(false) => self.core.settings = None,
            None if closed => self.core.settings = None,
            None => self.core.settings = Some(draft),
        }
    }
}
