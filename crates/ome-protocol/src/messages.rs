use crate::codec::{DecodeError, Decoder, Encoder};
use ome_core::candles::Candle;
use ome_core::order_book::Level;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, Balance, OrderId, OrderKind, Qty, Side, Timestamp};

pub mod template {
    pub const LOGON: u16 = 1;
    pub const CLIENT_HEARTBEAT: u16 = 2;
    pub const NEW_ORDER: u16 = 10;
    pub const CANCEL: u16 = 11;
    pub const CANCEL_ALL: u16 = 12;
    pub const SUBSCRIBE: u16 = 20;
    pub const UNSUBSCRIBE: u16 = 21;
    pub const QUERY_ACCOUNT: u16 = 22;
    pub const QUERY_INSTRUMENTS: u16 = 23;
    pub const CREATE_ACCOUNT: u16 = 30;
    pub const SET_BALANCE: u16 = 31;
    pub const LIST_INSTRUMENT: u16 = 32;
    pub const HALT: u16 = 33;
    pub const RESUME: u16 = 34;
    pub const QUERY_ACCOUNTS: u16 = 35;

    pub const LOGON_ACK: u16 = 100;
    pub const SERVER_HEARTBEAT: u16 = 101;
    pub const REJECT: u16 = 102;
    pub const ACK: u16 = 103;
    pub const EXECUTION_REPORT: u16 = 110;
    pub const ACCOUNT_SNAPSHOT: u16 = 111;
    pub const INSTRUMENT_LIST: u16 = 120;
    pub const INSTRUMENT: u16 = 121;
    pub const BOOK_SNAPSHOT: u16 = 122;
    pub const TRADE: u16 = 123;
    pub const CANDLE_SNAPSHOT: u16 = 124;
    pub const ACCOUNT_LIST: u16 = 130;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Admin = 1,
    Retail = 2,
    Institutional = 3,
    Bot = 4,
}

impl Role {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Role::Admin,
            2 => Role::Retail,
            3 => Role::Institutional,
            4 => Role::Bot,
            _ => return None,
        })
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.to_ascii_lowercase().as_str() {
            "admin" => Role::Admin,
            "retail" | "user" => Role::Retail,
            "institutional" | "inst" => Role::Institutional,
            "bot" => Role::Bot,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Retail => "retail",
            Role::Institutional => "institutional",
            Role::Bot => "bot",
        }
    }

    pub fn can_trade(self) -> bool {
        self != Role::Admin
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    New = 1,
    PartiallyFilled = 2,
    Filled = 3,
    Cancelled = 4,
    Rejected = 5,
    Expired = 6,
}

impl OrderStatus {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => OrderStatus::New,
            2 => OrderStatus::PartiallyFilled,
            3 => OrderStatus::Filled,
            4 => OrderStatus::Cancelled,
            5 => OrderStatus::Rejected,
            6 => OrderStatus::Expired,
            _ => return None,
        })
    }

    pub fn is_open(self) -> bool {
        matches!(self, OrderStatus::New | OrderStatus::PartiallyFilled)
    }

    pub fn label(self) -> &'static str {
        match self {
            OrderStatus::New => "new",
            OrderStatus::PartiallyFilled => "partial",
            OrderStatus::Filled => "filled",
            OrderStatus::Cancelled => "cancelled",
            OrderStatus::Rejected => "rejected",
            OrderStatus::Expired => "expired",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstrumentStatus {
    Trading = 1,
    Halted = 2,
}

impl InstrumentStatus {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(InstrumentStatus::Trading),
            2 => Some(InstrumentStatus::Halted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Asset {
    Cash,
    Shares(Symbol),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionReport {
    pub order_id: OrderId,
    pub client_order_id: u64,
    pub symbol: Symbol,
    pub side: Side,
    pub status: OrderStatus,
    pub price: Price,
    pub qty: Qty,
    pub filled: Qty,
    pub leaves: Qty,
    pub last_price: Price,
    pub last_qty: Qty,
    pub timestamp: Timestamp,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    pub symbol: Symbol,
    pub balance: Balance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSnapshot {
    pub account_id: AccountId,
    pub role: Role,
    pub cash: Balance,
    pub positions: Vec<Position>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentInfo {
    pub symbol: Symbol,
    pub status: InstrumentStatus,
    pub reference_price: Price,
    pub last_price: Option<Price>,
    pub low_limit: Price,
    pub high_limit: Price,
    pub volume: u64,
    pub name: String,
    pub halt_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookSnapshot {
    pub symbol: Symbol,
    pub timestamp: Timestamp,
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradeEvent {
    pub symbol: Symbol,
    pub trade_id: u64,
    pub price: Price,
    pub qty: Qty,
    pub aggressor: Side,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandleSnapshot {
    pub symbol: Symbol,
    pub interval: u64,
    pub candles: Vec<Candle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    pub account_id: AccountId,
    pub role: Role,
    pub cash: Balance,
    pub open_orders: u32,
    pub username: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    Logon {
        username: String,
        secret: String,
    },
    Heartbeat,
    NewOrder {
        client_order_id: u64,
        symbol: Symbol,
        side: Side,
        kind: OrderKind,
        qty: Qty,
    },
    Cancel {
        order_id: OrderId,
    },
    CancelAll {
        symbol: Option<Symbol>,
    },
    Subscribe {
        symbol: Symbol,
    },
    Unsubscribe {
        symbol: Symbol,
    },
    QueryAccount,
    QueryInstruments,
    CreateAccount {
        role: Role,
        cash: u64,
        username: String,
        secret: String,
    },
    SetBalance {
        account_id: AccountId,
        asset: Asset,
        amount: u64,
    },
    ListInstrument {
        symbol: Symbol,
        ipo_price: Price,
        shares: Qty,
        issuer: AccountId,
        name: String,
    },
    Halt {
        symbol: Symbol,
        reason: String,
    },
    Resume {
        symbol: Symbol,
    },
    QueryAccounts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMessage {
    LogonAck {
        account_id: AccountId,
        role: Role,
        server_time: Timestamp,
    },
    Heartbeat {
        server_time: Timestamp,
    },
    Reject {
        ref_template: u16,
        reason: String,
    },
    Ack {
        ref_template: u16,
        text: String,
    },
    Execution(ExecutionReport),
    Account(AccountSnapshot),
    InstrumentList(Vec<InstrumentInfo>),
    Instrument(InstrumentInfo),
    Book(BookSnapshot),
    Trade(TradeEvent),
    Candles(CandleSnapshot),
    AccountList(Vec<AccountInfo>),
}

pub fn side_to_u8(side: Side) -> u8 {
    match side {
        Side::Buy => 1,
        Side::Sell => 2,
    }
}

pub fn side_from_u8(v: u8) -> Result<Side, DecodeError> {
    match v {
        1 => Ok(Side::Buy),
        2 => Ok(Side::Sell),
        _ => Err(DecodeError::Invalid("side")),
    }
}

fn role_from(v: u8) -> Result<Role, DecodeError> {
    Role::from_u8(v).ok_or(DecodeError::Invalid("role"))
}

fn put_balance(e: &mut Encoder, b: &Balance) {
    e.u64(b.available).u64(b.reserved);
}

fn get_balance(d: &mut Decoder) -> Result<Balance, DecodeError> {
    Ok(Balance {
        available: d.u64()?,
        reserved: d.u64()?,
    })
}

impl ClientMessage {
    pub fn template_id(&self) -> u16 {
        use template::*;
        match self {
            ClientMessage::Logon { .. } => LOGON,
            ClientMessage::Heartbeat => CLIENT_HEARTBEAT,
            ClientMessage::NewOrder { .. } => NEW_ORDER,
            ClientMessage::Cancel { .. } => CANCEL,
            ClientMessage::CancelAll { .. } => CANCEL_ALL,
            ClientMessage::Subscribe { .. } => SUBSCRIBE,
            ClientMessage::Unsubscribe { .. } => UNSUBSCRIBE,
            ClientMessage::QueryAccount => QUERY_ACCOUNT,
            ClientMessage::QueryInstruments => QUERY_INSTRUMENTS,
            ClientMessage::CreateAccount { .. } => CREATE_ACCOUNT,
            ClientMessage::SetBalance { .. } => SET_BALANCE,
            ClientMessage::ListInstrument { .. } => LIST_INSTRUMENT,
            ClientMessage::Halt { .. } => HALT,
            ClientMessage::Resume { .. } => RESUME,
            ClientMessage::QueryAccounts => QUERY_ACCOUNTS,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new(self.template_id());
        match self {
            ClientMessage::Logon { username, secret } => {
                e.end_block().var(username).var(secret);
            }
            ClientMessage::Heartbeat
            | ClientMessage::QueryAccount
            | ClientMessage::QueryInstruments
            | ClientMessage::QueryAccounts => {
                e.end_block();
            }
            ClientMessage::NewOrder {
                client_order_id,
                symbol,
                side,
                kind,
                qty,
            } => {
                let (ord_type, price) = match kind {
                    OrderKind::Limit(price) => (1, *price),
                    OrderKind::Market => (2, 0),
                };
                e.u64(*client_order_id)
                    .symbol(Some(*symbol))
                    .u8(side_to_u8(*side))
                    .u8(ord_type)
                    .u64(price)
                    .u64(*qty)
                    .end_block();
            }
            ClientMessage::Cancel { order_id } => {
                e.u64(*order_id).end_block();
            }
            ClientMessage::CancelAll { symbol } => {
                e.symbol(*symbol).end_block();
            }
            ClientMessage::Subscribe { symbol }
            | ClientMessage::Unsubscribe { symbol }
            | ClientMessage::Resume { symbol } => {
                e.symbol(Some(*symbol)).end_block();
            }
            ClientMessage::CreateAccount {
                role,
                cash,
                username,
                secret,
            } => {
                e.u8(*role as u8)
                    .u64(*cash)
                    .end_block()
                    .var(username)
                    .var(secret);
            }
            ClientMessage::SetBalance {
                account_id,
                asset,
                amount,
            } => {
                let symbol = match asset {
                    Asset::Cash => None,
                    Asset::Shares(symbol) => Some(*symbol),
                };
                e.u64(*account_id).symbol(symbol).u64(*amount).end_block();
            }
            ClientMessage::ListInstrument {
                symbol,
                ipo_price,
                shares,
                issuer,
                name,
            } => {
                e.symbol(Some(*symbol))
                    .u64(*ipo_price)
                    .u64(*shares)
                    .u64(*issuer)
                    .end_block()
                    .var(name);
            }
            ClientMessage::Halt { symbol, reason } => {
                e.symbol(Some(*symbol)).end_block().var(reason);
            }
        }
        e.finish()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        use template::*;
        let mut d = Decoder::new(bytes)?;
        let message = match d.template_id {
            LOGON => {
                d.end_block()?;
                ClientMessage::Logon {
                    username: d.var()?,
                    secret: d.var()?,
                }
            }
            CLIENT_HEARTBEAT => {
                d.end_block()?;
                ClientMessage::Heartbeat
            }
            QUERY_ACCOUNT => {
                d.end_block()?;
                ClientMessage::QueryAccount
            }
            QUERY_INSTRUMENTS => {
                d.end_block()?;
                ClientMessage::QueryInstruments
            }
            QUERY_ACCOUNTS => {
                d.end_block()?;
                ClientMessage::QueryAccounts
            }
            NEW_ORDER => {
                let client_order_id = d.u64()?;
                let symbol = d.symbol()?;
                let side = side_from_u8(d.u8()?)?;
                let ord_type = d.u8()?;
                let price = d.u64()?;
                let qty = d.u64()?;
                d.end_block()?;
                let kind = match ord_type {
                    1 => OrderKind::Limit(price),
                    2 => OrderKind::Market,
                    _ => return Err(DecodeError::Invalid("order type")),
                };
                ClientMessage::NewOrder {
                    client_order_id,
                    symbol,
                    side,
                    kind,
                    qty,
                }
            }
            CANCEL => {
                let order_id = d.u64()?;
                d.end_block()?;
                ClientMessage::Cancel { order_id }
            }
            CANCEL_ALL => {
                let symbol = d.optional_symbol()?;
                d.end_block()?;
                ClientMessage::CancelAll { symbol }
            }
            SUBSCRIBE => {
                let symbol = d.symbol()?;
                d.end_block()?;
                ClientMessage::Subscribe { symbol }
            }
            UNSUBSCRIBE => {
                let symbol = d.symbol()?;
                d.end_block()?;
                ClientMessage::Unsubscribe { symbol }
            }
            RESUME => {
                let symbol = d.symbol()?;
                d.end_block()?;
                ClientMessage::Resume { symbol }
            }
            CREATE_ACCOUNT => {
                let role = role_from(d.u8()?)?;
                let cash = d.u64()?;
                d.end_block()?;
                ClientMessage::CreateAccount {
                    role,
                    cash,
                    username: d.var()?,
                    secret: d.var()?,
                }
            }
            SET_BALANCE => {
                let account_id = d.u64()?;
                let asset = match d.optional_symbol()? {
                    None => Asset::Cash,
                    Some(symbol) => Asset::Shares(symbol),
                };
                let amount = d.u64()?;
                d.end_block()?;
                ClientMessage::SetBalance {
                    account_id,
                    asset,
                    amount,
                }
            }
            LIST_INSTRUMENT => {
                let symbol = d.symbol()?;
                let ipo_price = d.u64()?;
                let shares = d.u64()?;
                let issuer = d.u64()?;
                d.end_block()?;
                ClientMessage::ListInstrument {
                    symbol,
                    ipo_price,
                    shares,
                    issuer,
                    name: d.var()?,
                }
            }
            HALT => {
                let symbol = d.symbol()?;
                d.end_block()?;
                ClientMessage::Halt {
                    symbol,
                    reason: d.var()?,
                }
            }
            other => return Err(DecodeError::UnknownTemplate(other)),
        };
        Ok(message)
    }
}

impl ServerMessage {
    pub fn template_id(&self) -> u16 {
        use template::*;
        match self {
            ServerMessage::LogonAck { .. } => LOGON_ACK,
            ServerMessage::Heartbeat { .. } => SERVER_HEARTBEAT,
            ServerMessage::Reject { .. } => REJECT,
            ServerMessage::Ack { .. } => ACK,
            ServerMessage::Execution(_) => EXECUTION_REPORT,
            ServerMessage::Account(_) => ACCOUNT_SNAPSHOT,
            ServerMessage::InstrumentList(_) => INSTRUMENT_LIST,
            ServerMessage::Instrument(_) => INSTRUMENT,
            ServerMessage::Book(_) => BOOK_SNAPSHOT,
            ServerMessage::Trade(_) => TRADE,
            ServerMessage::Candles(_) => CANDLE_SNAPSHOT,
            ServerMessage::AccountList(_) => ACCOUNT_LIST,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new(self.template_id());
        match self {
            ServerMessage::LogonAck {
                account_id,
                role,
                server_time,
            } => {
                e.u64(*account_id)
                    .u8(*role as u8)
                    .u64(*server_time)
                    .end_block();
            }
            ServerMessage::Heartbeat { server_time } => {
                e.u64(*server_time).end_block();
            }
            ServerMessage::Reject {
                ref_template,
                reason,
            } => {
                e.u16(*ref_template).end_block().var(reason);
            }
            ServerMessage::Ack { ref_template, text } => {
                e.u16(*ref_template).end_block().var(text);
            }
            ServerMessage::Execution(r) => {
                e.u64(r.order_id)
                    .u64(r.client_order_id)
                    .symbol(Some(r.symbol))
                    .u8(side_to_u8(r.side))
                    .u8(r.status as u8)
                    .u64(r.price)
                    .u64(r.qty)
                    .u64(r.filled)
                    .u64(r.leaves)
                    .u64(r.last_price)
                    .u64(r.last_qty)
                    .u64(r.timestamp)
                    .end_block()
                    .var(&r.reason);
            }
            ServerMessage::Account(a) => {
                e.u64(a.account_id).u8(a.role as u8);
                put_balance(&mut e, &a.cash);
                e.end_block();
                let mut g = e.begin_group(a.positions.len());
                for p in &a.positions {
                    e.begin_entry(&mut g);
                    e.symbol(Some(p.symbol));
                    put_balance(&mut e, &p.balance);
                    e.end_entry_block(&mut g);
                }
            }
            ServerMessage::InstrumentList(list) => {
                e.end_block();
                let mut g = e.begin_group(list.len());
                for info in list {
                    e.begin_entry(&mut g);
                    put_instrument_block(&mut e, info);
                    e.end_entry_block(&mut g);
                    e.var(&info.name).var(&info.halt_reason);
                }
            }
            ServerMessage::Instrument(info) => {
                put_instrument_block(&mut e, info);
                e.end_block().var(&info.name).var(&info.halt_reason);
            }
            ServerMessage::Book(b) => {
                e.symbol(Some(b.symbol)).u64(b.timestamp).end_block();
                for levels in [&b.bids, &b.asks] {
                    let mut g = e.begin_group(levels.len());
                    for l in levels {
                        e.begin_entry(&mut g);
                        e.u64(l.price).u64(l.qty).u32(l.orders);
                        e.end_entry_block(&mut g);
                    }
                }
            }
            ServerMessage::Trade(t) => {
                e.symbol(Some(t.symbol))
                    .u64(t.trade_id)
                    .u64(t.price)
                    .u64(t.qty)
                    .u8(side_to_u8(t.aggressor))
                    .u64(t.timestamp)
                    .end_block();
            }
            ServerMessage::Candles(c) => {
                e.symbol(Some(c.symbol)).u64(c.interval).end_block();
                let mut g = e.begin_group(c.candles.len());
                for k in &c.candles {
                    e.begin_entry(&mut g);
                    e.u64(k.start)
                        .u64(k.open)
                        .u64(k.high)
                        .u64(k.low)
                        .u64(k.close)
                        .u64(k.volume)
                        .u64(k.trades);
                    e.end_entry_block(&mut g);
                }
            }
            ServerMessage::AccountList(list) => {
                e.end_block();
                let mut g = e.begin_group(list.len());
                for a in list {
                    e.begin_entry(&mut g);
                    e.u64(a.account_id).u8(a.role as u8);
                    put_balance(&mut e, &a.cash);
                    e.u32(a.open_orders);
                    e.end_entry_block(&mut g);
                    e.var(&a.username);
                }
            }
        }
        e.finish()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        use template::*;
        let mut d = Decoder::new(bytes)?;
        let message = match d.template_id {
            LOGON_ACK => {
                let account_id = d.u64()?;
                let role = role_from(d.u8()?)?;
                let server_time = d.u64()?;
                d.end_block()?;
                ServerMessage::LogonAck {
                    account_id,
                    role,
                    server_time,
                }
            }
            SERVER_HEARTBEAT => {
                let server_time = d.u64()?;
                d.end_block()?;
                ServerMessage::Heartbeat { server_time }
            }
            REJECT => {
                let ref_template = d.u16()?;
                d.end_block()?;
                ServerMessage::Reject {
                    ref_template,
                    reason: d.var()?,
                }
            }
            ACK => {
                let ref_template = d.u16()?;
                d.end_block()?;
                ServerMessage::Ack {
                    ref_template,
                    text: d.var()?,
                }
            }
            EXECUTION_REPORT => {
                let order_id = d.u64()?;
                let client_order_id = d.u64()?;
                let symbol = d.symbol()?;
                let side = side_from_u8(d.u8()?)?;
                let status = OrderStatus::from_u8(d.u8()?).ok_or(DecodeError::Invalid("status"))?;
                let price = d.u64()?;
                let qty = d.u64()?;
                let filled = d.u64()?;
                let leaves = d.u64()?;
                let last_price = d.u64()?;
                let last_qty = d.u64()?;
                let timestamp = d.u64()?;
                d.end_block()?;
                ServerMessage::Execution(ExecutionReport {
                    order_id,
                    client_order_id,
                    symbol,
                    side,
                    status,
                    price,
                    qty,
                    filled,
                    leaves,
                    last_price,
                    last_qty,
                    timestamp,
                    reason: d.var()?,
                })
            }
            ACCOUNT_SNAPSHOT => {
                let account_id = d.u64()?;
                let role = role_from(d.u8()?)?;
                let cash = get_balance(&mut d)?;
                d.end_block()?;
                let g = d.group()?;
                let mut positions = Vec::with_capacity(g.count);
                for _ in 0..g.count {
                    let start = d.entry_start();
                    let symbol = d.symbol()?;
                    let balance = get_balance(&mut d)?;
                    d.end_entry_block(start, &g)?;
                    positions.push(Position { symbol, balance });
                }
                ServerMessage::Account(AccountSnapshot {
                    account_id,
                    role,
                    cash,
                    positions,
                })
            }
            INSTRUMENT_LIST => {
                d.end_block()?;
                let g = d.group()?;
                let mut list = Vec::with_capacity(g.count);
                for _ in 0..g.count {
                    let start = d.entry_start();
                    let mut info = get_instrument_block(&mut d)?;
                    d.end_entry_block(start, &g)?;
                    info.name = d.var()?;
                    info.halt_reason = d.var()?;
                    list.push(info);
                }
                ServerMessage::InstrumentList(list)
            }
            INSTRUMENT => {
                let mut info = get_instrument_block(&mut d)?;
                d.end_block()?;
                info.name = d.var()?;
                info.halt_reason = d.var()?;
                ServerMessage::Instrument(info)
            }
            BOOK_SNAPSHOT => {
                let symbol = d.symbol()?;
                let timestamp = d.u64()?;
                d.end_block()?;
                let mut sides = [Vec::new(), Vec::new()];
                for levels in &mut sides {
                    let g = d.group()?;
                    for _ in 0..g.count {
                        let start = d.entry_start();
                        let level = Level {
                            price: d.u64()?,
                            qty: d.u64()?,
                            orders: d.u32()?,
                        };
                        d.end_entry_block(start, &g)?;
                        levels.push(level);
                    }
                }
                let [bids, asks] = sides;
                ServerMessage::Book(BookSnapshot {
                    symbol,
                    timestamp,
                    bids,
                    asks,
                })
            }
            TRADE => {
                let symbol = d.symbol()?;
                let trade_id = d.u64()?;
                let price = d.u64()?;
                let qty = d.u64()?;
                let aggressor = side_from_u8(d.u8()?)?;
                let timestamp = d.u64()?;
                d.end_block()?;
                ServerMessage::Trade(TradeEvent {
                    symbol,
                    trade_id,
                    price,
                    qty,
                    aggressor,
                    timestamp,
                })
            }
            CANDLE_SNAPSHOT => {
                let symbol = d.symbol()?;
                let interval = d.u64()?;
                d.end_block()?;
                let g = d.group()?;
                let mut candles = Vec::with_capacity(g.count);
                for _ in 0..g.count {
                    let start = d.entry_start();
                    let candle = Candle {
                        start: d.u64()?,
                        open: d.u64()?,
                        high: d.u64()?,
                        low: d.u64()?,
                        close: d.u64()?,
                        volume: d.u64()?,
                        trades: d.u64()?,
                    };
                    d.end_entry_block(start, &g)?;
                    candles.push(candle);
                }
                ServerMessage::Candles(CandleSnapshot {
                    symbol,
                    interval,
                    candles,
                })
            }
            ACCOUNT_LIST => {
                d.end_block()?;
                let g = d.group()?;
                let mut list = Vec::with_capacity(g.count);
                for _ in 0..g.count {
                    let start = d.entry_start();
                    let account_id = d.u64()?;
                    let role = role_from(d.u8()?)?;
                    let cash = get_balance(&mut d)?;
                    let open_orders = d.u32()?;
                    d.end_entry_block(start, &g)?;
                    list.push(AccountInfo {
                        account_id,
                        role,
                        cash,
                        open_orders,
                        username: d.var()?,
                    });
                }
                ServerMessage::AccountList(list)
            }
            other => return Err(DecodeError::UnknownTemplate(other)),
        };
        Ok(message)
    }
}

fn put_instrument_block(e: &mut Encoder, info: &InstrumentInfo) {
    e.symbol(Some(info.symbol))
        .u8(info.status as u8)
        .u64(info.reference_price)
        .u64(info.last_price.unwrap_or(0))
        .u64(info.low_limit)
        .u64(info.high_limit)
        .u64(info.volume);
}

fn get_instrument_block(d: &mut Decoder) -> Result<InstrumentInfo, DecodeError> {
    let symbol = d.symbol()?;
    let status =
        InstrumentStatus::from_u8(d.u8()?).ok_or(DecodeError::Invalid("instrument status"))?;
    let reference_price = d.u64()?;
    let last_price = Some(d.u64()?).filter(|&p| p > 0);
    Ok(InstrumentInfo {
        symbol,
        status,
        reference_price,
        last_price,
        low_limit: d.u64()?,
        high_limit: d.u64()?,
        volume: d.u64()?,
        name: String::new(),
        halt_reason: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(s: &str) -> Symbol {
        Symbol::parse(s).unwrap()
    }

    fn all_client_messages() -> Vec<ClientMessage> {
        vec![
            ClientMessage::Logon {
                username: "alice".into(),
                secret: "pw".into(),
            },
            ClientMessage::Heartbeat,
            ClientMessage::NewOrder {
                client_order_id: 9,
                symbol: sym("ACME"),
                side: Side::Buy,
                kind: OrderKind::Limit(2_500),
                qty: 10,
            },
            ClientMessage::NewOrder {
                client_order_id: 10,
                symbol: sym("ACME"),
                side: Side::Sell,
                kind: OrderKind::Market,
                qty: 3,
            },
            ClientMessage::Cancel { order_id: 77 },
            ClientMessage::CancelAll { symbol: None },
            ClientMessage::CancelAll {
                symbol: Some(sym("NOVA")),
            },
            ClientMessage::Subscribe {
                symbol: sym("ACME"),
            },
            ClientMessage::Unsubscribe {
                symbol: sym("ACME"),
            },
            ClientMessage::QueryAccount,
            ClientMessage::QueryInstruments,
            ClientMessage::CreateAccount {
                role: Role::Institutional,
                cash: 1_000_000,
                username: "fund".into(),
                secret: "s3cret".into(),
            },
            ClientMessage::SetBalance {
                account_id: 4,
                asset: Asset::Cash,
                amount: 500,
            },
            ClientMessage::SetBalance {
                account_id: 4,
                asset: Asset::Shares(sym("ACME")),
                amount: 20,
            },
            ClientMessage::ListInstrument {
                symbol: sym("ZEN"),
                ipo_price: 1_000,
                shares: 50_000,
                issuer: 3,
                name: "Zen Robotics".into(),
            },
            ClientMessage::Halt {
                symbol: sym("ZEN"),
                reason: "news pending".into(),
            },
            ClientMessage::Resume { symbol: sym("ZEN") },
            ClientMessage::QueryAccounts,
        ]
    }

    fn all_server_messages() -> Vec<ServerMessage> {
        let info = InstrumentInfo {
            symbol: sym("ACME"),
            status: InstrumentStatus::Halted,
            reference_price: 2_500,
            last_price: Some(3_300),
            low_limit: 1_750,
            high_limit: 3_250,
            volume: 1_234,
            name: "Acme Corp".into(),
            halt_reason: "circuit breaker".into(),
        };
        vec![
            ServerMessage::LogonAck {
                account_id: 1,
                role: Role::Admin,
                server_time: 99,
            },
            ServerMessage::Heartbeat { server_time: 5 },
            ServerMessage::Reject {
                ref_template: template::HALT,
                reason: "no".into(),
            },
            ServerMessage::Ack {
                ref_template: template::CREATE_ACCOUNT,
                text: "ok".into(),
            },
            ServerMessage::Execution(ExecutionReport {
                order_id: 1,
                client_order_id: 2,
                symbol: sym("ACME"),
                side: Side::Sell,
                status: OrderStatus::PartiallyFilled,
                price: 2_500,
                qty: 10,
                filled: 4,
                leaves: 6,
                last_price: 2_510,
                last_qty: 4,
                timestamp: 1_700_000_000_000,
                reason: String::new(),
            }),
            ServerMessage::Account(AccountSnapshot {
                account_id: 3,
                role: Role::Bot,
                cash: Balance {
                    available: 10,
                    reserved: 5,
                },
                positions: vec![
                    Position {
                        symbol: sym("ACME"),
                        balance: Balance {
                            available: 1,
                            reserved: 2,
                        },
                    },
                    Position {
                        symbol: sym("NOVA"),
                        balance: Balance::default(),
                    },
                ],
            }),
            ServerMessage::InstrumentList(vec![
                info.clone(),
                InstrumentInfo {
                    last_price: None,
                    name: "Other".into(),
                    halt_reason: String::new(),
                    ..info.clone()
                },
            ]),
            ServerMessage::InstrumentList(vec![]),
            ServerMessage::Instrument(info),
            ServerMessage::Book(BookSnapshot {
                symbol: sym("ACME"),
                timestamp: 7,
                bids: vec![Level {
                    price: 2_490,
                    qty: 10,
                    orders: 2,
                }],
                asks: vec![
                    Level {
                        price: 2_500,
                        qty: 3,
                        orders: 1,
                    },
                    Level {
                        price: 2_510,
                        qty: 8,
                        orders: 4,
                    },
                ],
            }),
            ServerMessage::Trade(TradeEvent {
                symbol: sym("ACME"),
                trade_id: 11,
                price: 2_505,
                qty: 2,
                aggressor: Side::Buy,
                timestamp: 8,
            }),
            ServerMessage::Candles(CandleSnapshot {
                symbol: sym("ACME"),
                interval: 1_000,
                candles: vec![Candle {
                    start: 0,
                    open: 1,
                    high: 3,
                    low: 1,
                    close: 2,
                    volume: 9,
                    trades: 4,
                }],
            }),
            ServerMessage::AccountList(vec![AccountInfo {
                account_id: 2,
                role: Role::Retail,
                cash: Balance {
                    available: 100,
                    reserved: 0,
                },
                open_orders: 3,
                username: "alice".into(),
            }]),
        ]
    }

    #[test]
    fn every_client_message_round_trips() {
        for message in all_client_messages() {
            let bytes = message.encode();
            assert_eq!(ClientMessage::decode(&bytes).as_ref(), Ok(&message));
        }
    }

    #[test]
    fn every_server_message_round_trips() {
        for message in all_server_messages() {
            let bytes = message.encode();
            assert_eq!(ServerMessage::decode(&bytes).as_ref(), Ok(&message));
        }
    }

    #[test]
    fn corrupted_messages_never_panic() {
        let messages: Vec<Vec<u8>> = all_client_messages()
            .iter()
            .map(ClientMessage::encode)
            .chain(all_server_messages().iter().map(ServerMessage::encode))
            .collect();
        for bytes in messages {
            for cut in 0..bytes.len() {
                let _ = ClientMessage::decode(&bytes[..cut]);
                let _ = ServerMessage::decode(&bytes[..cut]);
            }
            for i in 0..bytes.len() {
                let mut flipped = bytes.clone();
                flipped[i] ^= 0xFF;
                let _ = ClientMessage::decode(&flipped);
                let _ = ServerMessage::decode(&flipped);
            }
        }
    }

    #[test]
    fn new_order_has_fixed_compact_layout() {
        let bytes = all_client_messages()[2].encode();
        assert_eq!(bytes.len(), 8 + 8 + 8 + 1 + 1 + 8 + 8);
        assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), 34);
        assert_eq!(
            u16::from_le_bytes([bytes[2], bytes[3]]),
            template::NEW_ORDER
        );
    }
}
