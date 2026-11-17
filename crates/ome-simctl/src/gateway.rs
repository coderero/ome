use crate::fleet::{Fleet, Move, agent_key};
use crate::journal::{Fact, Mark, Record};
use crate::provision::{Outcome, Task};
use crate::recording::{Replay, Step};
use crate::state::SimState;
use crate::world::WorldFact;
use crate::world::company::AccountSpec;
use crate::world::ideas::Idea;
use ome_client::upstream::{Upstream, UpstreamEvent};
use ome_client::{Client, LinkEvent};
use ome_core::symbol::Symbol;
use ome_core::types::Timestamp;
use ome_net::auth::{SecretHash, verify};
use ome_net::registry::Report;
use ome_net::{Outbox, SessionId, log};
use ome_protocol::framing::frame;
use ome_protocol::{
    Assignment, Decoder, GroupInfo, MarketPhase, MemberMessage, Role, Scenario, ServerMessage,
    SimCommand, SimMessage, SimRole,
};
use ome_store::FactLog;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;

pub const UNMEASURED_DAY_MS: Timestamp = 23_400_000;
const PLAN_EVERY_MS: Timestamp = 1_000;
const RETRY_MS: Timestamp = 3_000;

pub struct IdeaJob {
    pub symbol: Symbol,
    pub context: crate::world::ideas::IdeaContext,
    pub seed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeaDone {
    pub symbol: Symbol,
    pub result: Result<Idea, String>,
    pub elapsed_ms: u64,
}

#[derive(Default)]
struct Ideas {
    model: String,
    queue: VecDeque<(Symbol, Idea)>,
    jobs: Option<Sender<IdeaJob>>,
    pending: usize,
    used: u64,
    refused: u64,
    problem: String,
}

#[derive(Default)]
struct Provisioning {
    tasks: Option<Sender<Task>>,
    in_flight: BTreeSet<String>,
    retry_at: BTreeMap<String, Timestamp>,
    problems: BTreeMap<String, String>,
    brokers: BTreeMap<String, String>,
    last_plan: Timestamp,
}

pub struct Credentials {
    pub operator: (String, SecretHash),
    pub runner: (String, SecretHash),
}

pub struct ExchangeAccess {
    pub addr: String,
    pub name: String,
    pub secret: String,
}

struct Session {
    outbox: Box<dyn Outbox>,
    role: Option<SimRole>,
    reports: Vec<GroupInfo>,
}

pub struct Gateway {
    state: SimState,
    journal: Option<Box<dyn FactLog<Record>>>,
    credentials: Credentials,
    exchange: ExchangeAccess,
    sessions: BTreeMap<SessionId, Session>,
    clock: Timestamp,
    replay: Option<Replay>,
    rng: StdRng,
    accounts: Vec<AccountSpec>,
    news_per_day: f64,
    phase: MarketPhase,
    open_started: Option<Timestamp>,
    open_ms: Timestamp,
    last_tick: Option<Timestamp>,
    calendar: Option<Upstream>,
    wire: Option<Upstream>,
    ideas: Ideas,
    provisioning: Provisioning,
    fleet: Fleet,
    agent_secret: String,
}

fn encode(message: &ServerMessage) -> Arc<[u8]> {
    frame(&message.encode()).into()
}

impl Gateway {
    pub fn new(
        state: SimState,
        journal: Option<Box<dyn FactLog<Record>>>,
        credentials: Credentials,
        exchange: ExchangeAccess,
        clock: Timestamp,
    ) -> Self {
        Self {
            state,
            journal,
            credentials,
            exchange,
            sessions: BTreeMap::new(),
            clock,
            replay: None,
            rng: StdRng::from_os_rng(),
            accounts: Vec::new(),
            news_per_day: 0.0,
            phase: MarketPhase::Closed,
            open_started: None,
            open_ms: UNMEASURED_DAY_MS,
            last_tick: None,
            calendar: None,
            wire: None,
            ideas: Ideas::default(),
            provisioning: Provisioning::default(),
            fleet: Fleet::default(),
            agent_secret: String::new(),
        }
    }

    pub fn with_world(mut self, accounts: Vec<AccountSpec>, news_per_day: f64, seed: Option<u64>) -> Self {
        self.accounts = accounts;
        self.news_per_day = news_per_day;
        if let Some(seed) = seed {
            self.rng = StdRng::seed_from_u64(seed);
        }
        self
    }

    pub fn with_calendar(mut self, calendar: Upstream) -> Self {
        self.calendar = Some(calendar);
        self
    }

    pub fn with_wire(mut self, wire: Upstream) -> Self {
        self.wire = Some(wire);
        self
    }

    pub fn with_ideas(mut self, jobs: Sender<IdeaJob>, model: &str) -> Self {
        self.ideas.jobs = Some(jobs);
        self.ideas.model = model.to_string();
        self
    }

    pub fn with_provisioner(mut self, tasks: Sender<Task>) -> Self {
        self.provisioning.tasks = Some(tasks);
        self
    }

    pub fn with_fleet(mut self, fleet: Fleet, agent_secret: &str) -> Self {
        self.fleet = fleet;
        self.agent_secret = agent_secret.to_string();
        self
    }

    pub fn start_up(&mut self, now: Timestamp) {
        self.clock = self.clock.max(now);
        for symbol in self.state.world().unfiled() {
            if let Some(fact) = self.state.world().decide_prospectus(symbol) {
                self.commit_world(fact, self.clock);
            }
        }
    }

    fn commit_world(&mut self, fact: WorldFact, now: Timestamp) -> Option<String> {
        let wire = match &fact {
            WorldFact::Happened { wire, .. } => Some(wire.clone()),
            WorldFact::DayStarted { .. } => None,
        };
        if let Err(reason) = self.commit(Fact::World(fact), now) {
            log(&format!("world event not recorded: {reason}"));
            return None;
        }
        let wire = wire?;
        let text = format!(
            "#{} [{}{}] {} (sentiment {:+}, fair {:+} bps)",
            wire.seq,
            wire.kind.label(),
            wire.symbol.map(|s| format!(" {s}")).unwrap_or_default(),
            if wire.headline.is_empty() { &wire.event } else { &wire.headline },
            wire.sentiment,
            wire.impact_bps
        );
        log(&text);
        if let Some(link) = self.wire.as_ref().filter(|l| l.is_ready()) {
            link.send(&MemberMessage::Wire(wire));
        }
        Some(text)
    }

    pub fn connect(&mut self, id: SessionId, outbox: Box<dyn Outbox>) {
        self.sessions.insert(
            id,
            Session {
                outbox,
                role: None,
                reports: Vec::new(),
            },
        );
    }

    pub fn disconnect(&mut self, id: SessionId) {
        if let Some(Session {
            role: Some(SimRole::Runner),
            ..
        }) = self.sessions.remove(&id)
        {
            let freed = self.fleet.runner_left(id);
            log(&format!("runner session {id} left; {freed} agent(s) to reassign"));
        }
    }

    pub fn flush(&mut self) -> io::Result<()> {
        match &mut self.journal {
            Some(journal) => journal.flush(),
            None => Ok(()),
        }
    }

    fn reply(&mut self, id: SessionId, message: &ServerMessage) {
        if let Some(session) = self.sessions.get(&id)
            && !session.outbox.send(encode(message))
        {
            self.sessions.remove(&id);
        }
    }

    fn ack(&mut self, id: SessionId, ref_template: u16, text: String) {
        self.reply(id, &ServerMessage::Ack { ref_template, text });
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

    fn runners(&self) -> Vec<SessionId> {
        self.sessions
            .iter()
            .filter(|(_, s)| s.role == Some(SimRole::Runner))
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn handle(&mut self, id: SessionId, bytes: &[u8], now: Timestamp) {
        self.clock = self.clock.max(now);
        let now = self.clock;
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        let message = match SimMessage::decode(bytes) {
            Ok(message) => message,
            Err(e) => {
                let ref_template = Decoder::peek_template(bytes).unwrap_or(0);
                return self.reject(id, ref_template, &format!("decode error: {e}"));
            }
        };
        let ref_template = message.template_id();
        match (session.role, message) {
            (None, SimMessage::Logon { role, name, secret }) => {
                let (expected, hash) = match role {
                    SimRole::Operator => &self.credentials.operator,
                    SimRole::Runner => &self.credentials.runner,
                };
                if name != *expected || !verify(hash, &name, &secret) {
                    return self.reject(id, ref_template, "invalid simulation credentials");
                }
                if let Some(session) = self.sessions.get_mut(&id) {
                    session.role = Some(role);
                }
                self.ack(id, ref_template, "logged on to ome-simctl".into());
                if role == SimRole::Runner {
                    let replay = self.state.replay(now);
                    log(&format!(
                        "runner session {id} joined; sending {} setting(s)",
                        replay.len()
                    ));
                    for command in replay {
                        self.reply(id, &ServerMessage::SimEvent(command));
                    }
                }
            }
            (None, _) => self.reject(id, ref_template, "logon required"),
            (Some(_), SimMessage::Logon { .. }) => {
                self.reject(id, ref_template, "already logged on")
            }
            (Some(SimRole::Runner), SimMessage::Groups(groups)) => {
                if let Some(session) = self.sessions.get_mut(&id) {
                    session.reports = groups;
                }
            }
            (Some(_), SimMessage::QueryStatus) => {
                let reports: Vec<Vec<GroupInfo>> = self
                    .sessions
                    .values()
                    .filter(|s| s.role == Some(SimRole::Runner))
                    .map(|s| s.reports.clone())
                    .collect();
                let status = self.state.status(&reports, now);
                self.reply(id, &ServerMessage::SimStatus(status));
            }
            (Some(SimRole::Operator), SimMessage::Command(command)) => {
                match self.issue(command, now) {
                    Ok(text) => self.ack(id, ref_template, text),
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
            (
                Some(SimRole::Operator),
                SimMessage::Accelerate {
                    pre_open_s,
                    open_s,
                    closing_s,
                    closed_s,
                },
            ) => match self.speed([pre_open_s, open_s, closing_s, closed_s], now) {
                Ok(text) => self.ack(id, ref_template, text),
                Err(reason) => self.reject(id, ref_template, &reason),
            },
            (Some(SimRole::Operator), SimMessage::Record { name, start }) => {
                match self.record(name, start, now) {
                    Ok(text) => self.ack(id, ref_template, text),
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
            (Some(SimRole::Operator), SimMessage::Replay { name, speed_pct }) => {
                match self.start_replay(&name, speed_pct, now) {
                    Ok(text) => self.ack(id, ref_template, text),
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
            (Some(SimRole::Operator), SimMessage::StopReplay) => match self.replay.take() {
                Some(replay) => {
                    let text = format!(
                        "replay of '{}' stopped after {}/{} step(s)",
                        replay.name, replay.issued, replay.total
                    );
                    log(&text);
                    self.ack(id, ref_template, text)
                }
                None => self.reject(id, ref_template, "no replay is running"),
            },
            (Some(_), SimMessage::QueryWorld) => {
                let mut world = self.state.world().status();
                world.provisioning = self.provisioning_status();
                world.ideas = self.ideas_status();
                self.reply(id, &ServerMessage::WorldStatus(world));
            }
            (Some(SimRole::Operator), SimMessage::Inject { event, symbol }) => {
                match self.state.world().inject(symbol, &event, &mut self.rng) {
                    Ok(fact) => match self.commit_world(fact, now) {
                        Some(text) => self.ack(id, ref_template, text),
                        None => self.reject(id, ref_template, "journal unavailable"),
                    },
                    Err(reason) => self.reject(id, ref_template, &reason),
                }
            }
            (Some(_), SimMessage::QueryRecordings { name }) => {
                let recordings = self.state.recordings();
                let list: Vec<_> = if name.is_empty() {
                    recordings
                        .values()
                        .map(|r| r.info(self.replay.as_ref(), false))
                        .collect()
                } else {
                    match recordings.get(&name) {
                        Some(r) => vec![r.info(self.replay.as_ref(), true)],
                        None => {
                            return self.reject(
                                id,
                                ref_template,
                                &format!("no recording named '{name}'"),
                            );
                        }
                    }
                };
                self.reply(id, &ServerMessage::SimRecordings(list));
            }
            _ => self.reject(id, ref_template, "not permitted for this role"),
        }
    }

    fn commit(&mut self, fact: Fact, now: Timestamp) -> Result<(), String> {
        self.state.check(&fact)?;
        if let Some(journal) = &mut self.journal
            && journal
                .append(&Record {
                    timestamp: now,
                    fact: fact.clone(),
                })
                .is_err()
        {
            return Err("journal unavailable".into());
        }
        self.state.apply_fact(&fact, now);
        Ok(())
    }

    fn issue(&mut self, command: SimCommand, now: Timestamp) -> Result<String, String> {
        if let SimCommand::Scenario(Scenario::NewsShock { symbol, move_bps, .. }) = &command
            && let Some(event) = self.state.world().decide_shock(*symbol, *move_bps)
        {
            self.state.check(&Fact::Forwarded(command.clone()))?;
            self.commit(Fact::Forwarded(command.clone()), now)?;
            let story = self.commit_world(event, now).unwrap_or_default();
            let text = format!("{} -> world event {story}", command.describe());
            return Ok(text);
        }
        self.commit(Fact::Command(command.clone()), now)?;
        if let SimCommand::Fund { group, amount } = &command {
            self.fund(group, *amount);
        }
        let runners = self.runners();
        for runner in &runners {
            self.reply(*runner, &ServerMessage::SimEvent(command.clone()));
        }
        let text = format!(
            "{} (sent to {} runner(s))",
            command.describe(),
            runners.len()
        );
        log(&text);
        Ok(text)
    }

    fn speed(&mut self, seconds: [u64; 4], now: Timestamp) -> Result<String, String> {
        let text = self.accelerate(seconds)?;
        if let Err(reason) = self.commit(Fact::Accelerated(seconds), now) {
            log(&format!("speed change not journaled: {reason}"));
        }
        log(&text);
        Ok(text)
    }

    fn record(&mut self, name: String, start: bool, now: Timestamp) -> Result<String, String> {
        if start {
            self.state.check(&Fact::RecordingStarted {
                name: name.clone(),
                marks: vec![],
            })?;
            let marks = self.tape_marks();
            let symbols = marks.len();
            self.commit(
                Fact::RecordingStarted {
                    name: name.clone(),
                    marks,
                },
                now,
            )?;
            let baseline = self.state.recordings()[&name].baseline.len();
            let text = format!(
                "recording '{name}' started: {baseline} baseline setting(s), tape marked for {symbols} symbol(s)"
            );
            log(&text);
            return Ok(text);
        }
        match self.state.recording() {
            Some(active) if active == name => {}
            Some(active) => return Err(format!("the active recording is '{active}'")),
            None => return Err("nothing is being recorded".into()),
        }
        let marks = self.tape_marks();
        self.commit(Fact::RecordingStopped { marks }, now)?;
        let recording = &self.state.recordings()[&name];
        let text = format!(
            "recording '{name}' stopped: {} step(s) over {}s",
            recording.steps.len(),
            now.saturating_sub(recording.started_at) / 1_000
        );
        log(&text);
        Ok(text)
    }

    fn start_replay(
        &mut self,
        name: &str,
        speed_pct: u32,
        now: Timestamp,
    ) -> Result<String, String> {
        if let Some(replay) = &self.replay {
            return Err(format!(
                "already replaying '{}'; stop it first",
                replay.name
            ));
        }
        if !(10..=10_000).contains(&speed_pct) {
            return Err("replay speed is 0.1x - 100x".into());
        }
        let recording = self
            .state
            .recordings()
            .get(name)
            .ok_or(format!("no recording named '{name}'"))?;
        let Some(stopped_at) = recording.stopped_at else {
            return Err(format!("stop recording '{name}' before replaying it"));
        };
        let replay = Replay::new(recording, now, speed_pct);
        let seconds = (stopped_at - recording.started_at) * 100 / u64::from(speed_pct) / 1_000;
        let text = format!(
            "replaying '{name}': {} step(s) over {seconds}s at {}.{:02}x",
            replay.total,
            speed_pct / 100,
            speed_pct % 100
        );
        log(&text);
        self.replay = Some(replay);
        self.tick(now);
        Ok(text)
    }

    pub fn tick(&mut self, now: Timestamp) {
        self.clock = self.clock.max(now);
        let now = self.clock;
        if let Some(link) = &mut self.calendar {
            link.tick(now, u64::MAX);
        }
        if let Some(link) = &mut self.wire {
            link.tick(now, 0);
        }
        self.intraday(now);
        if now >= self.provisioning.last_plan + PLAN_EVERY_MS {
            self.provisioning.last_plan = now;
            self.plan(now);
        }
        let Some(replay) = &mut self.replay else {
            return;
        };
        let steps = replay.due(now);
        let finished = replay.finished();
        for step in steps {
            let result = match step {
                Step::Command(command) => self.issue(command, now),
                Step::Accelerate(seconds) => self.speed(seconds, now),
            };
            if let Err(reason) = result {
                log(&format!("replay step skipped: {reason}"));
            }
        }
        if finished && let Some(replay) = self.replay.take() {
            log(&format!(
                "replay of '{}' finished ({} step(s))",
                replay.name, replay.total
            ));
        }
    }

    pub fn replaying(&self) -> Option<&Replay> {
        self.replay.as_ref()
    }

    fn tape_marks(&self) -> Vec<Mark> {
        let query = || -> Result<Vec<Mark>, String> {
            let (mut ops, _) = Client::connect_member(
                &self.exchange.addr,
                &self.exchange.name,
                &self.exchange.secret,
            )
            .map_err(|e| e.to_string())?;
            ops.send_member(&MemberMessage::QueryInstruments)
                .map_err(|e| e.to_string())?;
            match ops
                .wait_for(Duration::from_secs(2), |m| {
                    matches!(m, ServerMessage::InstrumentList(_))
                })
                .map_err(|e| e.to_string())?
            {
                ServerMessage::InstrumentList(list) => Ok(list
                    .iter()
                    .map(|i| Mark {
                        symbol: i.symbol,
                        last: i.last_price.unwrap_or(i.reference_price),
                        volume: i.volume,
                    })
                    .collect()),
                _ => Ok(vec![]),
            }
        };
        query().unwrap_or_else(|e| {
            log(&format!("tape not marked, exchange unavailable: {e}"));
            vec![]
        })
    }

    fn accelerate(&self, seconds: [u64; 4]) -> Result<String, String> {
        let (mut ops, _) = Client::connect_member(
            &self.exchange.addr,
            &self.exchange.name,
            &self.exchange.secret,
        )
        .map_err(|e| format!("cannot reach the exchange: {e}"))?;
        ops.request_member(&MemberMessage::SetSchedule {
            pre_open_ms: seconds[0] * 1_000,
            continuous_ms: seconds[1] * 1_000,
            closing_ms: seconds[2] * 1_000,
            closed_ms: seconds[3] * 1_000,
        })
        .map_err(|e| format!("exchange refused the schedule: {e}"))
    }

    pub fn state(&self) -> &SimState {
        &self.state
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}

impl Gateway {
    pub fn on_calendar(&mut self, generation: u64, event: LinkEvent, now: Timestamp) {
        self.clock = self.clock.max(now);
        let now = self.clock;
        let Some(link) = &mut self.calendar else { return };
        if let Some(UpstreamEvent::Message(ServerMessage::MarketStatus(status))) = link.on_event(generation, event, now) {
            self.on_market(status.phase, status.day, now);
        }
    }

    pub fn on_wire(&mut self, generation: u64, event: LinkEvent, now: Timestamp) {
        self.clock = self.clock.max(now);
        let now = self.clock;
        let Some(link) = &mut self.wire else { return };
        match link.on_event(generation, event, now) {
            Some(UpstreamEvent::Ready { next_seq }) => {
                let backlog = self.state.world().since(next_seq);
                if !backlog.is_empty() {
                    log(&format!("newsroom expects event #{next_seq}; sending {} event(s)", backlog.len()));
                }
                for wire in backlog {
                    link.send(&MemberMessage::Wire(wire));
                }
            }
            Some(UpstreamEvent::Message(ServerMessage::Reject { reason, .. })) => {
                log(&format!("newsroom refused an event: {reason}"));
            }
            _ => {}
        }
    }

    pub fn on_market(&mut self, phase: MarketPhase, day: u64, now: Timestamp) {
        if phase == MarketPhase::Open {
            self.open_started.get_or_insert(now);
        } else if let Some(start) = self.open_started.take() {
            self.open_ms = now.saturating_sub(start).max(5_000);
        }
        self.phase = phase;
        let Some(started) = self.state.world().decide_day(day, &mut self.rng) else {
            return;
        };
        self.commit_world(started, now);
        log(&format!("trading day {day} begins in the world"));
        for group in self.fleet.groups.clone().into_iter().filter(|g| g.inflow_bps > 0) {
            let amount = group.cash * group.inflow_bps / 10_000;
            if amount > 0 {
                log(&format!("savers put {} into each {} fund", ome_core::price::format_price(amount), group.name));
                self.fund(&group.name, amount);
            }
        }
        for release in self.state.world().releases_due() {
            let fact = self.state.world().decide_release(release, &mut self.rng);
            self.commit_world(fact, now);
        }
        for symbol in self.state.world().earnings_due() {
            if let Some(fact) = self.state.world().decide_earnings(symbol, &mut self.rng) {
                self.commit_world(fact, now);
            }
        }
    }

    fn intraday(&mut self, now: Timestamp) {
        let elapsed = now.saturating_sub(self.last_tick.unwrap_or(now));
        self.last_tick = Some(now);
        if self.news_per_day <= 0.0 {
            return;
        }
        self.request_ideas();
        if self.phase != MarketPhase::Open {
            return;
        }
        let companies = self.state.world().symbols().len();
        if companies == 0 {
            return;
        }
        let chance = (self.news_per_day * companies as f64 * elapsed as f64 / self.open_ms as f64).min(1.0);
        if !self.rng.random_bool(chance) {
            return;
        }
        let fact = match self.ideas.queue.pop_front() {
            Some((symbol, idea)) => {
                self.ideas.used += 1;
                self.state.world().decide_idea(symbol, &idea)
            }
            None => self.state.world().decide_random(&mut self.rng),
        };
        if let Some(fact) = fact {
            self.commit_world(fact, now);
        }
    }

    fn request_ideas(&mut self) {
        let Some(jobs) = &self.ideas.jobs else { return };
        let symbols = self.state.world().symbols();
        while !symbols.is_empty() && self.ideas.queue.len() + self.ideas.pending < 2 {
            let symbol = symbols[self.rng.random_range(0..symbols.len())];
            let Some(context) = self.state.world().idea_context(symbol) else { return };
            if jobs.send(IdeaJob { symbol, context, seed: self.rng.random() }).is_err() {
                self.ideas.jobs = None;
                return;
            }
            self.ideas.pending += 1;
        }
    }

    pub fn on_idea(&mut self, done: IdeaDone) {
        self.ideas.pending = self.ideas.pending.saturating_sub(1);
        match done.result {
            Ok(idea) => {
                log(&format!("LLM idea for {} in {} ms: {}", done.symbol, done.elapsed_ms, idea.headline));
                self.ideas.queue.push_back((done.symbol, idea));
            }
            Err(problem) => {
                self.ideas.refused += 1;
                if self.ideas.problem != problem {
                    log(&format!("LLM idea for {} not used: {problem}", done.symbol));
                }
                self.ideas.problem = problem;
            }
        }
    }

    fn ideas_status(&self) -> String {
        if self.ideas.model.is_empty() {
            return "event catalogue (no LLM)".into();
        }
        format!(
            "llm {} ({} invented, {} refused{})",
            self.ideas.model,
            self.ideas.used,
            self.ideas.refused,
            if self.ideas.problem.is_empty() { String::new() } else { format!(", last problem: {}", self.ideas.problem) }
        )
    }

    fn want(&mut self, task: Task, now: Timestamp) {
        let Some(key) = task.key().map(str::to_string) else { return };
        if self.state.provisioned(&key)
            || self.provisioning.in_flight.contains(&key)
            || self.provisioning.retry_at.get(&key).is_some_and(|t| *t > now)
        {
            return;
        }
        let Some(tasks) = &self.provisioning.tasks else { return };
        if tasks.send(task).is_ok() {
            self.provisioning.in_flight.insert(key);
        }
    }

    fn listings_done(&self) -> bool {
        self.state
            .world()
            .specs()
            .iter()
            .filter(|s| !s.underwriter.is_empty())
            .all(|s| self.state.provisioned(&format!("ipo:{}", s.symbol)))
    }

    fn plan(&mut self, now: Timestamp) {
        if self.provisioning.tasks.is_none() {
            return;
        }
        let filings = self.state.world().prospectuses();
        for spec in self.state.world().specs().to_vec() {
            if spec.underwriter.is_empty() {
                continue;
            }
            let symbol = spec.symbol;
            let lower = symbol.to_string().to_lowercase();
            if let Some(filing) = filings.iter().find(|f| f.symbol == symbol) {
                self.want(
                    Task::FileProspectus {
                        key: format!("prospectus:{symbol}"),
                        symbol,
                        reference: filing.reference,
                        name: filing.name.clone(),
                    },
                    now,
                );
            }
            self.want(
                Task::OpenAccount {
                    key: format!("issuer:{symbol}"),
                    broker: spec.underwriter.clone(),
                    username: format!("{lower}_ir"),
                    secret: format!("{lower}-ir-pw"),
                    role: Role::Issuer,
                    cash: 0,
                },
                now,
            );
            if self.state.provisioned(&format!("prospectus:{symbol}")) && self.state.provisioned(&format!("issuer:{symbol}")) {
                self.want(
                    Task::ListIpo {
                        key: format!("ipo:{symbol}"),
                        symbol,
                        price: spec.price,
                        shares: spec.float,
                        underwriter: spec.underwriter.clone(),
                        issuer: format!("{lower}_ir"),
                        name: spec.name.clone(),
                    },
                    now,
                );
            }
            if !spec.dmm.is_empty() && self.state.provisioned(&format!("ipo:{symbol}")) {
                self.want(Task::SetDmm { key: format!("dmm:{symbol}"), symbol, member: spec.dmm.clone() }, now);
            }
        }
        for account in self.accounts.clone() {
            self.want(
                Task::OpenAccount {
                    key: format!("account:{}:{}", account.broker, account.username),
                    broker: account.broker,
                    username: account.username,
                    secret: account.secret,
                    role: account.role,
                    cash: account.cash,
                },
                now,
            );
        }
        if !self.listings_done() {
            return;
        }
        let runners = self.runners();
        let state = &self.state;
        let moves = self.fleet.plan(
            |g| state.group_count(&g.name).unwrap_or(g.count),
            |g, name| state.provisioned(&agent_key(&g.broker, name)),
            &runners,
        );
        for mv in moves {
            match &mv {
                Move::Open { group, name } => {
                    let g = self.fleet.groups[*group].clone();
                    self.want(
                        Task::OpenAccount {
                            key: agent_key(&g.broker, name),
                            broker: g.broker,
                            username: name.clone(),
                            secret: self.agent_secret.clone(),
                            role: g.role,
                            cash: g.cash,
                        },
                        now,
                    );
                }
                Move::Assign { group, name, runner } => {
                    let g = &self.fleet.groups[*group];
                    let Some(addr) = self.provisioning.brokers.get(&g.broker).cloned() else { continue };
                    let assignment = Assignment {
                        name: name.clone(),
                        group: g.name.clone(),
                        broker: g.broker.clone(),
                        addr,
                        secret: self.agent_secret.clone(),
                        spec: g.line.clone(),
                    };
                    self.reply(*runner, &ServerMessage::SimAssign(assignment));
                    self.fleet.confirm(&mv);
                }
                Move::Retire { name, runner } => {
                    self.reply(*runner, &ServerMessage::SimRetire { name: name.clone() });
                    log(&format!("retiring {name}"));
                    self.fleet.confirm(&mv);
                }
            }
        }
    }

    fn fund(&mut self, group: &str, amount: u64) {
        let Some(tasks) = &self.provisioning.tasks else { return };
        let mut by_broker: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for name in self.fleet.members(group) {
            let Some((g, _)) = name.rsplit_once('-') else { continue };
            if let Some(spec) = self.fleet.group(g) {
                by_broker.entry(spec.broker.clone()).or_default().push(name.clone());
            }
        }
        for (broker, usernames) in by_broker {
            let _ = tasks.send(Task::Fund { broker, usernames, amount });
        }
    }

    pub fn on_outcome(&mut self, outcome: Outcome, now: Timestamp) {
        self.clock = self.clock.max(now);
        let now = self.clock;
        match outcome {
            Outcome::Done { key, text } => {
                self.provisioning.in_flight.remove(&key);
                self.provisioning.problems.remove(&key);
                if !key.starts_with("agent:") {
                    log(&format!("provisioned {key}: {text}"));
                }
                if let Err(reason) = self.commit(Fact::Provisioned { key }, now) {
                    log(&format!("provisioning not recorded: {reason}"));
                }
            }
            Outcome::Failed { key, reason } => {
                self.provisioning.in_flight.remove(&key);
                self.provisioning.retry_at.insert(key.clone(), now + RETRY_MS);
                if self.provisioning.problems.get(&key) != Some(&reason) {
                    log(&format!("{key} not done yet: {reason}"));
                    self.provisioning.problems.insert(key, reason);
                }
            }
            Outcome::Funded { text } => log(&text),
            Outcome::Brokers(brokers) => {
                log(&format!("brokers known: {}", brokers.keys().cloned().collect::<Vec<_>>().join(", ")));
                self.provisioning.brokers = brokers;
            }
        }
    }

    fn provisioning_status(&self) -> String {
        let companies = self.state.world().specs().iter().filter(|s| !s.underwriter.is_empty()).count();
        let wanted: u32 = self
            .fleet
            .groups
            .iter()
            .map(|g| self.state.group_count(&g.name).unwrap_or(g.count))
            .sum();
        format!(
            "listings {}/{companies}, accounts {}/{}, agents {}/{wanted} assigned to {} runner(s), brokers known {}",
            self.state.provisioned_count("ipo:"),
            self.state.provisioned_count("account:"),
            self.accounts.len(),
            self.fleet.assigned(),
            self.runners().len(),
            self.provisioning.brokers.len()
        )
    }

    pub fn health(&self) -> Report {
        let mut report = Report::default();
        if let Some(link) = &self.calendar {
            report.dep("exchange", link.is_ready(), link.problem());
        }
        if let Some(link) = &self.wire {
            report.dep("newsroom", link.is_ready(), link.problem());
        }
        report.metric("events", self.state.world().next_seq() as i64 - 1);
        report.metric("day", self.state.world().day() as i64);
        report.metric("runners", self.runners().len() as i64);
        report.metric("agents", self.fleet.assigned() as i64);
        report.metric("listings", self.state.provisioned_count("ipo:") as i64);
        let mut report = report.ready_unless(&["exchange"]);
        if self.provisioning.tasks.is_some() && !self.listings_done() {
            report.health = Some(ome_protocol::Health::Starting);
            report.detail = format!("seeding: {}", self.provisioning_status());
        } else if report.detail.is_empty() {
            report.detail = self.provisioning_status();
        }
        report
    }
}
