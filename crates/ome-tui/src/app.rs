use crate::command::{ChartMode, Command, parse_command};
use ome_client::{Client, ClientError, Session};
use ome_core::candles::CandleSeries;
use ome_core::order_book::Level;
use ome_core::price::format_price;
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, OrderId, Side, Timestamp};
use ome_protocol::{
    AccountInfo, AccountSnapshot, Asset, ClientMessage, ExecutionReport, InstrumentInfo,
    InstrumentStatus, OrderStatus, Role, ServerMessage, TradeEvent,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

pub const TICK_MS: u64 = 50;
const LOG_CAPACITY: usize = 300;
const HISTORY_CAPACITY: usize = 100;
const TAPE_CAPACITY: usize = 200;
const CANDLE_CAPACITY: usize = 600;
const MAX_MESSAGES_PER_TICK: usize = 20_000;
const ACCOUNTS_REFRESH: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Success,
    Warning,
    Error,
}

pub struct LogEntry {
    pub time: Timestamp,
    pub level: LogLevel,
    pub text: String,
}

pub struct MarketView {
    pub symbol: Symbol,
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
    pub candles: CandleSeries,
    pub tape: VecDeque<TradeEvent>,
}

impl MarketView {
    fn new(symbol: Symbol) -> Self {
        Self {
            symbol,
            bids: Vec::new(),
            asks: Vec::new(),
            candles: CandleSeries::new(1_000, CANDLE_CAPACITY),
            tape: VecDeque::with_capacity(TAPE_CAPACITY),
        }
    }
}

pub trait Connection {
    fn send(&mut self, message: &ClientMessage) -> Result<(), ClientError>;
    fn try_recv(&mut self) -> Result<Option<ServerMessage>, ClientError>;
}

impl Connection for Client {
    fn send(&mut self, message: &ClientMessage) -> Result<(), ClientError> {
        Client::send(self, message)
    }

    fn try_recv(&mut self) -> Result<Option<ServerMessage>, ClientError> {
        Client::try_recv(self)
    }
}

pub struct App {
    connection: Box<dyn Connection>,
    pub role: Role,
    pub account_id: AccountId,
    pub username: String,
    pub addr: String,
    pub connected: bool,
    pub server_time: Timestamp,
    pub instruments: BTreeMap<Symbol, InstrumentInfo>,
    pub view: Option<MarketView>,
    pub account: Option<AccountSnapshot>,
    pub orders: BTreeMap<OrderId, ExecutionReport>,
    pub accounts: Vec<AccountInfo>,
    pub chart_mode: ChartMode,
    pub show_help: bool,
    pub input: String,
    pub log: VecDeque<LogEntry>,
    pub should_quit: bool,
    pub clear_screen: bool,
    preferred_symbol: Option<Symbol>,
    history: Vec<String>,
    history_cursor: Option<usize>,
    next_client_order_id: u64,
    last_accounts_refresh: Instant,
}

impl App {
    pub fn new(
        connection: Box<dyn Connection>,
        session: Session,
        username: String,
        addr: String,
        preferred_symbol: Option<Symbol>,
    ) -> Self {
        let mut app = Self {
            connection,
            role: session.role,
            account_id: session.account_id,
            username,
            addr,
            connected: true,
            server_time: session.server_time,
            instruments: BTreeMap::new(),
            view: None,
            account: None,
            orders: BTreeMap::new(),
            accounts: Vec::new(),
            chart_mode: ChartMode::Candles,
            show_help: false,
            input: String::new(),
            log: VecDeque::new(),
            should_quit: false,
            clear_screen: false,
            preferred_symbol,
            history: Vec::new(),
            history_cursor: None,
            next_client_order_id: session.server_time << 8,
            last_accounts_refresh: Instant::now(),
        };
        app.push_log(
            LogLevel::Info,
            format!(
                "Logged on as {} ({}, account {}). Type 'help' for commands.",
                app.username,
                app.role.label(),
                app.account_id
            ),
        );
        app
    }

    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }

    pub fn selected(&self) -> Option<&InstrumentInfo> {
        self.view
            .as_ref()
            .and_then(|v| self.instruments.get(&v.symbol))
    }

    pub fn on_tick(&mut self) {
        for _ in 0..MAX_MESSAGES_PER_TICK {
            match self.connection.try_recv() {
                Ok(Some(message)) => self.apply(message),
                Ok(None) => break,
                Err(_) => {
                    if self.connected {
                        self.connected = false;
                        self.push_log(
                            LogLevel::Error,
                            "Disconnected from server. Restart to reconnect.".into(),
                        );
                    }
                    break;
                }
            }
        }
        if let Some(view) = &mut self.view {
            view.candles.roll_to(self.server_time);
        }
        if self.is_admin() && self.last_accounts_refresh.elapsed() >= ACCOUNTS_REFRESH {
            self.last_accounts_refresh = Instant::now();
            self.send(ClientMessage::QueryAccounts);
        }
    }

    pub fn apply(&mut self, message: ServerMessage) {
        match message {
            ServerMessage::LogonAck { .. } => {}
            ServerMessage::Heartbeat { server_time } => {
                self.server_time = self.server_time.max(server_time)
            }
            ServerMessage::Reject { reason, .. } => self.push_log(LogLevel::Error, reason),
            ServerMessage::Ack { text, .. } => self.push_log(LogLevel::Success, text),
            ServerMessage::Execution(report) => self.apply_execution(report),
            ServerMessage::Account(snapshot) => self.account = Some(snapshot),
            ServerMessage::AccountList(list) => self.accounts = list,
            ServerMessage::InstrumentList(list) => {
                for info in list {
                    self.instruments.insert(info.symbol, info);
                }
                self.ensure_selection();
            }
            ServerMessage::Instrument(info) => self.apply_instrument(info),
            ServerMessage::Book(book) => {
                if let Some(view) = self.view.as_mut().filter(|v| v.symbol == book.symbol) {
                    view.bids = book.bids;
                    view.asks = book.asks;
                }
                self.server_time = self.server_time.max(book.timestamp);
            }
            ServerMessage::Trade(trade) => {
                if let Some(info) = self.instruments.get_mut(&trade.symbol) {
                    info.last_price = Some(trade.price);
                    if trade.trade_id != 0 {
                        info.volume += trade.qty;
                    }
                }
                if let Some(view) = self.view.as_mut().filter(|v| v.symbol == trade.symbol) {
                    if trade.trade_id != 0 {
                        view.candles.record(trade.price, trade.qty, trade.timestamp);
                    }
                    if view.tape.len() == TAPE_CAPACITY {
                        view.tape.pop_back();
                    }
                    view.tape.push_front(trade);
                }
                self.server_time = self.server_time.max(trade.timestamp);
            }
            ServerMessage::Candles(snapshot) => {
                if let Some(view) = self.view.as_mut().filter(|v| v.symbol == snapshot.symbol) {
                    view.candles = CandleSeries::from_candles(
                        snapshot.interval,
                        CANDLE_CAPACITY,
                        snapshot.candles,
                    );
                }
            }
        }
    }

    fn apply_execution(&mut self, report: ExecutionReport) {
        let price = if report.price == 0 {
            "MKT".to_string()
        } else {
            format_price(report.price)
        };
        let summary = format!(
            "#{} {} {} {} @ {}",
            report.order_id,
            report.symbol,
            report.side.label(),
            report.qty,
            price
        );
        match report.status {
            OrderStatus::New if report.timestamp + 2_000 >= self.server_time => {
                self.push_log(LogLevel::Info, format!("Accepted {summary}"))
            }
            OrderStatus::New => {}
            OrderStatus::PartiallyFilled | OrderStatus::Filled if report.last_qty > 0 => self
                .push_log(
                    LogLevel::Success,
                    format!(
                        "{} #{} {} {} @ {} ({}/{})",
                        if report.status == OrderStatus::Filled {
                            "Filled"
                        } else {
                            "Partial"
                        },
                        report.order_id,
                        report.side.label(),
                        report.last_qty,
                        format_price(report.last_price),
                        report.filled,
                        report.qty
                    ),
                ),
            OrderStatus::PartiallyFilled | OrderStatus::Filled => {}
            OrderStatus::Cancelled => self.push_log(
                LogLevel::Info,
                format!("Cancelled {summary}: {}", report.reason),
            ),
            OrderStatus::Expired => self.push_log(
                LogLevel::Warning,
                format!("Expired {summary}: {}", report.reason),
            ),
            OrderStatus::Rejected => self.push_log(
                LogLevel::Error,
                format!("Rejected {summary}: {}", report.reason),
            ),
        }
        if report.status.is_open() && report.order_id != 0 {
            self.orders.insert(report.order_id, report);
        } else {
            self.orders.remove(&report.order_id);
        }
    }

    fn apply_instrument(&mut self, info: InstrumentInfo) {
        match self.instruments.get(&info.symbol) {
            None => self.push_log(
                LogLevel::Info,
                format!(
                    "New listing: {} ({}) at {}",
                    info.symbol,
                    info.name,
                    format_price(info.reference_price)
                ),
            ),
            Some(old) if old.status != info.status => match info.status {
                InstrumentStatus::Halted => self.push_log(
                    LogLevel::Warning,
                    format!("{} HALTED: {}", info.symbol, info.halt_reason),
                ),
                InstrumentStatus::Trading => self.push_log(
                    LogLevel::Info,
                    format!(
                        "{} resumed, new reference {}",
                        info.symbol,
                        format_price(info.reference_price)
                    ),
                ),
            },
            Some(_) => {}
        }
        self.instruments.insert(info.symbol, info);
        self.ensure_selection();
    }

    fn ensure_selection(&mut self) {
        if self.view.is_some() {
            return;
        }
        let pick = self
            .preferred_symbol
            .filter(|s| self.instruments.contains_key(s))
            .or_else(|| self.instruments.keys().next().copied());
        if let Some(symbol) = pick {
            self.select(symbol);
        }
    }

    fn select(&mut self, symbol: Symbol) {
        if let Some(old) = self.view.as_ref().map(|v| v.symbol) {
            if old == symbol {
                return;
            }
            self.send(ClientMessage::Unsubscribe { symbol: old });
        }
        self.view = Some(MarketView::new(symbol));
        self.preferred_symbol = Some(symbol);
        self.send(ClientMessage::Subscribe { symbol });
    }

    fn cycle_symbol(&mut self, forward: bool) {
        let symbols: Vec<Symbol> = self.instruments.keys().copied().collect();
        if symbols.is_empty() {
            return;
        }
        let current = self
            .view
            .as_ref()
            .and_then(|v| symbols.iter().position(|s| *s == v.symbol))
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % symbols.len()
        } else {
            (current + symbols.len() - 1) % symbols.len()
        };
        self.select(symbols[next]);
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        let idle = self.input.is_empty();
        match key.code {
            KeyCode::Char('l') if ctrl => self.execute(Command::Clear),
            KeyCode::Enter => self.submit_input(),
            KeyCode::Esc => self.input.clear(),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Up => self.recall_history(true),
            KeyCode::Down => self.recall_history(false),
            KeyCode::Tab => {
                self.chart_mode = match self.chart_mode {
                    ChartMode::Candles => ChartMode::Line,
                    ChartMode::Line => ChartMode::Candles,
                }
            }
            KeyCode::Left if idle => self.cycle_symbol(false),
            KeyCode::Right if idle => self.cycle_symbol(true),
            KeyCode::F(1) => self.show_help = true,
            KeyCode::Char('?') if idle => self.show_help = true,
            KeyCode::Char(c) if !ctrl => self.input.push(c),
            _ => {}
        }
    }

    pub fn execute(&mut self, command: Command) {
        let current = self.view.as_ref().map(|v| v.symbol);
        match command {
            Command::Order { side, kind, qty } => {
                if !self.require_trader() {
                    return;
                }
                let Some(symbol) = current else {
                    return self.push_log(LogLevel::Error, "no instrument selected".into());
                };
                self.next_client_order_id += 1;
                self.send(ClientMessage::NewOrder {
                    client_order_id: self.next_client_order_id,
                    symbol,
                    side,
                    kind,
                    qty,
                });
            }
            Command::Cancel(order_id) => {
                if self.require_trader() {
                    self.send(ClientMessage::Cancel { order_id });
                }
            }
            Command::CancelAll => {
                if self.require_trader() {
                    self.send(ClientMessage::CancelAll { symbol: None });
                }
            }
            Command::Select(symbol) => {
                if self.instruments.contains_key(&symbol) {
                    self.select(symbol);
                } else {
                    self.push_log(LogLevel::Error, format!("unknown symbol {symbol}"));
                }
            }
            Command::Chart(mode) => self.chart_mode = mode,
            Command::Clear => {
                self.log.clear();
                self.clear_screen = true;
            }
            Command::Help => self.show_help = true,
            Command::Quit => self.should_quit = true,
            Command::CreateAccount {
                username,
                secret,
                role,
                cash,
            } => {
                if self.require_admin() {
                    self.send(ClientMessage::CreateAccount {
                        role,
                        cash,
                        username,
                        secret,
                    });
                }
            }
            Command::SetCash { account, amount } => {
                if self.require_admin()
                    && let Some(account_id) = self.resolve(&account)
                {
                    self.send(ClientMessage::SetBalance {
                        account_id,
                        asset: Asset::Cash,
                        amount,
                    });
                }
            }
            Command::SetShares {
                account,
                symbol,
                qty,
            } => {
                if self.require_admin()
                    && let Some(account_id) = self.resolve(&account)
                {
                    self.send(ClientMessage::SetBalance {
                        account_id,
                        asset: Asset::Shares(symbol),
                        amount: qty,
                    });
                }
            }
            Command::Ipo {
                symbol,
                price,
                shares,
                issuer,
                name,
            } => {
                if self.require_admin()
                    && let Some(issuer) = self.resolve(&issuer)
                {
                    self.send(ClientMessage::ListInstrument {
                        symbol,
                        ipo_price: price,
                        shares,
                        issuer,
                        name,
                    });
                }
            }
            Command::Halt { symbol, reason } => {
                if self.require_admin()
                    && let Some(symbol) = symbol.or(current)
                {
                    self.send(ClientMessage::Halt { symbol, reason });
                }
            }
            Command::Resume(symbol) => {
                if self.require_admin()
                    && let Some(symbol) = symbol.or(current)
                {
                    self.send(ClientMessage::Resume { symbol });
                }
            }
            Command::Accounts => {
                if self.require_admin() {
                    self.send(ClientMessage::QueryAccounts);
                }
            }
        }
    }

    fn require_admin(&mut self) -> bool {
        if !self.is_admin() {
            self.push_log(
                LogLevel::Error,
                "admin command: log on as an admin account".into(),
            );
        }
        self.is_admin()
    }

    fn require_trader(&mut self) -> bool {
        if self.is_admin() {
            self.push_log(LogLevel::Error, "admin accounts cannot trade".into());
        }
        !self.is_admin()
    }

    fn resolve(&mut self, account: &str) -> Option<AccountId> {
        let found = account.parse().ok().or_else(|| {
            self.accounts
                .iter()
                .find(|a| a.username == account)
                .map(|a| a.account_id)
        });
        if found.is_none() {
            self.push_log(
                LogLevel::Error,
                format!("no account '{account}' (use an id or username)"),
            );
        }
        found
    }

    fn send(&mut self, message: ClientMessage) {
        if self.connection.send(&message).is_err() && self.connected {
            self.connected = false;
            self.push_log(LogLevel::Error, "Disconnected from server.".into());
        }
    }

    fn submit_input(&mut self) {
        let line = std::mem::take(&mut self.input).trim().to_string();
        self.history_cursor = None;
        if line.is_empty() {
            return;
        }
        if self.history.last() != Some(&line) {
            self.history.push(line.clone());
            if self.history.len() > HISTORY_CAPACITY {
                self.history.remove(0);
            }
        }
        match parse_command(&line) {
            Ok(command) => self.execute(command),
            Err(message) => self.push_log(LogLevel::Error, message),
        }
    }

    fn recall_history(&mut self, older: bool) {
        if self.history.is_empty() {
            return;
        }
        let last = self.history.len() - 1;
        self.history_cursor = match (self.history_cursor, older) {
            (None, true) => Some(last),
            (None, false) => None,
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) if i < last => Some(i + 1),
            (Some(_), false) => None,
        };
        self.input = self
            .history_cursor
            .map(|i| self.history[i].clone())
            .unwrap_or_default();
    }

    fn push_log(&mut self, level: LogLevel, text: String) {
        if self.log.len() == LOG_CAPACITY {
            self.log.pop_front();
        }
        self.log.push_back(LogEntry {
            time: self.server_time,
            level,
            text,
        });
    }
}

pub fn side_symbol(side: Side) -> &'static str {
    match side {
        Side::Buy => "▲",
        Side::Sell => "▼",
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use ome_core::candles::Candle;
    use ome_core::types::{Balance, OrderKind};
    use ome_protocol::{BookSnapshot, CandleSnapshot, Position};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub struct FakeConnection {
        pub sent: Arc<Mutex<Vec<ClientMessage>>>,
        pub inbox: Arc<Mutex<VecDeque<ServerMessage>>>,
    }

    impl Connection for FakeConnection {
        fn send(&mut self, message: &ClientMessage) -> Result<(), ClientError> {
            self.sent.lock().unwrap().push(message.clone());
            Ok(())
        }

        fn try_recv(&mut self) -> Result<Option<ServerMessage>, ClientError> {
            Ok(self.inbox.lock().unwrap().pop_front())
        }
    }

    pub fn sym(s: &str) -> Symbol {
        Symbol::parse(s).unwrap()
    }

    pub fn info(symbol: &str, status: InstrumentStatus) -> InstrumentInfo {
        InstrumentInfo {
            symbol: sym(symbol),
            status,
            reference_price: 25_00,
            last_price: Some(25_10),
            low_limit: 17_50,
            high_limit: 32_50,
            volume: 1_000,
            name: format!("{symbol} Corp"),
            halt_reason: if status == InstrumentStatus::Halted {
                "circuit breaker".into()
            } else {
                String::new()
            },
        }
    }

    pub fn app(role: Role) -> (App, FakeConnection) {
        let fake = FakeConnection::default();
        let session = Session {
            account_id: 3,
            role,
            server_time: 120_000,
        };
        let mut app = App::new(
            Box::new(fake.clone()),
            session,
            "tester".into(),
            "local".into(),
            None,
        );
        app.apply(ServerMessage::InstrumentList(vec![
            info("ACME", InstrumentStatus::Trading),
            info("NOVA", InstrumentStatus::Trading),
        ]));
        (app, fake)
    }

    pub fn populated(role: Role) -> (App, FakeConnection) {
        let (mut app, fake) = app(role);
        let candles = (0..120)
            .map(|i| {
                let base = 25_00 + (i * 37) % 90;
                Candle {
                    start: i * 1_000,
                    open: base,
                    high: base + 15,
                    low: base - 12,
                    close: base + 4,
                    volume: 50 + i % 30,
                    trades: 3,
                }
            })
            .collect();
        app.apply(ServerMessage::Candles(CandleSnapshot {
            symbol: sym("ACME"),
            interval: 1_000,
            candles,
        }));
        app.apply(ServerMessage::Book(BookSnapshot {
            symbol: sym("ACME"),
            timestamp: 120_000,
            bids: (0..8)
                .map(|i| Level {
                    price: 25_05 - i * 2,
                    qty: 10 + i * 7,
                    orders: 2,
                })
                .collect(),
            asks: (0..8)
                .map(|i| Level {
                    price: 25_07 + i * 2,
                    qty: 12 + i * 5,
                    orders: 1,
                })
                .collect(),
        }));
        for i in 0..10 {
            app.apply(ServerMessage::Trade(TradeEvent {
                symbol: sym("ACME"),
                trade_id: i + 1,
                price: 25_05 + i % 3,
                qty: 5 + i,
                aggressor: if i % 2 == 0 { Side::Buy } else { Side::Sell },
                timestamp: 120_000,
            }));
        }
        app.apply(ServerMessage::Account(AccountSnapshot {
            account_id: 3,
            role,
            cash: Balance {
                available: 20_000_00,
                reserved: 500_00,
            },
            positions: vec![Position {
                symbol: sym("ACME"),
                balance: Balance {
                    available: 40,
                    reserved: 10,
                },
            }],
        }));
        app.apply(ServerMessage::AccountList(vec![AccountInfo {
            account_id: 3,
            role: Role::Retail,
            cash: Balance {
                available: 20_000_00,
                reserved: 0,
            },
            open_orders: 1,
            username: "alice".into(),
        }]));
        (app, fake)
    }

    fn type_line(app: &mut App, line: &str) {
        for c in line.chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.on_key(KeyEvent::from(KeyCode::Enter));
    }

    #[test]
    fn selects_first_instrument_and_subscribes() {
        let (app, fake) = app(Role::Retail);
        assert_eq!(app.view.as_ref().unwrap().symbol, sym("ACME"));
        assert_eq!(
            fake.sent.lock().unwrap()[0],
            ClientMessage::Subscribe {
                symbol: sym("ACME")
            }
        );
    }

    #[test]
    fn trader_orders_go_to_the_selected_symbol() {
        let (mut app, fake) = app(Role::Retail);
        type_line(&mut app, "sym nova");
        type_line(&mut app, "buy 25.10 4");

        let sent = fake.sent.lock().unwrap();
        assert!(sent.contains(&ClientMessage::Unsubscribe {
            symbol: sym("ACME")
        }));
        assert!(matches!(
            sent.last().unwrap(),
            ClientMessage::NewOrder { symbol, side: Side::Buy, kind: OrderKind::Limit(2510), qty: 4, .. } if *symbol == sym("NOVA")
        ));
    }

    #[test]
    fn admin_commands_are_blocked_for_traders_and_vice_versa() {
        let (mut trader, trader_fake) = app(Role::Retail);
        type_line(&mut trader, "halt ACME");
        assert!(trader.log.back().unwrap().text.contains("admin command"));
        assert_eq!(trader_fake.sent.lock().unwrap().len(), 1);

        let (mut admin, admin_fake) = populated(Role::Admin);
        type_line(&mut admin, "buy 25 1");
        assert!(admin.log.back().unwrap().text.contains("cannot trade"));
        type_line(&mut admin, "halt");
        type_line(&mut admin, "cash alice 100.50");
        let sent = admin_fake.sent.lock().unwrap();
        assert!(sent.contains(&ClientMessage::Halt {
            symbol: sym("ACME"),
            reason: String::new()
        }));
        assert!(sent.contains(&ClientMessage::SetBalance {
            account_id: 3,
            asset: Asset::Cash,
            amount: 100_50
        }));
    }

    #[test]
    fn execution_reports_maintain_open_orders() {
        let (mut app, _) = app(Role::Retail);
        let mut report = ExecutionReport {
            order_id: 9,
            client_order_id: 1,
            symbol: sym("ACME"),
            side: Side::Buy,
            status: OrderStatus::New,
            price: 25_00,
            qty: 10,
            filled: 0,
            leaves: 10,
            last_price: 0,
            last_qty: 0,
            timestamp: 10_000,
            reason: String::new(),
        };
        app.apply(ServerMessage::Execution(report.clone()));
        assert!(app.orders.contains_key(&9));

        report.status = OrderStatus::Filled;
        report.last_qty = 10;
        report.last_price = 25_00;
        app.apply(ServerMessage::Execution(report));
        assert!(app.orders.is_empty());
        assert!(app.log.back().unwrap().text.starts_with("Filled #9"));
    }

    #[test]
    fn halts_are_logged_and_backfill_trades_skip_candles() {
        let (mut app, _) = populated(Role::Retail);
        app.apply(ServerMessage::Instrument(info(
            "ACME",
            InstrumentStatus::Halted,
        )));
        assert!(app.log.back().unwrap().text.contains("ACME HALTED"));

        let before = app
            .view
            .as_ref()
            .unwrap()
            .candles
            .candles()
            .back()
            .unwrap()
            .volume;
        app.apply(ServerMessage::Trade(TradeEvent {
            symbol: sym("ACME"),
            trade_id: 0,
            price: 25_00,
            qty: 999,
            aggressor: Side::Buy,
            timestamp: 10_000,
        }));
        assert_eq!(
            app.view
                .as_ref()
                .unwrap()
                .candles
                .candles()
                .back()
                .unwrap()
                .volume,
            before
        );
    }

    #[test]
    fn arrow_keys_cycle_symbols() {
        let (mut app, _) = app(Role::Retail);
        app.on_key(KeyEvent::from(KeyCode::Right));
        assert_eq!(app.view.as_ref().unwrap().symbol, sym("NOVA"));
        app.on_key(KeyEvent::from(KeyCode::Right));
        assert_eq!(app.view.as_ref().unwrap().symbol, sym("ACME"));
    }
}
