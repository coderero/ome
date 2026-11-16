use crate::prefs::Prefs;
use crate::session::{Connection, Connector};
use crate::ticket::{Kind, OrderForm, Quote, describe, priced};
use crate::view::{Link, Saved, View};
use ome_core::price::{Price, format_price};
use ome_core::symbol::Symbol;
use ome_core::types::Side;
use ome_protocol::{ClientMessage, ConditionalStatus, OrderStatus, ServerMessage};
use ome_ui::Theme;
use std::path::PathBuf;
use ome_ui::chart::Studies;
use ome_ui::toast::{Level, Toasts};
use ome_ui_data::{Credentials, LinkUpdate, MarketData, MarketStore, OrderRequest};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

pub const FLASH: Duration = Duration::from_millis(700);
pub const SQUARE_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Login,
    Trading,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub request: OrderRequest,
    pub text: String,
    pub side: Side,
    pub qty: u64,
    pub price: Price,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drag {
    pub side: Side,
    pub price: Price,
}

#[derive(Default)]
pub struct Login {
    pub broker: String,
    pub user: String,
    pub password: String,
    pub error: Option<String>,
    pub connecting: bool,
    pub reveal: bool,
    pub remember: bool,
    pub advanced: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    One(Symbol),
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsDraft {
    pub mode: ome_ui::theme::Mode,
    pub scale: u32,
    pub confirm: bool,
    pub qty: String,
    pub tif: ome_core::types::TimeInForce,
    pub error: Option<String>,
}

pub struct Core {
    pub theme: Theme,
    pub prefs: Prefs,
    pub prefs_file: Option<PathBuf>,
    pub restyle: bool,
    pub settings: Option<SettingsDraft>,
    pub square_confirm: Option<Scope>,
    pub squaring: Vec<(Symbol, Instant)>,
    pub squared: Vec<(Symbol, Instant)>,
    pub screen: Screen,
    pub login: Login,
    connector: Connector,
    pub conn: Option<Connection>,
    pub store: MarketStore,
    pub user: String,
    pub selected: Option<Symbol>,
    pub form: OrderForm,
    pub form_error: Option<String>,
    pub pending: Option<Pending>,
    pub toasts: Toasts,
    pub ladder_centers: HashMap<Symbol, Price>,
    pub groups: HashMap<Link, Symbol>,
    pub link: Link,
    pub form_symbol: Option<Symbol>,
    pub interval: u64,
    pub studies: Studies,
    pub chart_views: HashMap<u32, ome_ui::chart::ChartView>,
    pub view: View,
    pub news_selected: Option<u64>,
    pub show_shortcuts: bool,
    pub flashes: HashMap<Symbol, (bool, Instant)>,
    seen: HashMap<Symbol, Price>,
    requested: HashSet<(Symbol, u64)>,
    price_pending: bool,
}

impl Core {
    pub fn new(prefs: Prefs, prefs_file: Option<PathBuf>, connector: Connector) -> Self {
        let form = OrderForm {
            qty: prefs.qty.to_string(),
            tif: prefs.tif,
            ..OrderForm::default()
        };
        Self {
            theme: Theme::of(prefs.mode),
            prefs_file,
            restyle: true,
            settings: None,
            square_confirm: None,
            squaring: Vec::new(),
            squared: Vec::new(),
            login: Login {
                broker: prefs.broker.clone(),
                user: prefs.user.clone(),
                remember: prefs.remember,
                ..Login::default()
            },
            prefs,
            screen: Screen::Login,
            connector,
            conn: None,
            store: MarketStore::default(),
            user: String::new(),
            selected: None,
            form,
            form_error: None,
            pending: None,
            toasts: Toasts::default(),
            ladder_centers: HashMap::new(),
            groups: HashMap::new(),
            link: Link::Red,
            form_symbol: None,
            interval: 1_000,
            studies: Studies::default(),
            chart_views: HashMap::new(),
            view: View::default(),
            news_selected: None,
            show_shortcuts: false,
            flashes: HashMap::new(),
            seen: HashMap::new(),
            requested: HashSet::new(),
            price_pending: false,
        }
    }

    pub fn log_in(&mut self) {
        if self.login.connecting {
            return;
        }
        let broker = self.login.broker.trim().to_string();
        let user = self.login.user.trim().to_string();
        if broker.is_empty() || user.is_empty() || self.login.password.is_empty() {
            self.login.error = Some("Enter the broker address, your login ID and your password.".into());
            return;
        }
        let secret = std::mem::take(&mut self.login.password);
        self.conn = Some((self.connector)(Credentials {
            addr: broker.clone(),
            user: user.clone(),
            secret,
        }));
        self.prefs.broker = broker;
        self.prefs.remember = self.login.remember;
        self.prefs.user = if self.login.remember { user.clone() } else { String::new() };
        self.user = user;
        self.login.error = None;
        self.login.connecting = true;
    }

    pub fn log_out(&mut self) {
        self.conn = None;
        self.store = MarketStore::default();
        self.screen = Screen::Login;
        self.selected = None;
        self.groups.clear();
        self.form_symbol = None;
        self.pending = None;
        self.square_confirm = None;
        self.squaring.clear();
        self.squared.clear();
        self.settings = None;
        self.requested.clear();
        self.seen.clear();
        self.flashes.clear();
        self.login.connecting = false;
        self.login.password.clear();
    }

    pub fn pump(&mut self, now_ms: u64) -> bool {
        let mut updates = Vec::new();
        if let Some(conn) = &self.conn {
            while let Ok(update) = conn.updates.try_recv() {
                updates.push(update);
            }
        }
        let changed = !updates.is_empty();
        for update in updates {
            match update {
                LinkUpdate::Message(message) => {
                    if matches!(message, ServerMessage::LogonAck { .. }) {
                        self.screen = Screen::Trading;
                        self.login.connecting = false;
                        self.prefs.save(self.prefs_file.as_deref());
                    }
                    if self.screen == Screen::Trading {
                        self.notify(&message);
                    }
                    self.store.apply(message);
                }
                LinkUpdate::Down(reason) => {
                    if self.screen == Screen::Login {
                        self.fail(format!("Cannot reach the broker ({reason})."));
                        return true;
                    }
                    self.store.disconnected();
                    self.toasts.push(Level::Bad, "Connection lost", "Reconnecting to the broker…");
                }
                LinkUpdate::Rejected(reason) => {
                    self.fail(format!("Login refused: {reason}."));
                    return true;
                }
            }
        }
        if self.screen == Screen::Trading {
            self.store.roll(now_ms.max(self.store.now));
            if self.selected.is_none()
                && let Some(first) = self.store.symbols().first().copied()
            {
                self.select(first);
            }
            if self.price_pending {
                self.fill_from_book();
            }
            self.release_squares();
            self.track_flashes();
        }
        changed
    }

    fn fail(&mut self, reason: String) {
        self.conn = None;
        self.login.connecting = false;
        self.login.error = Some(reason);
        self.screen = Screen::Login;
    }

    fn notify(&mut self, message: &ServerMessage) {
        let verb = |side: Side| if side == Side::Buy { "Bought" } else { "Sold" };
        match message {
            ServerMessage::Execution(r) if r.last_qty > 0 => {
                let done = if r.status == OrderStatus::Filled { "filled" } else { "partially filled" };
                self.toasts.push(
                    Level::Good,
                    format!("{} {} {} at {}", verb(r.side), r.last_qty, r.symbol, format_price(r.last_price)),
                    format!("Order #{} {done}, {} of {}", r.order_id, r.filled, r.qty),
                );
            }
            ServerMessage::Execution(r) if r.status == OrderStatus::Rejected => {
                self.toasts.push(Level::Bad, format!("Order rejected · {}", r.symbol), r.reason.clone());
            }
            ServerMessage::ConditionalUpdate(c) if c.status == ConditionalStatus::Triggered => {
                self.toasts.push(Level::Info, format!("Conditional triggered · {}", c.symbol), c.trigger.describe());
            }
            ServerMessage::ConditionalUpdate(c) if c.status == ConditionalStatus::Rejected => {
                self.toasts.push(Level::Bad, format!("Conditional rejected · {}", c.symbol), c.reason.clone());
            }
            ServerMessage::Reject { reason, .. } => {
                self.toasts.push(Level::Bad, "Request refused", reason.clone());
            }
            _ => {}
        }
    }

    fn track_flashes(&mut self) {
        for symbol in self.store.symbols() {
            let Some(last) = self.store.last(symbol) else { continue };
            if let Some(previous) = self.seen.insert(symbol, last)
                && previous != last
            {
                self.flashes.insert(symbol, (last > previous, Instant::now()));
            }
        }
    }

    pub fn tick(&self, symbol: Symbol) -> Option<bool> {
        self.flashes.get(&symbol).map(|(up, _)| *up)
    }

    pub fn flashing(&self) -> bool {
        self.flashes.values().any(|(_, at)| at.elapsed() < FLASH)
    }

    pub fn quote(&self, symbol: Symbol) -> Quote {
        let book = self.store.book(symbol);
        Quote {
            bid: book.and_then(|b| b.bids.first()).map(|l| l.price),
            ask: book.and_then(|b| b.asks.first()).map(|l| l.price),
            last: self.store.last(symbol),
        }
    }

    pub fn select(&mut self, symbol: Symbol) {
        self.selected = Some(symbol);
        self.ladder_centers.remove(&symbol);
        self.price_pending = true;
        self.fill_from_book();
        self.request_history();
    }

    fn fill_from_book(&mut self) {
        let Some(symbol) = self.selected else { return };
        let quote = self.quote(symbol);
        if quote.bid.is_none() && quote.ask.is_none() && !self.form.price.is_empty() {
            return;
        }
        self.form.fill_price(quote);
        self.price_pending = quote.bid.is_none() && quote.ask.is_none();
    }

    pub fn step_symbol(&mut self, delta: i64) {
        let symbols = self.store.symbols();
        if symbols.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|s| symbols.iter().position(|x| *x == s))
            .unwrap_or(0) as i64;
        self.select(symbols[(at + delta).rem_euclid(symbols.len() as i64) as usize]);
    }

    pub fn request_history(&mut self) {
        let (Some(symbol), Some(conn)) = (self.selected, &self.conn) else { return };
        if self.interval >= 60_000 && self.requested.insert((symbol, self.interval)) {
            conn.session.query(ClientMessage::QueryHistory {
                symbol,
                interval_ms: self.interval,
                limit: 600,
            });
        }
    }

    pub fn set_price(&mut self, price: Price) {
        self.form.price = format_price(price);
    }

    pub fn review(&mut self, side: Option<Side>) {
        if let Some(side) = side {
            self.form.side = side;
        }
        let Some(symbol) = self.selected else { return };
        match self.form.build(symbol) {
            Ok(request) => {
                self.form_error = None;
                self.stage(request);
            }
            Err(e) => self.form_error = Some(e),
        }
    }

    pub fn stage(&mut self, request: OrderRequest) {
        let Some(symbol) = self.selected else { return };
        let Some((side, qty, price)) = priced(&request, self.quote(symbol)) else {
            self.form_error = Some("No price yet for this symbol.".into());
            return;
        };
        if !self.prefs.confirm {
            self.submit(request);
            self.toasts.push(Level::Info, "Order sent", describe(&request));
            return;
        }
        self.store.estimate = None;
        if let Some(conn) = &self.conn {
            conn.session.query(ClientMessage::EstimateCharges { symbol, side, qty, price });
        }
        self.pending = Some(Pending {
            text: describe(&request),
            request,
            side,
            qty,
            price,
        });
    }

    pub fn send_pending(&mut self) {
        if let Some(pending) = self.pending.take() {
            self.submit(pending.request);
            self.toasts.push(Level::Info, "Order sent", pending.text);
        }
    }

    pub fn submit(&mut self, request: OrderRequest) {
        if let Some(conn) = &self.conn {
            conn.session.submit(request);
        }
    }

    pub fn ladder_order(&mut self, side: Side, price: Price) {
        let Some(symbol) = self.selected else { return };
        match self.form.qty() {
            Ok(qty) => self.stage(OrderRequest::Limit {
                symbol,
                side,
                price,
                qty,
                tif: self.form.tif,
            }),
            Err(e) => self.form_error = Some(e),
        }
    }

    pub fn market(&mut self, side: Side) {
        let Some(symbol) = self.selected else { return };
        match self.form.qty() {
            Ok(qty) => {
                self.form_error = None;
                self.stage(OrderRequest::Market { symbol, side, qty });
            }
            Err(e) => self.form_error = Some(e),
        }
    }

    pub fn move_orders(&mut self, drag: Drag, price: Price) {
        let Some(symbol) = self.selected else { return };
        let orders: Vec<(u64, u64)> = self
            .store
            .orders
            .values()
            .filter(|o| o.symbol == symbol && o.side == drag.side && o.price == drag.price)
            .map(|o| (o.order_id, o.leaves))
            .collect();
        for (id, leaves) in orders {
            self.submit(OrderRequest::Cancel(id));
            if price != drag.price {
                self.submit(OrderRequest::Limit {
                    symbol,
                    side: drag.side,
                    price,
                    qty: leaves,
                    tif: self.form.tif,
                });
            }
        }
    }

    pub fn cancel_symbol(&mut self) {
        let Some(symbol) = self.selected else { return };
        self.submit(OrderRequest::CancelAll(Some(symbol)));
        let armed: Vec<u64> = self
            .store
            .conditionals
            .values()
            .filter(|c| c.symbol == symbol)
            .map(|c| c.cond_id)
            .collect();
        for id in armed {
            self.submit(OrderRequest::CancelConditional(id));
        }
    }

    pub fn set_kind(&mut self, kind: Kind) {
        self.form.kind = kind;
        self.form_error = None;
    }

    pub fn set_interval(&mut self, interval: u64) {
        self.interval = interval;
        self.request_history();
    }
}

pub struct RetailApp {
    pub core: Core,
    pub saved: Saved,
    owner: Option<String>,
}

impl RetailApp {
    pub fn new(prefs: Prefs, prefs_file: Option<PathBuf>, connector: Connector, saved: Saved) -> Self {
        Self {
            core: Core::new(prefs, prefs_file, connector),
            saved,
            owner: None,
        }
    }

    pub fn adopt_view(&mut self) {
        if self.owner.as_deref() == Some(self.core.user.as_str()) {
            return;
        }
        self.persist();
        self.core.view = self.saved.get(&self.core.user).cloned().unwrap_or_default();
        self.owner = Some(self.core.user.clone());
    }

    pub fn persist(&mut self) {
        if let Some(owner) = &self.owner {
            self.saved.insert(owner.clone(), self.core.view.clone());
        }
    }
}

impl Core {
    pub fn open_settings(&mut self) {
        self.settings = Some(SettingsDraft {
            mode: self.prefs.mode,
            scale: self.prefs.scale,
            confirm: self.prefs.confirm,
            qty: self.prefs.qty.to_string(),
            tif: self.prefs.tif,
            error: None,
        });
    }

    pub fn save_settings(&mut self) {
        let Some(draft) = self.settings.as_mut() else { return };
        let qty = match draft.qty.trim().replace(',', "").parse::<u64>() {
            Ok(q) if q > 0 => q,
            _ => {
                draft.error = Some("Default quantity must be a whole number above zero.".into());
                return;
            }
        };
        let draft = self.settings.take().unwrap_or_else(|| unreachable!());
        if draft.mode != self.prefs.mode || draft.scale != self.prefs.scale {
            self.restyle = true;
        }
        if qty != self.prefs.qty {
            self.form.qty = qty.to_string();
        }
        if draft.tif != self.prefs.tif {
            self.form.tif = draft.tif;
        }
        self.prefs.mode = draft.mode;
        self.prefs.scale = draft.scale;
        self.prefs.confirm = draft.confirm;
        self.prefs.qty = qty;
        self.prefs.tif = draft.tif;
        self.theme = Theme::of(draft.mode);
        self.prefs.save(self.prefs_file.as_deref());
        self.toasts.push(Level::Info, "Settings saved", "");
    }
}
