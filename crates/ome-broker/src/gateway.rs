use crate::broker::{Broker, Effect, Fact};
use crate::journal::Record;
use crate::market::MarketCache;
use ome_client::LinkEvent;
use ome_core::price::format_price;
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, Timestamp};
use ome_net::auth::{hash_secret, valid_username, verify};
use ome_net::{Outbox, SessionId, log};
use ome_protocol::framing::frame;
use ome_protocol::{ClientMessage, Decoder, MemberMessage, Role, ServerMessage, template};
use ome_store::Journal;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::Arc;

const HEARTBEAT_MS: Timestamp = 1_000;
const RECONNECT_MS: Timestamp = 1_000;
const REJECTED_LOGON_RETRY_MS: Timestamp = 5_000;

pub trait Uplink: Send {
    fn send(&self, message: &MemberMessage) -> bool;
}

pub trait Connector: Send {
    fn connect(&mut self, generation: u64) -> Option<Box<dyn Uplink>>;
}

pub struct MemberCredentials {
    pub name: String,
    pub secret: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    Down { retry_at: Timestamp },
    LoggingOn,
    Replaying,
    Ready,
}

struct Session {
    outbox: Box<dyn Outbox>,
    login: Option<(AccountId, Role)>,
    subscriptions: BTreeSet<Symbol>,
}

pub struct Gateway {
    broker: Broker,
    market: MarketCache,
    journal: Option<Journal<Record>>,
    sessions: BTreeMap<SessionId, Session>,
    dropped: BTreeSet<SessionId>,
    connector: Box<dyn Connector>,
    credentials: MemberCredentials,
    uplink: Option<Box<dyn Uplink>>,
    link_state: LinkState,
    generation: u64,
    clock: Timestamp,
    last_heartbeat: Timestamp,
}

impl Gateway {
    pub fn new(
        broker: Broker,
        journal: Option<Journal<Record>>,
        connector: Box<dyn Connector>,
        credentials: MemberCredentials,
        clock: Timestamp,
    ) -> Self {
        Self {
            broker,
            market: MarketCache::default(),
            journal,
            sessions: BTreeMap::new(),
            dropped: BTreeSet::new(),
            connector,
            credentials,
            uplink: None,
            link_state: LinkState::Down { retry_at: 0 },
            generation: 0,
            clock,
            last_heartbeat: clock,
        }
    }

    pub fn broker(&self) -> &Broker {
        &self.broker
    }

    pub fn link_state(&self) -> LinkState {
        self.link_state
    }

    pub fn ensure_admin(&mut self, name: &str, secret: &str, now: Timestamp) -> Result<(), String> {
        if self
            .broker
            .ledger()
            .accounts()
            .any(|a| a.role == Role::BrokerAdmin)
        {
            return Ok(());
        }
        let fact = self.broker.decide_open_account(
            Role::BrokerAdmin,
            name,
            hash_secret(name, secret),
            0,
        )?;
        self.record(fact, now)
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
        self.sessions.remove(&id);
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

    pub fn tick(&mut self, now: Timestamp) {
        let now = self.advance(now);
        if let LinkState::Down { retry_at } = self.link_state
            && now >= retry_at
        {
            self.open_link(now);
        }
        self.market.roll(now);
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

    fn open_link(&mut self, now: Timestamp) {
        self.generation += 1;
        match self.connector.connect(self.generation) {
            Some(uplink) => {
                let logon = MemberMessage::Logon {
                    name: self.credentials.name.clone(),
                    secret: self.credentials.secret.clone(),
                    last_seq: self.broker.last_seq(),
                };
                if uplink.send(&logon) {
                    self.uplink = Some(uplink);
                    self.link_state = LinkState::LoggingOn;
                    return;
                }
                self.link_state = LinkState::Down {
                    retry_at: now + RECONNECT_MS,
                };
            }
            None => {
                if self.generation % 10 == 1 {
                    log("exchange unreachable, retrying");
                }
                self.link_state = LinkState::Down {
                    retry_at: now + RECONNECT_MS,
                };
            }
        }
    }

    fn link_down(&mut self, now: Timestamp, retry_in: Timestamp, why: &str) {
        if !matches!(self.link_state, LinkState::Down { .. }) {
            log(&format!("exchange link down: {why}"));
        }
        self.uplink = None;
        self.link_state = LinkState::Down {
            retry_at: now + retry_in,
        };
    }

    fn send_exchange(&mut self, message: &MemberMessage) -> bool {
        self.uplink.as_ref().is_some_and(|u| u.send(message))
    }

    pub fn on_exchange(&mut self, generation: u64, event: LinkEvent, now: Timestamp) {
        let now = self.advance(now);
        if generation != self.generation {
            return;
        }
        let message = match event {
            LinkEvent::Closed => return self.link_down(now, RECONNECT_MS, "connection closed"),
            LinkEvent::Message(message) => message,
        };
        match message {
            ServerMessage::MemberLogonAck {
                member_id, kind, ..
            } => {
                log(&format!(
                    "logged on to exchange as member {member_id} ({}), replaying from seq {}",
                    kind.label(),
                    self.broker.last_seq() + 1
                ));
                self.link_state = LinkState::Replaying;
            }
            ServerMessage::Reject {
                ref_template: template::MEMBER_LOGON,
                reason,
            } if self.link_state == LinkState::LoggingOn => {
                self.link_down(
                    now,
                    REJECTED_LOGON_RETRY_MS,
                    &format!("logon rejected: {reason}"),
                );
            }
            ServerMessage::Reject { reason, .. } => {
                log(&format!("exchange rejected a request: {reason}"))
            }
            ServerMessage::Execution(report) => {
                if self.broker.is_new(report.seq) {
                    let charges = self.broker.charges_for(&report);
                    let _ = self.record(Fact::ExchangeReport { report, charges }, now);
                }
            }
            ServerMessage::Allocation(allocation) => {
                if self.broker.is_new(allocation.seq) {
                    let _ = self.record(Fact::Allocated(allocation), now);
                }
            }
            ServerMessage::ReplayComplete { next_seq } => {
                let pending = self.broker.pending_orders();
                if !pending.is_empty() {
                    log(&format!(
                        "abandoning {} order(s) the exchange never received",
                        pending.len()
                    ));
                }
                for order_ref in pending {
                    let _ = self.record(Fact::OrderAbandoned { order_ref }, now);
                }
                log(&format!(
                    "exchange replay complete, next seq {next_seq}; ready"
                ));
                self.link_state = LinkState::Ready;
            }
            ServerMessage::InstrumentList(_) => {
                self.market.apply(&message);
                let symbols: Vec<Symbol> = self.market.symbols().collect();
                for symbol in symbols {
                    self.send_exchange(&MemberMessage::Subscribe { symbol });
                }
                let list = ServerMessage::InstrumentList(self.market.instruments());
                self.broadcast(&list);
            }
            ServerMessage::Instrument(_) => {
                if let Some(symbol) = self.market.apply(&message) {
                    self.send_exchange(&MemberMessage::Subscribe { symbol });
                }
                self.broadcast(&message);
            }
            ServerMessage::Book(ref book) => {
                let symbol = book.symbol;
                self.market.apply(&message);
                self.send_to_subscribers(symbol, &message);
            }
            ServerMessage::Trade(trade) => {
                self.market.apply(&message);
                if trade.trade_id != 0 {
                    self.send_to_subscribers(trade.symbol, &message);
                }
            }
            ServerMessage::Candles(_) => {
                self.market.apply(&message);
            }
            _ => {}
        }
        self.drop_slow_sessions();
    }

    fn record(&mut self, fact: Fact, now: Timestamp) -> io::Result<Vec<Effect>> {
        if let Some(journal) = &mut self.journal {
            journal.append(&Record {
                timestamp: now,
                fact: fact.clone(),
            })?;
        }
        let effects = self.broker.apply(&fact);
        self.route(&effects);
        Ok(effects)
    }

    fn route(&mut self, effects: &[Effect]) {
        let mut changed = BTreeSet::new();
        for effect in effects {
            match effect {
                Effect::AccountChanged(account) => {
                    changed.insert(*account);
                }
                Effect::ToAccount { account, message } => self.send_to_account(*account, message),
                Effect::ToExchange(message) => {
                    self.send_exchange(message);
                }
                Effect::AccountOpened(_) => {}
            }
        }
        for account in changed {
            if let Some(snapshot) = self.broker.account_snapshot(account) {
                self.send_to_account(account, &ServerMessage::Account(snapshot));
            }
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
                return self.reply(
                    id,
                    ServerMessage::Reject {
                        ref_template,
                        reason: format!("decode error: {e}"),
                    },
                );
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

    fn logon(&mut self, id: SessionId, username: &str, secret: &str, now: Timestamp) {
        let Some(account) = self
            .broker
            .ledger()
            .find(username)
            .filter(|a| verify(&a.secret, username, secret))
        else {
            return self.reject(id, template::LOGON, "invalid username or secret");
        };
        let (account_id, role) = (account.id, account.role);
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
        self.reply(id, ServerMessage::InstrumentList(self.market.instruments()));
        if role.can_trade() {
            if let Some(snapshot) = self.broker.account_snapshot(account_id) {
                self.reply(id, ServerMessage::Account(snapshot));
            }
            for report in self.broker.open_order_reports(account_id, now) {
                self.reply(id, ServerMessage::Execution(report));
            }
        } else {
            self.reply(id, ServerMessage::AccountList(self.broker.account_list()));
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
        let admin = role == Role::BrokerAdmin;
        match message {
            ClientMessage::Subscribe { symbol } => {
                let snapshot = self.market.snapshot(symbol);
                if snapshot.is_empty() {
                    return self.reject(id, ref_template, &format!("unknown symbol {symbol}"));
                }
                if let Some(session) = self.sessions.get_mut(&id) {
                    session.subscriptions.insert(symbol);
                }
                for message in snapshot {
                    self.reply(id, message);
                }
            }
            ClientMessage::Unsubscribe { symbol } => {
                if let Some(session) = self.sessions.get_mut(&id) {
                    session.subscriptions.remove(&symbol);
                }
            }
            ClientMessage::QueryInstruments => {
                self.reply(id, ServerMessage::InstrumentList(self.market.instruments()))
            }
            ClientMessage::QueryAccount if !admin => {
                if let Some(snapshot) = self.broker.account_snapshot(account) {
                    self.reply(id, ServerMessage::Account(snapshot));
                }
            }
            ClientMessage::QueryFees if admin => {
                self.reply(id, ServerMessage::FeeSummary(self.broker.fee_summary()))
            }
            ClientMessage::QueryAccounts if admin => {
                self.reply(id, ServerMessage::AccountList(self.broker.account_list()))
            }
            ClientMessage::NewOrder {
                client_order_id,
                symbol,
                side,
                kind,
                qty,
            } if !admin => {
                let decision = if self.link_state == LinkState::Ready {
                    self.broker.decide_order(
                        account,
                        client_order_id,
                        symbol,
                        side,
                        kind,
                        qty,
                        &self.market,
                        now,
                    )
                } else {
                    let mut report = self
                        .broker
                        .decide_order(
                            account,
                            client_order_id,
                            symbol,
                            side,
                            kind,
                            qty,
                            &self.market,
                            now,
                        )
                        .err()
                        .unwrap_or_else(|| {
                            Box::new(rejected(account, client_order_id, symbol, side, qty, now))
                        });
                    report.reason = "exchange link is down, try again shortly".into();
                    Err(report)
                };
                match decision {
                    Ok(fact) => {
                        if self.record(fact, now).is_err() {
                            self.reject(
                                id,
                                ref_template,
                                "journal unavailable, order not accepted",
                            );
                        }
                    }
                    Err(report) => {
                        self.send_to_account(account, &ServerMessage::Execution(*report))
                    }
                }
            }
            ClientMessage::Cancel { order_id } if !admin => match self
                .broker
                .decide_cancel(account, order_id)
            {
                Ok(message) if self.send_exchange(&message) => {}
                Ok(_) => self.reject(id, ref_template, "exchange link is down, try again shortly"),
                Err(reason) => self.reject(id, ref_template, &reason),
            },
            ClientMessage::CancelAll { symbol } if !admin => {
                if !self.send_exchange(&MemberMessage::CancelAll {
                    account: Some(account),
                    symbol,
                }) {
                    self.reject(id, ref_template, "exchange link is down, try again shortly");
                }
            }
            ClientMessage::CreateAccount {
                role,
                cash,
                username,
                secret,
            } if admin => {
                if !valid_username(&username) || secret.is_empty() {
                    return self.reject(
                        id,
                        ref_template,
                        "username must be 1-32 chars of a-z, 0-9, _ or - and secret non-empty",
                    );
                }
                match self.broker.decide_open_account(
                    role,
                    &username,
                    hash_secret(&username, &secret),
                    cash,
                ) {
                    Ok(fact) => match self.record(fact, now) {
                        Ok(effects) => {
                            let account_id = effects.iter().find_map(|e| match e {
                                Effect::AccountOpened(id) => Some(*id),
                                _ => None,
                            });
                            let text = format!(
                                "account {} created: {username} ({}) cash {}",
                                account_id.unwrap_or(0),
                                role.label(),
                                format_price(cash)
                            );
                            log(&text);
                            self.reply(id, ServerMessage::Ack { ref_template, text });
                        }
                        Err(_) => self.reject(id, ref_template, "journal unavailable"),
                    },
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
            ClientMessage::SetBalance {
                account_id,
                asset,
                amount,
            } if admin => match self.broker.decide_set_balance(account_id, asset, amount) {
                Ok(fact) => {
                    let text = match asset {
                        ome_protocol::Asset::Cash => {
                            format!("account {account_id} cash set to {}", format_price(amount))
                        }
                        ome_protocol::Asset::Shares(symbol) => {
                            format!("account {account_id} {symbol} shares set to {amount}")
                        }
                    };
                    match self.record(fact, now) {
                        Ok(_) => self.reply(id, ServerMessage::Ack { ref_template, text }),
                        Err(_) => self.reject(id, ref_template, "journal unavailable"),
                    }
                }
                Err(reason) => self.reject(id, ref_template, &reason),
            },
            _ => self.reject(
                id,
                ref_template,
                &format!("not permitted for {} accounts", role.label()),
            ),
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

fn rejected(
    account: AccountId,
    client_order_id: u64,
    symbol: Symbol,
    side: ome_core::types::Side,
    qty: u64,
    now: Timestamp,
) -> ome_protocol::ExecutionReport {
    ome_protocol::ExecutionReport {
        seq: 0,
        order_id: 0,
        client_order_id,
        account,
        symbol,
        side,
        status: ome_protocol::OrderStatus::Rejected,
        price: 0,
        qty,
        filled: 0,
        leaves: 0,
        last_price: 0,
        last_qty: 0,
        fees: 0,
        timestamp: now,
        reason: String::new(),
    }
}

fn encode(message: &ServerMessage) -> Arc<[u8]> {
    frame(&message.encode()).into()
}
