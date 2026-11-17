use crate::app::{RetailApp, Screen};
use crate::desk::{Action, Desk};
use crate::view::{Kind, Link, Tab, Trading};
use egui_dock::DockArea;
use egui::{Align, Color32, Frame, Key, Layout, Margin, RichText, Stroke, TextEdit, Ui};
use ome_core::types::Side;
use ome_protocol::MarketPhase;
use ome_ui::Theme;
use ome_ui::format;
use ome_ui::widgets::{divider, grip, main_tab, menu_label, shortcuts};
use ome_ui_data::{AccountView, Connection, MarketData};

pub const KEYS: [(&str, &str); 13] = [
    ("B / S", "Review a buy / sell from the order entry"),
    ("Enter", "Send the order under review"),
    ("Esc", "Cancel the review, close dialogs"),
    ("↑ / ↓", "Previous / next symbol"),
    ("C", "Center the Active Trader ladder"),
    ("Ctrl+1 … 4", "Monitor, Trade, Charts, MarketWatch"),
    ("Ctrl+,", "Setup"),
    ("? / F1", "Show these shortcuts"),
    ("Click a strip", "Fold or unfold a section"),
    ("Drag a tab", "Move it, stack it or split the area beside it"),
    ("+ on a tab bar", "Add a gadget there"),
    ("Right-click a tab", "Link colour, detach to a window"),
    ("Drag the sidebar edge", "Resize the gadgets; ◀ hides them"),
];

pub struct Money {
    pub net_liq: i64,
    pub cash: i64,
    pub buying_power: u64,
    pub unsettled: i64,
    pub value: u64,
    pub day_pnl: i64,
    pub open_pnl: i64,
}

impl crate::app::Core {
    pub fn money(&self) -> Money {
        let Some(account) = self.store.account() else {
            return Money { net_liq: 0, cash: 0, buying_power: 0, unsettled: 0, value: 0, day_pnl: 0, open_pnl: 0 };
        };
        let mut value = 0u64;
        let mut open = 0i64;
        for p in &account.positions {
            let qty = p.balance.available + p.balance.reserved;
            let mark = self.store.last(p.symbol).unwrap_or(0);
            value += qty * mark;
            if p.avg_cost > 0 {
                open += qty as i64 * (mark as i64 - p.avg_cost as i64);
            }
        }
        let cash = (account.cash.available + account.cash.reserved) as i64;
        Money {
            net_liq: cash + value as i64,
            cash,
            buying_power: account.cash.available,
            unsettled: account.unsettled_cash,
            value,
            day_pnl: account.day_pnl,
            open_pnl: open,
        }
    }

}

impl RetailApp {
    pub fn draw(&mut self, ui: &mut Ui, now_ms: u64) {
        if self.core.restyle {
            self.core.theme.apply(ui.ctx());
            ui.ctx().set_zoom_factor(self.core.prefs.scale as f32 / 100.0);
            self.core.restyle = false;
            ui.ctx().request_repaint();
            return;
        }
        if self.core.pump(now_ms) || self.core.flashing() {
            ui.ctx().request_repaint();
        }
        if let Some(symbol) = self.core.selected {
            self.core.groups.insert(Link::Red, symbol);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        match self.core.screen {
            Screen::Login => self.login(ui),
            Screen::Trading => {
                self.adopt_view();
                self.trading(ui);
                self.detached(ui);
            }
        }
        let t = self.core.theme;
        self.core.toasts.show(ui.ctx(), &t);
    }

    fn login(&mut self, ui: &mut Ui) {
        let t = self.core.theme;
        let core = &mut self.core;
        let mut submit = false;
        egui::CentralPanel::default()
            .frame(Frame::new().fill(t.base))
            .show(ui, |ui| {
                let footer = egui::Rect::from_min_max(egui::pos2(ui.max_rect().left(), ui.max_rect().bottom() - 28.0), ui.max_rect().max);
                ui.painter().rect_filled(footer, 0.0, t.status);
                ui.painter().text(
                    footer.left_center() + egui::vec2(12.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    format!("OME Trader {} · simulated market, every price comes from real orders in the engine", env!("CARGO_PKG_VERSION")),
                    Theme::sans(12.0),
                    t.muted,
                );
                ui.with_layout(Layout::top_down(Align::Center), |ui| {
                    ui.add_space((ui.available_height() / 2.0 - 230.0).max(24.0));
                    let mut wordmark = egui::text::LayoutJob::default();
                    wordmark.append("OME", 0.0, egui::TextFormat::simple(Theme::bold(34.0), t.strong));
                    wordmark.append("Trader", 10.0, egui::TextFormat::simple(Theme::sans(34.0), t.text));
                    ui.add(egui::Label::new(wordmark).selectable(false));
                    ui.label(RichText::new("Log in to your brokerage account").color(t.muted));
                    ui.add_space(18.0);
                    Frame::new().fill(t.tab_bar).stroke(Stroke::new(1.0, t.divider)).inner_margin(Margin::same(20)).show(ui, |ui| {
                        ui.set_width(320.0);
                        ui.with_layout(Layout::top_down(Align::Min), |ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            let label = ui.label(RichText::new("Login ID").color(t.text));
                            ui.add(TextEdit::singleline(&mut core.login.user).desired_width(f32::INFINITY).margin(Margin::symmetric(8, 5)).font(Theme::sans(14.0)))
                                .labelled_by(label.id);
                            ui.add_space(4.0);
                            let password_label = ui.horizontal(|ui| {
                                let label = ui.label(RichText::new("Password").color(t.text));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let toggle = if core.login.reveal { "Hide" } else { "Show" };
                                    if ui.add(egui::Button::new(RichText::new(toggle).color(t.accent)).frame(false)).clicked() {
                                        core.login.reveal = !core.login.reveal;
                                    }
                                });
                                label.id
                            }).inner;
                            ui.add(
                                TextEdit::singleline(&mut core.login.password)
                                    .password(!core.login.reveal)
                                    .desired_width(f32::INFINITY)
                                    .margin(Margin::symmetric(8, 5))
                                    .font(Theme::sans(14.0)),
                            )
                            .labelled_by(password_label);
                            ui.checkbox(&mut core.login.remember, "Remember login ID");
                            ui.add_space(6.0);
                            let label = if core.login.connecting { "Logging in…" } else { "Log In" };
                            let button = egui::Button::new(RichText::new(label).font(Theme::bold(15.0)).color(t.on_fill))
                                .fill(t.up_fill)
                                .corner_radius(egui::CornerRadius::same(2))
                                .min_size(egui::vec2(ui.available_width(), 36.0));
                            if ui.add_enabled(!core.login.connecting, button).clicked() {
                                submit = true;
                            }
                            ui.add_space(4.0);
                            if let Some(error) = &core.login.error {
                                Frame::new().fill(t.sell_row).inner_margin(Margin::symmetric(8, 6)).show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.label(RichText::new(error).color(t.strong));
                                });
                            } else if core.login.connecting {
                                ui.label(RichText::new("Connecting to the broker…").color(t.warning));
                            }
                            ui.add_space(6.0);
                            let mut open = core.login.advanced;
                            ome_ui::widgets::section(ui, &t, "Connection", &mut open, false, |ui| {
                                ui.label(RichText::new(&core.login.broker).color(t.muted));
                            });
                            core.login.advanced = open;
                            if open {
                                ui.add_space(4.0);
                                let label = ui.label(RichText::new("Broker").color(t.text));
                                ui.add(TextEdit::singleline(&mut core.login.broker).desired_width(f32::INFINITY).margin(Margin::symmetric(8, 5)))
                                    .labelled_by(label.id);
                                ui.label(RichText::new("Host and port of your broker's client gateway.").color(t.muted));
                            }
                        });
                    });
                    ui.add_space(10.0);
                    ui.label(RichText::new("Your password is never stored.").color(t.muted));
                });
            });
        if ui.input(|i| i.key_pressed(Key::Enter)) {
            submit = true;
        }
        if submit {
            core.log_in();
        }
    }

    fn trading(&mut self, ui: &mut Ui) {
        let t = self.core.theme;
        self.keys(ui);
        egui::Panel::top("status-strip")
            .frame(Frame::new().fill(t.status).inner_margin(Margin::symmetric(6, 2)))
            .show(ui, |ui| self.status_strip(ui));
        egui::Panel::top("balances-strip")
            .frame(Frame::new().fill(t.status).inner_margin(Margin::symmetric(6, 2)).stroke(Stroke::new(1.0, t.divider)))
            .show(ui, |ui| self.balances(ui));
        if self.core.view.sidebar_open {
            let panel = egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(360.0)
                .size_range(260.0..=620.0)
                .frame(Frame::new().fill(t.base))
                .show(ui, |ui| self.core.sidebar(ui));
            let edge = panel.response.rect;
            let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("sidebar-grip")));
            painter.rect_filled(egui::Rect::from_min_max(egui::pos2(edge.right(), edge.top()), egui::pos2(edge.right() + 6.0, edge.bottom())), 0.0, t.tab_bar);
            grip(&painter, &t, egui::pos2(edge.right() + 3.0, edge.center().y), true);
            let hide = egui::Area::new(egui::Id::new("sidebar-collapse"))
                .order(egui::Order::Foreground)
                .fixed_pos(egui::pos2(edge.right() - 2.0, edge.center().y - 56.0))
                .show(ui.ctx(), |ui| {
                    ui.add(egui::Button::new(RichText::new("◀").size(9.0).color(t.muted)).frame(false).min_size(egui::vec2(10.0, 24.0)))
                        .on_hover_text("Hide gadgets")
                        .clicked()
                })
                .inner;
            if hide {
                self.core.view.sidebar_open = false;
            }
        } else {
            egui::Panel::left("sidebar-collapsed")
                .resizable(false)
                .exact_size(14.0)
                .frame(Frame::new().fill(t.tab_bar))
                .show(ui, |ui| {
                    ui.add_space(ui.available_height() / 2.0 - 60.0);
                    if ui.add(egui::Button::new(RichText::new("▶").size(9.0)).frame(false)).on_hover_text("Show gadgets").clicked() {
                        self.core.view.sidebar_open = true;
                    }
                });
        }
        egui::CentralPanel::default()
            .frame(Frame::new().fill(t.base))
            .show(ui, |ui| {
                self.tab_bar(ui);
                self.desk(ui);
            });
        self.confirm(ui);
        self.square_dialog(ui);
        self.settings_dialog(ui);
        let mut open = self.core.show_shortcuts;
        shortcuts(ui.ctx(), &t, &mut open, &KEYS);
        self.core.show_shortcuts = open;
    }

    fn desk(&mut self, ui: &mut Ui) {
        let t = self.core.theme;
        let style = t.dock_style(ui.style());
        let mut dock = std::mem::replace(self.core.view.dock(), egui_dock::DockState::new(Vec::new()));
        if dock.iter_all_tabs().next().is_none() {
            ome_ui::widgets::empty_state(ui, &t, "Nothing here", "Use Layout to add a gadget or reset this tab.");
        }
        let tab = self.core.view.tab;
        let mut desk = Desk { core: &mut self.core, actions: Vec::new() };
        DockArea::new(&mut dock)
            .id(egui::Id::new(("desk", tab as usize)))
            .style(style)
            .show_close_buttons(true)
            .show_add_buttons(true)
            .show_add_popup(true)
            .show_leaf_close_all_buttons(false)
            .show_leaf_collapse_buttons(false)
            .show_inside(ui, &mut desk);
        let actions = std::mem::take(&mut desk.actions);
        *self.core.view.dock() = dock;
        for action in actions {
            self.apply(action);
        }
    }

    pub fn apply(&mut self, action: Action) {
        let view = &mut self.core.view;
        match action {
            Action::Add(path, kind) => {
                let gadget = view.gadget(kind);
                let dock = view.dock();
                dock.set_focused_node_and_surface(path);
                dock.push_to_focused_leaf(gadget);
            }
            Action::Detach(id) => {
                view.detach(id);
            }
            Action::Link(id, link) => view.set_link(id, link),
        }
    }

    fn detached(&mut self, ui: &mut Ui) {
        let t = self.core.theme;
        let gadgets = self.core.view.detached.clone();
        for mut g in gadgets {
            let title = format!("{} · OME Trader", g.kind.title());
            let builder = egui::ViewportBuilder::default().with_title(&title).with_inner_size([900.0, 600.0]);
            let core = &mut self.core;
            let closed = ui.ctx().show_viewport_immediate(egui::ViewportId::from_hash_of(("gadget", g.id)), builder, |ui, _| {
                egui::CentralPanel::default().frame(Frame::new().fill(t.base)).show(ui, |ui| core.gadget(ui, &mut g));
                ui.ctx().input(|i| i.viewport().close_requested())
            });
            if let Some(slot) = self.core.view.detached.iter_mut().find(|d| d.id == g.id) {
                *slot = g;
            }
            if closed {
                self.core.view.reattach(g.id);
            }
        }
    }

    fn keys(&mut self, ui: &mut Ui) {
        let typing = ui.ctx().egui_wants_keyboard_input();
        let core = &mut self.core;
        ui.input(|i| {
            if i.modifiers.command {
                for (k, tab) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4].into_iter().zip(Tab::ALL) {
                    if i.key_pressed(k) {
                        core.view.tab = tab;
                    }
                }
                if i.key_pressed(Key::Comma) {
                    core.open_settings();
                }
            }
            if i.key_pressed(Key::Escape) {
                core.pending = None;
                core.square_confirm = None;
                core.settings = None;
                core.show_shortcuts = false;
            }
            if i.key_pressed(Key::F1) || (!typing && i.key_pressed(Key::Questionmark)) {
                core.show_shortcuts = !core.show_shortcuts;
            }
            if core.pending.is_some() {
                if i.key_pressed(Key::Enter) {
                    core.send_pending();
                }
                return;
            }
            if core.square_confirm.is_some() || core.settings.is_some() || core.show_shortcuts {
                return;
            }
            if typing {
                if i.key_pressed(Key::Enter) {
                    core.review(None);
                }
                return;
            }
            if i.modifiers.any() {
                return;
            }
            if i.key_pressed(Key::B) {
                core.review(Some(Side::Buy));
            }
            if i.key_pressed(Key::S) {
                core.review(Some(Side::Sell));
            }
            if i.key_pressed(Key::ArrowDown) {
                core.step_symbol(1);
            }
            if i.key_pressed(Key::ArrowUp) {
                core.step_symbol(-1);
            }
            if i.key_pressed(Key::C) {
                core.ladder_centers.clear();
            }
        });
    }

    fn status_strip(&mut self, ui: &mut Ui) {
        let t = self.core.theme;
        let now = self.core.store.now;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let online = matches!(self.core.store.connection, Connection::Online { .. });
            let (status, color) = match self.core.store.connection {
                Connection::Online { .. } => ("Connected", t.up),
                Connection::Connecting => ("Connecting…", t.warning),
                Connection::Offline => ("Reconnecting…", t.down),
            };
            ui.label(RichText::new(status).color(color));
            divider(ui, &t);
            ui.label(RichText::new(if online { "Realtime data" } else { "No data" }).color(if online { t.up } else { t.warning }));
            divider(ui, &t);
            ui.label(RichText::new(format::date(now)).color(t.text));
            ui.label(RichText::new(format::clock(now)).font(Theme::bold(13.0)).color(t.strong)).on_hover_text("UTC");
            divider(ui, &t);
            Frame::new()
                .stroke(Stroke::new(1.0, t.accent))
                .inner_margin(Margin::symmetric(8, 1))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(RichText::new("Account:").color(t.text));
                    let account = match self.core.store.connection {
                        Connection::Online { account, .. } => format!("{} · {account} (Retail)", self.core.user),
                        _ => self.core.user.clone(),
                    };
                    ui.label(RichText::new(account).font(Theme::bold(13.0)).color(t.strong));
                });
            divider(ui, &t);
            self.phase(ui);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                let link = |ui: &mut Ui, text: &str| ui.add(egui::Button::new(RichText::new(text).color(t.accent)).frame(false));
                if link(ui, "Log out").clicked() {
                    self.core.log_out();
                }
                if link(ui, "⚙ Setup").on_hover_text("Ctrl+,").clicked() {
                    self.core.open_settings();
                }
                if link(ui, "Shortcuts").on_hover_text("? or F1").clicked() {
                    self.core.show_shortcuts = true;
                }
                if let Some(n) = self.core.store.notices().first() {
                    ui.add(egui::Label::new(RichText::new(&n.text).color(if n.error { t.down } else { t.muted })).truncate());
                }
            });
        });
    }

    fn phase(&self, ui: &mut Ui) {
        let t = self.core.theme;
        let Some(status) = self.core.store.status else {
            ui.label(RichText::new("No market session").color(t.muted));
            return;
        };
        let (label, color, next) = match status.phase {
            MarketPhase::Open => ("Market open", t.up, "closing auction"),
            MarketPhase::PreOpen => ("Pre-open", t.warning, "open"),
            MarketPhase::ClosingAuction => ("Closing auction", t.warning, "close"),
            MarketPhase::Closed => ("Market closed", t.muted, "pre-open"),
        };
        ui.label(RichText::new(label).color(color));
        if status.next_change_at > self.core.store.now {
            let left = (status.next_change_at - self.core.store.now) / 1_000;
            ui.label(RichText::new(format!("{next} in {left}s")).color(t.text));
        }
        ui.label(RichText::new(format!("day {}", status.day)).color(t.muted));
    }

    fn balances(&mut self, ui: &mut Ui) {
        let t = self.core.theme;
        let m = self.core.money();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let pair = |ui: &mut Ui, label: &str, value: String, color: Color32| {
                menu_label(ui, &t, label);
                ui.add_space(6.0);
                ui.label(RichText::new(value).font(Theme::bold(13.0)).color(color));
                divider(ui, &t);
            };
            pair(ui, "Stock BP", format::money(m.buying_power), t.strong);
            pair(ui, "Net Liq", format::money(m.net_liq.max(0) as u64), t.strong);
            pair(ui, "Cash", format::signed_money(m.cash), t.strong);
            pair(ui, "Unsettled", format::signed_money(m.unsettled), t.strong);
            pair(ui, "Day P/L", format::signed_money(m.day_pnl), t.tone(m.day_pnl as f64));
            pair(ui, "P/L Open", format::signed_money(m.open_pnl), t.tone(m.open_pnl as f64));
        });
    }

    fn tab_bar(&mut self, ui: &mut Ui) {
        let t = self.core.theme;
        Frame::new().fill(t.tab_bar).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(4.0);
                for tab in Tab::ALL {
                    let on = self.core.view.tab == tab;
                    if main_tab(ui, &t, tab.label(), on).on_hover_text(format!("Ctrl+{}", tab as usize + 1)).clicked() {
                        self.core.view.tab = tab;
                    }
                    divider(ui, &t);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(8.0);
                    let layout = menu_label(ui, &t, "Layout");
                    egui::Popup::menu(&layout).show(|ui| self.layout_menu(ui));
                });
            });
        });
    }

    fn layout_menu(&mut self, ui: &mut Ui) {
        let t = self.core.theme;
        ui.set_min_width(200.0);
        ui.label(RichText::new("Add gadget").color(t.muted));
        for kind in Kind::ALL {
            if ui.button(kind.title()).clicked() {
                self.core.view.add(kind);
                ui.close();
            }
        }
        ui.separator();
        ui.label(RichText::new("Trading layouts").color(t.muted));
        for layout in Trading::ALL {
            if ui.button(layout.label()).clicked() {
                self.core.view.set_trading_layout(layout);
                ui.close();
            }
        }
        ui.separator();
        ui.label(RichText::new("Chart grid").color(t.muted));
        for (cells, label) in [(1, "1 chart"), (2, "2 charts side by side"), (4, "2 × 2 charts")] {
            if ui.button(label).clicked() {
                self.core.view.set_chart_grid(cells);
                ui.close();
            }
        }
        ui.separator();
        let here: Vec<(u32, &'static str)> = self.core.view.dock().iter_all_tabs().map(|(_, g)| (g.id, g.kind.title())).collect();
        ui.menu_button("Detach to a window", |ui| {
            for (id, title) in here {
                if ui.button(title).clicked() {
                    self.core.view.detach(id);
                    ui.close();
                }
            }
        });
        if !self.core.view.detached.is_empty() && ui.button("Bring detached windows back").clicked() {
            for id in self.core.view.detached.iter().map(|g| g.id).collect::<Vec<_>>() {
                self.core.view.reattach(id);
            }
            ui.close();
        }
        if ui.button(format!("Reset {} layout", self.core.view.tab.label())).clicked() {
            self.core.view.reset_tab();
            ui.close();
        }
    }
}
