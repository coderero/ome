use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderId, Qty, Side, Timestamp};
use ome_protocol::{InstrumentInfo, InstrumentStatus, MarketPhase, ServerMessage};
use std::collections::{BTreeMap, HashSet, VecDeque};

const PRICE_HISTORY: usize = 600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenOrder {
    pub side: Side,
    pub price: Price,
    pub leaves: Qty,
    pub placed_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct BotState {
    pub symbol: Symbol,
    pub now: Timestamp,
    pub trading: bool,
    pub instrument_trading: bool,
    pub market_open: bool,
    pub reference_price: Price,
    pub last_price: Option<Price>,
    pub best_bid: Option<Price>,
    pub best_ask: Option<Price>,
    pub prices: VecDeque<Price>,
    pub cash: u64,
    pub cash_reserved: u64,
    pub shares: Qty,
    pub shares_reserved: Qty,
    pub open_orders: BTreeMap<OrderId, OpenOrder>,
    pub pending: HashSet<u64>,
    pub fills: u64,
}

impl BotState {
    pub fn new(symbol: Symbol) -> Self {
        Self {
            symbol,
            now: 0,
            trading: false,
            instrument_trading: false,
            market_open: true,
            reference_price: 0,
            last_price: None,
            best_bid: None,
            best_ask: None,
            prices: VecDeque::with_capacity(PRICE_HISTORY),
            cash: 0,
            cash_reserved: 0,
            shares: 0,
            shares_reserved: 0,
            open_orders: BTreeMap::new(),
            pending: HashSet::new(),
            fills: 0,
        }
    }

    pub fn fair_price(&self) -> Option<Price> {
        self.last_price
            .or_else(|| Some((self.best_bid? + self.best_ask?) / 2))
            .or((self.reference_price > 0).then_some(self.reference_price))
    }

    pub fn total_shares(&self) -> Qty {
        self.shares + self.shares_reserved
    }

    pub fn apply(&mut self, message: &ServerMessage) {
        match message {
            ServerMessage::LogonAck { server_time, .. }
            | ServerMessage::Heartbeat { server_time } => {
                self.now = self.now.max(*server_time);
            }
            ServerMessage::InstrumentList(list) => {
                if let Some(info) = list.iter().find(|i| i.symbol == self.symbol) {
                    self.apply_instrument(info);
                }
            }
            ServerMessage::Instrument(info) if info.symbol == self.symbol => {
                self.apply_instrument(info)
            }
            ServerMessage::MarketStatus(status) => {
                self.market_open = status.phase != MarketPhase::Closed;
                self.trading = self.instrument_trading && self.market_open;
            }
            ServerMessage::Book(book) if book.symbol == self.symbol => {
                self.best_bid = book.bids.first().map(|l| l.price);
                self.best_ask = book.asks.first().map(|l| l.price);
                self.now = self.now.max(book.timestamp);
            }
            ServerMessage::Trade(trade) if trade.symbol == self.symbol => {
                if self.prices.len() == PRICE_HISTORY {
                    self.prices.pop_front();
                }
                self.prices.push_back(trade.price);
                self.last_price = Some(trade.price);
                self.now = self.now.max(trade.timestamp);
            }
            ServerMessage::Account(account) => {
                self.cash = account.cash.available;
                self.cash_reserved = account.cash.reserved;
                let position = account
                    .positions
                    .iter()
                    .find(|p| p.symbol == self.symbol)
                    .map(|p| p.balance)
                    .unwrap_or_default();
                self.shares = position.available;
                self.shares_reserved = position.reserved;
            }
            ServerMessage::Execution(report) if report.symbol == self.symbol => {
                self.pending.remove(&report.client_order_id);
                if report.last_qty > 0 {
                    self.fills += 1;
                }
                if report.status.is_open() && report.order_id != 0 {
                    let placed_at = self
                        .open_orders
                        .get(&report.order_id)
                        .map_or(report.timestamp, |o| o.placed_at);
                    self.open_orders.insert(
                        report.order_id,
                        OpenOrder {
                            side: report.side,
                            price: report.price,
                            leaves: report.leaves,
                            placed_at,
                        },
                    );
                } else {
                    self.open_orders.remove(&report.order_id);
                }
            }
            _ => {}
        }
    }

    fn apply_instrument(&mut self, info: &InstrumentInfo) {
        self.instrument_trading = info.status == InstrumentStatus::Trading;
        self.trading = self.instrument_trading && self.market_open;
        self.reference_price = info.reference_price;
        if info.last_price.is_some() {
            self.last_price = info.last_price;
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use ome_protocol::{ExecutionReport, OrderStatus};

    pub fn state_at(price: Price, cash: u64, shares: Qty) -> BotState {
        let mut state = BotState::new(Symbol::parse("ACME").unwrap());
        state.trading = true;
        state.reference_price = price;
        state.last_price = Some(price);
        state.best_bid = Some(price - 5);
        state.best_ask = Some(price + 5);
        state.cash = cash;
        state.shares = shares;
        state
    }

    fn report(order_id: OrderId, status: OrderStatus, leaves: Qty) -> ServerMessage {
        ServerMessage::Execution(ExecutionReport {
            seq: 0,
            order_id,
            client_order_id: 9,
            account: 1,
            symbol: Symbol::parse("ACME").unwrap(),
            side: Side::Buy,
            status,
            price: 100,
            qty: 10,
            filled: 10 - leaves,
            leaves,
            last_price: 0,
            last_qty: 0,
            fees: 0,
            cycle: 0,
            timestamp: 5,
            reason: String::new(),
        })
    }

    #[test]
    fn tracks_open_orders_from_execution_reports() {
        let mut state = state_at(100, 0, 0);
        state.pending.insert(9);

        state.apply(&report(1, OrderStatus::New, 10));
        assert!(state.pending.is_empty());
        assert_eq!(state.open_orders[&1].leaves, 10);

        state.apply(&report(1, OrderStatus::PartiallyFilled, 4));
        assert_eq!(state.open_orders[&1].leaves, 4);

        state.apply(&report(1, OrderStatus::Filled, 0));
        assert!(state.open_orders.is_empty());
    }

    #[test]
    fn fair_price_falls_back_from_last_to_mid_to_reference() {
        let mut state = state_at(100, 0, 0);
        assert_eq!(state.fair_price(), Some(100));
        state.last_price = None;
        assert_eq!(state.fair_price(), Some(100));
        state.best_bid = None;
        state.reference_price = 90;
        assert_eq!(state.fair_price(), Some(90));
    }
}
