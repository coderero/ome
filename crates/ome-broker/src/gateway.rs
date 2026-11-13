use crate::broker::{Broker, Effect, Fact};
use crate::journal::Record;
use crate::market::MarketCache;
use ome_client::LinkEvent;
use ome_client::upstream::{LinkState, Upstream, UpstreamEvent};
use ome_core::price::Price;
use ome_core::price::format_price;
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, Timestamp};
use ome_net::auth::{hash_secret, valid_username, verify};
use ome_net::{Outbox, SessionId, log};
use ome_protocol::framing::frame;
use ome_protocol::{
    ClientMessage, Decoder, MarketPhase, MemberMessage, Role, ServerMessage, template,
};
use ome_store::Journal;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::Arc;

const HEARTBEAT_MS: Timestamp = 1_000;
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
    exchange: Upstream,
    clearing: Option<Upstream>,
    clock: Timestamp,
    last_heartbeat: Timestamp,
}

impl Gateway {
    pub fn new(
        broker: Broker,
        journal: Option<Journal<Record>>,
        exchange: Upstream,
        clearing: Option<Upstream>,
        clock: Timestamp,
    ) -> Self {
        Self {
            broker,
            market: MarketCache::default(),
            journal,
            sessions: BTreeMap::new(),
            dropped: BTreeSet::new(),
            exchange,
            clearing,
            clock,
            last_heartbeat: clock,
        }
    }

    pub fn broker(&self) -> &Broker {
        &self.broker
    }

    pub fn link_state(&self) -> LinkState {
        self.exchange.state()
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
        self.exchange.tick(now, self.broker.last_seq());
        if let Some(clearing) = &mut self.clearing {
            clearing.tick(now, self.broker.last_settlement_seq());
        }
        self.market.roll(now);
        self.release_after_market_orders(now);
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

    fn release_after_market_orders(&mut self, now: Timestamp) {
        if !self.exchange.is_ready() || self.market.phase() == MarketPhase::Closed {
            return;
        }
        for cond_id in self.broker.waiting_for_open() {
            if let Some(fact) = self.broker.decide_trigger(cond_id, 0, &self.market, now) {
                let _ = self.record(fact, now);
            }
        }
    }

    fn evaluate_conditionals(&mut self, symbol: Symbol, last: Price, now: Timestamp) {
        if !self.exchange.is_ready()
            || self.market.phase() != MarketPhase::Open
            || !self.market.is_trading(symbol)
        {
            return;
        }
        let (trails, due) = self.broker.scan_conditionals(symbol, last);
        for fact in trails {
            let _ = self.record(fact, now);
        }
        for cond_id in due {
            if let Some(fact) = self.broker.decide_trigger(cond_id, last, &self.market, now) {
                let _ = self.record(fact, now);
            }
        }
    }

    fn send_exchange(&mut self, message: &MemberMessage) -> bool {
        self.exchange.send(message)
    }

    pub fn on_clearing(&mut self, generation: u64, event: LinkEvent, now: Timestamp) {
        let now = self.advance(now);
        let Some(clearing) = &mut self.clearing else {
            return;
        };
        match clearing.on_event(generation, event, now) {
            Some(UpstreamEvent::LoggedOn { member_id, .. }) => log(&format!(
                "logged on to clearing as member {member_id}, replaying settlements from seq {}",
                self.broker.last_settlement_seq() + 1
            )),
            Some(UpstreamEvent::Ready { .. }) => log("clearing replay complete; ready"),
            Some(UpstreamEvent::Message(ServerMessage::SettlementReport(report)))
                if self.broker.is_new_settlement(report.seq) =>
            {
                log(&format!(
                    "cycle {} settled: cash {}{}",
                    report.cycle,
                    if report.cash < 0 { "-" } else { "" },
                    format_price(report.cash.unsigned_abs())
                ));
                let _ = self.record(Fact::SettlementReceived(report), now);
            }
            _ => {}
        }
        self.drop_slow_sessions();
    }

    pub fn on_exchange(&mut self, generation: u64, event: LinkEvent, now: Timestamp) {
        let now = self.advance(now);
        let message = match self.exchange.on_event(generation, event, now) {
            None | Some(UpstreamEvent::Down) => return,
            Some(UpstreamEvent::LoggedOn { member_id, kind }) => {
                log(&format!(
                    "logged on to exchange as member {member_id} ({}), replaying from seq {}",
                    kind.label(),
                    self.broker.last_seq() + 1
                ));
                return;
            }
            Some(UpstreamEvent::Ready { next_seq }) => {
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
                return;
            }
            Some(UpstreamEvent::Message(message)) => message,
        };
        match message {
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
                    self.evaluate_conditionals(trade.symbol, trade.price, now);
                }
            }
            ServerMessage::Candles(_) => {
                self.market.apply(&message);
            }
            ServerMessage::MarketStatus(status) => {
                self.market.apply(&message);
                log(&format!(
                    "market {} (day {})",
                    status.phase.label(),
                    status.day
                ));
                self.broadcast(&message);
            }
            ServerMessage::AuctionInfo(info) => self.send_to_subscribers(info.symbol, &message),
            _ => {}
        }
        self.drop_slow_sessions();
    }

    fn record(&mut self, fact: Fact, now: Timestamp) -> io::Result<Vec<Effect>> {
        if let Fact::ConditionalTriggered {
            cond_id,
            price,
            order,
            reason,
        } = &fact
        {
            log(&format!(
                "conditional #{cond_id} triggered at {}{}",
                format_price(*price),
                if order.is_some() {
                    String::new()
                } else {
                    format!(", order rejected: {reason}")
                }
            ));
        }
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
        if let Some(status) = self.market.status() {
            self.reply(id, ServerMessage::MarketStatus(status));
        }
        if role.can_trade() {
            if let Some(snapshot) = self.broker.account_snapshot(account_id) {
                self.reply(id, ServerMessage::Account(snapshot));
            }
            for report in self.broker.open_order_reports(account_id, now) {
                self.reply(id, ServerMessage::Execution(report));
            }
            for report in self.broker.conditional_reports(account_id) {
                self.reply(id, ServerMessage::ConditionalUpdate(report));
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
                options,
                qty,
            } if !admin => {
                let decision = if self.exchange.is_ready() {
                    self.broker.decide_order(
                        account,
                        client_order_id,
                        symbol,
                        side,
                        kind,
                        options,
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
                            options,
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
            ClientMessage::Conditional {
                client_order_id,
                symbol,
                side,
                qty,
                trigger,
                kind,
                options,
            } if !admin => {
                let decision = self.broker.decide_conditional(
                    account,
                    client_order_id,
                    symbol,
                    side,
                    qty,
                    trigger,
                    kind,
                    options,
                    &self.market,
                );
                self.record_or_reject(id, ref_template, decision, now);
            }
            ClientMessage::Oco {
                client_order_id,
                symbol,
                side,
                qty,
                take_profit,
                stop,
                stop_limit,
            } if !admin => {
                let decision = self.broker.decide_oco(
                    account,
                    client_order_id,
                    symbol,
                    side,
                    qty,
                    take_profit,
                    stop,
                    stop_limit,
                    &self.market,
                );
                self.record_or_reject(id, ref_template, decision, now);
            }
            ClientMessage::Bracket {
                client_order_id,
                symbol,
                side,
                qty,
                entry,
                take_profit,
                stop_loss,
            } if !admin => {
                if !self.exchange.is_ready() {
                    return self.reject(
                        id,
                        ref_template,
                        "exchange link is down, try again shortly",
                    );
                }
                match self.broker.decide_bracket(
                    account,
                    client_order_id,
                    symbol,
                    side,
                    qty,
                    entry,
                    take_profit,
                    stop_loss,
                    &self.market,
                    now,
                ) {
                    Ok(fact) => {
                        if self.record(fact, now).is_err() {
                            self.reject(id, ref_template, "journal unavailable");
                        }
                    }
                    Err(report) => {
                        self.send_to_account(account, &ServerMessage::Execution(*report))
                    }
                }
            }
            ClientMessage::CancelConditional { cond_id } if !admin => {
                let decision = self.broker.decide_cancel_conditional(account, cond_id);
                self.record_or_reject(id, ref_template, decision, now);
            }
            ClientMessage::Cancel { order_id } if !admin => match self
                .broker
                .decide_cancel(account, order_id)
            {
                Ok(message) if self.send_exchange(&message) => {}
                Ok(_) => self.reject(id, ref_template, "exchange link is down, try again shortly"),
                Err(reason) => self.reject(id, ref_template, &reason),
            },
            ClientMessage::Quote { symbol, bid, ask } if !admin => {
                if !self.exchange.is_ready() {
                    return self.reject(
                        id,
                        ref_template,
                        "exchange link is down, try again shortly",
                    );
                }
                match self
                    .broker
                    .decide_quote(account, symbol, bid, ask, &self.market)
                {
                    Ok(fact) => {
                        if self.record(fact, now).is_err() {
                            self.reject(
                                id,
                                ref_template,
                                "journal unavailable, quote not accepted",
                            );
                        }
                    }
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
            ClientMessage::SetMmp { window_ms, max_qty } if !admin => {
                match self.broker.decide_mmp(account, window_ms, max_qty) {
                    Ok(message) if self.send_exchange(&message) => self.reply(
                        id,
                        ServerMessage::Ack {
                            ref_template,
                            text: format!(
                                "market maker protection set: {max_qty} shares per {window_ms} ms"
                            ),
                        },
                    ),
                    Ok(_) => {
                        self.reject(id, ref_template, "exchange link is down, try again shortly")
                    }
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
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

    fn record_or_reject(
        &mut self,
        id: SessionId,
        ref_template: u16,
        decision: Result<Fact, String>,
        now: Timestamp,
    ) {
        match decision {
            Ok(fact) => {
                if self.record(fact, now).is_err() {
                    self.reject(id, ref_template, "journal unavailable");
                }
            }
            Err(reason) => self.reject(id, ref_template, &reason),
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
        cycle: 0,
        timestamp: now,
        reason: String::new(),
    }
}

fn encode(message: &ServerMessage) -> Arc<[u8]> {
    frame(&message.encode()).into()
}
