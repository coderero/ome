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
