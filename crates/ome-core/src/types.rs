use crate::price::Price;

pub type Qty = u64;
pub type OrderId = u64;
pub type AccountId = u64;
pub type Timestamp = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKind {
    Limit(Price),
    Market,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Day,
    Fok,
    Gtd(Timestamp),
    Auction,
}

impl TimeInForce {
    pub fn is_immediate(self) -> bool {
        matches!(self, TimeInForce::Ioc | TimeInForce::Fok)
    }

    pub fn label(self) -> &'static str {
        match self {
            TimeInForce::Gtc => "gtc",
            TimeInForce::Ioc => "ioc",
            TimeInForce::Day => "day",
            TimeInForce::Fok => "fok",
            TimeInForce::Gtd(_) => "gtd",
            TimeInForce::Auction => "auction",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StpMode {
    #[default]
    CancelResting,
    CancelIncoming,
    CancelBoth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderOptions {
    pub tif: TimeInForce,
    pub post_only: bool,
    pub display_qty: Option<Qty>,
    pub stp: StpMode,
}

impl Default for OrderOptions {
    fn default() -> Self {
        Self::tif(TimeInForce::Gtc)
    }
}

impl OrderOptions {
    pub fn tif(tif: TimeInForce) -> Self {
        Self {
            tif,
            post_only: false,
            display_qty: None,
            stp: StpMode::CancelResting,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderRequest {
    pub side: Side,
    pub kind: OrderKind,
    pub qty: Qty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    pub id: OrderId,
    pub account: AccountId,
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
    pub timestamp: Timestamp,
    pub display: Option<Qty>,
    pub visible: Qty,
}

impl Order {
    pub fn new(
        id: OrderId,
        account: AccountId,
        side: Side,
        price: Price,
        qty: Qty,
        timestamp: Timestamp,
    ) -> Self {
        Self {
            id,
            account,
            side,
            price,
            qty,
            timestamp,
            display: None,
            visible: qty,
        }
    }

    pub fn with_display(mut self, display: Option<Qty>) -> Self {
        self.display = display.filter(|d| *d > 0 && *d < self.qty);
        self.refill();
        self
    }

    pub fn refill(&mut self) {
        self.visible = self.display.map_or(self.qty, |d| d.min(self.qty));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trade {
    pub maker_order: OrderId,
    pub taker_order: OrderId,
    pub maker_account: AccountId,
    pub taker_account: AccountId,
    pub taker_side: Side,
    pub price: Price,
    pub qty: Qty,
    pub timestamp: Timestamp,
    pub venue: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Balance {
    pub available: u64,
    pub reserved: u64,
}

impl Balance {
    pub fn total(&self) -> u64 {
        self.available + self.reserved
    }
}
