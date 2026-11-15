use crate::broker::{Broker, Effect, Fact};
use crate::journal::Record;
use crate::market::MarketCache;
use crate::risk::EntryGuard;
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
    ClientMessage, Decoder, MarketPhase, MemberMessage, Role, ServerMessage, Venue, template,
};
use ome_store::Journal;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::sync::Arc;

const HEARTBEAT_MS: Timestamp = 1_000;
const NEWS_CACHE: usize = 200;
struct Session {
    outbox: Box<dyn Outbox>,
    login: Option<(AccountId, Role)>,
    subscriptions: BTreeSet<Symbol>,
    news: bool,
}

pub struct Gateway {
    broker: Broker,
    market: MarketCache,
    journal: Option<Journal<Record>>,
    sessions: BTreeMap<SessionId, Session>,
    dropped: BTreeSet<SessionId>,
    exchange: Upstream,
    clearing: Option<Upstream>,
    venues: BTreeMap<Venue, Upstream>,
    clock: Timestamp,
    last_heartbeat: Timestamp,
    guard: EntryGuard,
    algo_steps: BTreeMap<u64, Timestamp>,
    news: Option<Upstream>,
    news_seq: u64,
    news_cache: VecDeque<ome_protocol::NewsItem>,
    history: Option<std::sync::mpsc::Sender<crate::history::HistoryJob>>,
}

impl Gateway {
    pub fn new(
        broker: Broker,
        journal: Option<Journal<Record>>,
        exchange: Upstream,
        clearing: Option<Upstream>,
        venues: BTreeMap<Venue, Upstream>,
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
            venues,
            clock,
            last_heartbeat: clock,
            guard: EntryGuard::default(),
            algo_steps: BTreeMap::new(),
            news: None,
            news_seq: 0,
            news_cache: VecDeque::new(),
            history: None,
        }
    }

    pub fn with_history(
        mut self,
        history: Option<std::sync::mpsc::Sender<crate::history::HistoryJob>>,
    ) -> Self {
        self.history = history;
        self
    }

    pub fn on_history(&mut self, result: crate::history::HistoryResult) {
        let snapshot = result.snapshot.or_else(|| {
            self.market
                .history(result.symbol, result.interval, result.limit as usize)
        });
        if let Some(snapshot) = snapshot {
            self.reply(result.session, ServerMessage::Candles(snapshot));
        }
        self.drop_slow_sessions();
    }

    pub fn with_news(mut self, news: Option<Upstream>) -> Self {
        self.news = news;
        self
    }

    pub fn on_news(&mut self, generation: u64, event: LinkEvent, now: Timestamp) {
        let now = self.advance(now);
        let Some(link) = self.news.as_mut() else {
            return;
        };
        match link.on_event(generation, event, now) {
            Some(UpstreamEvent::LoggedOn { .. }) => log(&format!(
                "logged on to the news feed, replaying after #{}",
                self.news_seq
            )),
            Some(UpstreamEvent::Ready { next_seq }) => {
                log(&format!("news feed ready, next story #{next_seq}"))
            }
            Some(UpstreamEvent::Message(ServerMessage::News(item))) if item.seq > self.news_seq => {
                self.news_seq = item.seq;
                if self.news_cache.len() == NEWS_CACHE {
                    self.news_cache.pop_front();
                }
                self.news_cache.push_back(item.clone());
                let targets: Vec<SessionId> = self
                    .sessions
                    .iter()
                    .filter(|(_, s)| s.news)
                    .map(|(id, _)| *id)
                    .collect();
                self.send_to(targets, &ServerMessage::News(item));
            }
            Some(UpstreamEvent::Message(ServerMessage::Reject { reason, .. })) => {
                log(&format!("news feed rejected a request: {reason}"))
            }
            _ => {}
        }
        self.drop_slow_sessions();
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
                news: false,
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
        for (venue, link) in self.venues.iter_mut() {
            link.tick(now, self.broker.last_venue_seq(*venue));
        }
        if let Some(news) = &mut self.news {
            news.tick(now, self.news_seq);
        }
        self.market.roll(now);
        self.release_after_market_orders(now);
        self.step_algos(now);
        self.run_crossing(now);
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

    fn run_crossing(&mut self, now: Timestamp) {
        if !self.exchange.is_ready() || self.broker.held_orders().is_empty() {
            return;
        }
        while let Some(fact) = self.broker.next_cross(&self.market, now) {
            if self.record(fact, now).is_err() {
                break;
            }
        }
        let wholesaler = self
            .venues
            .get(&Venue::Wholesaler)
            .is_some_and(|link| link.is_ready());
        for order_ref in self.broker.held_orders() {
            let fact = self.broker.route_held(order_ref, wholesaler);
            let _ = self.record(fact, now);
        }
    }

    fn step_algos(&mut self, now: Timestamp) {
        if !self.exchange.is_ready() {
            return;
        }
        let working = self.broker.working_algos();
        self.algo_steps
            .retain(|id, _| working.iter().any(|(w, _)| w == id));
        for (algo_id, interval) in working {
            let last = self.algo_steps.get(&algo_id).copied();
            if last.is_some_and(|at| now < at + interval) {
                continue;
            }
            self.algo_steps.insert(algo_id, now);
            if let Some(fact) = self.broker.decide_algo_step(algo_id, &self.market, now) {
                if let Fact::AlgoFinished { status, reason, .. } = &fact {
                    log(&format!("algo #{algo_id} {}: {reason}", status.label()));
                }
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

    pub fn on_venue(&mut self, venue: Venue, generation: u64, event: LinkEvent, now: Timestamp) {
        let now = self.advance(now);
        let Some(link) = self.venues.get_mut(&venue) else {
            return;
        };
        match link.on_event(generation, event, now) {
            Some(UpstreamEvent::LoggedOn { member_id, .. }) => log(&format!(
                "logged on to the {} as member {member_id}",
                venue.label()
            )),
            Some(UpstreamEvent::Ready { .. }) => {
                for order_ref in self.broker.pending_orders(venue) {
                    let _ = self.record(Fact::OrderAbandoned { order_ref }, now);
                }
                log(&format!("{} replay complete; ready", venue.label()));
            }
            Some(UpstreamEvent::Message(ServerMessage::Execution(report)))
                if self.broker.is_new_from(venue, report.seq) =>
            {
                let charges = self.broker.venue_charges(&report);
                let _ = self.record(
                    Fact::VenueReport {
                        venue,
                        report,
                        charges,
                    },
                    now,
                );
            }
            Some(UpstreamEvent::Message(ServerMessage::Reject { reason, .. })) => {
                log(&format!("{} rejected a request: {reason}", venue.label()))
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
                let pending = self.broker.pending_orders(Venue::Exchange);
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
            ServerMessage::InstrumentList(ref list) => {
                for info in list {
                    self.broker
                        .mark(info.symbol, info.last_price.unwrap_or(info.reference_price));
                }
                self.market.apply(&message);
                let symbols: Vec<Symbol> = self.market.symbols().collect();
                for symbol in symbols {
                    self.send_exchange(&MemberMessage::Subscribe { symbol });
                }
                let list = ServerMessage::InstrumentList(self.market.instruments());
                self.broadcast(&list);
            }
            ServerMessage::Instrument(ref info) => {
                self.broker
                    .mark(info.symbol, info.last_price.unwrap_or(info.reference_price));
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
                    self.broker.mark(trade.symbol, trade.price);
                    self.send_to_subscribers(trade.symbol, &message);
                    self.evaluate_conditionals(trade.symbol, trade.price, now);
                }
            }
            ServerMessage::Candles(_) => {
                self.market.apply(&message);
            }
            ServerMessage::MarketStatus(status) => {
                self.market.apply(&message);
                if let Some(fact) = self.broker.decide_trading_day(status.day, &self.market) {
                    let _ = self.record(fact, now);
                }
                log(&format!(
                    "market {} (day {})",
                    status.phase.label(),
                    status.day
                ));
                self.broadcast(&message);
            }
            ServerMessage::AuctionInfo(info) => self.send_to_subscribers(info.symbol, &message),
            ServerMessage::MarketStats(stats) => {
                self.market.apply(&message);
                self.send_to_subscribers(stats.symbol, &message);
            }
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
                Effect::ToVenue(venue, message) => {
                    if !self
                        .venues
                        .get_mut(venue)
                        .is_some_and(|link| link.send(message))
                    {
                        log(&format!("{} link is down: message not sent", venue.label()));
                    }
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
            for report in self.broker.algo_reports(account_id) {
                self.reply(id, ServerMessage::AlgoUpdate(report));
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
        let entry = matches!(
            message,
            ClientMessage::NewOrder { .. }
                | ClientMessage::Cancel { .. }
                | ClientMessage::CancelAll { .. }
                | ClientMessage::Quote { .. }
                | ClientMessage::SetMmp { .. }
                | ClientMessage::Conditional { .. }
                | ClientMessage::Oco { .. }
                | ClientMessage::Bracket { .. }
                | ClientMessage::CancelConditional { .. }
                | ClientMessage::AlgoOrder { .. }
                | ClientMessage::CancelAlgo { .. }
                | ClientMessage::DarkOrder { .. }
        );
        if entry
            && !admin
            && let Err(reason) = self.guard.admit(account, role, now)
        {
            return self.reject(id, ref_template, &reason);
        }
        if let ClientMessage::NewOrder {
            symbol,
            side,
            kind,
            qty,
            ..
        } = &message
        {
            let price = match kind {
                ome_core::types::OrderKind::Limit(p) => *p,
                ome_core::types::OrderKind::Market => 0,
            };
            if let Err(reason) =
                self.guard
                    .duplicate((account, *symbol, *side, price, *qty), role, now)
            {
                return self.reject(id, ref_template, &reason);
            }
        }
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
            ClientMessage::SubscribeNews { recent } => {
                if self.news.is_none() {
                    return self.reject(id, ref_template, "this broker has no news feed");
                }
                if let Some(session) = self.sessions.get_mut(&id) {
                    session.news = true;
                }
                let skip = self.news_cache.len().saturating_sub(recent as usize);
                let items: Vec<_> = self.news_cache.iter().skip(skip).cloned().collect();
                for item in items {
                    self.reply(id, ServerMessage::News(item));
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
            ClientMessage::QueryHistory {
                symbol,
                interval_ms,
                limit,
            } => {
                let limit = limit.clamp(1, 2_000);
                let job = crate::history::HistoryJob {
                    session: id,
                    symbol,
                    interval: interval_ms.max(1_000),
                    limit,
                };
                match &self.history {
                    Some(history) if history.send(job).is_ok() => {}
                    _ => match self.market.history(symbol, interval_ms, limit as usize) {
                        Some(snapshot) => self.reply(id, ServerMessage::Candles(snapshot)),
                        None => self.reject(id, ref_template, &format!("unknown symbol {symbol}")),
                    },
                }
            }
            ClientMessage::EstimateCharges {
                symbol,
                side,
                qty,
                price,
            } => {
                let price = if price == 0 {
                    self.market.last(symbol).unwrap_or(0)
                } else {
                    price
                };
                match self.broker.estimate(account, symbol, side, price, qty) {
                    Some(estimate) if price > 0 && qty > 0 => {
                        self.reply(id, ServerMessage::ChargesEstimate(estimate))
                    }
                    _ => self.reject(id, ref_template, "cannot estimate this order"),
                }
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
                        let fact = self.broker.hold_for_crossing(fact, &self.market);
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
            ClientMessage::AlgoOrder {
                client_order_id,
                symbol,
                side,
                qty,
                limit,
                strategy,
            } if !admin => {
                let decision = self.broker.decide_algo(
                    account,
                    client_order_id,
                    symbol,
                    side,
                    qty,
                    limit,
                    strategy,
                    &self.market,
                    now,
                );
                if decision.is_ok() {
                    log(&format!(
                        "account {account} started {} to {} {qty} {symbol}",
                        strategy.describe(),
                        if side == ome_core::types::Side::Buy {
                            "buy"
                        } else {
                            "sell"
                        }
                    ));
                }
                self.record_or_reject(id, ref_template, decision, now);
            }
            ClientMessage::CancelAlgo { algo_id } if !admin => {
                let decision = self.broker.decide_cancel_algo(account, algo_id);
                self.record_or_reject(id, ref_template, decision, now);
            }
            ClientMessage::CancelConditional { cond_id } if !admin => {
                let decision = self.broker.decide_cancel_conditional(account, cond_id);
                self.record_or_reject(id, ref_template, decision, now);
            }
            ClientMessage::Cancel { order_id } if !admin => {
                match self.broker.decide_cancel(account, order_id) {
                    Ok((venue, message)) => {
                        let sent = match venue {
                            Venue::Exchange => self.send_exchange(&message),
                            other => self
                                .venues
                                .get_mut(&other)
                                .is_some_and(|link| link.send(&message)),
                        };
                        if !sent {
                            self.reject(id, ref_template, "venue link is down, try again shortly");
                        }
                    }
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
            ClientMessage::DarkOrder {
                client_order_id,
                symbol,
                side,
                qty,
                limit,
                min_qty,
            } if !admin => {
                if !self.venues.get(&Venue::Ats).is_some_and(|l| l.is_ready()) {
                    return self.reject(id, ref_template, "no dark pool is connected");
                }
                match self.broker.decide_dark(
                    account,
                    client_order_id,
                    symbol,
                    side,
                    qty,
                    limit,
                    min_qty,
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
                for (venue, order_id) in self.broker.venue_orders(account, symbol) {
                    if let Some(link) = self.venues.get_mut(&venue) {
                        link.send(&MemberMessage::Cancel { order_id });
                    }
                }
                if !self.send_exchange(&MemberMessage::CancelAll {
                    account: Some(account),
                    symbol,
                }) {
                    self.reject(id, ref_template, "exchange link is down, try again shortly");
                }
            }
            ClientMessage::SetAccountStatus {
                account_id,
                suspended,
            } if admin => {
                let decision = self.broker.decide_account_status(account_id, suspended);
                let text = format!(
                    "account {account_id} {}",
                    if suspended {
                        "suspended: open orders cancelled, conditionals removed"
                    } else {
                        "reinstated"
                    }
                );
                self.record_and_ack(id, ref_template, decision, text, now);
            }
            ClientMessage::SetRestricted { symbol, restricted } if admin => {
                let decision = self
                    .broker
                    .decide_restriction(symbol, restricted, &self.market);
                let text = format!(
                    "{symbol} {}",
                    if restricted {
                        "added to the restricted list (closing sells only)"
                    } else {
                        "removed from the restricted list"
                    }
                );
                self.record_and_ack(id, ref_template, decision, text, now);
            }
            ClientMessage::SetLossLimit { account_id, amount } if admin => {
                let decision = self.broker.decide_loss_limit(account_id, amount);
                let text = if amount == 0 {
                    format!("account {account_id} daily loss limit removed")
                } else {
                    format!(
                        "account {account_id} daily loss limit set to {}",
                        format_price(amount)
                    )
                };
                self.record_and_ack(id, ref_template, decision, text, now);
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

    fn record_and_ack(
        &mut self,
        id: SessionId,
        ref_template: u16,
        decision: Result<Fact, String>,
        text: String,
        now: Timestamp,
    ) {
        match decision {
            Ok(fact) => match self.record(fact, now) {
                Ok(_) => {
                    log(&text);
                    self.reply(id, ServerMessage::Ack { ref_template, text });
                }
                Err(_) => self.reject(id, ref_template, "journal unavailable"),
            },
            Err(reason) => self.reject(id, ref_template, &reason),
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
