use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderId, Qty, Side, Timestamp};
use ome_protocol::{
    AlgoReport, AlgoStatus, ConditionalReport, ConditionalStatus, InstrumentInfo, InstrumentStatus,
    MarketPhase, ServerMessage,
};
use std::collections::{BTreeMap, HashMap, VecDeque};

const PRICE_HISTORY: usize = 600;
const PENDING_TTL: Timestamp = 3_000;

#[derive(Debug, Clone, Default)]
pub struct Market {
    pub trading: bool,
    pub reference: Price,
    pub last: Option<Price>,
    pub bid: Option<Price>,
    pub ask: Option<Price>,
    pub prices: VecDeque<Price>,
}

impl Market {
    pub fn fair(&self) -> Option<Price> {
        self.last
            .or_else(|| Some((self.bid? + self.ask?) / 2))
            .or((self.reference > 0).then_some(self.reference))
    }

    pub fn mid(&self) -> Option<Price> {
        Some((self.bid? + self.ask?) / 2)
    }

    pub fn record(&mut self, price: Price) {
        if self.prices.len() == PRICE_HISTORY {
            self.prices.pop_front();
        }
        self.prices.push_back(price);
        self.last = Some(price);
    }

    pub fn recent(&self, n: usize) -> Vec<f64> {
        self.prices
            .iter()
            .rev()
            .take(n)
            .rev()
            .map(|&p| p as f64)
            .collect()
    }

    pub fn z_score(&self, window: usize) -> Option<f64> {
        let w = self.recent(window);
        if w.len() < window / 3 || w.len() < 5 {
            return None;
        }
        let (mean, sd) = mean_sd(&w);
        (sd > 0.0).then(|| (w[w.len() - 1] - mean) / sd)
    }

    pub fn change_bps(&self, lookback: usize) -> Option<f64> {
        let w = self.recent(lookback + 1);
        if w.len() < lookback / 2 + 1 || w.len() < 2 {
            return None;
        }
        Some((w[w.len() - 1] / w[0] - 1.0) * 10_000.0)
    }

    pub fn volatility_bps(&self, window: usize) -> Option<f64> {
        let w = self.recent(window + 1);
        if w.len() < 10 {
            return None;
        }
        let returns: Vec<f64> = w
            .windows(2)
            .map(|p| (p[1] / p[0] - 1.0) * 10_000.0)
            .collect();
        Some(mean_sd(&returns).1)
    }

    pub fn high(&self, window: usize) -> Option<Price> {
        self.prices.iter().rev().take(window).copied().max()
    }
}

pub fn mean_sd(values: &[f64]) -> (f64, f64) {
    let n = values.len().max(1) as f64;
    let mean = values.iter().sum::<f64>() / n;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    (mean, var.sqrt())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Holding {
    pub available: Qty,
    pub reserved: Qty,
}

impl Holding {
    pub fn total(&self) -> Qty {
        self.available + self.reserved
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenOrder {
    pub symbol: Symbol,
    pub side: Side,
    pub price: Price,
    pub leaves: Qty,
    pub placed_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct AgentState {
    pub now: Timestamp,
    pub phase: MarketPhase,
    pub next_change_at: Timestamp,
    pub mmp_tripped: bool,
    pub cash: u64,
    pub cash_reserved: u64,
    pub day_pnl: i64,
    pub markets: BTreeMap<Symbol, Market>,
    pub holdings: BTreeMap<Symbol, Holding>,
    pub open_orders: BTreeMap<OrderId, OpenOrder>,
    pub pending: HashMap<u64, Timestamp>,
    pub armed: BTreeMap<u64, ConditionalReport>,
    pub algos: BTreeMap<u64, AlgoReport>,
    pub fills: u64,
    pub rejects: u64,
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            now: 0,
            phase: MarketPhase::Open,
            next_change_at: 0,
            mmp_tripped: false,
            cash: 0,
            cash_reserved: 0,
            day_pnl: 0,
            markets: BTreeMap::new(),
            holdings: BTreeMap::new(),
            open_orders: BTreeMap::new(),
            pending: HashMap::new(),
            armed: BTreeMap::new(),
            algos: BTreeMap::new(),
            fills: 0,
            rejects: 0,
        }
    }
}

impl AgentState {
    pub fn market(&self, symbol: Symbol) -> Option<&Market> {
        self.markets.get(&symbol)
    }

    pub fn continuous(&self) -> bool {
        self.phase == MarketPhase::Open
    }

    pub fn trading(&self, symbol: Symbol) -> bool {
        self.continuous() && self.market(symbol).is_some_and(|m| m.trading)
    }

    pub fn closing_soon(&self) -> bool {
        self.continuous() && self.next_change_at > 0 && self.now + 8_000 >= self.next_change_at
    }

    pub fn holding(&self, symbol: Symbol) -> Holding {
        self.holdings.get(&symbol).copied().unwrap_or_default()
    }

    pub fn open_in(&self, symbol: Symbol) -> usize {
        self.open_orders
            .values()
            .filter(|o| o.symbol == symbol)
            .count()
    }

    pub fn armed_in(&self, symbol: Symbol) -> usize {
        self.armed.values().filter(|c| c.symbol == symbol).count()
    }

    pub fn algo_in(&self, symbol: Symbol) -> bool {
        self.algos.values().any(|a| a.symbol == symbol)
    }

    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn equity(&self) -> u64 {
        let stock: u64 = self
            .holdings
            .iter()
            .filter_map(|(s, h)| Some(h.total() * self.market(*s)?.fair()?))
            .sum();
        self.cash + self.cash_reserved + stock
    }

    pub fn expect(&mut self, client_order_id: u64) {
        self.pending.insert(client_order_id, self.now);
    }

    fn settle(&mut self, client_order_id: u64) {
        self.pending.remove(&client_order_id);
    }

    pub fn apply(&mut self, message: &ServerMessage) {
        match message {
            ServerMessage::LogonAck { server_time, .. }
            | ServerMessage::Heartbeat { server_time } => {
                self.now = self.now.max(*server_time);
            }
            ServerMessage::InstrumentList(list) => {
                for info in list {
                    self.apply_instrument(info);
                }
            }
            ServerMessage::Instrument(info) => self.apply_instrument(info),
            ServerMessage::MarketStatus(status) => {
                self.phase = status.phase;
                self.next_change_at = status.next_change_at;
            }
            ServerMessage::Book(book) => {
                let market = self.markets.entry(book.symbol).or_default();
                market.bid = book.bids.first().map(|l| l.price);
                market.ask = book.asks.first().map(|l| l.price);
                self.now = self.now.max(book.timestamp);
            }
            ServerMessage::Trade(trade) => {
                self.markets
                    .entry(trade.symbol)
                    .or_default()
                    .record(trade.price);
                self.now = self.now.max(trade.timestamp);
            }
            ServerMessage::Account(account) => {
                self.cash = account.cash.available;
                self.cash_reserved = account.cash.reserved;
                self.day_pnl = account.day_pnl;
                self.holdings = account
                    .positions
                    .iter()
                    .map(|p| {
                        (
                            p.symbol,
                            Holding {
                                available: p.balance.available,
                                reserved: p.balance.reserved,
                            },
                        )
                    })
                    .collect();
            }
            ServerMessage::Execution(report) => {
                self.settle(report.client_order_id);
                if report.reason.contains("market maker protection") {
                    self.mmp_tripped = true;
                }
                if report.last_qty > 0 {
                    self.fills += 1;
                }
                if report.status == ome_protocol::OrderStatus::Rejected {
                    self.rejects += 1;
                }
                if report.status.is_open() && report.order_id != 0 {
                    let placed_at = self
                        .open_orders
                        .get(&report.order_id)
                        .map_or(report.timestamp.max(self.now), |o| o.placed_at);
                    self.open_orders.insert(
                        report.order_id,
                        OpenOrder {
                            symbol: report.symbol,
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
            ServerMessage::ConditionalUpdate(c) => {
                self.settle(c.client_order_id);
                if c.status == ConditionalStatus::Armed {
                    self.armed.insert(c.cond_id, c.clone());
                } else {
                    self.armed.remove(&c.cond_id);
                }
            }
            ServerMessage::AlgoUpdate(a) => {
                self.settle(a.client_order_id);
                if a.status == AlgoStatus::Working {
                    self.algos.insert(a.algo_id, a.clone());
                } else {
                    self.algos.remove(&a.algo_id);
                }
            }
            ServerMessage::Reject { .. } => self.rejects += 1,
            _ => {}
        }
        let now = self.now;
        self.pending
            .retain(|_, at| now.saturating_sub(*at) < PENDING_TTL);
    }

    fn apply_instrument(&mut self, info: &InstrumentInfo) {
        let market = self.markets.entry(info.symbol).or_default();
        market.trading = info.status == InstrumentStatus::Trading;
        market.reference = info.reference_price;
        if info.last_price.is_some() && market.last.is_none() {
            market.last = info.last_price;
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use ome_protocol::{ExecutionReport, OrderStatus, Trigger};

    pub fn acme() -> Symbol {
        Symbol::parse("ACME").unwrap()
    }

    pub fn nova() -> Symbol {
        Symbol::parse("NOVA").unwrap()
    }

    pub fn state_with(prices: &[(Symbol, Price)], cash: u64) -> AgentState {
        let mut state = AgentState {
            cash,
            now: 1_000_000,
            ..AgentState::default()
        };
        for (symbol, price) in prices {
            state.markets.insert(
                *symbol,
                Market {
                    trading: true,
                    reference: *price,
                    last: Some(*price),
                    bid: Some(price - 5),
                    ask: Some(price + 5),
                    prices: VecDeque::from(vec![*price; 50]),
                },
            );
        }
        state
    }

    pub fn hold(state: &mut AgentState, symbol: Symbol, qty: Qty) {
        state.holdings.insert(
            symbol,
            Holding {
                available: qty,
                reserved: 0,
            },
        );
    }

    fn report(order_id: OrderId, status: OrderStatus, leaves: Qty) -> ServerMessage {
        ServerMessage::Execution(ExecutionReport {
            seq: 0,
            order_id,
            client_order_id: 9,
            account: 1,
            symbol: acme(),
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
    fn tracks_orders_conditionals_and_pending_requests() {
        let mut state = state_with(&[(acme(), 100)], 0);
        state.expect(9);
        state.apply(&report(1, OrderStatus::New, 10));
        assert!(!state.busy());
        assert_eq!(state.open_in(acme()), 1);
        state.apply(&report(1, OrderStatus::Filled, 0));
        assert_eq!(state.open_in(acme()), 0);

        let mut cond = ConditionalReport {
            cond_id: 4,
            client_order_id: 11,
            account: 1,
            symbol: acme(),
            side: Side::Sell,
            qty: 5,
            status: ConditionalStatus::Armed,
            trigger: Trigger::Below(90),
            level: 90,
            reason: String::new(),
        };
        state.apply(&ServerMessage::ConditionalUpdate(cond.clone()));
        assert_eq!(state.armed_in(acme()), 1);
        cond.status = ConditionalStatus::Triggered;
        state.apply(&ServerMessage::ConditionalUpdate(cond));
        assert_eq!(state.armed_in(acme()), 0);

        state.expect(77);
        state.now += 5_000;
        state.apply(&ServerMessage::Heartbeat { server_time: 0 });
        assert!(!state.busy(), "lost requests expire");
    }

    #[test]
    fn market_statistics_come_from_recent_trades() {
        let mut m = Market::default();
        for p in [100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110] {
            m.record(p);
        }
        assert!(m.z_score(10).unwrap() > 1.0);
        assert!((m.change_bps(10).unwrap() - 1_000.0).abs() < 1.0);
        assert!(m.volatility_bps(10).unwrap() > 0.0);
        assert_eq!(m.high(5), Some(110));
    }
}
