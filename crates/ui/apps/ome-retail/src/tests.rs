use crate::app::RetailApp;
use crate::view::{Kind, Link, Saved, Side as Gadgetry, Tab};
use crate::prefs::Prefs;
use crate::session::{Connection, Connector, Session};
use egui_kittest::Harness;
use egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use ome_core::candles::Candle;
use ome_core::order_book::Level;
use ome_core::symbol::Symbol;
use ome_core::types::{Balance, Side, TimeInForce};
use ome_ui::theme::Mode;
use ome_protocol::{
    AccountSnapshot, BookSnapshot, CandleSnapshot, Charges, ChargesEstimate, ClientMessage,
    ConditionalReport, ConditionalStatus, ContractNote, ExecutionReport, InstrumentInfo,
    InstrumentStatus, MarketPhase, MarketStats, MarketStatus, NewsItem, NewsKind, OrderStatus,
    Position, Role as AccountRole, ServerMessage, TradeEvent, Trigger, Venue,
};
use ome_ui_data::{Credentials, LinkUpdate, OrderRequest};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{self, Sender};

const T0: u64 = 1_794_726_000_000;

#[derive(Default)]
struct Broker {
    logins: Vec<Credentials>,
    orders: Vec<OrderRequest>,
    queries: Vec<ClientMessage>,
    feed: Option<Sender<LinkUpdate>>,
}

struct Fake(Rc<RefCell<Broker>>);

impl Session for Fake {
    fn submit(&self, request: OrderRequest) {
        self.0.borrow_mut().orders.push(request);
    }

    fn query(&self, message: ClientMessage) {
        self.0.borrow_mut().queries.push(message);
    }
}

fn fake() -> (Connector, Rc<RefCell<Broker>>) {
    let broker = Rc::new(RefCell::new(Broker::default()));
    let shared = broker.clone();
    let connector: Connector = Box::new(move |credentials| {
        let (tx, updates) = mpsc::channel();
        let mut b = shared.borrow_mut();
        b.logins.push(credentials);
        b.feed = Some(tx);
        Connection {
            session: Box::new(Fake(shared.clone())),
            updates,
        }
    });
    (connector, broker)
}

fn send(broker: &Rc<RefCell<Broker>>, messages: Vec<ServerMessage>) {
    let b = broker.borrow();
    let feed = b.feed.as_ref().expect("connected");
    for m in messages {
        feed.send(LinkUpdate::Message(m)).unwrap();
    }
}

fn sym(s: &str) -> Symbol {
    Symbol::parse(s).unwrap()
}

fn instrument(symbol: &str, name: &str, reference: u64, last: u64) -> InstrumentInfo {
    InstrumentInfo {
        symbol: sym(symbol),
        status: InstrumentStatus::Trading,
        reference_price: reference,
        last_price: Some(last),
        low_limit: reference * 9 / 10,
        high_limit: reference * 11 / 10,
        band_bps: 1_000,
        volume: 12_400,
        name: name.into(),
        halt_reason: String::new(),
    }
}

fn candles() -> Vec<Candle> {
    let walk = raw_candles();
    let shift = 25_40 - walk.last().map_or(25_40, |c| c.close as i64);
    walk.into_iter()
        .map(|c| Candle {
            open: (c.open as i64 + shift) as u64,
            high: (c.high as i64 + shift) as u64,
            low: (c.low as i64 + shift) as u64,
            close: (c.close as i64 + shift) as u64,
            ..c
        })
        .collect()
}

fn raw_candles() -> Vec<Candle> {
    let mut seed: u64 = 7;
    let mut price: i64 = 24_80;
    (0..180)
        .map(|i| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let step = ((seed >> 33) % 9) as i64 - 4 + if i > 120 { 1 } else { 0 };
            let open = price;
            price = (price + step).max(1);
            let mut c = Candle::new(T0 - (180 - i) * 1_000, open as u64, 20 + (seed >> 40) % 300);
            c.update(price as u64, 10);
            c.update((open.max(price) + 2) as u64, 1);
            c.update((open.min(price) - 2) as u64, 1);
            c.update(price as u64, 1);
            c
        })
        .collect()
}

fn order(id: u64, side: Side, price: u64, qty: u64, status: OrderStatus, filled: u64, last_qty: u64) -> ServerMessage {
    ServerMessage::Execution(ExecutionReport {
        seq: 0,
        order_id: id,
        client_order_id: id,
        account: 2,
        symbol: sym("ACME"),
        side,
        status,
        price,
        qty,
        filled,
        leaves: qty - filled,
        last_price: if last_qty > 0 { price } else { 0 },
        last_qty,
        fees: 0,
        cycle: 0,
        timestamp: T0,
        reason: String::new(),
    })
}

fn market() -> Vec<ServerMessage> {
    let acme = sym("ACME");
    let mut messages = vec![
        ServerMessage::MarketStatus(MarketStatus {
            phase: MarketPhase::PreOpen,
            day: 3,
            since: T0 - 90_000,
            next_change_at: T0 - 85_000,
        }),
        ServerMessage::MarketStatus(MarketStatus {
            phase: MarketPhase::Open,
            day: 3,
            since: T0 - 20_000,
            next_change_at: T0 + 25_000,
        }),
        ServerMessage::InstrumentList(vec![
            instrument("ACME", "Acme Robotics", 25_00, 25_40),
            instrument("NOVA", "Nova Energy", 12_50, 12_31),
            instrument("ZEN", "Zen Biotech", 80_00, 81_15),
        ]),
        ServerMessage::Candles(CandleSnapshot {
            symbol: acme,
            interval: 1_000,
            candles: candles(),
        }),
        ServerMessage::Book(BookSnapshot {
            symbol: acme,
            timestamp: T0,
            bids: (0..8)
                .map(|i| Level { price: 25_38 - i, qty: [120, 300, 80, 450, 60, 900, 200, 150][i as usize], orders: 1 + i as u32 % 3 })
                .collect(),
            asks: (0..8)
                .map(|i| Level { price: 25_41 + i, qty: [90, 210, 400, 75, 600, 130, 260, 1_000][i as usize], orders: 1 + i as u32 % 2 })
                .collect(),
        }),
        ServerMessage::MarketStats(MarketStats {
            symbol: acme,
            open: 25_00,
            high: 25_62,
            low: 24_71,
            last: 25_40,
            vwap: 25_12,
            trades: 412,
            turnover: 31_000_000,
            exchange_volume: 9_800,
            internal_volume: 600,
            wholesaler_volume: 1_700,
            ats_volume: 300,
        }),
        ServerMessage::Account(AccountSnapshot {
            account_id: 2,
            role: AccountRole::Retail,
            cash: Balance { available: 22_396_57, reserved: 253_00 },
            unsettled_cash: -962_00,
            day_pnl: 1_284_00,
            positions: vec![
                Position { symbol: acme, balance: Balance { available: 40, reserved: 0 }, unsettled: 40, avg_cost: 24_05, day_cost: 25_00 },
                Position { symbol: sym("NOVA"), balance: Balance { available: 100, reserved: 0 }, unsettled: 0, avg_cost: 12_80, day_cost: 12_50 },
            ],
        }),
        order(7, Side::Buy, 25_30, 10, OrderStatus::New, 0, 0),
        ServerMessage::ConditionalUpdate(ConditionalReport {
            cond_id: 3,
            client_order_id: 3,
            account: 2,
            symbol: acme,
            side: Side::Sell,
            qty: 20,
            status: ConditionalStatus::Armed,
            trigger: Trigger::Below(23_00),
            level: 23_00,
            reason: String::new(),
        }),
        ServerMessage::ContractNote(ContractNote {
            account: 2,
            order_id: 5,
            symbol: acme,
            side: Side::Buy,
            qty: 40,
            price: 24_05,
            charges: Charges { commission: 9, exchange_fee: 3, transaction_tax: 0, stamp_duty: 14, regulatory_fee: 1, gst: 2 },
            net_amount: -962_29,
            timestamp: T0 - 60_000,
        }),
    ];
    for i in 0..30u64 {
        messages.push(ServerMessage::Trade(TradeEvent {
            symbol: acme,
            trade_id: 100 + i,
            price: 25_36 + i % 6,
            qty: 5 + (i * 37) % 140,
            aggressor: if i % 3 == 0 { Side::Sell } else { Side::Buy },
            timestamp: T0 - 30_000 + i * 1_000,
            venue: [Venue::Exchange, Venue::Wholesaler, Venue::Internal][i as usize % 3],
        }));
    }
    let stories = [
        (NewsKind::Earnings, Some("ACME"), 41, 290, "Acme Robotics tops forecasts as quarterly profit rises"),
        (NewsKind::Macro, None, -12, -35, "Inflation runs hot at 2.86%, raising the odds of tighter policy"),
        (NewsKind::Company, Some("NOVA"), -38, -410, "Nova Energy recall raises cost concerns"),
        (NewsKind::Company, Some("ZEN"), 52, 220, "Zen Biotech wins a multi-year contract"),
    ];
    for (i, (kind, symbol, sentiment, impact, headline)) in stories.into_iter().enumerate() {
        messages.push(ServerMessage::News(NewsItem {
            seq: i as u64 + 1,
            time: T0 - 200_000 + i as u64 * 40_000,
            day: 3,
            kind,
            symbol: symbol.map(sym),
            sentiment,
            impact_bps: impact,
            sector: "robotics".into(),
            headline: headline.into(),
            body: "Quarterly earnings of 0.32 per share against a consensus of 0.31, on revenue of $140,683.".into(),
            data: "eps=0.32 consensus=0.31".into(),
        }));
    }
    messages
}

fn harness(connector: Connector) -> Harness<'static, RetailApp> {
    let app = RetailApp::new(Prefs::default(), None, connector, Saved::new());
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1500.0, 920.0))
        .build_ui_state(|ui, app: &mut RetailApp| app.draw(ui, T0), app);
    harness.run_steps(4);
    harness
}

fn type_into(harness: &mut Harness<'static, RetailApp>, role: Role, label: &str, text: &str) {
    harness.get_by_role_and_label(role, label).focus();
    harness.run_steps(2);
    harness.get_by_role_and_label(role, label).type_text(text);
    harness.run_steps(2);
}

fn log_in(harness: &mut Harness<'static, RetailApp>, user: &str, password: &str) {
    type_into(harness, Role::TextInput, "Login ID", user);
    type_into(harness, Role::PasswordInput, "Password", password);
    harness.get_by_label("Log In").click();
    harness.run_steps(3);
}

fn trading(extra: Vec<ServerMessage>) -> (Harness<'static, RetailApp>, Rc<RefCell<Broker>>) {
    let (connector, broker) = fake();
    let mut harness = harness(connector);
    log_in(&mut harness, "alice", "alice-pw");
    let mut first = vec![ServerMessage::LogonAck { account_id: 2, role: AccountRole::Retail, server_time: T0 }];
    first.extend(market());
    first.extend(extra);
    send(&broker, first);
    harness.run_steps(6);
    (harness, broker)
}

fn sent(broker: &Rc<RefCell<Broker>>) -> Vec<OrderRequest> {
    std::mem::take(&mut broker.borrow_mut().orders)
}

#[test]
fn the_login_screen_validates_typed_credentials_and_reports_a_refusal() {
    let (connector, broker) = fake();
    let mut harness = harness(connector);
    harness.snapshot("login");
    harness.get_by_label("Log In").click();
    harness.run_steps(2);
    assert!(harness.query_by_label_contains("Enter the broker address").is_some());
    log_in(&mut harness, "alice", "pässwörd");
    assert_eq!(broker.borrow().logins.len(), 1);
    assert_eq!(broker.borrow().logins[0].user, "alice");
    assert_eq!(broker.borrow().logins[0].secret, "pässwörd", "typed multi-byte password reaches the broker intact");
    assert!(harness.state().core.login.password.is_empty(), "the password is not kept after sending");
    broker.borrow().feed.as_ref().unwrap().send(LinkUpdate::Rejected("invalid username or secret".into())).unwrap();
    harness.run_steps(3);
    assert!(harness.query_by_label_contains("Login refused: invalid username or secret").is_some());
    harness.snapshot("login-refused");
}

#[test]
fn a_logged_in_trader_reviews_charges_sends_an_order_and_sees_the_fill() {
    let (mut harness, broker) = trading(Vec::new());
    assert!(harness.query_by_label("Position Statement").is_some(), "the first screen is Monitor");
    assert!(harness.query_by_label("Net Liq & Day P/L").is_some(), "the sidebar shows account info");
    harness.snapshot("monitor");

    harness.get_by_label("Trade").click();
    harness.run_steps(3);
    assert!(harness.query_by_label("Order Entry").is_some());
    harness.snapshot("trade");
    harness.get_by_label("Confirm and Send").click();
    harness.run_steps(3);
    let estimate = broker
        .borrow()
        .queries
        .iter()
        .find_map(|q| match q {
            ClientMessage::EstimateCharges { symbol, side, qty, price } => Some((*symbol, *side, *qty, *price)),
            _ => None,
        })
        .expect("the review asks the broker for charges");
    assert_eq!(estimate, (sym("ACME"), Side::Buy, 10, 25_41), "buyers start at the ask");
    send(
        &broker,
        vec![ServerMessage::ChargesEstimate(ChargesEstimate {
            symbol: sym("ACME"),
            side: Side::Buy,
            qty: 10,
            price: 25_41,
            cash_effect: -254_38,
            charges: Charges { commission: 13, exchange_fee: 3, transaction_tax: 0, stamp_duty: 8, regulatory_fee: 1, gst: 3 },
        })],
    );
    harness.run_steps(4);
    assert!(harness.query_by_label_contains("Total charges").is_some());
    harness.snapshot("review-order");

    harness.get_by_label("Send buy order").click();
    harness.run_steps(3);
    assert_eq!(
        sent(&broker).last(),
        Some(&OrderRequest::Limit { symbol: sym("ACME"), side: Side::Buy, price: 25_41, qty: 10, tif: TimeInForce::Day })
    );
    send(&broker, vec![order(8, Side::Buy, 25_41, 10, OrderStatus::Filled, 10, 10)]);
    harness.run_steps(4);
    assert!(harness.query_by_label_contains("Bought 10 ACME at 25.41").is_some());
    harness.snapshot("order-filled");
}

#[test]
fn squaring_off_cancels_the_symbols_orders_first_then_sells_everything_at_market() {
    let (mut harness, broker) = trading(vec![order(9, Side::Sell, 26_00, 5, OrderStatus::New, 0, 0)]);
    harness.query_all_by_label("Square off").next().expect("a button per position").click();
    harness.run_steps(3);
    assert!(harness.query_by_label_contains("Square off ACME").is_some());
    assert!(harness.query_by_label_contains("First cancels 2 working orders and 1 armed conditional").is_some());
    harness.snapshot("square-off");
    assert!(sent(&broker).is_empty(), "nothing is sent before confirming");

    harness.get_by_label("Confirm square off").click();
    harness.run_steps(3);
    assert_eq!(sent(&broker), [OrderRequest::CancelAll(Some(sym("ACME"))), OrderRequest::CancelConditional(3)], "the sell waits for its working sell order to cancel");
    send(&broker, vec![order(9, Side::Sell, 26_00, 5, OrderStatus::Cancelled, 0, 0)]);
    harness.run_steps(3);
    assert_eq!(sent(&broker), [OrderRequest::Market { symbol: sym("ACME"), side: Side::Sell, qty: 40 }]);
    assert!(harness.query_by_label_contains("Squaring off ACME").is_some());

    harness.get_by_label("Square off all").click();
    harness.run_steps(3);
    assert!(harness.query_by_label_contains("Square off all positions").is_some());
    harness.get_by_label("Confirm square off").click();
    harness.run_steps(3);
    assert_eq!(
        sent(&broker),
        [OrderRequest::Market { symbol: sym("NOVA"), side: Side::Sell, qty: 100 }],
        "ACME, already being squared off, is not sold twice"
    );
}

#[test]
fn keeping_positions_sends_nothing() {
    let (mut harness, broker) = trading(Vec::new());
    harness.get_by_label("Square off all").click();
    harness.run_steps(3);
    harness.get_by_label("Keep positions").click();
    harness.run_steps(3);
    assert!(harness.query_by_label("Confirm square off").is_none());
    assert!(sent(&broker).is_empty());
}

#[test]
fn settings_switch_the_theme_and_turn_off_confirmation() {
    let (mut harness, broker) = trading(Vec::new());
    harness.get_by_label("⚙ Setup").click();
    harness.run_steps(3);
    harness.get_by_label("Light").click();
    harness.get_by_label("On").click();
    harness.run_steps(2);
    assert!(harness.query_by_label_contains("go out at once").is_some(), "turning confirmation off warns");
    harness.get_by_label("Save").click();
    harness.run_steps(4);
    let prefs = &harness.state().core.prefs;
    assert_eq!((prefs.mode, prefs.confirm), (Mode::Light, false));
    assert_eq!(harness.state().core.theme.mode, Mode::Light);
    harness.snapshot("monitor-light");

    harness.get_by_label("Trade").click();
    harness.run_steps(3);
    harness.get_by_label("Send").click();
    harness.run_steps(3);
    assert_eq!(
        sent(&broker),
        [OrderRequest::Limit { symbol: sym("ACME"), side: Side::Buy, price: 25_41, qty: 10, tif: TimeInForce::Day }]
    );
    assert!(harness.query_by_label("Send buy order").is_none(), "no review dialog");
}

fn kinds(harness: &Harness<'static, RetailApp>, tab: Tab) -> Vec<Kind> {
    harness.state().core.view.docks[tab as usize].iter_all_tabs().map(|(_, g)| g.kind).collect()
}

fn menu(harness: &mut Harness<'static, RetailApp>, item: &str) {
    harness.get_by_label("Layout").click();
    harness.run_steps(2);
    harness.get_by_label(item).click();
    harness.run_steps(3);
}

#[test]
fn gadgets_dock_side_by_side_grid_detach_and_are_remembered() {
    let (mut harness, broker) = trading(Vec::new());
    harness.get_by_label("Account Statement").click();
    harness.run_steps(3);
    assert!(harness.query_by_label("Net Liquidating Value").is_some());
    harness.snapshot("statement");
    harness.get_by_label("Trade History").click();
    harness.run_steps(2);
    assert!(!harness.state().core.view.is_open("Trade History"));

    harness.get_by_label("Trade").click();
    harness.run_steps(2);
    harness.get_by_label("Active Trader · ACME").click();
    harness.run_steps(4);
    assert!(harness.query_by_label("Bid Size").is_some(), "the ladder");
    harness.snapshot("active-trader");
    harness.get_by_label("Flatten").click();
    harness.run_steps(3);
    assert!(harness.query_by_label_contains("Square off ACME").is_some(), "flatten squares off the ladder's symbol");
    harness.get_by_label("Keep positions").click();
    harness.run_steps(2);
    assert!(sent(&broker).is_empty());

    harness.get_by_label("Charts").click();
    harness.run_steps(3);
    harness.snapshot("charts");
    menu(&mut harness, "2 × 2 charts");
    assert_eq!(kinds(&harness, Tab::Charts), [Kind::Chart; 4]);
    harness.run_steps(3);
    harness.snapshot("charts-grid");
    menu(&mut harness, "Level II");
    assert!(kinds(&harness, Tab::Charts).contains(&Kind::LevelTwo), "any gadget can join any tab");

    harness.get_by_label("Layout").click();
    harness.run_steps(2);
    harness.get_by_label_contains("Detach to a window").click();
    harness.run_steps(2);
    harness.query_all_by_label("Level II").last().expect("the submenu lists the tab's gadgets").click();
    harness.run_steps(4);
    assert_eq!(harness.state().core.view.detached.len(), 1);
    assert!(!kinds(&harness, Tab::Charts).contains(&Kind::LevelTwo));
    assert!(harness.query_by_label_contains("Level II · OME Trader").is_some(), "the detached gadget gets its own window");
    menu(&mut harness, "Bring detached windows back");
    assert!(harness.state().core.view.detached.is_empty());

    harness.get_by_label("MarketWatch").click();
    harness.run_steps(3);
    assert!(harness.query_by_label("Description").is_some(), "the quote board");
    harness.snapshot("marketwatch");

    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num3);
    harness.run_steps(3);
    harness.state_mut().persist();
    let view = &harness.state().saved["alice"];
    assert_eq!(view.tab, Tab::Charts);
    assert!(!view.is_open("Trade History"));
    assert_eq!(view.docks[Tab::Charts as usize].iter_all_tabs().count(), 5);
}

#[test]
fn the_chart_docks_beside_the_ladder_and_watchlist() {
    let (mut harness, _broker) = trading(Vec::new());
    harness.get_by_label("Trade").click();
    harness.run_steps(2);
    harness.get_by_label("Active Trader · ACME").click();
    harness.run_steps(4);
    assert!(harness.query_by_label("Bid Size").is_some() && harness.query_by_label("Chart · ACME").is_some(), "ladder and chart on screen together");
    menu(&mut harness, "Watchlist, chart and ladder");
    harness.run_steps(4);
    assert!(harness.query_all_by_label("Watchlist").count() >= 2, "the watchlist gadget joins the sidebar one");
    assert!(harness.query_by_label("Bid Size").is_some() && harness.query_by_label("Chart · ACME").is_some());
    harness.snapshot("trade-watch-chart-ladder");
}

#[test]
fn linked_gadgets_follow_their_colour_group() {
    let (mut harness, _broker) = trading(Vec::new());
    harness.get_by_label("Charts").click();
    harness.run_steps(2);
    menu(&mut harness, "2 charts side by side");
    harness.query_all_by_label("Symbol link").nth(1).expect("two charts").click();
    harness.run_steps(2);
    harness.get_by_label("■  Blue").click();
    harness.run_steps(3);
    let links: Vec<Link> = harness.state().core.view.docks[Tab::Charts as usize].iter_all_tabs().map(|(_, g)| g.link).collect();
    assert_eq!(links.iter().filter(|l| **l == Link::Blue).count(), 1);
    harness.state_mut().core.groups.insert(Link::Blue, sym("NOVA"));
    harness.run_steps(3);
    assert!(harness.query_by_label("Chart · NOVA").is_some());
    assert!(harness.query_by_label("Chart · ACME").is_some(), "the red chart keeps the watchlist symbol");
    harness.get_by_label("ZEN").click();
    harness.run_steps(3);
    assert!(harness.query_by_label("Chart · ZEN").is_some() && harness.query_by_label("Chart · NOVA").is_some());
    harness.snapshot("linked-charts");
}

#[test]
fn sidebar_gadgets_are_added_removed_reordered_and_hidden() {
    let (mut harness, _broker) = trading(Vec::new());
    harness.get_by_label("Live News menu").click();
    harness.run_steps(2);
    harness.get_by_label("Remove gadget").click();
    harness.run_steps(2);
    assert!(!harness.state().core.view.sidebar.contains(&Gadgetry::LiveNews));
    harness.get_by_label("Add gadget to the sidebar").click();
    harness.run_steps(2);
    harness.get_by_label("Quote Details").click();
    harness.run_steps(3);
    assert!(harness.query_by_label("Prev Close").is_some());
    harness.get_by_label("Watchlist menu").click();
    harness.run_steps(2);
    harness.get_by_label("Move up").click();
    harness.run_steps(3);
    assert_eq!(harness.state().core.view.sidebar[0], Gadgetry::Watchlist);
    harness.snapshot("sidebar");
    harness.get_by_label("◀").click();
    harness.run_steps(3);
    assert!(!harness.state().core.view.sidebar_open);
    harness.get_by_label("▶").click();
    harness.run_steps(3);
    assert!(harness.state().core.view.sidebar_open);
}

#[test]
fn table_columns_are_chosen_per_table() {
    let (mut harness, _broker) = trading(Vec::new());
    assert!(harness.query_by_label("P/L %").is_some());
    harness.get_by_label("Customize positions").click();
    harness.run_steps(2);
    harness.get_by_role_and_label(Role::CheckBox, "P/L %").click();
    harness.run_steps(2);
    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert!(harness.state().core.view.hidden.contains("positions:P/L %"));
    assert!(harness.query_by_label("P/L %").is_none());
    assert!(harness.query_by_label("Trade Price").is_some());
}

#[test]
fn the_shortcut_overlay_opens_with_a_question_mark_and_closes_with_escape() {
    let (mut harness, _broker) = trading(Vec::new());
    harness.key_press(egui::Key::Questionmark);
    harness.run_steps(3);
    assert!(harness.query_by_label_contains("Keyboard shortcuts").is_some());
    harness.snapshot("shortcuts");
    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert!(harness.query_by_label_contains("Keyboard shortcuts").is_none());
}
