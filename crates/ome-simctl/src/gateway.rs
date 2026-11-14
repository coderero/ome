use crate::journal::Record;
use crate::state::{SimState, validate};
use ome_client::Client;
use ome_core::types::Timestamp;
use ome_net::auth::{SecretHash, verify};
use ome_net::{Outbox, SessionId, log};
use ome_protocol::framing::frame;
use ome_protocol::{Decoder, GroupInfo, MemberMessage, ServerMessage, SimMessage, SimRole};
use ome_store::Journal;
use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

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
        }
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
                if let Err(reason) = validate(&command) {
                    return self.reject(id, ref_template, &reason);
                }
                if let Some(journal) = &mut self.journal
                    && journal
                        .append(&Record {
                            timestamp: now,
                            command: command.clone(),
                        })
                        .is_err()
                {
                    return self.reject(id, ref_template, "journal unavailable");
                }
                self.state.apply(&command, now);
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
                self.ack(id, ref_template, text);
            }
            (
                Some(SimRole::Operator),
                SimMessage::Accelerate {
                    pre_open_s,
                    open_s,
                    closing_s,
                    closed_s,
                },
            ) => match self.accelerate([pre_open_s, open_s, closing_s, closed_s]) {
                Ok(text) => {
                    log(&text);
                    self.ack(id, ref_template, text)
                }
                Err(reason) => self.reject(id, ref_template, &reason),
            },
            _ => self.reject(id, ref_template, "not permitted for this role"),
        }
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
