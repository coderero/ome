use crate::auth::{hash_secret, valid_username, verify};
use crate::exchange::{Command, Event, Exchange};
use crate::journal::Journal;
use ome_core::price::format_price;
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, Side, Timestamp};
use ome_protocol::framing::frame;
use ome_protocol::{
    BookSnapshot, CandleSnapshot, ClientMessage, Decoder, Role, ServerMessage, TradeEvent,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::Arc;

pub type SessionId = u64;

const HEARTBEAT_MS: Timestamp = 1_000;
const CANDLE_BACKFILL: usize = 300;
const TRADE_BACKFILL: usize = 60;

pub trait Outbox: Send {
    fn send(&self, frame: Arc<[u8]>) -> bool;
}

struct Session {
    outbox: Box<dyn Outbox>,
    login: Option<(AccountId, Role)>,
    subscriptions: BTreeSet<Symbol>,
}

pub struct Gateway {
    exchange: Exchange,
    journal: Option<Journal>,
    sessions: BTreeMap<SessionId, Session>,
    dirty_books: BTreeSet<Symbol>,
    dropped: BTreeSet<SessionId>,
    clock: Timestamp,
    last_heartbeat: Timestamp,
}

impl Gateway {
    pub fn new(exchange: Exchange, journal: Option<Journal>, clock: Timestamp) -> Self {
        Self {
            exchange,
            journal,
            sessions: BTreeMap::new(),
            dirty_books: BTreeSet::new(),
            dropped: BTreeSet::new(),
            clock,
            last_heartbeat: clock,
        }
    }

    pub fn exchange(&self) -> &Exchange {
        &self.exchange
    }

    pub fn ensure_admin(
        &mut self,
        username: &str,
        secret: &str,
        now: Timestamp,
    ) -> Result<(), String> {
        if self
            .exchange
            .ledger()
            .accounts()
            .any(|a| a.role == Role::Admin)
        {
            return Ok(());
        }
        let command = Command::CreateAccount {
            role: Role::Admin,
            username: username.to_string(),
            secret: hash_secret(username, secret),
            cash: 0,
        };
        let now = self.advance(now);
        self.journal_write(now, &command)
            .map_err(|e| e.to_string())?;
        self.exchange
            .execute(&command, now)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn connect(&mut self, id: SessionId, outbox: Box<dyn Outbox>) {
        self.sessions.insert(
            id,
            Session {
                outbox,
                login: None,
                subscriptions: BTreeSet::new(),
            },
        );
    }

    pub fn disconnect(&mut self, id: SessionId) {
        if let Some(session) = self.sessions.remove(&id)
            && let Some((account, _)) = session.login
            && let Some(user) = self.exchange.ledger().get(account)
        {
            log(&format!("session {id} ({}) disconnected", user.username));
        }
    }

    pub fn handle(&mut self, id: SessionId, bytes: &[u8], now: Timestamp) {
        let now = self.advance(now);
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        let message = match ClientMessage::decode(bytes) {
            Ok(message) => message,
            Err(e) => {
                let ref_template = Decoder::peek_template(bytes).unwrap_or(0);
                self.reply(
                    id,
                    ServerMessage::Reject {
                        ref_template,
                        reason: format!("decode error: {e}"),
                    },
                );
                return;
            }
        };
        let ref_template = message.template_id();

        match (session.login, message) {
            (None, ClientMessage::Logon { username, secret }) => {
                self.logon(id, &username, &secret, now)
            }
            (_, ClientMessage::Heartbeat) => {}
            (None, _) => self.reject(id, ref_template, "logon required"),
            (Some(_), ClientMessage::Logon { .. }) => {
                self.reject(id, ref_template, "already logged on")
            }
            (Some((account, role)), message) => self.dispatch(id, account, role, message, now),
        }
        self.drop_slow_sessions();
    }

    pub fn tick(&mut self, now: Timestamp) {
        let now = self.advance(now);
        for command in self.exchange.due_commands(now) {
            self.run(None, &command, now);
        }
        self.exchange.roll_candles(now);

        let dirty = std::mem::take(&mut self.dirty_books);
        for symbol in dirty {
            if let Some(book) = self.book_snapshot(symbol, now) {
                self.send_to_subscribers(symbol, &ServerMessage::Book(book));
            }
        }

        if now >= self.last_heartbeat + HEARTBEAT_MS {
            self.last_heartbeat = now;
            let heartbeat = encode(&ServerMessage::Heartbeat { server_time: now });
            let ids: Vec<SessionId> = self.sessions.keys().copied().collect();
            for id in ids {
                self.send_frame(id, heartbeat.clone());
            }
        }
        self.drop_slow_sessions();
    }

    pub fn flush(&mut self) -> io::Result<()> {
        match &mut self.journal {
            Some(journal) => journal.flush(),
            None => Ok(()),
        }
    }

    fn advance(&mut self, now: Timestamp) -> Timestamp {
        self.clock = self.clock.max(now);
        self.clock
    }

    fn logon(&mut self, id: SessionId, username: &str, secret: &str, now: Timestamp) {
        let Some(account) = self
            .exchange
            .ledger()
            .find(username)
            .filter(|a| verify(&a.secret, username, secret))
        else {
            log(&format!("session {id}: failed logon for '{username}'"));
            self.reject(
                id,
                ome_protocol::template::LOGON,
                "invalid username or secret",
            );
            return;
        };
        let (account_id, role) = (account.id, account.role);
        log(&format!(
            "session {id}: {username} logged on as {}",
            role.label()
        ));

        if let Some(session) = self.sessions.get_mut(&id) {
            session.login = Some((account_id, role));
        }
        self.reply(
            id,
            ServerMessage::LogonAck {
                account_id,
                role,
                server_time: now,
            },
        );
        self.reply(
            id,
            ServerMessage::InstrumentList(self.exchange.instrument_list()),
        );
        if role.can_trade() {
            if let Some(snapshot) = self.exchange.account_snapshot(account_id) {
                self.reply(id, ServerMessage::Account(snapshot));
            }
            for report in self.exchange.open_order_reports(account_id, now) {
                self.reply(id, ServerMessage::Execution(report));
            }
        } else {
            self.reply(id, ServerMessage::AccountList(self.exchange.account_list()));
        }
    }

    fn dispatch(
        &mut self,
        id: SessionId,
        account: AccountId,
        role: Role,
        message: ClientMessage,
        now: Timestamp,
    ) {
        let ref_template = message.template_id();
        let admin = role == Role::Admin;
        let trader = role.can_trade();

        let command = match message {
            ClientMessage::Subscribe { symbol } => return self.subscribe(id, symbol, now),
            ClientMessage::Unsubscribe { symbol } => {
                if let Some(session) = self.sessions.get_mut(&id) {
                    session.subscriptions.remove(&symbol);
                }
                return;
            }
            ClientMessage::QueryInstruments => {
                return self.reply(
                    id,
                    ServerMessage::InstrumentList(self.exchange.instrument_list()),
                );
            }
            ClientMessage::QueryAccount if trader => {
                if let Some(snapshot) = self.exchange.account_snapshot(account) {
                    self.reply(id, ServerMessage::Account(snapshot));
                }
                return;
            }
            ClientMessage::QueryAccounts if admin => {
                return self.reply(id, ServerMessage::AccountList(self.exchange.account_list()));
            }
            ClientMessage::NewOrder {
                client_order_id,
                symbol,
                side,
                kind,
                qty,
            } if trader => Command::NewOrder {
                account,
                client_order_id,
                symbol,
                side,
                kind,
                qty,
            },
            ClientMessage::Cancel { order_id } if trader => Command::Cancel { account, order_id },
            ClientMessage::CancelAll { symbol } if trader => Command::CancelAll { account, symbol },
            ClientMessage::CreateAccount {
                role,
                cash,
                username,
                secret,
            } if admin => {
                if !valid_username(&username) {
                    return self.reject(
                        id,
                        ref_template,
                        "username must be 1-32 chars of a-z, 0-9, _ or -",
                    );
                }
                if secret.is_empty() {
                    return self.reject(id, ref_template, "secret must not be empty");
                }
                Command::CreateAccount {
                    role,
                    secret: hash_secret(&username, &secret),
                    username,
                    cash,
                }
            }
            ClientMessage::SetBalance {
                account_id,
                asset,
                amount,
            } if admin => Command::SetBalance {
                account: account_id,
                asset,
                amount,
            },
            ClientMessage::ListInstrument {
                symbol,
                ipo_price,
                shares,
                issuer,
                name,
            } if admin => Command::ListInstrument {
                symbol,
                name,
                ipo_price,
                shares,
                issuer,
            },
            ClientMessage::Halt { symbol, reason } if admin => Command::Halt { symbol, reason },
            ClientMessage::Resume { symbol } if admin => Command::Resume { symbol },
            _ => {
                return self.reject(
                    id,
                    ref_template,
                    &format!("not permitted for {} accounts", role.label()),
                );
            }
        };
        self.run(Some((id, ref_template)), &command, now);
    }

    fn run(&mut self, requester: Option<(SessionId, u16)>, command: &Command, now: Timestamp) {
        if let Err(e) = self.journal_write(now, command) {
            log(&format!("journal write failed: {e}"));
            if let Some((id, ref_template)) = requester {
                self.reject(
                    id,
                    ref_template,
                    "journal unavailable, request not processed",
                );
            }
            return;
        }

        match self.exchange.execute(command, now) {
            Ok(events) => {
                if let Some(text) = describe_admin_action(command, &events) {
                    log(&text);
                    if let Some((id, ref_template)) = requester {
                        self.reply(id, ServerMessage::Ack { ref_template, text });
                    }
                }
                self.route(events);
            }
            Err(e) => {
                if let Some((id, ref_template)) = requester {
                    self.reject(id, ref_template, &e.to_string());
                }
            }
        }
    }

    fn route(&mut self, events: Vec<Event>) {
        let mut changed_accounts = BTreeSet::new();
        for event in events {
            match event {
                Event::Execution { account, report } => {
                    self.send_to_account(account, &ServerMessage::Execution(report))
                }
                Event::AccountChanged(account) => {
                    changed_accounts.insert(account);
                }
                Event::Trade(trade) => {
                    self.send_to_subscribers(trade.symbol, &ServerMessage::Trade(trade))
                }
                Event::BookChanged(symbol) => {
                    self.dirty_books.insert(symbol);
                }
                Event::InstrumentChanged(symbol) => {
                    if let Some(info) = self.exchange.instrument_info(symbol) {
                        if !info.halt_reason.is_empty() {
                            log(&format!("{symbol} halted: {}", info.halt_reason));
                        }
                        self.broadcast(&ServerMessage::Instrument(info));
                    }
                }
                Event::AccountCreated(_) => {}
            }
        }
        for account in changed_accounts {
            if let Some(snapshot) = self.exchange.account_snapshot(account) {
                self.send_to_account(account, &ServerMessage::Account(snapshot));
            }
        }
    }

    fn subscribe(&mut self, id: SessionId, symbol: Symbol, now: Timestamp) {
        let Some(instrument) = self.exchange.instrument(symbol) else {
            return self.reject(
                id,
                ome_protocol::template::SUBSCRIBE,
                &format!("unknown symbol {symbol}"),
            );
        };
        let candles = instrument.candles.candles();
        let candles = CandleSnapshot {
            symbol,
            interval: instrument.candles.interval(),
            candles: candles
                .iter()
                .skip(candles.len().saturating_sub(CANDLE_BACKFILL))
                .copied()
                .collect(),
        };
        let mut trades: Vec<TradeEvent> = instrument
            .tape
            .newest_first()
            .take(TRADE_BACKFILL)
            .map(|t| TradeEvent {
                symbol,
                trade_id: 0,
                price: t.price,
                qty: t.qty,
                aggressor: t.taker_side,
                timestamp: t.timestamp,
            })
            .collect();
        trades.reverse();

        if let Some(session) = self.sessions.get_mut(&id) {
            session.subscriptions.insert(symbol);
        }
        if let Some(info) = self.exchange.instrument_info(symbol) {
            self.reply(id, ServerMessage::Instrument(info));
        }
        self.reply(id, ServerMessage::Candles(candles));
        for trade in trades {
            self.reply(id, ServerMessage::Trade(trade));
        }
        if let Some(book) = self.book_snapshot(symbol, now) {
            self.reply(id, ServerMessage::Book(book));
        }
    }

    fn book_snapshot(&self, symbol: Symbol, now: Timestamp) -> Option<BookSnapshot> {
        let instrument = self.exchange.instrument(symbol)?;
        let levels = self.exchange.config().depth_levels;
        Some(BookSnapshot {
            symbol,
            timestamp: now,
            bids: instrument.book.depth(Side::Buy, levels),
            asks: instrument.book.depth(Side::Sell, levels),
        })
    }

    fn journal_write(&mut self, now: Timestamp, command: &Command) -> io::Result<()> {
        match &mut self.journal {
            Some(journal) => journal.append(now, command),
            None => Ok(()),
        }
    }

    fn reject(&mut self, id: SessionId, ref_template: u16, reason: &str) {
        self.reply(
            id,
            ServerMessage::Reject {
                ref_template,
                reason: reason.to_string(),
            },
        );
    }

    fn reply(&mut self, id: SessionId, message: ServerMessage) {
        self.send_frame(id, encode(&message));
    }

    fn send_to_account(&mut self, account: AccountId, message: &ServerMessage) {
        let targets: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.login.is_some_and(|(a, _)| a == account))
            .map(|(id, _)| *id)
            .collect();
        self.send_to(targets, message);
    }

    fn send_to_subscribers(&mut self, symbol: Symbol, message: &ServerMessage) {
        let targets: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.subscriptions.contains(&symbol))
            .map(|(id, _)| *id)
            .collect();
        self.send_to(targets, message);
    }

    fn broadcast(&mut self, message: &ServerMessage) {
        let targets: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.login.is_some())
            .map(|(id, _)| *id)
            .collect();
        self.send_to(targets, message);
    }

    fn send_to(&mut self, targets: Vec<SessionId>, message: &ServerMessage) {
        if targets.is_empty() {
            return;
        }
        let frame = encode(message);
        for id in targets {
            self.send_frame(id, frame.clone());
        }
    }

    fn send_frame(&mut self, id: SessionId, frame: Arc<[u8]>) {
        if let Some(session) = self.sessions.get(&id)
            && !session.outbox.send(frame)
        {
            self.dropped.insert(id);
        }
    }

    fn drop_slow_sessions(&mut self) {
        for id in std::mem::take(&mut self.dropped) {
            log(&format!(
                "session {id} dropped: outbound queue full or closed"
            ));
            self.sessions.remove(&id);
        }
    }
}

fn encode(message: &ServerMessage) -> Arc<[u8]> {
    frame(&message.encode()).into()
}

fn describe_admin_action(command: &Command, events: &[Event]) -> Option<String> {
    Some(match command {
        Command::CreateAccount {
            role,
            username,
            cash,
            ..
        } => {
            let id = events.iter().find_map(|e| match e {
                Event::AccountCreated(id) => Some(*id),
                _ => None,
            })?;
            format!(
                "account {id} created: {username} ({}) cash {}",
                role.label(),
                format_price(*cash)
            )
        }
        Command::SetBalance {
            account,
            asset,
            amount,
        } => match asset {
            ome_protocol::Asset::Cash => {
                format!("account {account} cash set to {}", format_price(*amount))
            }
            ome_protocol::Asset::Shares(symbol) => {
                format!("account {account} {symbol} shares set to {amount}")
            }
        },
        Command::ListInstrument {
            symbol,
            name,
            ipo_price,
            shares,
            issuer,
        } => format!(
            "IPO listed {symbol} ({name}): {shares} shares @ {} from account {issuer}",
            format_price(*ipo_price)
        ),
        Command::Halt { symbol, .. } => format!("{symbol} halted"),
        Command::Resume { symbol } => format!("{symbol} resumed"),
        _ => return None,
    })
}

pub fn log(message: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let s = now % 86_400;
    eprintln!(
        "{:02}:{:02}:{:02} {message}",
        s / 3600,
        s % 3600 / 60,
        s % 60
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::ExchangeConfig;
    use ome_core::types::OrderKind;
    use ome_protocol::{Asset, OrderStatus, template};
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<ServerMessage>>>, Arc<Mutex<bool>>);

    impl Outbox for Recorder {
        fn send(&self, frame: Arc<[u8]>) -> bool {
            if *self.1.lock().unwrap() {
                return false;
            }
            let payload = &frame[ome_protocol::framing::SOFH_LEN..];
            self.0
                .lock()
                .unwrap()
                .push(ServerMessage::decode(payload).unwrap());
            true
        }
    }

    impl Recorder {
        fn take(&self) -> Vec<ServerMessage> {
            std::mem::take(&mut self.0.lock().unwrap())
        }
    }

    fn sym(s: &str) -> Symbol {
        Symbol::parse(s).unwrap()
    }

    fn gateway() -> Gateway {
        let mut gw = Gateway::new(Exchange::new(ExchangeConfig::default()), None, 0);
        gw.ensure_admin("admin", "root", 0).unwrap();
        gw
    }

    fn session(gw: &mut Gateway, id: SessionId, user: &str, secret: &str) -> Recorder {
        let recorder = Recorder::default();
        gw.connect(id, Box::new(recorder.clone()));
        send(
            gw,
            id,
            ClientMessage::Logon {
                username: user.into(),
                secret: secret.into(),
            },
        );
        recorder
    }

    fn send(gw: &mut Gateway, id: SessionId, message: ClientMessage) {
        gw.handle(id, &message.encode(), 1);
    }

    fn setup_market(gw: &mut Gateway, admin: SessionId) {
        for (name, role, cash) in [
            ("issuer", Role::Institutional, 0),
            ("alice", Role::Retail, 10_000_00),
        ] {
            send(
                gw,
                admin,
                ClientMessage::CreateAccount {
                    role,
                    cash,
                    username: name.into(),
                    secret: "pw".into(),
                },
            );
        }
        send(
            gw,
            admin,
            ClientMessage::ListInstrument {
                symbol: sym("ACME"),
                ipo_price: 10_00,
                shares: 100,
                issuer: 2,
                name: "Acme".into(),
            },
        );
    }

    #[test]
    fn bad_credentials_and_unauthenticated_requests_are_rejected() {
        let mut gw = gateway();
        let out = session(&mut gw, 1, "admin", "wrong");
        send(&mut gw, 1, ClientMessage::QueryInstruments);

        let messages = out.take();
        assert!(matches!(
            &messages[0],
            ServerMessage::Reject {
                ref_template: template::LOGON,
                ..
            }
        ));
        assert!(
            matches!(&messages[1], ServerMessage::Reject { reason, .. } if reason == "logon required")
        );
    }

    #[test]
    fn admin_creates_accounts_and_lists_ipo() {
        let mut gw = gateway();
        let admin = session(&mut gw, 1, "admin", "root");
        setup_market(&mut gw, 1);

        let acks: Vec<String> = admin
            .take()
            .into_iter()
            .filter_map(|m| match m {
                ServerMessage::Ack { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(acks.len(), 3);
        assert!(acks[0].starts_with("account 2 created: issuer"));
        assert!(acks[2].contains("IPO listed ACME"));
    }

    #[test]
    fn roles_are_enforced() {
        let mut gw = gateway();
        session(&mut gw, 1, "admin", "root");
        setup_market(&mut gw, 1);
        let alice = session(&mut gw, 2, "alice", "pw");
        alice.take();

        send(
            &mut gw,
            2,
            ClientMessage::Halt {
                symbol: sym("ACME"),
                reason: String::new(),
            },
        );
        send(
            &mut gw,
            1,
            ClientMessage::NewOrder {
                client_order_id: 1,
                symbol: sym("ACME"),
                side: Side::Buy,
                kind: OrderKind::Market,
                qty: 1,
            },
        );

        assert!(
            matches!(&alice.take()[0], ServerMessage::Reject { reason, .. } if reason.contains("retail"))
        );
    }

    #[test]
    fn trader_gets_executions_balances_and_market_data() {
        let mut gw = gateway();
        session(&mut gw, 1, "admin", "root");
        setup_market(&mut gw, 1);
        let alice = session(&mut gw, 2, "alice", "pw");
        send(
            &mut gw,
            2,
            ClientMessage::Subscribe {
                symbol: sym("ACME"),
            },
        );
        alice.take();

        send(
            &mut gw,
            2,
            ClientMessage::NewOrder {
                client_order_id: 42,
                symbol: sym("ACME"),
                side: Side::Buy,
                kind: OrderKind::Limit(10_00),
                qty: 5,
            },
        );
        gw.tick(2);

        let messages = alice.take();
        assert!(messages.iter().any(|m| matches!(m, ServerMessage::Execution(r) if r.client_order_id == 42 && r.status == OrderStatus::Filled)));
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, ServerMessage::Trade(t) if t.qty == 5))
        );
        assert!(messages.iter().any(
            |m| matches!(m, ServerMessage::Account(a) if a.cash.available == 10_000_00 - 50_00)
        ));
        let books = messages
            .iter()
            .filter(|m| matches!(m, ServerMessage::Book(_)))
            .count();
        assert_eq!(books, 1, "book updates are conflated per tick");
    }

    #[test]
    fn halts_are_broadcast_to_everyone() {
        let mut gw = gateway();
        session(&mut gw, 1, "admin", "root");
        setup_market(&mut gw, 1);
        let alice = session(&mut gw, 2, "alice", "pw");
        alice.take();

        send(
            &mut gw,
            1,
            ClientMessage::Halt {
                symbol: sym("ACME"),
                reason: "news".into(),
            },
        );

        assert!(
            alice
                .take()
                .iter()
                .any(|m| matches!(m, ServerMessage::Instrument(i) if i.halt_reason == "news"))
        );
    }

    #[test]
    fn slow_consumers_are_disconnected() {
        let mut gw = gateway();
        let admin = session(&mut gw, 1, "admin", "root");
        *admin.1.lock().unwrap() = true;

        gw.tick(5_000);

        assert!(gw.sessions.is_empty());
    }

    #[test]
    fn set_balance_via_admin_updates_logged_in_user() {
        let mut gw = gateway();
        session(&mut gw, 1, "admin", "root");
        setup_market(&mut gw, 1);
        let alice = session(&mut gw, 2, "alice", "pw");
        alice.take();

        send(
            &mut gw,
            1,
            ClientMessage::SetBalance {
                account_id: 3,
                asset: Asset::Cash,
                amount: 77,
            },
        );

        assert!(
            alice
                .take()
                .iter()
                .any(|m| matches!(m, ServerMessage::Account(a) if a.cash.available == 77))
        );
    }
}
