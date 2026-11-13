use crate::codec::{DecodeError, Decoder, Encoder};
use ome_core::candles::Candle;
use ome_core::order_book::Level;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, Balance, OrderId, OrderKind, Qty, Side, TimeInForce, Timestamp};

pub type MemberId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    BrokerAdmin = 1,
    Retail = 2,
    Quant = 3,
    Hft = 4,
    Prop = 5,
    LpDesk = 6,
    Issuer = 7,
}

impl Role {
    pub const ALL: [Role; 7] = [
        Role::BrokerAdmin,
        Role::Retail,
        Role::Quant,
        Role::Hft,
        Role::Prop,
        Role::LpDesk,
        Role::Issuer,
    ];

    pub fn from_u8(v: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|r| *r as u8 == v)
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.to_ascii_lowercase().as_str() {
            "admin" | "broker-admin" => Role::BrokerAdmin,
            "retail" | "user" => Role::Retail,
            "quant" | "institutional" => Role::Quant,
            "hft" => Role::Hft,
            "prop" => Role::Prop,
            "lp" | "lp-desk" => Role::LpDesk,
            "issuer" => Role::Issuer,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::BrokerAdmin => "admin",
            Role::Retail => "retail",
            Role::Quant => "quant",
            Role::Hft => "hft",
            Role::Prop => "prop",
            Role::LpDesk => "lp",
            Role::Issuer => "issuer",
        }
    }

    pub fn can_trade(self) -> bool {
        self != Role::BrokerAdmin
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MemberKind {
    Admin = 1,
    Broker = 2,
    LiquidityProvider = 3,
    Clearing = 4,
}

impl MemberKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(MemberKind::Admin),
            2 => Some(MemberKind::Broker),
            3 => Some(MemberKind::LiquidityProvider),
            4 => Some(MemberKind::Clearing),
            _ => None,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "admin" => Some(MemberKind::Admin),
            "broker" => Some(MemberKind::Broker),
            "lp" | "liquidity-provider" => Some(MemberKind::LiquidityProvider),
            "clearing" => Some(MemberKind::Clearing),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MemberKind::Admin => "admin",
            MemberKind::Broker => "broker",
            MemberKind::LiquidityProvider => "liquidity-provider",
            MemberKind::Clearing => "clearing",
        }
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
    pub seq: u64,
    pub order_id: OrderId,
    pub client_order_id: u64,
    pub account: AccountId,
    pub symbol: Symbol,
    pub side: Side,
    pub status: OrderStatus,
    pub price: Price,
    pub qty: Qty,
    pub filled: Qty,
    pub leaves: Qty,
    pub last_price: Price,
    pub last_qty: Qty,
    pub fees: i64,
    pub cycle: u64,
    pub timestamp: Timestamp,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Charges {
    pub commission: u64,
    pub exchange_fee: i64,
    pub transaction_tax: u64,
    pub stamp_duty: u64,
    pub regulatory_fee: u64,
    pub gst: u64,
}

impl Charges {
    pub fn total(&self) -> i64 {
        (self.commission + self.transaction_tax + self.stamp_duty + self.regulatory_fee + self.gst)
            as i64
            + self.exchange_fee
    }

    pub fn add(&mut self, other: &Charges) {
        self.commission += other.commission;
        self.exchange_fee += other.exchange_fee;
        self.transaction_tax += other.transaction_tax;
        self.stamp_duty += other.stamp_duty;
        self.regulatory_fee += other.regulatory_fee;
        self.gst += other.gst;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractNote {
    pub account: AccountId,
    pub order_id: OrderId,
    pub symbol: Symbol,
    pub side: Side,
    pub qty: Qty,
    pub price: Price,
    pub charges: Charges,
    pub net_amount: i64,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FeeSummary {
    pub charges: Charges,
    pub fills: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocation {
    pub seq: u64,
    pub allocation_id: u64,
    pub member: MemberId,
    pub symbol: Symbol,
    pub account: AccountId,
    pub shares: Qty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearedTrade {
    pub seq: u64,
    pub trade_id: u64,
    pub cycle: u64,
    pub symbol: Symbol,
    pub price: Price,
    pub qty: Qty,
    pub buy_member: MemberId,
    pub buy_account: AccountId,
    pub buy_fee: i64,
    pub sell_member: MemberId,
    pub sell_account: AccountId,
    pub sell_fee: i64,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledPosition {
    pub symbol: Symbol,
    pub delivered: i64,
    pub holding: i64,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementReport {
    pub seq: u64,
    pub cycle: u64,
    pub member: MemberId,
    pub cash: i64,
    pub collateral: i64,
    pub margin_required: u64,
    pub positions: Vec<SettledPosition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearingMemberInfo {
    pub member_id: MemberId,
    pub collateral: i64,
    pub margin_required: u64,
    pub unsettled_gross: u64,
    pub credit_limit: u64,
    pub fails: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearingStatus {
    pub cycle: u64,
    pub settled_cycle: u64,
    pub fee_pool: i64,
    pub members: Vec<ClearingMemberInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    pub symbol: Symbol,
    pub balance: Balance,
    pub unsettled: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSnapshot {
    pub account_id: AccountId,
    pub role: Role,
    pub cash: Balance,
    pub unsettled_cash: i64,
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
    pub band_bps: u64,
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
pub struct MemberInfo {
    pub member_id: MemberId,
    pub kind: MemberKind,
    pub connected: bool,
    pub open_orders: u32,
    pub net_fees: i64,
    pub credit_limit: u64,
    pub exposure: u64,
    pub name: String,
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

pub fn put_order_kind(e: &mut Encoder, kind: OrderKind) {
    match kind {
        OrderKind::Limit(price) => e.u8(1).u64(price),
        OrderKind::Market => e.u8(2).u64(0),
    };
}

pub fn get_order_kind(d: &mut Decoder) -> Result<OrderKind, DecodeError> {
    let ord_type = d.u8()?;
    let price = d.u64()?;
    match ord_type {
        1 => Ok(OrderKind::Limit(price)),
        2 => Ok(OrderKind::Market),
        _ => Err(DecodeError::Invalid("order type")),
    }
}

pub fn tif_to_u8(tif: TimeInForce) -> u8 {
    match tif {
        TimeInForce::Gtc => 1,
        TimeInForce::Ioc => 2,
    }
}

pub fn tif_from_u8(v: u8) -> Result<TimeInForce, DecodeError> {
    match v {
        1 => Ok(TimeInForce::Gtc),
        2 => Ok(TimeInForce::Ioc),
        _ => Err(DecodeError::Invalid("time in force")),
    }
}

pub fn role_from(v: u8) -> Result<Role, DecodeError> {
    Role::from_u8(v).ok_or(DecodeError::Invalid("role"))
}

pub fn member_kind_from(v: u8) -> Result<MemberKind, DecodeError> {
    MemberKind::from_u8(v).ok_or(DecodeError::Invalid("member kind"))
}

pub fn asset_symbol(asset: Asset) -> Option<Symbol> {
    match asset {
        Asset::Cash => None,
        Asset::Shares(symbol) => Some(symbol),
    }
}

pub fn put_charges(e: &mut Encoder, c: &Charges) {
    e.u64(c.commission)
        .i64(c.exchange_fee)
        .u64(c.transaction_tax)
        .u64(c.stamp_duty)
        .u64(c.regulatory_fee)
        .u64(c.gst);
}

pub fn get_charges(d: &mut Decoder) -> Result<Charges, DecodeError> {
    Ok(Charges {
        commission: d.u64()?,
        exchange_fee: d.i64()?,
        transaction_tax: d.u64()?,
        stamp_duty: d.u64()?,
        regulatory_fee: d.u64()?,
        gst: d.u64()?,
    })
}

pub fn put_balance(e: &mut Encoder, b: &Balance) {
    e.u64(b.available).u64(b.reserved);
}

pub fn get_balance(d: &mut Decoder) -> Result<Balance, DecodeError> {
    Ok(Balance {
        available: d.u64()?,
        reserved: d.u64()?,
    })
}
