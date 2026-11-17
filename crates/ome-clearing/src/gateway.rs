use crate::clearing::{Clearing, Effect, Fact};
use crate::journal::Record;
use ome_client::LinkEvent;
use ome_client::upstream::{Upstream, UpstreamEvent};
use ome_core::price::format_price;
use ome_core::types::Timestamp;
use ome_net::auth::{SecretHash, hash_secret, valid_username, verify};
use ome_net::onboard::Applications;
use ome_net::{Outbox, SessionId, log};
use ome_protocol::framing::frame;
use ome_protocol::{
    Decoder, MarketPhase, MemberId, MemberInfo, MemberKind, MemberMessage, ServerMessage, template,
};
use ome_store::FactLog;
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
    clearing: Clearing,
    journal: Option<Box<dyn FactLog<Record>>>,
    exchange: Upstream,
    ops: OpsCredentials,
    exchange_members: Vec<MemberInfo>,
    sessions: BTreeMap<SessionId, Session>,
    dropped: BTreeSet<SessionId>,
    next_cycle_at: Option<Timestamp>,
    clock: Timestamp,
    last_heartbeat: Timestamp,
    applications: Applications,
}

impl Gateway {
    pub fn new(
        clearing: Clearing,
        journal: Option<Box<dyn FactLog<Record>>>,
        exchange: Upstream,
        ops: OpsCredentials,
        clock: Timestamp,
    ) -> Self {
        Self {
            clearing,
            journal,
            exchange,
            ops,
            exchange_members: Vec::new(),
            sessions: BTreeMap::new(),
            dropped: BTreeSet::new(),
            next_cycle_at: None,
            clock,
            last_heartbeat: clock,
            applications: Applications::default(),
        }
    }

    pub fn health(&self) -> ome_net::registry::Report {
        let mut report = ome_net::registry::Report::default();
        report.dep("exchange", self.exchange.is_ready(), self.exchange.problem());
        report.metric("sessions", self.sessions_logged_on() as i64);
        report.metric("applications", self.applications.len() as i64);
        let mut report = report.ready_unless(&["exchange"]);
        if self.exchange.awaiting_approval() {
            report.detail = "clearing membership at the exchange awaiting approval".into();
        }
        report
    }

    pub fn applications(&self) -> &Applications {
        &self.applications
    }

    pub fn exchange_link(&self) -> &Upstream {
        &self.exchange
    }

    pub fn sessions_logged_on(&self) -> usize {
        self.sessions.values().filter(|s| s.login.is_some()).count()
    }

    pub fn clearing(&self) -> &Clearing {
        &self.clearing
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
        self.exchange.tick(now, self.clearing.last_exchange_seq());
        if self.exchange.is_ready() && !self.clearing.follows_trading_days() {
            let due = *self.next_cycle_at.get_or_insert(now);
            if now >= due {
                self.next_cycle_at = Some(now + self.clearing.config().cycle_ms);
                for fact in self.clearing.due_on_open() {
                    if let Fact::CycleOpened { cycle } = fact {
                        log(&format!("opening trade cycle {cycle}"));
                    }
                    let _ = self.record(fact, now);
                }
                self.exchange.send(&MemberMessage::QueryMembers);
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
            UpstreamEvent::LoggedOn { member_id, .. } => log(&format!(
                "logged on to exchange as clearing member {member_id}, replaying from seq {}",
                self.clearing.last_exchange_seq() + 1
            )),
            UpstreamEvent::Ready { next_seq } => {
                log(&format!(
                    "exchange replay complete, next seq {next_seq}; resyncing cycle and credit limits"
                ));
                for message in self.clearing.resync() {
                    self.exchange.send(&message);
                }
                self.exchange.send(&MemberMessage::QueryMembers);
            }
            UpstreamEvent::Down => self.next_cycle_at = None,
            UpstreamEvent::Message(ServerMessage::ClearedTrade(trade)) => {
                if self.clearing.is_new(trade.seq) {
                    let _ = self.record(Fact::TradeCleared(trade), now);
                }
            }
            UpstreamEvent::Message(ServerMessage::Allocation(allocation)) => {
                if self.clearing.is_new(allocation.seq) {
                    let _ = self.record(Fact::AllocationCleared(allocation), now);
                }
            }
            UpstreamEvent::Message(ServerMessage::MemberList(list)) => self.exchange_members = list,
            UpstreamEvent::Message(ServerMessage::MarketStatus(status))
                if status.phase == MarketPhase::PreOpen =>
            {
                for fact in self.clearing.due_on_trading_day(status.day) {
                    if let Fact::CycleOpened { cycle } = fact {
                        log(&format!(
                            "trading day {} started: opening clearing cycle {cycle}",
                            status.day
                        ));
                    }
                    let _ = self.record(fact, now);
                }
                self.exchange.send(&MemberMessage::QueryMembers);
            }
            UpstreamEvent::Message(ServerMessage::Reject { reason, .. }) => {
                log(&format!("exchange rejected a request: {reason}"))
            }
            UpstreamEvent::Message(_) => {}
        }
        self.drop_slow_sessions();
    }

    fn record(&mut self, fact: Fact, now: Timestamp) -> io::Result<()> {
        if let Some(journal) = &mut self.journal {
            journal.append(&Record {
                timestamp: now,
                fact: fact.clone(),
            })?;
        }
        for effect in self.clearing.apply(&fact) {
            match effect {
                Effect::ToExchange(message) => {
                    self.exchange.send(&message);
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
            (
                None,
                MemberMessage::Apply {
                    kind,
                    name,
                    secret,
                    note,
                },
            ) => {
                let result = match self.clearing.find(&name) {
                    Some((_, Some(_))) => Err(format!("{name} is already a clearing member")),
                    _ => self.applications.receive(kind, &name, &secret, &note, now),
                };
                match result {
                    Ok(text) => {
                        log(&format!("clearing membership {text}"));
                        self.reply(id, ServerMessage::Ack { ref_template, text });
                    }
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
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
            match self.clearing.find(name) {
                Some((member, Some(hash))) if verify(&hash, name, secret) => Login::Member(member),
                _ => {
                    return self.reject(
                        id,
                        template::MEMBER_LOGON,
                        "invalid clearing member or secret",
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
        log(&format!("session {id}: {name} logged on to clearing"));
        self.reply(
            id,
            ServerMessage::MemberLogonAck {
                member_id,
                kind,
                server_time: now,
            },
        );
        let replay = match login {
            Login::Ops => None,
            Login::Member(member) => Some(self.clearing.replay(member, last_seq)),
        };
        let next_seq = match replay {
            Some(replay) => {
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
            None => 1,
        };
        self.reply(id, ServerMessage::ReplayComplete { next_seq });
    }

    fn dispatch(&mut self, id: SessionId, login: Login, message: MemberMessage, now: Timestamp) {
        let ref_template = message.template_id();
        match (login, message) {
            (Login::Ops, MemberMessage::QueryClearing) => self.reply(
                id,
                ServerMessage::ClearingStatus(self.clearing.status(None)),
            ),
            (Login::Member(member), MemberMessage::QueryClearing) => self.reply(
                id,
                ServerMessage::ClearingStatus(self.clearing.status(Some(member))),
            ),
            (Login::Ops, MemberMessage::CreateMember { name, secret, .. }) => {
                if !valid_username(&name) || secret.is_empty() {
                    return self.reject(
                        id,
                        ref_template,
                        "member name must be 1-32 chars of a-z, 0-9, _ or - and secret non-empty",
                    );
                }
                match self.clearing.decide_register(
                    &name,
                    hash_secret(&name, &secret),
                    &self.exchange_members,
                ) {
                    Ok(fact) => {
                        let member = match &fact {
                            Fact::MemberRegistered { member, .. } => *member,
                            _ => 0,
                        };
                        self.ack_or_reject(
                            id,
                            ref_template,
                            fact,
                            now,
                            format!("clearing member {member} registered: {name}"),
                        );
                    }
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
            (Login::Ops, MemberMessage::Deposit { member, amount }) => {
                match self.clearing.decide_deposit(member, amount) {
                    Ok(fact) => {
                        let verb = if amount >= 0 { "deposited" } else { "withdrew" };
                        let text = format!(
                            "member {member} {verb} {} collateral",
                            format_price(amount.unsigned_abs())
                        );
                        self.ack_or_reject(id, ref_template, fact, now, text);
                    }
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
            (Login::Ops, MemberMessage::QueryApplications) => {
                self.reply(id, ServerMessage::Applications(self.applications.list()))
            }
            (
                Login::Ops,
                MemberMessage::Decide {
                    name,
                    approve,
                    amount,
                    note,
                },
            ) => {
                let Some(pending) = self.applications.take(&name) else {
                    return self.reject(id, ref_template, &format!("no pending application from {name}"));
                };
                if !approve {
                    let text = format!("clearing application from {name} refused{}", if note.is_empty() { String::new() } else { format!(": {note}") });
                    log(&text);
                    return self.reply(id, ServerMessage::Ack { ref_template, text });
                }
                let fact = match self.clearing.decide_register(&name, pending.secret, &self.exchange_members) {
                    Ok(fact) => fact,
                    Err(reason) => {
                        self.applications.restore(&name, pending);
                        return self.reject(id, ref_template, &reason);
                    }
                };
                let member = match &fact {
                    Fact::MemberRegistered { member, .. } => *member,
                    _ => 0,
                };
                if self.record(fact, now).is_err() {
                    return self.reject(id, ref_template, "journal unavailable");
                }
                let mut text = format!("clearing member {member} admitted: {name}");
                if amount > 0 {
                    match self.clearing.decide_deposit(member, amount as i64) {
                        Ok(deposit) => match self.record(deposit, now) {
                            Ok(()) => text.push_str(&format!(", collateral {}", format_price(amount))),
                            Err(_) => text.push_str(", collateral not recorded"),
                        },
                        Err(_) => text.push_str(", collateral not recorded"),
                    }
                }
                log(&text);
                self.reply(id, ServerMessage::Ack { ref_template, text });
            }
            (Login::Ops, MemberMessage::QueryMembers) => {
                self.reply(id, ServerMessage::MemberList(self.exchange_members.clone()))
            }
            _ => self.reject(id, ref_template, "not permitted"),
        }
    }

    fn ack_or_reject(
        &mut self,
        id: SessionId,
        ref_template: u16,
        fact: Fact,
        now: Timestamp,
        text: String,
    ) {
        match self.record(fact, now) {
            Ok(()) => {
                log(&text);
                self.reply(id, ServerMessage::Ack { ref_template, text });
            }
            Err(_) => self.reject(id, ref_template, "journal unavailable"),
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
