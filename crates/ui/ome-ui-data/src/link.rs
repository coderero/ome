use ome_client::{Client, ClientError};
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderId, OrderKind, OrderOptions, Qty, Side, TimeInForce};
use ome_protocol::{ClientMessage, ServerMessage, Trigger};
use std::collections::BTreeSet;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderRequest {
    Market {
        symbol: Symbol,
        side: Side,
        qty: Qty,
    },
    Limit {
        symbol: Symbol,
        side: Side,
        price: Price,
        qty: Qty,
        tif: TimeInForce,
    },
    Stop {
        symbol: Symbol,
        side: Side,
        qty: Qty,
        trigger: Trigger,
        kind: OrderKind,
    },
    Bracket {
        symbol: Symbol,
        side: Side,
        qty: Qty,
        entry: OrderKind,
        take_profit: Price,
        stop_loss: Price,
    },
    Cancel(OrderId),
    CancelConditional(u64),
    CancelAll(Option<Symbol>),
}

impl OrderRequest {
    pub fn message(self, client_order_id: u64) -> ClientMessage {
        match self {
            OrderRequest::Market { symbol, side, qty } => ClientMessage::NewOrder {
                client_order_id,
                symbol,
                side,
                kind: OrderKind::Market,
                options: OrderOptions::default(),
                qty,
            },
            OrderRequest::Limit {
                symbol,
                side,
                price,
                qty,
                tif,
            } => ClientMessage::NewOrder {
                client_order_id,
                symbol,
                side,
                kind: OrderKind::Limit(price),
                options: OrderOptions::tif(tif),
                qty,
            },
            OrderRequest::Stop {
                symbol,
                side,
                qty,
                trigger,
                kind,
            } => ClientMessage::Conditional {
                client_order_id,
                symbol,
                side,
                qty,
                trigger,
                kind,
                options: OrderOptions::default(),
            },
            OrderRequest::Bracket {
                symbol,
                side,
                qty,
                entry,
                take_profit,
                stop_loss,
            } => ClientMessage::Bracket {
                client_order_id,
                symbol,
                side,
                qty,
                entry,
                take_profit,
                stop_loss,
            },
            OrderRequest::Cancel(order_id) => ClientMessage::Cancel { order_id },
            OrderRequest::CancelConditional(cond_id) => {
                ClientMessage::CancelConditional { cond_id }
            }
            OrderRequest::CancelAll(symbol) => ClientMessage::CancelAll { symbol },
        }
    }
}

pub trait OrderEntry {
    fn submit(&self, request: OrderRequest) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkUpdate {
    Message(ServerMessage),
    Down(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub addr: String,
    pub user: String,
    pub secret: String,
}

pub struct BrokerLink {
    commands: Sender<OrderRequest>,
}

const POLL: Duration = Duration::from_millis(40);
const ACCOUNT_EVERY: Duration = Duration::from_secs(2);

impl BrokerLink {
    pub fn start(credentials: Credentials, updates: Sender<LinkUpdate>) -> Self {
        let (commands, inbox) = mpsc::channel();
        thread::spawn(move || {
            loop {
                let reason = match session(&credentials, &updates, &inbox) {
                    Ok(()) => return,
                    Err(e) => e.to_string(),
                };
                if updates.send(LinkUpdate::Down(reason)).is_err() {
                    return;
                }
                thread::sleep(Duration::from_secs(2));
            }
        });
        Self { commands }
    }
}

impl OrderEntry for BrokerLink {
    fn submit(&self, request: OrderRequest) -> bool {
        self.commands.send(request).is_ok()
    }
}

fn session(
    credentials: &Credentials,
    updates: &Sender<LinkUpdate>,
    inbox: &Receiver<OrderRequest>,
) -> Result<(), ClientError> {
    let (mut client, session) =
        Client::connect_and_logon(&credentials.addr, &credentials.user, &credentials.secret)?;
    let forward = |message: ServerMessage| updates.send(LinkUpdate::Message(message)).is_ok();
    if !forward(ServerMessage::LogonAck {
        account_id: session.account_id,
        role: session.role,
        server_time: session.server_time,
    }) {
        return Ok(());
    }
    client.send(&ClientMessage::QueryInstruments)?;
    client.send(&ClientMessage::SubscribeNews { recent: 50 })?;
    client.send(&ClientMessage::QueryAccount)?;
    let mut subscribed = BTreeSet::new();
    let mut next_account = Instant::now() + ACCOUNT_EVERY;
    loop {
        if let Some(message) = client.recv_timeout(POLL)? {
            let symbols: Vec<Symbol> = match &message {
                ServerMessage::InstrumentList(list) => list.iter().map(|i| i.symbol).collect(),
                ServerMessage::Instrument(info) => vec![info.symbol],
                _ => vec![],
            };
            for symbol in symbols {
                if subscribed.insert(symbol) {
                    client.send(&ClientMessage::Subscribe { symbol })?;
                }
            }
            if !forward(message) {
                return Ok(());
            }
        }
        loop {
            match inbox.try_recv() {
                Ok(request) => {
                    let id = client.next_order_id();
                    client.send(&request.message(id))?;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if Instant::now() >= next_account {
            next_account = Instant::now() + ACCOUNT_EVERY;
            client.send(&ClientMessage::QueryAccount)?;
        }
    }
}
