use crate::{Link, LinkEvent};
use ome_net::log;
use ome_protocol::{MemberId, MemberKind, MemberMessage, ServerMessage, template};
use std::sync::mpsc::Sender;

const RECONNECT_MS: u64 = 1_000;
const REJECTED_LOGON_RETRY_MS: u64 = 5_000;

pub trait Uplink: Send {
    fn send(&self, message: &MemberMessage) -> bool;
}

impl Uplink for Link {
    fn send(&self, message: &MemberMessage) -> bool {
        Link::send(self, message.encode())
    }
}

pub trait Connector: Send {
    fn connect(&mut self, generation: u64) -> Option<Box<dyn Uplink>>;
}

pub struct TcpConnector<T> {
    addr: String,
    events: Sender<T>,
    wrap: fn(u64, LinkEvent) -> T,
}

impl<T> TcpConnector<T> {
    pub fn new(addr: String, events: Sender<T>, wrap: fn(u64, LinkEvent) -> T) -> Self {
        Self { addr, events, wrap }
    }
}

impl<T: Send + 'static> Connector for TcpConnector<T> {
    fn connect(&mut self, generation: u64) -> Option<Box<dyn Uplink>> {
        let wrap = self.wrap;
        Link::connect(&self.addr, self.events.clone(), move |event| {
            wrap(generation, event)
        })
        .ok()
        .map(|link| Box::new(link) as Box<dyn Uplink>)
    }
}

pub struct MemberCredentials {
    pub name: String,
    pub secret: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    Down { retry_at: u64 },
    LoggingOn,
    Replaying,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamEvent {
    LoggedOn {
        member_id: MemberId,
        kind: MemberKind,
    },
    Ready {
        next_seq: u64,
    },
    Down,
    Message(ServerMessage),
}

pub struct Upstream {
    label: &'static str,
    connector: Box<dyn Connector>,
    credentials: MemberCredentials,
    uplink: Option<Box<dyn Uplink>>,
    state: LinkState,
    generation: u64,
}

impl Upstream {
    pub fn new(
        label: &'static str,
        connector: Box<dyn Connector>,
        credentials: MemberCredentials,
    ) -> Self {
        Self {
            label,
            connector,
            credentials,
            uplink: None,
            state: LinkState::Down { retry_at: 0 },
            generation: 0,
        }
    }

    pub fn state(&self) -> LinkState {
        self.state
    }

    pub fn is_ready(&self) -> bool {
        self.state == LinkState::Ready
    }

    pub fn send(&self, message: &MemberMessage) -> bool {
        self.uplink.as_ref().is_some_and(|u| u.send(message))
    }

    pub fn tick(&mut self, now: u64, last_seq: u64) {
        let LinkState::Down { retry_at } = self.state else {
            return;
        };
        if now < retry_at {
            return;
        }
        self.generation += 1;
        let logon = MemberMessage::Logon {
            name: self.credentials.name.clone(),
            secret: self.credentials.secret.clone(),
            last_seq,
        };
        match self.connector.connect(self.generation) {
            Some(uplink) if uplink.send(&logon) => {
                self.uplink = Some(uplink);
                self.state = LinkState::LoggingOn;
            }
            _ => {
                if self.generation % 10 == 1 {
                    log(&format!("{} unreachable, retrying", self.label));
                }
                self.state = LinkState::Down {
                    retry_at: now + RECONNECT_MS,
                };
            }
        }
    }

    pub fn on_event(
        &mut self,
        generation: u64,
        event: LinkEvent,
        now: u64,
    ) -> Option<UpstreamEvent> {
        if generation != self.generation {
            return None;
        }
        let message = match event {
            LinkEvent::Closed => {
                self.down(now, RECONNECT_MS, "connection closed");
                return Some(UpstreamEvent::Down);
            }
            LinkEvent::Message(message) => message,
        };
        match message {
            ServerMessage::MemberLogonAck {
                member_id, kind, ..
            } => {
                self.state = LinkState::Replaying;
                Some(UpstreamEvent::LoggedOn { member_id, kind })
            }
            ServerMessage::Reject {
                ref_template: template::MEMBER_LOGON,
                reason,
            } if self.state == LinkState::LoggingOn => {
                self.down(
                    now,
                    REJECTED_LOGON_RETRY_MS,
                    &format!("logon rejected: {reason}"),
                );
                Some(UpstreamEvent::Down)
            }
            ServerMessage::ReplayComplete { next_seq } => {
                self.state = LinkState::Ready;
                Some(UpstreamEvent::Ready { next_seq })
            }
            other => Some(UpstreamEvent::Message(other)),
        }
    }

    fn down(&mut self, now: u64, retry_in: u64, why: &str) {
        if !matches!(self.state, LinkState::Down { .. }) {
            log(&format!("{} link down: {why}", self.label));
        }
        self.uplink = None;
        self.state = LinkState::Down {
            retry_at: now + retry_in,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<MemberMessage>>>);

    impl Uplink for Recorder {
        fn send(&self, message: &MemberMessage) -> bool {
            self.0.lock().unwrap().push(message.clone());
            true
        }
    }

    struct FakeConnector(Recorder, bool);

    impl Connector for FakeConnector {
        fn connect(&mut self, _: u64) -> Option<Box<dyn Uplink>> {
            self.1.then(|| Box::new(self.0.clone()) as Box<dyn Uplink>)
        }
    }

    fn upstream(reachable: bool) -> (Upstream, Recorder) {
        let recorder = Recorder::default();
        let credentials = MemberCredentials {
            name: "b".into(),
            secret: "s".into(),
        };
        (
            Upstream::new(
                "test",
                Box::new(FakeConnector(recorder.clone(), reachable)),
                credentials,
            ),
            recorder,
        )
    }

    #[test]
    fn logs_on_with_last_seq_and_becomes_ready_after_replay() {
        let (mut up, sent) = upstream(true);
        up.tick(0, 41);
        assert_eq!(up.state(), LinkState::LoggingOn);
        assert!(matches!(
            &sent.0.lock().unwrap()[0],
            MemberMessage::Logon { last_seq: 41, .. }
        ));

        let ack = ServerMessage::MemberLogonAck {
            member_id: 2,
            kind: MemberKind::Broker,
            server_time: 0,
        };
        assert!(matches!(
            up.on_event(1, LinkEvent::Message(ack), 0),
            Some(UpstreamEvent::LoggedOn { member_id: 2, .. })
        ));
        assert_eq!(
            up.on_event(
                1,
                LinkEvent::Message(ServerMessage::ReplayComplete { next_seq: 42 }),
                0
            ),
            Some(UpstreamEvent::Ready { next_seq: 42 })
        );
        assert!(up.is_ready());
    }

    #[test]
    fn stale_generations_are_ignored_and_closure_schedules_reconnect() {
        let (mut up, _) = upstream(true);
        up.tick(0, 0);
        assert_eq!(up.on_event(0, LinkEvent::Closed, 10), None);
        assert_eq!(
            up.on_event(1, LinkEvent::Closed, 10),
            Some(UpstreamEvent::Down)
        );
        assert_eq!(
            up.state(),
            LinkState::Down {
                retry_at: 10 + RECONNECT_MS
            }
        );
        up.tick(500, 0);
        assert!(matches!(up.state(), LinkState::Down { .. }));
        up.tick(10 + RECONNECT_MS, 0);
        assert_eq!(up.state(), LinkState::LoggingOn);
    }

    #[test]
    fn rejected_logon_backs_off_and_unreachable_retries() {
        let (mut up, _) = upstream(true);
        up.tick(0, 0);
        let reject = ServerMessage::Reject {
            ref_template: template::MEMBER_LOGON,
            reason: "bad".into(),
        };
        assert_eq!(
            up.on_event(1, LinkEvent::Message(reject), 0),
            Some(UpstreamEvent::Down)
        );
        assert_eq!(
            up.state(),
            LinkState::Down {
                retry_at: REJECTED_LOGON_RETRY_MS
            }
        );

        let (mut unreachable, _) = upstream(false);
        unreachable.tick(0, 0);
        assert_eq!(
            unreachable.state(),
            LinkState::Down {
                retry_at: RECONNECT_MS
            }
        );
    }
}
