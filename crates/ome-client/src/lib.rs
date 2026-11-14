pub mod upstream;

use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, OrderKind, OrderOptions, Qty, Side, TimeInForce};
use ome_protocol::framing::{frame, read_frame};
use ome_protocol::{ClientMessage, MemberId, MemberKind, MemberMessage, Role, ServerMessage};
use std::fmt;
use std::io::{self, BufReader, BufWriter, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::mpsc::{
    Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, TrySendError, channel,
    sync_channel,
};
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_EXCHANGE_ADDR: &str = "127.0.0.1:7000";
pub const DEFAULT_BROKER_ADDR: &str = "127.0.0.1:7101";
const LINK_QUEUE: usize = 65_536;

#[derive(Debug)]
pub enum ClientError {
    Io(io::Error),
    Disconnected,
    Timeout,
    Rejected(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Io(e) => write!(f, "{e}"),
            ClientError::Disconnected => write!(f, "disconnected from server"),
            ClientError::Timeout => write!(f, "timed out waiting for server"),
            ClientError::Rejected(reason) => write!(f, "rejected: {reason}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<io::Error> for ClientError {
    fn from(e: io::Error) -> Self {
        ClientError::Io(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    pub account_id: AccountId,
    pub role: Role,
    pub server_time: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberSession {
    pub member_id: MemberId,
    pub kind: MemberKind,
    pub server_time: u64,
}

pub enum LinkEvent {
    Message(ServerMessage),
    Closed,
}

pub struct Link {
    tx: SyncSender<Vec<u8>>,
}

impl Link {
    pub fn connect<T: Send + 'static>(
        addr: &str,
        events: Sender<T>,
        wrap: impl Fn(LinkEvent) -> T + Send + 'static,
    ) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        let reader_stream = stream.try_clone()?;
        let (tx, rx) = sync_channel::<Vec<u8>>(LINK_QUEUE);

        thread::spawn(move || {
            let mut writer = BufWriter::new(&stream);
            'outer: while let Ok(payload) = rx.recv() {
                if writer.write_all(&frame(&payload)).is_err() {
                    break;
                }
                while let Ok(payload) = rx.try_recv() {
                    if writer.write_all(&frame(&payload)).is_err() {
                        break 'outer;
                    }
                }
                if writer.flush().is_err() {
                    break;
                }
            }
            drop(writer);
            let _ = stream.shutdown(Shutdown::Both);
        });
        thread::spawn(move || {
            let mut reader = BufReader::new(reader_stream);
            while let Ok(Some(payload)) = read_frame(&mut reader) {
                if let Ok(message) = ServerMessage::decode(&payload)
                    && events.send(wrap(LinkEvent::Message(message))).is_err()
                {
                    return;
                }
            }
            let _ = events.send(wrap(LinkEvent::Closed));
        });
        Ok(Self { tx })
    }

    pub fn send(&self, payload: Vec<u8>) -> bool {
        match self.tx.try_send(payload) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }
}

pub struct Client {
    link: Link,
    inbox: Receiver<ServerMessage>,
    next_client_order_id: u64,
}

fn into_message(event: LinkEvent) -> Option<ServerMessage> {
    match event {
        LinkEvent::Message(message) => Some(message),
        LinkEvent::Closed => None,
    }
}

impl Client {
    pub fn connect(addr: &str) -> Result<Self, ClientError> {
        let (tx, rx) = channel::<Option<ServerMessage>>();
        let link = Link::connect(addr, tx, into_message)?;
        let (inbox_tx, inbox) = channel();
        thread::spawn(move || {
            while let Ok(Some(message)) = rx.recv() {
                if inbox_tx.send(message).is_err() {
                    break;
                }
            }
        });
        let seed = ome_net_time();
        Ok(Self {
            link,
            inbox,
            next_client_order_id: seed << 16,
        })
    }

    pub fn connect_and_logon(
        addr: &str,
        username: &str,
        secret: &str,
    ) -> Result<(Self, Session), ClientError> {
        let mut client = Self::connect(addr)?;
        let session = client.logon(username, secret)?;
        Ok((client, session))
    }

    pub fn connect_member(
        addr: &str,
        name: &str,
        secret: &str,
    ) -> Result<(Self, MemberSession), ClientError> {
        let mut client = Self::connect(addr)?;
        let session = client.logon_member(name, secret, u64::MAX)?;
        Ok((client, session))
    }

    pub fn logon(&mut self, username: &str, secret: &str) -> Result<Session, ClientError> {
        self.send(&ClientMessage::Logon {
            username: username.into(),
            secret: secret.into(),
        })?;
        match self.wait_for(Duration::from_secs(5), |m| {
            matches!(
                m,
                ServerMessage::LogonAck { .. } | ServerMessage::Reject { .. }
            )
        })? {
            ServerMessage::LogonAck {
                account_id,
                role,
                server_time,
            } => Ok(Session {
                account_id,
                role,
                server_time,
            }),
            ServerMessage::Reject { reason, .. } => Err(ClientError::Rejected(reason)),
            _ => unreachable!(),
        }
    }

    pub fn logon_member(
        &mut self,
        name: &str,
        secret: &str,
        last_seq: u64,
    ) -> Result<MemberSession, ClientError> {
        self.send_member(&MemberMessage::Logon {
            name: name.into(),
            secret: secret.into(),
            last_seq,
        })?;
        match self.wait_for(Duration::from_secs(5), |m| {
            matches!(
                m,
                ServerMessage::MemberLogonAck { .. } | ServerMessage::Reject { .. }
            )
        })? {
            ServerMessage::MemberLogonAck {
                member_id,
                kind,
                server_time,
            } => Ok(MemberSession {
                member_id,
                kind,
                server_time,
            }),
            ServerMessage::Reject { reason, .. } => Err(ClientError::Rejected(reason)),
            _ => unreachable!(),
        }
    }

    pub fn send(&mut self, message: &ClientMessage) -> Result<(), ClientError> {
        self.send_bytes(message.encode())
    }

    pub fn send_member(&mut self, message: &MemberMessage) -> Result<(), ClientError> {
        self.send_bytes(message.encode())
    }

    pub fn send_sim(&mut self, message: &ome_protocol::SimMessage) -> Result<(), ClientError> {
        self.send_bytes(message.encode())
    }

    pub fn connect_sim(
        addr: &str,
        role: ome_protocol::SimRole,
        name: &str,
        secret: &str,
    ) -> Result<Self, ClientError> {
        let mut client = Self::connect(addr)?;
        client.send_sim(&ome_protocol::SimMessage::Logon {
            role,
            name: name.into(),
            secret: secret.into(),
        })?;
        match client.wait_for(Duration::from_secs(5), |m| {
            matches!(m, ServerMessage::Ack { .. } | ServerMessage::Reject { .. })
        })? {
            ServerMessage::Reject { reason, .. } => Err(ClientError::Rejected(reason)),
            _ => Ok(client),
        }
    }

    pub fn request_sim(
        &mut self,
        message: &ome_protocol::SimMessage,
    ) -> Result<String, ClientError> {
        self.send_sim(message)?;
        match self.wait_for(Duration::from_secs(10), |m| {
            matches!(m, ServerMessage::Ack { .. } | ServerMessage::Reject { .. })
        })? {
            ServerMessage::Ack { text, .. } => Ok(text),
            ServerMessage::Reject { reason, .. } => Err(ClientError::Rejected(reason)),
            _ => unreachable!(),
        }
    }

    fn send_bytes(&mut self, payload: Vec<u8>) -> Result<(), ClientError> {
        if self.link.send(payload) {
            Ok(())
        } else {
            Err(ClientError::Disconnected)
        }
    }

    pub fn send_order(
        &mut self,
        symbol: Symbol,
        side: Side,
        kind: OrderKind,
        qty: Qty,
    ) -> Result<u64, ClientError> {
        let tif = match kind {
            OrderKind::Limit(_) => TimeInForce::Gtc,
            OrderKind::Market => TimeInForce::Ioc,
        };
        self.send_order_with(symbol, side, kind, OrderOptions::tif(tif), qty)
    }

    pub fn next_order_id(&mut self) -> u64 {
        self.next_client_order_id += 1;
        self.next_client_order_id
    }

    pub fn send_order_with(
        &mut self,
        symbol: Symbol,
        side: Side,
        kind: OrderKind,
        options: OrderOptions,
        qty: Qty,
    ) -> Result<u64, ClientError> {
        self.next_client_order_id += 1;
        let client_order_id = self.next_client_order_id;
        self.send(&ClientMessage::NewOrder {
            client_order_id,
            symbol,
            side,
            kind,
            options,
            qty,
        })?;
        Ok(client_order_id)
    }

    pub fn try_recv(&self) -> Result<Option<ServerMessage>, ClientError> {
        match self.inbox.try_recv() {
            Ok(message) => Ok(Some(message)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(ClientError::Disconnected),
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<ServerMessage>, ClientError> {
        match self.inbox.recv_timeout(timeout) {
            Ok(message) => Ok(Some(message)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(ClientError::Disconnected),
        }
    }

    pub fn wait_for(
        &self,
        timeout: Duration,
        mut matches: impl FnMut(&ServerMessage) -> bool,
    ) -> Result<ServerMessage, ClientError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ClientError::Timeout);
            }
            match self.recv_timeout(remaining)? {
                Some(message) if matches(&message) => return Ok(message),
                _ => {}
            }
        }
    }

    pub fn request(&mut self, message: &ClientMessage) -> Result<String, ClientError> {
        let template = message.template_id();
        self.send(message)?;
        self.await_ack(template)
    }

    pub fn request_member(&mut self, message: &MemberMessage) -> Result<String, ClientError> {
        let template = message.template_id();
        self.send_member(message)?;
        self.await_ack(template)
    }

    fn await_ack(&self, template: u16) -> Result<String, ClientError> {
        match self.wait_for(Duration::from_secs(5), |m| match m {
            ServerMessage::Ack { ref_template, .. }
            | ServerMessage::Reject { ref_template, .. } => *ref_template == template,
            _ => false,
        })? {
            ServerMessage::Ack { text, .. } => Ok(text),
            ServerMessage::Reject { reason, .. } => Err(ClientError::Rejected(reason)),
            _ => unreachable!(),
        }
    }
}

fn ome_net_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(1)
}
