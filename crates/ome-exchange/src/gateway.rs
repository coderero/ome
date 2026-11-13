use crate::exchange::{Command, Event, Exchange};
use crate::journal::Record;
use crate::session::Schedule;
use ome_core::price::format_price;
use ome_core::symbol::Symbol;
use ome_core::types::{Side, Timestamp};
use ome_net::auth::{hash_secret, valid_username, verify};
use ome_net::{Outbox, SessionId, log};
use ome_protocol::framing::frame;
use ome_protocol::{
    BookSnapshot, CandleSnapshot, Decoder, MemberId, MemberKind, MemberMessage, ServerMessage,
    TradeEvent, template,
};
use ome_store::Journal;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::Arc;

const HEARTBEAT_MS: Timestamp = 1_000;
const CANDLE_BACKFILL: usize = 300;
const TRADE_BACKFILL: usize = 60;

struct Session {
    outbox: Box<dyn Outbox>,
    login: Option<(MemberId, MemberKind)>,
    subscriptions: BTreeSet<Symbol>,
}

pub struct Gateway {
    exchange: Exchange,
    journal: Option<Journal<Record>>,
    sessions: BTreeMap<SessionId, Session>,
    dirty_books: BTreeSet<Symbol>,
    dropped: BTreeSet<SessionId>,
    clock: Timestamp,
    last_heartbeat: Timestamp,
}

impl Gateway {
    pub fn new(exchange: Exchange, journal: Option<Journal<Record>>, clock: Timestamp) -> Self {
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

    pub fn ensure_admin(&mut self, name: &str, secret: &str, now: Timestamp) -> Result<(), String> {
        if self
            .exchange
            .members()
            .iter()
            .any(|m| m.kind == MemberKind::Admin)
        {
            return Ok(());
        }
        let command = Command::CreateMember {
            kind: MemberKind::Admin,
            name: name.to_string(),
            secret: hash_secret(name, secret),
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
            && let Some((member, _)) = session.login
            && let Some(m) = self.exchange.members().get(member)
        {
            log(&format!("session {id} ({}) disconnected", m.name));
        }
    }

    pub fn handle(&mut self, id: SessionId, bytes: &[u8], now: Timestamp) {
        let now = self.advance(now);
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        let message = match MemberMessage::decode(bytes) {
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
            (
                None,
                MemberMessage::Logon {
                    name,
                    secret,
                    last_seq,
                },
            ) => self.logon(id, &name, &secret, last_seq, now),
            (_, MemberMessage::Heartbeat) => {}
            (None, _) => self.reject(id, ref_template, "logon required"),
            (Some(_), MemberMessage::Logon { .. }) => {
                self.reject(id, ref_template, "already logged on")
            }
            (Some((member, kind)), message) => self.dispatch(id, member, kind, message, now),
        }
        self.drop_slow_sessions();
    }

    pub fn tick(&mut self, now: Timestamp) {
        let now = self.advance(now);
        for command in self.exchange.due_commands(now) {
            self.run(None, &command, now);
        }
        self.exchange.roll_candles(now);

        for symbol in std::mem::take(&mut self.dirty_books) {
            if let Some(book) = self.book_snapshot(symbol, now) {
                self.send_to_subscribers(symbol, &ServerMessage::Book(book));
            }
            if let Some(info) = self.exchange.auction_info(symbol) {
                self.send_to_subscribers(symbol, &ServerMessage::AuctionInfo(info));
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

    fn logon(&mut self, id: SessionId, name: &str, secret: &str, last_seq: u64, now: Timestamp) {
        let Some(member) = self
            .exchange
            .members()
            .find(name)
            .filter(|m| verify(&m.secret, name, secret))
        else {
            log(&format!("session {id}: failed member logon for '{name}'"));
            return self.reject(id, template::MEMBER_LOGON, "invalid member name or secret");
        };
        let (member_id, kind) = (member.id, member.kind);
        if let Some(session) = self.sessions.get_mut(&id) {
            session.login = Some((member_id, kind));
        }

        let replay = self.exchange.replay(member_id, last_seq);
        log(&format!(
            "session {id}: {name} logged on as {} (resending {} from seq {})",
            kind.label(),
            replay.messages.len(),
            last_seq.saturating_add(1)
        ));
        self.reply(
            id,
            ServerMessage::MemberLogonAck {
                member_id,
                kind,
                server_time: now,
            },
        );
        if replay.gap {
            self.reject(
                id,
                template::MEMBER_LOGON,
                "requested sequence is older than retained history",
            );
        }
        for message in replay.messages {
            self.reply(id, message);
        }
        self.reply(
            id,
            ServerMessage::ReplayComplete {
                next_seq: replay.next_seq,
            },
        );
        self.reply(
            id,
            ServerMessage::MarketStatus(self.exchange.market_status()),
        );
        self.reply(
            id,
            ServerMessage::InstrumentList(self.exchange.instrument_list()),
        );
        if kind == MemberKind::Admin {
            self.reply(id, ServerMessage::MemberList(self.member_list()));
        }
    }

    fn member_list(&self) -> Vec<ome_protocol::MemberInfo> {
        let online: BTreeSet<MemberId> = self
            .sessions
            .values()
            .filter_map(|s| s.login.map(|(m, _)| m))
            .collect();
        self.exchange.member_list(|m| online.contains(&m))
    }

    fn dispatch(
        &mut self,
        id: SessionId,
        member: MemberId,
        kind: MemberKind,
        message: MemberMessage,
        now: Timestamp,
    ) {
        let ref_template = message.template_id();
        let admin = kind == MemberKind::Admin;
        let clearing = kind == MemberKind::Clearing;
        let trader = !admin && !clearing;
        let command = match message {
            MemberMessage::Subscribe { symbol } => return self.subscribe(id, symbol, now),
            MemberMessage::Unsubscribe { symbol } => {
                if let Some(session) = self.sessions.get_mut(&id) {
                    session.subscriptions.remove(&symbol);
                }
                return;
            }
            MemberMessage::QueryInstruments => {
                return self.reply(
                    id,
                    ServerMessage::InstrumentList(self.exchange.instrument_list()),
                );
            }
            MemberMessage::QueryMembers if admin || clearing => {
                return self.reply(id, ServerMessage::MemberList(self.member_list()));
            }
            MemberMessage::NewOrder {
                member_order_id,
                account,
                symbol,
                side,
                kind,
                options,
                qty,
            } if trader => Command::NewOrder {
                member,
                member_order_id,
                account,
                symbol,
                side,
                kind,
                options,
                qty,
            },
            MemberMessage::Cancel { order_id } if trader => Command::Cancel { member, order_id },
            MemberMessage::CancelAll { account, symbol } if trader => Command::CancelAll {
                member,
                account,
                symbol,
            },
            MemberMessage::CreateMember { kind, name, secret } if admin => {
                if !valid_username(&name) || secret.is_empty() {
                    return self.reject(
                        id,
                        ref_template,
                        "member name must be 1-32 chars of a-z, 0-9, _ or - and secret non-empty",
                    );
                }
                Command::CreateMember {
                    kind,
                    secret: hash_secret(&name, &secret),
                    name,
                }
            }
            MemberMessage::ListInstrument {
                symbol,
                ipo_price,
                shares,
                underwriter,
                issuer_account,
                name,
            } if admin => Command::ListInstrument {
                symbol,
                name,
                ipo_price,
                shares,
                underwriter,
                issuer_account,
            },
            MemberMessage::Halt { symbol, reason } if admin => Command::Halt { symbol, reason },
            MemberMessage::Resume { symbol } if admin => Command::Resume { symbol },
            MemberMessage::SetSession { phase } if admin => Command::SetPhase { phase },
            MemberMessage::SetSchedule {
                pre_open_ms,
                continuous_ms,
                closing_ms,
                closed_ms,
            } if admin => {
                let schedule = Schedule {
                    pre_open_ms,
                    continuous_ms,
                    closing_ms,
                    closed_ms,
                };
                let total = pre_open_ms + continuous_ms + closing_ms + closed_ms;
                Command::SetSchedule {
                    schedule: (total > 0).then_some(schedule),
                }
            }
            MemberMessage::OpenCycle { cycle } if clearing => Command::OpenCycle { cycle },
            MemberMessage::Settled { cycle } if clearing => Command::Settled { cycle },
            MemberMessage::SetCreditLimit { member, limit } if clearing => {
                Command::SetCreditLimit { member, limit }
            }
            _ => {
                return self.reject(
                    id,
                    ref_template,
                    &format!("not permitted for {} members", kind.label()),
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
        for event in events {
            match event {
                Event::ToMember { member, message } => self.send_to_member(member, &message),
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
                Event::MemberCreated(_) => {}
                Event::SessionChanged => {
                    let status = self.exchange.market_status();
                    log(&format!(
                        "market {} (day {})",
                        status.phase.label(),
                        status.day
                    ));
                    self.broadcast(&ServerMessage::MarketStatus(status));
                }
            }
        }
    }

    fn subscribe(&mut self, id: SessionId, symbol: Symbol, now: Timestamp) {
        let Some(instrument) = self.exchange.instrument(symbol) else {
            return self.reject(
                id,
                template::MEMBER_SUBSCRIBE,
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
            Some(journal) => journal.append(&Record {
                timestamp: now,
                command: command.clone(),
            }),
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

    fn send_to_member(&mut self, member: MemberId, message: &ServerMessage) {
        let targets: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.login.is_some_and(|(m, _)| m == member))
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
        Command::CreateMember { kind, name, .. } => {
            let id = events.iter().find_map(|e| match e {
                Event::MemberCreated(id) => Some(*id),
                _ => None,
            })?;
            format!("member {id} admitted: {name} ({})", kind.label())
        }
        Command::ListInstrument {
            symbol,
            name,
            ipo_price,
            shares,
            underwriter,
            issuer_account,
        } => format!(
            "IPO listed {symbol} ({name}): {shares} shares @ {} underwritten by member {underwriter} for account {issuer_account}",
            format_price(*ipo_price)
        ),
        Command::Halt { symbol, .. } => format!("{symbol} halted"),
        Command::Resume { symbol } => format!("{symbol} resumed"),
        Command::SetPhase { phase } => format!("market moved to {}", phase.label()),
        Command::SetSchedule { schedule: None } => {
            "trading schedule cleared (manual sessions)".to_string()
        }
        Command::SetSchedule { schedule: Some(s) } => format!(
            "trading schedule: pre-open {}s, open {}s, closing auction {}s, closed {}s",
            s.pre_open_ms / 1_000,
            s.continuous_ms / 1_000,
            s.closing_ms / 1_000,
            s.closed_ms / 1_000
        ),
        Command::OpenCycle { cycle } => format!("clearing opened trade cycle {cycle}"),
        Command::Settled { cycle } => format!("clearing settled cycle {cycle}"),
        Command::SetCreditLimit { member, limit } => {
            if *limit == u64::MAX {
                format!("member {member} credit limit removed")
            } else {
                format!(
                    "member {member} credit limit set to {}",
                    format_price(*limit)
                )
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::ExchangeConfig;
    use ome_core::types::{OrderKind, OrderOptions, TimeInForce};
    use ome_protocol::OrderStatus;
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
        gw.ensure_admin("ops", "root", 0).unwrap();
        gw
    }

    fn send(gw: &mut Gateway, id: SessionId, message: MemberMessage) {
        gw.handle(id, &message.encode(), 1);
    }

    fn session(
        gw: &mut Gateway,
        id: SessionId,
        name: &str,
        secret: &str,
        last_seq: u64,
    ) -> Recorder {
        let recorder = Recorder::default();
        gw.connect(id, Box::new(recorder.clone()));
        send(
            gw,
            id,
            MemberMessage::Logon {
                name: name.into(),
                secret: secret.into(),
                last_seq,
            },
        );
        recorder
    }

    fn setup(gw: &mut Gateway) {
        session(gw, 1, "ops", "root", 0);
        for (name, kind) in [
            ("broker", MemberKind::Broker),
            ("lp", MemberKind::LiquidityProvider),
        ] {
            send(
                gw,
                1,
                MemberMessage::CreateMember {
                    kind,
                    name: name.into(),
                    secret: "pw".into(),
                },
            );
        }
        send(
            gw,
            1,
            MemberMessage::ListInstrument {
                symbol: sym("ACME"),
                ipo_price: 10_00,
                shares: 100,
                underwriter: 2,
                issuer_account: 1,
                name: "Acme".into(),
            },
        );
    }

    #[test]
    fn member_logon_replays_missed_messages_then_marks_completion() {
        let mut gw = gateway();
        setup(&mut gw);
        let broker = session(&mut gw, 2, "broker", "pw", 0);
        let messages = broker.take();
        assert!(matches!(
            messages[0],
            ServerMessage::MemberLogonAck {
                member_id: 2,
                kind: MemberKind::Broker,
                ..
            }
        ));
        assert!(
            matches!(&messages[1], ServerMessage::Allocation(a) if a.seq == 1 && a.shares == 100)
        );
        assert!(
            matches!(&messages[2], ServerMessage::Execution(r) if r.seq == 2 && r.status == OrderStatus::New)
        );
        assert!(matches!(
            messages[3],
            ServerMessage::ReplayComplete { next_seq: 3 }
        ));

        let again = session(&mut gw, 3, "broker", "pw", 2);
        let messages = again.take();
        assert!(matches!(
            messages[1],
            ServerMessage::ReplayComplete { next_seq: 3 }
        ));
    }

    #[test]
    fn trading_requires_a_trading_member_and_admin_ops_require_admin() {
        let mut gw = gateway();
        setup(&mut gw);
        let lp = session(&mut gw, 2, "lp", "pw", 0);
        lp.take();
        send(
            &mut gw,
            2,
            MemberMessage::Halt {
                symbol: sym("ACME"),
                reason: String::new(),
            },
        );
        assert!(
            matches!(&lp.take()[0], ServerMessage::Reject { reason, .. } if reason.contains("liquidity-provider"))
        );

        let admin = session(&mut gw, 3, "ops", "root", 0);
        admin.take();
        send(
            &mut gw,
            3,
            MemberMessage::NewOrder {
                member_order_id: 1,
                account: 1,
                symbol: sym("ACME"),
                side: Side::Buy,
                kind: OrderKind::Market,
                options: OrderOptions::tif(TimeInForce::Ioc),
                qty: 1,
            },
        );
        assert!(
            matches!(&admin.take()[0], ServerMessage::Reject { reason, .. } if reason.contains("admin"))
        );
    }

    #[test]
    fn fills_reach_both_members_and_books_are_conflated() {
        let mut gw = gateway();
        setup(&mut gw);
        let broker = session(&mut gw, 2, "broker", "pw", 0);
        let lp = session(&mut gw, 3, "lp", "pw", 0);
        send(
            &mut gw,
            3,
            MemberMessage::Subscribe {
                symbol: sym("ACME"),
            },
        );
        broker.take();
        lp.take();

        send(
            &mut gw,
            3,
            MemberMessage::NewOrder {
                member_order_id: 42,
                account: 7,
                symbol: sym("ACME"),
                side: Side::Buy,
                kind: OrderKind::Limit(10_00),
                options: OrderOptions::tif(TimeInForce::Gtc),
                qty: 5,
            },
        );
        gw.tick(2);

        let lp_messages = lp.take();
        assert!(lp_messages.iter().any(|m| matches!(m, ServerMessage::Execution(r) if r.client_order_id == 42 && r.status == OrderStatus::Filled)));
        assert!(
            lp_messages
                .iter()
                .any(|m| matches!(m, ServerMessage::Trade(t) if t.qty == 5))
        );
        assert_eq!(
            lp_messages
                .iter()
                .filter(|m| matches!(m, ServerMessage::Book(_)))
                .count(),
            1
        );
        assert!(broker.take().iter().any(|m| matches!(m, ServerMessage::Execution(r) if r.account == 1 && r.status == OrderStatus::PartiallyFilled)));
    }

    #[test]
    fn bad_credentials_are_rejected() {
        let mut gw = gateway();
        let out = session(&mut gw, 1, "ops", "wrong", 0);
        send(&mut gw, 1, MemberMessage::QueryInstruments);
        let messages = out.take();
        assert!(matches!(
            &messages[0],
            ServerMessage::Reject {
                ref_template: template::MEMBER_LOGON,
                ..
            }
        ));
        assert!(
            matches!(&messages[1], ServerMessage::Reject { reason, .. } if reason == "logon required")
        );
    }

    #[test]
    fn slow_consumers_are_disconnected() {
        let mut gw = gateway();
        let admin = session(&mut gw, 1, "ops", "root", 0);
        *admin.1.lock().unwrap() = true;
        gw.tick(5_000);
        assert!(gw.sessions.is_empty());
    }
}
