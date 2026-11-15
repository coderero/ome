use crate::codec::{DecodeError, Decoder, Encoder};
use ome_core::candles::Candle;
use ome_core::order_book::Level;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{
    AccountId, Balance, OrderId, OrderKind, OrderOptions, Qty, Side, StpMode, TimeInForce,
    Timestamp,
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketPhase {
    Closed = 1,
    PreOpen = 2,
    Open = 3,
    ClosingAuction = 4,
}

impl MarketPhase {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => MarketPhase::Closed,
            2 => MarketPhase::PreOpen,
            3 => MarketPhase::Open,
            4 => MarketPhase::ClosingAuction,
            _ => return None,
        })
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.to_ascii_lowercase().as_str() {
            "close" | "closed" => MarketPhase::Closed,
            "preopen" | "pre-open" => MarketPhase::PreOpen,
            "open" => MarketPhase::Open,
            "auction" | "closing" | "closing-auction" => MarketPhase::ClosingAuction,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            MarketPhase::Closed => "closed",
            MarketPhase::PreOpen => "pre-open",
            MarketPhase::Open => "open",
            MarketPhase::ClosingAuction => "closing auction",
        }
    }

    pub fn is_call(self) -> bool {
        matches!(self, MarketPhase::PreOpen | MarketPhase::ClosingAuction)
    }
}

pub fn phase_from(v: u8) -> Result<MarketPhase, DecodeError> {
    MarketPhase::from_u8(v).ok_or(DecodeError::Invalid("market phase"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Above(Price),
    Below(Price),
    TrailingAbove(Price),
    TrailingBelow(Price),
    MarketOpens,
}

impl Trigger {
    pub fn describe(&self) -> String {
        use ome_core::price::format_price;
        match self {
            Trigger::Above(p) => format!("last >= {}", format_price(*p)),
            Trigger::Below(p) => format!("last <= {}", format_price(*p)),
            Trigger::TrailingAbove(d) => format!("trailing {} above the low", format_price(*d)),
            Trigger::TrailingBelow(d) => format!("trailing {} below the high", format_price(*d)),
            Trigger::MarketOpens => "market opens".to_string(),
        }
    }
}

pub fn put_trigger(e: &mut Encoder, trigger: &Trigger) {
    let (code, value) = match trigger {
        Trigger::Above(p) => (1, *p),
        Trigger::Below(p) => (2, *p),
        Trigger::TrailingAbove(d) => (3, *d),
        Trigger::TrailingBelow(d) => (4, *d),
        Trigger::MarketOpens => (5, 0),
    };
    e.u8(code).u64(value);
}

pub fn get_trigger(d: &mut Decoder) -> Result<Trigger, DecodeError> {
    let code = d.u8()?;
    let value = d.u64()?;
    Ok(match code {
        1 => Trigger::Above(value),
        2 => Trigger::Below(value),
        3 => Trigger::TrailingAbove(value),
        4 => Trigger::TrailingBelow(value),
        5 => Trigger::MarketOpens,
        _ => return Err(DecodeError::Invalid("trigger")),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlgoStrategy {
    Twap { duration_ms: u64, slices: u64 },
    Vwap { duration_ms: u64 },
    Pov { rate_bps: u64, duration_ms: u64 },
    Iceberg { clip: u64 },
}

impl AlgoStrategy {
    pub fn describe(&self) -> String {
        match self {
            AlgoStrategy::Twap {
                duration_ms,
                slices,
            } => {
                format!("TWAP {slices} slices over {}s", duration_ms / 1_000)
            }
            AlgoStrategy::Vwap { duration_ms } => format!("VWAP over {}s", duration_ms / 1_000),
            AlgoStrategy::Pov {
                rate_bps,
                duration_ms,
            } => format!(
                "POV {}.{:02}% for up to {}s",
                rate_bps / 100,
                rate_bps % 100,
                duration_ms / 1_000
            ),
            AlgoStrategy::Iceberg { clip } => format!("iceberg clips of {clip}"),
        }
    }
}

pub fn put_algo_strategy(e: &mut Encoder, strategy: &AlgoStrategy) {
    let (code, a, b) = match *strategy {
        AlgoStrategy::Twap {
            duration_ms,
            slices,
        } => (1, duration_ms, slices),
        AlgoStrategy::Vwap { duration_ms } => (2, duration_ms, 0),
        AlgoStrategy::Pov {
            rate_bps,
            duration_ms,
        } => (3, rate_bps, duration_ms),
        AlgoStrategy::Iceberg { clip } => (4, clip, 0),
    };
    e.u8(code).u64(a).u64(b);
}

pub fn get_algo_strategy(d: &mut Decoder) -> Result<AlgoStrategy, DecodeError> {
    let code = d.u8()?;
    let a = d.u64()?;
    let b = d.u64()?;
    Ok(match code {
        1 => AlgoStrategy::Twap {
            duration_ms: a,
            slices: b,
        },
        2 => AlgoStrategy::Vwap { duration_ms: a },
        3 => AlgoStrategy::Pov {
            rate_bps: a,
            duration_ms: b,
        },
        4 => AlgoStrategy::Iceberg { clip: a },
        _ => return Err(DecodeError::Invalid("algo strategy")),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlgoStatus {
    Working = 1,
    Done = 2,
    Cancelled = 3,
    Expired = 4,
    Rejected = 5,
}

impl AlgoStatus {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => AlgoStatus::Working,
            2 => AlgoStatus::Done,
            3 => AlgoStatus::Cancelled,
            4 => AlgoStatus::Expired,
            5 => AlgoStatus::Rejected,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            AlgoStatus::Working => "working",
            AlgoStatus::Done => "done",
            AlgoStatus::Cancelled => "cancelled",
            AlgoStatus::Expired => "expired",
            AlgoStatus::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlgoReport {
    pub algo_id: u64,
    pub client_order_id: u64,
    pub account: AccountId,
    pub symbol: Symbol,
    pub side: Side,
    pub qty: Qty,
    pub filled: Qty,
    pub avg_price: Price,
    pub children: u64,
    pub status: AlgoStatus,
    pub strategy: AlgoStrategy,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionalStatus {
    Armed = 1,
    Triggered = 2,
    Cancelled = 3,
    Rejected = 4,
}

impl ConditionalStatus {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => ConditionalStatus::Armed,
            2 => ConditionalStatus::Triggered,
            3 => ConditionalStatus::Cancelled,
            4 => ConditionalStatus::Rejected,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            ConditionalStatus::Armed => "armed",
            ConditionalStatus::Triggered => "triggered",
            ConditionalStatus::Cancelled => "cancelled",
            ConditionalStatus::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionalReport {
    pub cond_id: u64,
    pub client_order_id: u64,
    pub account: AccountId,
    pub symbol: Symbol,
    pub side: Side,
    pub qty: Qty,
    pub status: ConditionalStatus,
    pub trigger: Trigger,
    pub level: Price,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuoteSide {
    pub price: Price,
    pub qty: Qty,
}

impl QuoteSide {
    pub fn is_empty(&self) -> bool {
        self.qty == 0
    }
}

pub fn put_quote_side(e: &mut Encoder, side: &QuoteSide) {
    e.u64(side.price).u64(side.qty);
}

pub fn get_quote_side(d: &mut Decoder) -> Result<QuoteSide, DecodeError> {
    Ok(QuoteSide {
        price: d.u64()?,
        qty: d.u64()?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketStatus {
    pub phase: MarketPhase,
    pub day: u64,
    pub since: Timestamp,
    pub next_change_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuctionInfo {
    pub symbol: Symbol,
    pub price: Price,
    pub volume: Qty,
    pub imbalance: i64,
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
    pub routed_orders: u64,
    pub internal_crosses: u64,
    pub internalized_shares: u64,
    pub wholesaler_orders: u64,
    pub wholesaler_shares: u64,
    pub pfof_earned: i64,
    pub dark_shares: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketStats {
    pub symbol: Symbol,
    pub open: Price,
    pub high: Price,
    pub low: Price,
    pub last: Price,
    pub vwap: Price,
    pub trades: u64,
    pub turnover: u64,
    pub exchange_volume: Qty,
    pub internal_volume: Qty,
    pub wholesaler_volume: Qty,
    pub ats_volume: Qty,
}

impl MarketStats {
    pub fn volume(&self) -> Qty {
        self.exchange_volume + self.internal_volume + self.wholesaler_volume + self.ats_volume
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObligationInfo {
    pub symbol: Symbol,
    pub member_id: MemberId,
    pub samples: u64,
    pub compliant: u64,
    pub quoting: bool,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueMemberInfo {
    pub member_id: MemberId,
    pub orders: u64,
    pub shares: u64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueStatus {
    pub venue: Venue,
    pub fills: u64,
    pub declined: u64,
    pub shares: u64,
    pub pfof_paid: i64,
    pub cash: i64,
    pub resting: u64,
    pub inventory: Vec<(Symbol, i64)>,
    pub members: Vec<VenueMemberInfo>,
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
    pub avg_cost: Price,
    pub day_cost: Price,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChargesEstimate {
    pub symbol: Symbol,
    pub side: Side,
    pub qty: Qty,
    pub price: Price,
    pub cash_effect: i64,
    pub charges: Charges,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSnapshot {
    pub account_id: AccountId,
    pub role: Role,
    pub cash: Balance,
    pub unsettled_cash: i64,
    pub day_pnl: i64,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Venue {
    #[default]
    Exchange = 0,
    Internal = 1,
    Wholesaler = 2,
    Ats = 3,
}

impl Venue {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Venue::Exchange,
            1 => Venue::Internal,
            2 => Venue::Wholesaler,
            3 => Venue::Ats,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Venue::Exchange => "exchange",
            Venue::Internal => "internal",
            Venue::Wholesaler => "wholesaler",
            Venue::Ats => "dark pool",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradeEvent {
    pub symbol: Symbol,
    pub trade_id: u64,
    pub price: Price,
    pub qty: Qty,
    pub aggressor: Side,
    pub timestamp: Timestamp,
    pub venue: Venue,
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
    pub suspended: bool,
    pub loss_limit: u64,
    pub day_pnl: i64,
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
    pub suspended: bool,
    pub cancel_on_disconnect: bool,
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

pub fn put_order_options(e: &mut Encoder, options: &OrderOptions) {
    let (code, expire_at) = match options.tif {
        TimeInForce::Gtc => (1, 0),
        TimeInForce::Ioc => (2, 0),
        TimeInForce::Day => (3, 0),
        TimeInForce::Fok => (4, 0),
        TimeInForce::Gtd(at) => (5, at),
        TimeInForce::Auction => (6, 0),
    };
    let stp = match options.stp {
        StpMode::CancelResting => 1,
        StpMode::CancelIncoming => 2,
        StpMode::CancelBoth => 3,
    };
    e.u8(code)
        .u64(expire_at)
        .u8(options.post_only as u8)
        .u64(options.display_qty.unwrap_or(0))
        .u8(stp);
}

pub fn get_order_options(d: &mut Decoder) -> Result<OrderOptions, DecodeError> {
    let code = d.u8()?;
    let expire_at = d.u64()?;
    let tif = match code {
        1 => TimeInForce::Gtc,
        2 => TimeInForce::Ioc,
        3 => TimeInForce::Day,
        4 => TimeInForce::Fok,
        5 => TimeInForce::Gtd(expire_at),
        6 => TimeInForce::Auction,
        _ => return Err(DecodeError::Invalid("time in force")),
    };
    let post_only = d.u8()? != 0;
    let display_qty = Some(d.u64()?).filter(|&q| q > 0);
    let stp = match d.u8()? {
        1 => StpMode::CancelResting,
        2 => StpMode::CancelIncoming,
        3 => StpMode::CancelBoth,
        _ => return Err(DecodeError::Invalid("self-trade prevention mode")),
    };
    Ok(OrderOptions {
        tif,
        post_only,
        display_qty,
        stp,
    })
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
