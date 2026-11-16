use crate::journal::Record;
use crate::links::{Done, IdeaDone, IdeaJob, Job};
use crate::newsroom::{Fact, Filing, Newsroom};
use crate::writer::Writer;
use ome_core::types::Timestamp;
use ome_net::auth::{SecretHash, hash_secret, valid_username, verify};
use ome_net::{Outbox, SessionId, log};
use ome_protocol::framing::frame;
use ome_protocol::{Decoder, MarketPhase, MemberKind, MemberMessage, ServerMessage, template};
use ome_store::Journal;
use rand::Rng;
use rand::rngs::StdRng;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::Arc;
use std::sync::mpsc::Sender;

pub struct OpsCredentials {
    pub name: String,
    pub secret: SecretHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Login {
    Ops,
    Subscriber,
}

struct Session {
    outbox: Box<dyn Outbox>,
    login: Option<Login>,
}

pub struct Gateway {
    room: Newsroom,
    journal: Option<Journal<Record>>,
    ops: OpsCredentials,
    sessions: BTreeMap<SessionId, Session>,
    clock: Timestamp,
    rng: StdRng,
    writer: Box<dyn Writer>,
    phase: MarketPhase,
    news_per_day: f64,
    open_started: Option<Timestamp>,
    open_ms: Timestamp,
    last_tick: Option<Timestamp>,
    filings: Option<Sender<Filing>>,
    rewriter: Option<Sender<Job>>,
    rewrite_wait_ms: Timestamp,
    pending: VecDeque<(u64, Timestamp, bool)>,
    published: u64,
    stats: WriterStats,
    ideas: VecDeque<(ome_core::symbol::Symbol, crate::llm::Idea)>,
    idea_jobs: Option<Sender<IdeaJob>>,
    ideas_pending: usize,
    ideas_used: u64,
    ideas_rejected: u64,
    idea_problem: String,
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

pub const UNMEASURED_DAY_MS: Timestamp = 23_400_000;

fn encode(message: &ServerMessage) -> Arc<[u8]> {
    frame(&message.encode()).into()
}

impl Gateway {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        room: Newsroom,
        journal: Option<Journal<Record>>,
        ops: OpsCredentials,
        writer: Box<dyn Writer>,
        rng: StdRng,
        news_per_day: f64,
        clock: Timestamp,
    ) -> Self {
        Self {
            room,
            journal,
            ops,
            sessions: BTreeMap::new(),
            clock,
            rng,
            writer,
            phase: MarketPhase::Closed,
            news_per_day,
            open_started: None,
            open_ms: UNMEASURED_DAY_MS,
            last_tick: None,
            filings: None,
            rewriter: None,
            rewrite_wait_ms: 0,
            pending: VecDeque::new(),
            published: 0,
            stats: WriterStats::default(),
            ideas: VecDeque::new(),
            idea_jobs: None,
            ideas_pending: 0,
            ideas_used: 0,
            ideas_rejected: 0,
            idea_problem: String::new(),
        }
        .caught_up()
    }

    fn caught_up(mut self) -> Self {
        self.published = self.room.next_seq() - 1;
        self
    }

    pub fn file_with(&mut self, filings: Sender<Filing>) {
        for filing in self.room.prospectuses() {
            let _ = filings.send(filing);
        }
        self.filings = Some(filings);
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

    fn record(&mut self, fact: Fact, now: Timestamp) -> io::Result<()> {
        if let Some(journal) = &mut self.journal {
            journal.append(&Record {
                timestamp: now,
                fact: fact.clone(),
            })?;
        }
        self.room.apply(&fact);
        if let Fact::Published { item, .. } = &fact
            && item.kind == ome_protocol::NewsKind::Prospectus
            && let Some(sink) = &self.filings
            && let Some(filing) = self
                .room
                .prospectuses()
                .into_iter()
                .find(|f| Some(f.symbol) == item.symbol)
        {
            let _ = sink.send(filing);
        }
        if let Fact::Published { item, .. } = &fact {
            let invented = item.data.split_whitespace().any(|w| w == "source=llm");
            match self.rewriter.as_ref().filter(|_| !invented) {
                Some(rewriter) => {
                    let company = item
                        .symbol
                        .and_then(|s| self.room.spec(s))
                        .map(|s| s.name.clone())
                        .unwrap_or_default();
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
            .filter(|(_, s)| s.login.is_some())
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

    pub fn use_ideas(&mut self, jobs: Sender<IdeaJob>) {
        self.idea_jobs = Some(jobs);
    }

    pub fn on_idea(&mut self, done: IdeaDone) {
        self.ideas_pending = self.ideas_pending.saturating_sub(1);
        match done.result {
            Ok(idea) => {
                log(&format!("LLM idea for {} in {} ms: {}", done.symbol, done.elapsed_ms, idea.headline));
                self.ideas.push_back((done.symbol, idea));
            }
            Err(problem) => {
                self.ideas_rejected += 1;
                if self.idea_problem != problem {
                    log(&format!("LLM idea for {} not used: {problem}", done.symbol));
                }
                self.idea_problem = problem;
            }
        }
    }

    fn request_ideas(&mut self) {
        let Some(jobs) = &self.idea_jobs else {
            return;
        };
        let symbols = self.room.symbols();
        while !symbols.is_empty() && self.ideas.len() + self.ideas_pending < 2 {
            let symbol = symbols[self.rng.random_range(0..symbols.len())];
            let Some(context) = self.room.idea_context(symbol) else {
                return;
            };
            if jobs.send(IdeaJob { symbol, context, seed: self.rng.random() }).is_err() {
                self.idea_jobs = None;
                return;
            }
            self.ideas_pending += 1;
        }
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
            "llm {} ({} invented, {} idea(s) refused, {} written, avg {} ms, {} fallback(s), {} timeout(s){})",
            self.stats.model,
            self.ideas_used,
            self.ideas_rejected,
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

    fn commit(&mut self, fact: Option<Fact>, now: Timestamp) {
        if let Some(fact) = fact
            && let Err(e) = self.record(fact, now)
        {
            log(&format!("journal append failed: {e}"));
        }
    }

    pub fn start_up(&mut self, now: Timestamp) {
        let now = self.advance(now);
        for symbol in self.room.unfiled() {
            let fact =
                self.room
                    .decide_prospectus(symbol, now, &mut self.rng, self.writer.as_mut());
            self.commit(fact, now);
        }
    }

    pub fn on_market(&mut self, phase: MarketPhase, day: u64, now: Timestamp) {
        let now = self.advance(now);
        if phase == MarketPhase::Open {
            self.open_started.get_or_insert(now);
        } else if let Some(start) = self.open_started.take() {
            self.open_ms = now.saturating_sub(start).max(5_000);
        }
        self.phase = phase;
        let Some(started) = self.room.decide_day(day, &mut self.rng) else {
            return;
        };
        self.commit(Some(started), now);
        log(&format!("trading day {day} begins"));
        for release in self.room.releases_due() {
            let fact = self
                .room
                .decide_release(release, now, &mut self.rng, self.writer.as_mut());
            self.commit(Some(fact), now);
        }
        for symbol in self.room.earnings_due() {
            let fact = self
                .room
                .decide_earnings(symbol, now, &mut self.rng, self.writer.as_mut());
            self.commit(fact, now);
        }
    }

    pub fn tick(&mut self, now: Timestamp) {
        let now = self.advance(now);
        self.release(now);
        let elapsed = now.saturating_sub(self.last_tick.unwrap_or(now));
        self.last_tick = Some(now);
        if self.news_per_day > 0.0 {
            self.request_ideas();
        }
        if self.phase != MarketPhase::Open || self.news_per_day <= 0.0 {
            return;
        }
        let companies = self.room.symbols().len().max(1) as f64;
        let chance = (self.news_per_day * companies * elapsed as f64 / self.open_ms as f64).min(1.0);
        if self.rng.random_bool(chance) {
            let fact = match self.ideas.pop_front() {
                Some((symbol, idea)) => {
                    self.ideas_used += 1;
                    self.room.decide_idea(symbol, &idea, now, &mut self.rng, self.writer.as_mut())
                }
                None => self.room.decide_random(now, &mut self.rng, self.writer.as_mut()),
            };
            self.commit(fact, now);
        }
    }

    pub fn handle(&mut self, id: SessionId, bytes: &[u8], now: Timestamp) {
        let now = self.advance(now);
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        let login = session.login;
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
            (_, MemberMessage::Heartbeat) => {}
            (None, _) => self.reject(id, ref_template, "logon required"),
            (Some(_), MemberMessage::Logon { .. }) => {
                self.reject(id, ref_template, "already logged on")
            }
            (Some(_), MemberMessage::QueryNewsroom) => {
                let mut status = self.room.status(self.subscribers(), self.writer_status());
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
            (Some(Login::Ops), MemberMessage::InjectNews { symbol, event }) => {
                match self
                    .room
                    .inject(symbol, &event, now, &mut self.rng, self.writer.as_mut())
                {
                    Ok(fact) => {
                        let headline = match &fact {
                            Fact::Published { item, .. } => format!(
                                "#{} {} (sentiment {:+}, fair {:+} bps)",
                                item.seq, item.headline, item.sentiment, item.impact_bps
                            ),
                            _ => String::new(),
                        };
                        match self.record(fact, now) {
                            Ok(()) => self.ack(id, ref_template, headline),
                            Err(_) => self.reject(id, ref_template, "journal unavailable"),
                        }
                    }
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
            _ => self.reject(id, ref_template, "not permitted for this login"),
        }
    }

    fn logon(&mut self, id: SessionId, name: &str, secret: &str, last_seq: u64, now: Timestamp) {
        let login = if name == self.ops.name && verify(&self.ops.secret, name, secret) {
            Login::Ops
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
            session.login = Some(login);
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
        let next_seq = self.published + 1;
        self.reply(id, &ServerMessage::ReplayComplete { next_seq });
    }
}
