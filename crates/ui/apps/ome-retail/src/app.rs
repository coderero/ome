use crate::prefs::Prefs;
use crate::ticket::{Kind, OrderForm, Quote, describe, priced};
use gpui::{
    Context, Entity, FocusHandle, Focusable, KeyBinding, MouseButton, Pixels, Point, Task, Window,
    actions, div, prelude::*,
};
use gpui_elements::editable_text::EditableTextState;
use gpui_elements::editable_text::actions::{Enter as FieldEnter, Tab as FieldTab};
use ome_core::price::{Price, format_price};
use ome_core::symbol::Symbol;
use ome_core::types::Side;
use ome_protocol::{ClientMessage, ServerMessage};
use ome_ui_data::{
    BrokerLink, Credentials, LinkUpdate, MarketData, MarketStore, OrderEntry, OrderRequest,
};
use ome_ui_kit::Theme;
use ome_ui_kit::chart::Studies;
use ome_ui_kit::input::{focus, plain_state, secret_state};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

pub const CONTEXT: &str = "Retail";
const KEYS: &str = "Retail && !EditableText";
pub const FLASH: Duration = Duration::from_millis(600);

actions!(
    retail,
    [
        BuyKey,
        SellKey,
        NextSymbol,
        PreviousSymbol,
        ShowMonitor,
        ShowTrade,
        ShowCharts,
        ShowNews,
        Confirm,
        Dismiss,
        Recenter
    ]
);

pub fn bind_keys(cx: &mut gpui::App) {
    cx.bind_keys(
        ome_ui_kit::input::default_bindings()
            .as_keybindings(Some(ome_ui_kit::input::DEFAULT_INPUT_CONTEXT)),
    );
    let k = Some(KEYS);
    let c = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("b", BuyKey, k),
        KeyBinding::new("s", SellKey, k),
        KeyBinding::new("down", NextSymbol, k),
        KeyBinding::new("up", PreviousSymbol, k),
        KeyBinding::new("c", Recenter, k),
        KeyBinding::new("enter", Confirm, k),
        KeyBinding::new("f1", ShowMonitor, c),
        KeyBinding::new("f2", ShowTrade, c),
        KeyBinding::new("f3", ShowCharts, c),
        KeyBinding::new("f4", ShowNews, c),
        KeyBinding::new("escape", Dismiss, c),
    ]);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Login,
    Trading,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Monitor,
    Trade,
    Charts,
    News,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Monitor, Tab::Trade, Tab::Charts, Tab::News];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Monitor => "Monitor",
            Tab::Trade => "Trade",
            Tab::Charts => "Charts",
            Tab::News => "News",
        }
    }
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

pub struct Fields {
    pub qty: Entity<EditableTextState>,
    pub price: Entity<EditableTextState>,
    pub target: Entity<EditableTextState>,
    pub stop: Entity<EditableTextState>,
}

pub struct LoginForm {
    pub broker: Entity<EditableTextState>,
    pub user: Entity<EditableTextState>,
    pub secret: Rc<RefCell<String>>,
    pub secret_field: Entity<EditableTextState>,
    pub error: Option<String>,
    pub connecting: bool,
}

pub struct RetailApp {
    pub theme: Theme,
    pub prefs: Prefs,
    pub screen: Screen,
    pub login: LoginForm,
    pub store: MarketStore,
    pub link: Option<BrokerLink>,
    updates: Option<Receiver<LinkUpdate>>,
    pub user: String,
    pub selected: Option<Symbol>,
    pub tab: Tab,
    pub activity: usize,
    pub form: OrderForm,
    pub form_error: Option<String>,
    pub fields: Fields,
    pub pending: Option<Pending>,
    pub auto_send: bool,
    pub ladder_center: Option<Price>,
    pub drag: Option<Drag>,
    pub interval: u64,
    pub studies: Studies,
    pub slot: f32,
    pub hover: Option<Point<Pixels>>,
    pub news_selected: Option<u64>,
    price_pending: bool,
    pub flashes: HashMap<Symbol, (bool, Instant)>,
    seen: HashMap<Symbol, Price>,
    requested: HashSet<(Symbol, u64)>,
    focus: FocusHandle,
    _pump: Task<()>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl RetailApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prefs = Prefs::load();
        let secret = Rc::new(RefCell::new(String::new()));
        let login = LoginForm {
            broker: plain_state(&prefs.broker, cx),
            user: plain_state(&prefs.user, cx),
            secret_field: secret_state(secret.clone(), cx),
            secret,
            error: None,
            connecting: false,
        };
        let form = OrderForm::default();
        let fields = Fields {
            qty: plain_state(&form.qty, cx),
            price: plain_state(&form.price, cx),
            target: plain_state(&form.target_pct, cx),
            stop: plain_state(&form.stop_pct, cx),
        };
        let focus_handle = cx.focus_handle();
        if prefs.user.is_empty() {
            focus(&login.user, window, cx);
        } else {
            focus(&login.secret_field, window, cx);
        }
        let pump = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(40))
                    .await;
                if this.update(cx, |app, cx| app.pump(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            theme: Theme::dark(),
            prefs,
            screen: Screen::Login,
            login,
            store: MarketStore::default(),
            link: None,
            updates: None,
            user: String::new(),
            selected: None,
            tab: Tab::Trade,
            activity: 0,
            form,
            form_error: None,
            fields,
            pending: None,
            auto_send: false,
            ladder_center: None,
            drag: None,
            interval: 1_000,
            studies: Studies::default(),
            slot: 8.0,
            hover: None,
            news_selected: None,
            price_pending: false,
            flashes: HashMap::new(),
            seen: HashMap::new(),
            requested: HashSet::new(),
            focus: focus_handle,
            _pump: pump,
        }
    }

    pub fn log_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.login.connecting {
            return;
        }
        let broker = self.login.broker.read(cx).as_str().trim().to_string();
        let user = self.login.user.read(cx).as_str().trim().to_string();
        let secret = self.login.secret.borrow().clone();
        if broker.is_empty() || user.is_empty() || secret.is_empty() {
            self.login.error =
                Some("Enter the broker address, your username and your password.".into());
            cx.notify();
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.link = Some(BrokerLink::start(
            Credentials {
                addr: broker.clone(),
                user: user.clone(),
                secret,
            },
            tx,
        ));
        self.updates = Some(rx);
        self.login
            .secret_field
            .update(cx, |s, cx| s.emplace("", cx));
        self.login.secret.borrow_mut().clear();
        self.prefs = Prefs {
            broker,
            user: user.clone(),
        };
        self.user = user;
        self.login.error = None;
        self.login.connecting = true;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub fn log_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.link = None;
        self.updates = None;
        self.store = MarketStore::default();
        self.screen = Screen::Login;
        self.selected = None;
        self.pending = None;
        self.requested.clear();
        self.seen.clear();
        self.flashes.clear();
        self.login.connecting = false;
        self.login.secret.borrow_mut().clear();
        self.login
            .secret_field
            .update(cx, |s, cx| s.emplace("", cx));
        focus(&self.login.secret_field, window, cx);
        cx.notify();
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let mut stop = None;
        if let Some(updates) = &self.updates {
            while let Ok(update) = updates.try_recv() {
                changed = true;
                match update {
                    LinkUpdate::Message(message) => {
                        if matches!(message, ServerMessage::LogonAck { .. }) {
                            self.screen = Screen::Trading;
                            self.login.connecting = false;
                            self.prefs.save();
                        }
                        self.store.apply(message);
                    }
                    LinkUpdate::Down(reason) => {
                        if self.screen == Screen::Login {
                            stop = Some(format!("Cannot reach the broker ({reason})."));
                            break;
                        }
                        self.store.disconnected();
                    }
                    LinkUpdate::Rejected(reason) => {
                        stop = Some(format!("Login refused: {reason}."));
                        break;
                    }
                }
            }
        }
        if let Some(reason) = stop {
            self.link = None;
            self.updates = None;
            self.login.connecting = false;
            self.login.error = Some(reason);
            self.screen = Screen::Login;
        }
        if self.screen == Screen::Trading {
            self.store.roll(now_ms());
            if self.selected.is_none()
                && let Some(first) = self.store.symbols().first().copied()
            {
                self.select(first, cx);
            }
            self.track_flashes();
            if self.price_pending {
                self.fill_from_book(cx);
            }
        }
        let flashing = self.flashes.values().any(|(_, at)| at.elapsed() < FLASH);
        if changed || flashing {
            cx.notify();
        }
    }

    fn track_flashes(&mut self) {
        for symbol in self.store.symbols() {
            let Some(last) = self.store.last(symbol) else {
                continue;
            };
            if let Some(previous) = self.seen.insert(symbol, last)
                && previous != last
            {
                self.flashes
                    .insert(symbol, (last > previous, Instant::now()));
            }
        }
    }

    pub fn flash(&self, symbol: Symbol) -> Option<bool> {
        self.flashes
            .get(&symbol)
            .filter(|(_, at)| at.elapsed() < FLASH)
            .map(|(up, _)| *up)
    }

    pub fn quote(&self, symbol: Symbol) -> Quote {
        let book = self.store.book(symbol);
        Quote {
            bid: book.and_then(|b| b.bids.first()).map(|l| l.price),
            ask: book.and_then(|b| b.asks.first()).map(|l| l.price),
            last: self.store.last(symbol),
        }
    }

    pub fn select(&mut self, symbol: Symbol, cx: &mut Context<Self>) {
        self.selected = Some(symbol);
        self.ladder_center = None;
        self.price_pending = true;
        self.fill_from_book(cx);
        self.request_history();
        cx.notify();
    }

    fn fill_from_book(&mut self, cx: &mut Context<Self>) {
        let Some(symbol) = self.selected else {
            return;
        };
        let quote = self.quote(symbol);
        if quote.bid.is_none() && quote.ask.is_none() && !self.form.price.is_empty() {
            return;
        }
        self.form.fill_price(quote);
        let price = self.form.price.clone();
        self.fields.price.update(cx, |s, cx| s.emplace(&price, cx));
        self.price_pending = quote.bid.is_none() && quote.ask.is_none();
    }

    pub fn step_symbol(&mut self, delta: i64, cx: &mut Context<Self>) {
        let symbols = self.store.symbols();
        if symbols.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|s| symbols.iter().position(|x| *x == s))
            .unwrap_or(0) as i64;
        let next = (at + delta).rem_euclid(symbols.len() as i64) as usize;
        self.select(symbols[next], cx);
    }

    pub fn request_history(&mut self) {
        let (Some(symbol), Some(link)) = (self.selected, &self.link) else {
            return;
        };
        if self.interval >= 60_000 && self.requested.insert((symbol, self.interval)) {
            link.query(ClientMessage::QueryHistory {
                symbol,
                interval_ms: self.interval,
                limit: 600,
            });
        }
    }

    pub fn set_interval(&mut self, interval: u64, cx: &mut Context<Self>) {
        self.interval = interval;
        self.request_history();
        cx.notify();
    }

    pub fn set_price(&mut self, price: Price, cx: &mut Context<Self>) {
        self.form.price = format_price(price);
        let text = self.form.price.clone();
        self.fields.price.update(cx, |s, cx| s.emplace(&text, cx));
        cx.notify();
    }

    pub fn sync_form(&mut self, cx: &mut Context<Self>) {
        self.form.qty = self.fields.qty.read(cx).as_str().to_string();
        self.form.price = self.fields.price.read(cx).as_str().to_string();
        self.form.target_pct = self.fields.target.read(cx).as_str().to_string();
        self.form.stop_pct = self.fields.stop.read(cx).as_str().to_string();
    }

    pub fn open_ticket(&mut self, side: Option<Side>, cx: &mut Context<Self>) {
        self.sync_form(cx);
        if let Some(side) = side {
            self.form.side = side;
        }
        let Some(symbol) = self.selected else {
            return;
        };
        match self.form.build(symbol) {
            Ok(request) => {
                self.form_error = None;
                self.stage(request, cx);
            }
            Err(e) => {
                self.form_error = Some(e);
                cx.notify();
            }
        }
    }

    pub fn stage(&mut self, request: OrderRequest, cx: &mut Context<Self>) {
        let Some(symbol) = self.selected else {
            return;
        };
        if self.auto_send {
            self.send(request, cx);
            return;
        }
        let Some((side, qty, price)) = priced(&request, self.quote(symbol)) else {
            self.form_error = Some("No price yet for this symbol.".into());
            cx.notify();
            return;
        };
        self.store.estimate = None;
        if let Some(link) = &self.link {
            link.query(ClientMessage::EstimateCharges {
                symbol,
                side,
                qty,
                price,
            });
        }
        self.pending = Some(Pending {
            text: describe(&request),
            request,
            side,
            qty,
            price,
        });
        cx.notify();
    }

    pub fn send(&mut self, request: OrderRequest, cx: &mut Context<Self>) {
        if let Some(link) = &self.link {
            link.submit(request);
        }
        self.pending = None;
        cx.notify();
    }

    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.screen {
            Screen::Login => self.log_in(window, cx),
            Screen::Trading => match self.pending.take() {
                Some(pending) => self.send(pending.request, cx),
                None => self.open_ticket(None, cx),
            },
        }
    }

    pub fn ladder_order(&mut self, side: Side, price: Price, cx: &mut Context<Self>) {
        let Some(symbol) = self.selected else {
            return;
        };
        self.sync_form(cx);
        match self.form.qty() {
            Ok(qty) => self.stage(
                OrderRequest::Limit {
                    symbol,
                    side,
                    price,
                    qty,
                    tif: self.form.tif,
                },
                cx,
            ),
            Err(e) => {
                self.form_error = Some(e);
                cx.notify();
            }
        }
    }

    pub fn drop_on(&mut self, side: Side, price: Price, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        let (Some(symbol), Some(link)) = (self.selected, &self.link) else {
            return;
        };
        if drag.side != side {
            return;
        }
        let orders: Vec<(u64, u64)> = self
            .store
            .orders
            .values()
            .filter(|o| o.symbol == symbol && o.side == side && o.price == drag.price)
            .map(|o| (o.order_id, o.leaves))
            .collect();
        for (id, leaves) in orders {
            link.submit(OrderRequest::Cancel(id));
            if price != drag.price {
                link.submit(OrderRequest::Limit {
                    symbol,
                    side,
                    price,
                    qty: leaves,
                    tif: self.form.tif,
                });
            }
        }
        cx.notify();
    }

    pub fn cancel_symbol(&mut self, cx: &mut Context<Self>) {
        let (Some(symbol), Some(link)) = (self.selected, &self.link) else {
            return;
        };
        link.submit(OrderRequest::CancelAll(Some(symbol)));
        for c in self
            .store
            .conditionals
            .values()
            .filter(|c| c.symbol == symbol)
        {
            link.submit(OrderRequest::CancelConditional(c.cond_id));
        }
        cx.notify();
    }

    pub fn cancel(&mut self, request: OrderRequest, cx: &mut Context<Self>) {
        if let Some(link) = &self.link {
            link.submit(request);
        }
        cx.notify();
    }

    pub fn set_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        self.form.kind = kind;
        self.form_error = None;
        cx.notify();
    }

    pub fn show(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.screen == Screen::Trading {
            self.tab = tab;
            cx.notify();
        }
    }

    pub fn prefill(&mut self, launch: crate::Launch, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(broker) = launch.broker {
            self.login.broker.update(cx, |s, cx| s.emplace(&broker, cx));
        }
        if let Some(user) = launch.user {
            self.login.user.update(cx, |s, cx| s.emplace(&user, cx));
        }
        if let Some(tab) = launch.tab.and_then(|name| {
            Tab::ALL
                .into_iter()
                .find(|t| t.label().eq_ignore_ascii_case(&name))
        }) {
            self.tab = tab;
        }
        if let Some(interval) = launch.interval {
            self.interval = interval.max(1_000);
        }
        if let Some(secret) = launch.secret {
            *self.login.secret.borrow_mut() = secret;
            self.log_in(window, cx);
        }
    }

    fn cycle_login_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = |state: &Entity<EditableTextState>, window: &Window, cx: &Context<Self>| {
            state.read(cx).focus_handle(cx).is_focused(window)
        };
        let next = if focused(&self.login.broker, window, cx) {
            self.login.user.clone()
        } else if focused(&self.login.user, window, cx) {
            self.login.secret_field.clone()
        } else {
            self.login.broker.clone()
        };
        focus(&next, window, cx);
    }
}

impl Render for RetailApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let content = match self.screen {
            Screen::Login => self.render_login(cx).into_any_element(),
            Screen::Trading => self.render_trading(window, cx).into_any_element(),
        };
        div()
            .size_full()
            .relative()
            .bg(t.background)
            .text_color(t.text)
            .font_family(ome_ui_kit::theme::SANS)
            .text_size(gpui::px(12.))
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|app, _: &BuyKey, _, cx| {
                if app.screen == Screen::Trading {
                    app.open_ticket(Some(Side::Buy), cx)
                }
            }))
            .on_action(cx.listener(|app, _: &SellKey, _, cx| {
                if app.screen == Screen::Trading {
                    app.open_ticket(Some(Side::Sell), cx)
                }
            }))
            .on_action(cx.listener(|app, _: &NextSymbol, _, cx| app.step_symbol(1, cx)))
            .on_action(cx.listener(|app, _: &PreviousSymbol, _, cx| app.step_symbol(-1, cx)))
            .on_action(cx.listener(|app, _: &Recenter, _, cx| {
                app.ladder_center = None;
                cx.notify();
            }))
            .on_action(cx.listener(|app, _: &ShowMonitor, _, cx| app.show(Tab::Monitor, cx)))
            .on_action(cx.listener(|app, _: &ShowTrade, _, cx| app.show(Tab::Trade, cx)))
            .on_action(cx.listener(|app, _: &ShowCharts, _, cx| app.show(Tab::Charts, cx)))
            .on_action(cx.listener(|app, _: &ShowNews, _, cx| app.show(Tab::News, cx)))
            .on_action(cx.listener(|app, _: &Confirm, window, cx| app.confirm(window, cx)))
            .on_action(cx.listener(|app, _: &FieldEnter, window, cx| app.confirm(window, cx)))
            .on_action(cx.listener(|app, _: &FieldTab, window, cx| {
                if app.screen == Screen::Login {
                    app.cycle_login_focus(window, cx);
                }
            }))
            .on_action(cx.listener(|app, _: &Dismiss, window, cx| {
                app.pending = None;
                app.drag = None;
                if app.screen == Screen::Trading {
                    window.focus(&app.focus, cx);
                }
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|app, _, _, cx| {
                    if app.drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .child(content)
    }
}
