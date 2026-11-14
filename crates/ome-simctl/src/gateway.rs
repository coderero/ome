use crate::journal::{Fact, Mark, Record};
use crate::recording::{Replay, Step};
use crate::state::SimState;
use ome_client::Client;
use ome_core::types::Timestamp;
use ome_net::auth::{SecretHash, verify};
use ome_net::{Outbox, SessionId, log};
use ome_protocol::framing::frame;
use ome_protocol::{
    Decoder, GroupInfo, MemberMessage, Scenario, ServerMessage, SimCommand, SimMessage, SimRole,
};
use ome_store::Journal;
use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

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
    journal: Option<Journal<Record>>,
    credentials: Credentials,
    exchange: ExchangeAccess,
    sessions: BTreeMap<SessionId, Session>,
    clock: Timestamp,
    replay: Option<Replay>,
    newsroom: Option<(String, String)>,
}

fn encode(message: &ServerMessage) -> Arc<[u8]> {
    frame(&message.encode()).into()
}

impl Gateway {
    pub fn new(
        state: SimState,
        journal: Option<Journal<Record>>,
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
            newsroom: None,
        }
    }

    pub fn with_newsroom(mut self, newsroom: Option<(String, String)>) -> Self {
        self.newsroom = newsroom;
        self
    }

    fn forward(&self, command: &SimCommand) -> Option<Result<String, String>> {
        let SimCommand::Scenario(Scenario::NewsShock {
            symbol, move_bps, ..
        }) = command
        else {
            return None;
        };
        let (addr, secret) = self.newsroom.as_ref()?;
        let published = Client::connect_member(addr, "ops", secret)
            .map_err(|e| format!("newsroom unreachable: {e}"))
            .and_then(|(mut news, _)| {
                news.request_member(&MemberMessage::InjectNews {
                    symbol: Some(*symbol),
                    event: format!("shock:{move_bps}"),
                })
                .map_err(|e| e.to_string())
            });
        Some(published)
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
            log(&format!("runner session {id} left"));
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
        match self.forward(&command) {
            Some(Ok(story)) => {
                self.state.check(&Fact::Forwarded(command.clone()))?;
                self.commit(Fact::Forwarded(command.clone()), now)?;
                let text = format!("{} -> newsroom story {story}", command.describe());
                log(&text);
                return Ok(text);
            }
            Some(Err(reason)) => log(&format!("{reason}; applying the shock directly")),
            None => {}
        }
        self.commit(Fact::Command(command.clone()), now)?;
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
