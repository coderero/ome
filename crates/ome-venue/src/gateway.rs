use crate::journal::Record;
use crate::quotes::Quotes;
use crate::venue::{Effect, Fact, VenueState};
use ome_client::LinkEvent;
use ome_client::upstream::{Upstream, UpstreamEvent};
use ome_core::price::format_price;
use ome_core::types::Timestamp;
use ome_net::auth::{SecretHash, hash_secret, valid_username, verify};
use ome_net::{Outbox, SessionId, log};
use ome_protocol::framing::frame;
use ome_protocol::{
    Decoder, MarketPhase, MemberId, MemberInfo, MemberKind, MemberMessage, ServerMessage, template,
};
use ome_store::Journal;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::Arc;

const HEARTBEAT_MS: Timestamp = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Login {
    Ops,
    Member(MemberId),
}

struct Session {
    outbox: Box<dyn Outbox>,
    login: Option<Login>,
}

pub struct OpsCredentials {
    pub name: String,
    pub secret: SecretHash,
}

pub struct Gateway {
    venue: VenueState,
    quotes: Quotes,
    journal: Option<Journal<Record>>,
    exchange: Upstream,
    ops: OpsCredentials,
    exchange_members: Vec<MemberInfo>,
    sessions: BTreeMap<SessionId, Session>,
    dropped: BTreeSet<SessionId>,
    clock: Timestamp,
    last_heartbeat: Timestamp,
}

impl Gateway {
    pub fn new(
        venue: VenueState,
        journal: Option<Journal<Record>>,
        exchange: Upstream,
        ops: OpsCredentials,
        clock: Timestamp,
    ) -> Self {
        Self {
            venue,
            quotes: Quotes::default(),
            journal,
            exchange,
            ops,
            exchange_members: Vec::new(),
            sessions: BTreeMap::new(),
            dropped: BTreeSet::new(),
            clock,
            last_heartbeat: clock,
        }
    }

    pub fn venue(&self) -> &VenueState {
        &self.venue
    }

    pub fn connect(&mut self, id: SessionId, outbox: Box<dyn Outbox>) {
        self.sessions.insert(
            id,
            Session {
                outbox,
                login: None,
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
        self.exchange.tick(now, self.venue.last_exchange_seq());
        if self.exchange.is_ready() {
            for fact in self.venue.hedges(&self.quotes, now) {
                if let Fact::HedgeSent {
                    symbol, side, qty, ..
                } = &fact
                {
                    log(&format!(
                        "hedging {qty} {symbol} ({}) on the exchange",
                        if *side == ome_core::types::Side::Buy {
                            "buy"
                        } else {
                            "sell"
                        }
                    ));
                }
                let _ = self.record(fact, now);
            }
            while let Some(fact) = self.venue.next_dark_match(&self.quotes, now) {
                if self.record(fact, now).is_err() {
                    break;
                }
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

    pub fn on_exchange(&mut self, generation: u64, event: LinkEvent, now: Timestamp) {
        let now = self.advance(now);
        let Some(event) = self.exchange.on_event(generation, event, now) else {
            return;
        };
        match event {
            UpstreamEvent::LoggedOn { member_id, .. } => {
                self.venue.set_self_member(member_id);
                log(&format!(
                    "logged on to exchange as member {member_id}, replaying from seq {}",
                    self.venue.last_exchange_seq() + 1
                ));
            }
            UpstreamEvent::Ready { next_seq } => {
                for order_ref in self.venue.pending_hedges() {
                    let _ = self.record(Fact::HedgeAbandoned { order_ref }, now);
                }
                log(&format!(
                    "exchange replay complete, next seq {next_seq}; ready"
                ));
                self.exchange.send(&MemberMessage::QueryMembers);
                self.exchange.send(&MemberMessage::QueryInstruments);
            }
            UpstreamEvent::Down => {}
            UpstreamEvent::Message(message) => self.on_market(message, now),
        }
        self.drop_slow_sessions();
    }

    fn on_market(&mut self, message: ServerMessage, now: Timestamp) {
        for symbol in self.quotes.apply(&message) {
            self.exchange.send(&MemberMessage::Subscribe { symbol });
        }
        match message {
            ServerMessage::Execution(report) if self.venue.is_new(report.seq) => {
                let _ = self.record(Fact::ExchangeReport(report), now);
            }
            ServerMessage::MemberList(list) => self.exchange_members = list,
            ServerMessage::MarketStatus(status) if status.phase == MarketPhase::Closed => {
                for fact in self.venue.expire_dark(now) {
                    let _ = self.record(fact, now);
                }
            }
            ServerMessage::Reject { reason, .. } => {
                log(&format!("exchange rejected a request: {reason}"))
            }
            _ => {}
        }
    }

    fn record(&mut self, fact: Fact, now: Timestamp) -> io::Result<()> {
        if let Some(journal) = &mut self.journal {
            journal.append(&Record {
                timestamp: now,
                fact: fact.clone(),
            })?;
        }
        for effect in self.venue.apply(&fact) {
            match effect {
                Effect::ToExchange(message) => {
                    if !self.exchange.send(&message) {
                        log("exchange link down: report or hedge not sent");
                    }
                }
                Effect::ToMember { member, message } => self.send_to_member(member, &message),
            }
        }
        Ok(())
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
            (Some(login), message) => self.dispatch(id, login, message, now),
        }
        self.drop_slow_sessions();
    }

    fn logon(&mut self, id: SessionId, name: &str, secret: &str, last_seq: u64, now: Timestamp) {
        let login = if name == self.ops.name && verify(&self.ops.secret, name, secret) {
            Login::Ops
        } else {
            match self.venue.find(name) {
                Some((member, hash)) if verify(&hash, name, secret) => Login::Member(member),
                _ => {
                    return self.reject(
                        id,
                        template::MEMBER_LOGON,
                        "invalid venue member or secret",
                    );
                }
            }
        };
        if let Some(session) = self.sessions.get_mut(&id) {
            session.login = Some(login);
        }
        let (member_id, kind) = match login {
            Login::Ops => (0, MemberKind::Admin),
            Login::Member(member) => (member, MemberKind::Broker),
        };
        log(&format!(
            "session {id}: {name} logged on to the {}",
            self.venue.kind().label()
        ));
        self.reply(
            id,
            ServerMessage::MemberLogonAck {
                member_id,
                kind,
                server_time: now,
            },
        );
        let next_seq = match login {
            Login::Ops => 1,
            Login::Member(member) => {
                let replay = self.venue.replay(member, last_seq);
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
                replay.next_seq
            }
        };
        self.reply(id, ServerMessage::ReplayComplete { next_seq });
    }

    fn dispatch(&mut self, id: SessionId, login: Login, message: MemberMessage, now: Timestamp) {
        let ref_template = message.template_id();
        match (login, message) {
            (_, MemberMessage::QueryVenue) => {
                self.reply(id, ServerMessage::VenueStatus(self.venue.status()))
            }
            (Login::Ops, MemberMessage::QueryMembers) => {
                self.reply(id, ServerMessage::MemberList(self.exchange_members.clone()))
            }
            (Login::Ops, MemberMessage::CreateMember { name, secret, .. }) => {
                if !valid_username(&name) || secret.is_empty() {
                    return self.reject(
                        id,
                        ref_template,
                        "member name must be 1-32 chars of a-z, 0-9, _ or - and secret non-empty",
                    );
                }
                match self.venue.decide_register(
                    &name,
                    hash_secret(&name, &secret),
                    &self.exchange_members,
                ) {
                    Ok(fact) => {
                        let text =
                            format!("{name} may now route to the {}", self.venue.kind().label());
                        match self.record(fact, now) {
                            Ok(()) => {
                                log(&text);
                                self.reply(id, ServerMessage::Ack { ref_template, text });
                            }
                            Err(_) => self.reject(id, ref_template, "journal unavailable"),
                        }
                    }
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
            (
                Login::Member(member),
                MemberMessage::NewOrder {
                    member_order_id,
                    account,
                    symbol,
                    side,
                    kind,
                    qty,
                    ..
                },
            ) => {
                let fact = self.venue.decide_order(
                    member,
                    member_order_id,
                    account,
                    symbol,
                    side,
                    kind,
                    qty,
                    &self.quotes,
                    now,
                );
                if let Fact::Filled { price, qty, .. } = &fact {
                    log(&format!(
                        "member {member}: filled {qty} {symbol} at {}",
                        format_price(*price)
                    ));
                }
                let _ = self.record(fact, now);
            }
            (
                Login::Member(member),
                MemberMessage::DarkOrder {
                    member_order_id,
                    account,
                    symbol,
                    side,
                    limit,
                    qty,
                    min_qty,
                },
            ) => {
                let fact = self.venue.decide_dark(
                    member,
                    member_order_id,
                    account,
                    symbol,
                    side,
                    (limit, qty, min_qty),
                    &self.quotes,
                    now,
                );
                let _ = self.record(fact, now);
            }
            (Login::Member(member), MemberMessage::Cancel { order_id }) => {
                match self.venue.decide_cancel(member, order_id, now) {
                    Ok(fact) => {
                        let _ = self.record(fact, now);
                    }
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
            (Login::Member(member), MemberMessage::CancelAll { account, symbol }) => {
                for fact in self.venue.decide_cancel_all(member, account, symbol, now) {
                    let _ = self.record(fact, now);
                }
            }
            (
                Login::Member(_),
                MemberMessage::Subscribe { .. } | MemberMessage::Unsubscribe { .. },
            ) => {}
            _ => self.reject(id, ref_template, "not permitted"),
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
        let frame = encode(message);
        let targets: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.login == Some(Login::Member(member)))
            .map(|(id, _)| *id)
            .collect();
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
