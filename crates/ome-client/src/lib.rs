use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, OrderKind, Qty, Side};
use ome_protocol::framing::{read_frame, write_frame};
use ome_protocol::{ClientMessage, Role, ServerMessage};
use std::fmt;
use std::io::{self, BufReader, BufWriter, Write};
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError, channel};
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_ADDR: &str = "127.0.0.1:7000";

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

pub struct Client {
    writer: BufWriter<TcpStream>,
    inbox: Receiver<ServerMessage>,
    next_client_order_id: u64,
}

impl Client {
    pub fn connect(addr: &str) -> Result<Self, ClientError> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        let reader_stream = stream.try_clone()?;
        let (tx, inbox) = channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(reader_stream);
            while let Ok(Some(payload)) = read_frame(&mut reader) {
                let Ok(message) = ServerMessage::decode(&payload) else {
                    continue;
                };
                if tx.send(message).is_err() {
                    break;
                }
            }
        });
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(1);
        Ok(Self {
            writer: BufWriter::new(stream),
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

    pub fn send(&mut self, message: &ClientMessage) -> Result<(), ClientError> {
        write_frame(&mut self.writer, &message.encode())?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn send_order(
        &mut self,
        symbol: Symbol,
        side: Side,
        kind: OrderKind,
        qty: Qty,
    ) -> Result<u64, ClientError> {
        self.next_client_order_id += 1;
        let client_order_id = self.next_client_order_id;
        self.send(&ClientMessage::NewOrder {
            client_order_id,
            symbol,
            side,
            kind,
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
