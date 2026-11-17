use crate::journal::Record;
use crate::links::{Done, Job};
use crate::newsroom::{Fact, Newsroom};
use crate::writer::Writer;
use ome_core::types::Timestamp;
use ome_net::auth::{SecretHash, hash_secret, valid_username, verify};
use ome_net::onboard::Applications;
use ome_net::registry::Report;
use ome_net::{Outbox, SessionId, log};
use ome_protocol::framing::frame;
use ome_protocol::{Decoder, MemberKind, MemberMessage, ServerMessage, WireItem, template};
use ome_store::FactLog;
use rand::rngs::StdRng;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::Arc;
use std::sync::mpsc::Sender;

pub struct OpsCredentials {
    pub name: String,
    pub secret: SecretHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Login {
    Ops,
    Subscriber,
    Source(String),
}

struct Session {
    outbox: Box<dyn Outbox>,
    login: Option<Login>,
}

pub struct Gateway {
    room: Newsroom,
    journal: Option<Box<dyn FactLog<Record>>>,
    ops: OpsCredentials,
    sources: BTreeMap<String, SecretHash>,
    sessions: BTreeMap<SessionId, Session>,
    clock: Timestamp,
    rng: StdRng,
    writer: Box<dyn Writer>,
    rewriter: Option<Sender<Job>>,
    rewrite_wait_ms: Timestamp,
    pending: VecDeque<(u64, Timestamp, bool)>,
    published: u64,
    stats: WriterStats,
    applications: Applications,
    wires: BTreeMap<u64, String>,
}

#[derive(Default)]
struct WriterStats {
    model: String,
    written: u64,
    fallbacks: u64,
    timeouts: u64,
    total_ms: u64,
    last_error: String,
}

fn encode(message: &ServerMessage) -> Arc<[u8]> {
    frame(&message.encode()).into()
}

impl Gateway {
    pub fn new(
        room: Newsroom,
        journal: Option<Box<dyn FactLog<Record>>>,
        ops: OpsCredentials,
        sources: BTreeMap<String, SecretHash>,
        writer: Box<dyn Writer>,
        rng: StdRng,
        clock: Timestamp,
    ) -> Self {
        Self {
            room,
            journal,
            ops,
            sources,
            sessions: BTreeMap::new(),
            clock,
            rng,
            writer,
            rewriter: None,
            rewrite_wait_ms: 0,
            pending: VecDeque::new(),
            published: 0,
            stats: WriterStats::default(),
            applications: Applications::default(),
            wires: BTreeMap::new(),
        }
        .caught_up()
    }

    fn caught_up(mut self) -> Self {
        self.published = self.room.next_seq() - 1;
        self
    }

    pub fn room(&self) -> &Newsroom {
        &self.room
    }

    fn advance(&mut self, now: Timestamp) -> Timestamp {
        self.clock = self.clock.max(now);
        self.clock
    }

    pub fn flush(&mut self) -> io::Result<()> {
        match &mut self.journal {
            Some(journal) => journal.flush(),
            None => Ok(()),
        }
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

    fn reply(&mut self, id: SessionId, message: &ServerMessage) {
        if let Some(session) = self.sessions.get(&id)
            && !session.outbox.send(encode(message))
        {
            self.sessions.remove(&id);
        }
    }

    fn reject(&mut self, id: SessionId, ref_template: u16, reason: &str) {
        self.reply(
            id,
            &ServerMessage::Reject {
                ref_template,
                reason: reason.to_string(),
            },
        );
    }

    fn ack(&mut self, id: SessionId, ref_template: u16, text: String) {
        self.reply(id, &ServerMessage::Ack { ref_template, text });
    }

    fn subscribers(&self) -> u32 {
        self.sessions
            .values()
            .filter(|s| s.login == Some(Login::Subscriber))
            .count() as u32
    }

    fn sources_connected(&self) -> u32 {
        self.sessions
            .values()
            .filter(|s| matches!(s.login, Some(Login::Source(_))))
            .count() as u32
    }

    pub fn health(&self) -> Report {
        let mut report = Report::default();
        for name in self.sources.keys() {
            let up = self.sessions.values().any(|s| s.login.as_ref() == Some(&Login::Source(name.clone())));
            report.dep(&format!("source {name}"), up, if up { "" } else { "not connected" });
        }
        if !self.stats.model.is_empty() {
            let offline = !self.stats.last_error.is_empty() && self.stats.written == 0;
            report.dep("llm writer", !offline, self.stats.last_error.clone());
        }
        report.metric("published", self.published as i64);
        report.metric("subscribers", self.subscribers() as i64);
        report.metric("pending_rewrites", self.pending.len() as i64);
        report.metric("applications", self.applications.len() as i64);
        report.ready_unless(&[])
    }

    fn record(&mut self, fact: Fact, now: Timestamp) -> io::Result<()> {
        if let Some(journal) = &mut self.journal {
            journal.append(&Record {
                timestamp: now,
                fact: fact.clone(),
            })?;
        }
        self.room.apply(&fact);
        if let Fact::Published { item, .. } = &fact {
            let invented = item.data.split_whitespace().any(|w| w == "source=llm");
            match self.rewriter.as_ref().filter(|_| !invented) {
                Some(rewriter) => {
                    let company = self.company_of(item.seq);
                    let job = Job {
                        item: item.clone(),
                        company,
                    };
                    self.pending
                        .push_back((item.seq, now + self.rewrite_wait_ms, false));
                    if rewriter.send(job).is_err() {
                        self.rewriter = None;
                    }
                    self.release(now);
                }
                None => self.publish(item.seq),
            }
        }
        Ok(())
    }

    fn publish(&mut self, seq: u64) {
        let Some(item) = self.room.item(seq).cloned() else {
            return;
        };
        self.published = self.published.max(seq);
        log(&format!(
            "#{} [{}{}] {} (sentiment {:+}, fair {:+} bps)",
            item.seq,
            item.kind.label(),
            item.symbol.map(|s| format!(" {s}")).unwrap_or_default(),
            item.headline,
            item.sentiment,
            item.impact_bps
        ));
        let message = encode(&ServerMessage::News(item));
        let dead: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, s)| matches!(s.login, Some(Login::Ops | Login::Subscriber)))
            .filter(|(_, s)| !s.outbox.send(message.clone()))
            .map(|(id, _)| *id)
            .collect();
        for id in dead {
            self.sessions.remove(&id);
        }
    }

    fn release(&mut self, now: Timestamp) {
        while let Some((seq, deadline, ready)) = self.pending.front().copied() {
            if !ready && now < deadline {
                return;
            }
            if !ready {
                self.stats.timeouts += 1;
            }
            self.pending.pop_front();
            self.publish(seq);
        }
    }

    pub fn on_rewrite(&mut self, done: Done, now: Timestamp) {
        let now = self.advance(now);
        let Some(slot) = self.pending.iter_mut().find(|(seq, _, _)| *seq == done.seq) else {
            return;
        };
        slot.2 = true;
        match done.result {
            Ok(copy) => {
                self.stats.written += 1;
                self.stats.total_ms += done.elapsed_ms;
                let fact = Fact::Rewritten {
                    seq: done.seq,
                    headline: copy.headline,
                    body: copy.body,
                };
                let journaled = match &mut self.journal {
                    Some(journal) => journal
                        .append(&Record {
                            timestamp: now,
                            fact: fact.clone(),
                        })
                        .is_ok(),
                    None => true,
                };
                if journaled {
                    self.room.apply(&fact);
                } else {
                    log("journal append failed; keeping the template text");
                }
            }
            Err(reason) => {
                self.stats.fallbacks += 1;
                if self.stats.last_error != reason {
                    log(&format!(
                        "story #{} keeps its template text: {reason}",
                        done.seq
                    ));
                }
                self.stats.last_error = reason;
            }
        }
        self.release(now);
    }

    pub fn use_rewriter(&mut self, rewriter: Sender<Job>, model: String, wait_ms: u64) {
        self.rewriter = Some(rewriter);
        self.stats.model = model;
        self.rewrite_wait_ms = wait_ms;
    }

    fn writer_status(&self) -> String {
        if self.stats.model.is_empty() {
            return self.writer.name();
        }
        let average = self
            .stats
            .total_ms
            .checked_div(self.stats.written)
            .unwrap_or(0);
        format!(
            "llm {} ({} written, avg {} ms, {} fallback(s), {} timeout(s){})",
            self.stats.model,
            self.stats.written,
            average,
            self.stats.fallbacks,
            self.stats.timeouts,
            if self.stats.last_error.is_empty() {
                String::new()
            } else {
                format!(", last problem: {}", self.stats.last_error)
            }
        )
    }

    pub fn tick(&mut self, now: Timestamp) {
        let now = self.advance(now);
        self.release(now);
    }

    fn company_of(&self, seq: u64) -> String {
        self.wires.get(&seq).cloned().unwrap_or_default()
    }

    fn on_wire(&mut self, id: SessionId, source: &str, wire: WireItem, now: Timestamp) {
        match self.room.decide_wire(source, &wire, now, &mut self.rng, self.writer.as_mut()) {
            Ok(Some(fact)) => {
                if let Fact::Published { item, .. } = &fact {
                    self.wires.insert(item.seq, wire.company.clone());
                    if self.wires.len() > 64 {
                        self.wires.pop_first();
                    }
                }
                if let Err(e) = self.record(fact, now) {
                    log(&format!("journal append failed: {e}"));
                    self.reject(id, template::WIRE, "journal unavailable");
                }
            }
            Ok(None) => {}
            Err(reason) => {
                log(&format!("wire from {source} refused: {reason}"));
                self.reject(id, template::WIRE, &reason);
            }
        }
    }

    pub fn handle(&mut self, id: SessionId, bytes: &[u8], now: Timestamp) {
        let now = self.advance(now);
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        let login = session.login.clone();
        let message = match MemberMessage::decode(bytes) {
            Ok(message) => message,
            Err(e) => {
                let ref_template = Decoder::peek_template(bytes).unwrap_or(0);
                return self.reject(id, ref_template, &format!("decode error: {e}"));
            }
        };
        let ref_template = message.template_id();
        match (login, message) {
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
                let result = if self.room.subscriber(&name).is_some() {
                    Err(format!("{name} already subscribes"))
                } else {
                    self.applications.receive(kind, &name, &secret, &note, now)
                };
                match result {
                    Ok(text) => {
                        log(&format!("subscription {text}"));
                        self.ack(id, ref_template, text);
                    }
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
            (Some(Login::Source(source)), MemberMessage::Wire(wire)) => self.on_wire(id, &source, wire, now),
            (_, MemberMessage::Heartbeat) => {}
            (None, _) => self.reject(id, ref_template, "logon required"),
            (Some(_), MemberMessage::Logon { .. }) => {
                self.reject(id, ref_template, "already logged on")
            }
            (Some(_), MemberMessage::QueryNewsroom) => {
                let mut status = self.room.status(self.subscribers(), self.sources_connected(), self.writer_status());
                status.published = self.published;
                self.reply(id, &ServerMessage::NewsroomStatus(status));
            }
            (Some(_), MemberMessage::QueryNews { limit, symbol }) => {
                let published = self.published;
                let limit = limit.clamp(1, 500) as usize;
                let mut items: Vec<_> = self
                    .room
                    .recent(limit + self.pending.len(), symbol)
                    .into_iter()
                    .filter(|i| i.seq <= published)
                    .collect();
                let skip = items.len().saturating_sub(limit);
                items.drain(..skip);
                let count = items.len();
                for item in items {
                    self.reply(id, &ServerMessage::News(item));
                }
                self.ack(id, ref_template, format!("{count} item(s)"));
            }
            (Some(Login::Ops), MemberMessage::CreateMember { name, secret, .. }) => {
                if !valid_username(&name) || secret.is_empty() {
                    return self.reject(
                        id,
                        ref_template,
                        "subscriber name must be 1-32 chars of a-z, 0-9, _ or - and secret non-empty",
                    );
                }
                match self
                    .room
                    .decide_register(&name, hash_secret(&name, &secret))
                {
                    Ok(fact) => match self.record(fact, now) {
                        Ok(()) => {
                            let text = format!("{name} now subscribes to the news feed");
                            log(&text);
                            self.ack(id, ref_template, text);
                        }
                        Err(_) => self.reject(id, ref_template, "journal unavailable"),
                    },
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
            (Some(Login::Ops), MemberMessage::QueryApplications) => {
                self.reply(id, &ServerMessage::Applications(self.applications.list()));
            }
            (Some(Login::Ops), MemberMessage::Decide { name, approve, note, .. }) => {
                let Some(pending) = self.applications.take(&name) else {
                    return self.reject(id, ref_template, &format!("no pending application from {name}"));
                };
                if !approve {
                    let text = format!("subscription request from {name} refused{}", if note.is_empty() { String::new() } else { format!(": {note}") });
                    log(&text);
                    return self.ack(id, ref_template, text);
                }
                match self.room.decide_register(&name, pending.secret) {
                    Ok(fact) => match self.record(fact, now) {
                        Ok(()) => {
                            let text = format!("{name} admitted: now subscribes to the news feed");
                            log(&text);
                            self.ack(id, ref_template, text);
                        }
                        Err(_) => self.reject(id, ref_template, "journal unavailable"),
                    },
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
            _ => self.reject(id, ref_template, "not permitted for this login"),
        }
    }

    fn logon(&mut self, id: SessionId, name: &str, secret: &str, last_seq: u64, now: Timestamp) {
        let login = if name == self.ops.name && verify(&self.ops.secret, name, secret) {
            Login::Ops
        } else if let Some(hash) = self.sources.get(name).filter(|h| verify(h, name, secret)) {
            let _ = hash;
            Login::Source(name.to_string())
        } else {
            match self.room.subscriber(name) {
                Some(hash) if verify(&hash, name, secret) => Login::Subscriber,
                _ => {
                    return self.reject(
                        id,
                        template::MEMBER_LOGON,
                        "invalid news subscriber or secret",
                    );
                }
            }
        };
        if let Some(session) = self.sessions.get_mut(&id) {
            session.login = Some(login.clone());
        }
        log(&format!("session {id}: {name} logged on to the news feed"));
        self.reply(
            id,
            &ServerMessage::MemberLogonAck {
                member_id: 0,
                kind: if login == Login::Ops {
                    MemberKind::Admin
                } else {
                    MemberKind::Broker
                },
                server_time: now,
            },
        );
        if login == Login::Subscriber {
            if last_seq.saturating_add(1) < self.room.oldest_retained() {
                log(&format!(
                    "{name} asked for news after #{last_seq}; older items are no longer retained"
                ));
            }
            let published = self.published;
            for item in self
                .room
                .since(last_seq)
                .into_iter()
                .filter(|i| i.seq <= published)
            {
                self.reply(id, &ServerMessage::News(item));
            }
        }
        let next_seq = match &login {
            Login::Source(source) => self.room.received(source) + 1,
            _ => self.published + 1,
        };
        self.reply(id, &ServerMessage::ReplayComplete { next_seq });
    }
}
