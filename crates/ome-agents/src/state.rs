use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderId, Qty, Side, Timestamp};
use ome_core::order_book::Level;
use ome_protocol::{
    AlgoReport, AlgoStatus, ConditionalReport, ConditionalStatus, DayBar, InstrumentInfo, InstrumentStatus,
    MarketPhase, ServerMessage,
};
use std::collections::{BTreeMap, HashMap, VecDeque};

const PRICE_HISTORY: usize = 600;
const PENDING_TTL: Timestamp = 3_000;
pub const SESSION_MINUTES: u64 = 375;
const BAR_HISTORY: usize = SESSION_MINUTES as usize * 6;
const UNSCHEDULED_OPEN_MS: Timestamp = SESSION_MINUTES * 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    pub minute: u64,
    pub close: Price,
    pub volume: Qty,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wake {
    pub news: bool,
    pub fill: bool,
    pub phase: bool,
    pub day: bool,
    pub moved: Vec<Symbol>,
}

impl Wake {
    pub fn any(&self) -> bool {
        self.news || self.fill || self.phase || self.day || !self.moved.is_empty()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Activity {
    pub fills: u64,
    pub traded: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Market {
    pub trading: bool,
    pub paused: bool,
    pub reference: Price,
    pub last: Option<Price>,
    pub bid: Option<Price>,
    pub ask: Option<Price>,
    pub prices: VecDeque<Price>,
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
    pub bars: VecDeque<Bar>,
    pub days: Vec<DayBar>,
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

    pub fn sample(&mut self, minute: u64, price: Price, qty: Qty) {
        match self.bars.back_mut() {
            Some(bar) if bar.minute >= minute => {
                bar.close = price;
                bar.volume += qty;
            }
            _ => {
                if self.bars.len() == BAR_HISTORY {
                    self.bars.pop_front();
                }
                self.bars.push_back(Bar { minute, close: price, volume: qty });
            }
        }
    }

    fn window(&self, clock: u64, minutes: u64) -> impl Iterator<Item = &Bar> {
        let from = clock.saturating_sub(minutes);
        self.bars.iter().filter(move |b| b.minute >= from)
    }

    pub fn price_ago(&self, clock: u64, minutes: u64) -> Option<f64> {
        let at = clock.checked_sub(minutes)?;
        self.bars.iter().rev().find(|b| b.minute <= at).map(|b| b.close as f64)
    }

    pub fn change_over(&self, clock: u64, minutes: u64) -> Option<f64> {
        Some((self.fair()? as f64 / self.price_ago(clock, minutes)? - 1.0) * 10_000.0)
    }

    pub fn minute_vol_bps(&self, clock: u64, minutes: u64) -> Option<f64> {
        let bars: Vec<&Bar> = self.window(clock, minutes).collect();
        if bars.len() < 5 {
            return None;
        }
        let span = (bars[bars.len() - 1].minute - bars[0].minute).max(1) as f64;
        let squares: f64 = bars.windows(2).map(|w| ((w[1].close as f64 / w[0].close as f64 - 1.0) * 10_000.0).powi(2)).sum();
        Some((squares / span).sqrt())
    }

    pub fn z_over(&self, clock: u64, minutes: u64) -> Option<f64> {
        let closes: Vec<f64> = self.window(clock, minutes).map(|b| b.close as f64).collect();
        if closes.len() < 5 {
            return None;
        }
        let (mean, sd) = mean_sd(&closes);
        (sd > 0.0).then(|| (self.fair().map_or(closes[closes.len() - 1], |p| p as f64) - mean) / sd)
    }

    pub fn mean_over(&self, clock: u64, minutes: u64) -> Option<f64> {
        let closes: Vec<f64> = self.window(clock, minutes).map(|b| b.close as f64).collect();
        (closes.len() >= 5).then(|| mean_sd(&closes).0)
    }

    pub fn prior_high(&self, clock: u64, minutes: u64) -> Option<Price> {
        let bars: Vec<&Bar> = self.window(clock, minutes).collect();
        if bars.len() < 5 {
            return None;
        }
        bars[..bars.len() - 1].iter().map(|b| b.close).max()
    }

    pub fn volume_over(&self, clock: u64, minutes: u64) -> Qty {
        self.window(clock, minutes).map(|b| b.volume).sum()
    }

    pub fn day_change(&self, days: usize) -> Option<f64> {
        let then = self.days.len().checked_sub(days).and_then(|i| self.days.get(i))?;
        Some((self.fair()? as f64 / then.close.max(1) as f64 - 1.0) * 10_000.0)
    }

    pub fn daily_vol_bps(&self) -> Option<f64> {
        let closes: Vec<f64> = self.days.iter().rev().take(21).rev().map(|d| d.close as f64).collect();
        if closes.len() < 4 {
            return None;
        }
        let moves: Vec<f64> = closes.windows(2).map(|w| (w[1] / w[0] - 1.0) * 10_000.0).collect();
        Some((moves.iter().map(|m| m * m).sum::<f64>() / moves.len() as f64).sqrt())
    }

    pub fn usual_volume(&self) -> Option<f64> {
        let recent: Vec<f64> = self.days.iter().rev().take(10).map(|d| d.volume as f64).collect();
        (!recent.is_empty()).then(|| recent.iter().sum::<f64>() / recent.len() as f64)
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
    pub day: u64,
    pub phase_since: Timestamp,
    pub next_change_at: Timestamp,
    pub open_ms: Timestamp,
    pub cycle_ms: Option<Timestamp>,
    day_started: Option<Timestamp>,
    pub wake: Wake,
    pub marks: BTreeMap<Symbol, Price>,
    pub traded: u64,
    pub today: Activity,
    pub yesterday: Activity,
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
    pub world: crate::world::WorldView,
    pub news: crate::news::NewsDesk,
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            now: 0,
            phase: MarketPhase::Open,
            day: 0,
            phase_since: 0,
            next_change_at: 0,
            open_ms: UNSCHEDULED_OPEN_MS,
            cycle_ms: None,
            day_started: None,
            wake: Wake::default(),
            marks: BTreeMap::new(),
            traded: 0,
            today: Activity::default(),
            yesterday: Activity::default(),
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
            world: crate::world::WorldView::default(),
            news: crate::news::NewsDesk::default(),
        }
    }
}

impl AgentState {
    pub fn sentiment(&self, symbol: Symbol) -> f64 {
        (self.world.sentiment(symbol) + self.news.mood(symbol, self.now)).clamp(-1.0, 1.0)
    }

    pub fn market(&self, symbol: Symbol) -> Option<&Market> {
        self.markets.get(&symbol)
    }

    pub fn continuous(&self) -> bool {
        self.phase == MarketPhase::Open
    }

    pub fn trading(&self, symbol: Symbol) -> bool {
        self.continuous() && self.market(symbol).is_some_and(|m| m.trading)
    }

    pub fn scheduled(&self) -> bool {
        self.next_change_at > self.phase_since
    }

    pub fn minute_ms(&self) -> Timestamp {
        (self.open_ms / SESSION_MINUTES).max(1)
    }

    pub fn day_ms(&self) -> Timestamp {
        self.cycle_ms.unwrap_or(self.open_ms).max(1)
    }

    pub fn clock_at(&self, time: Timestamp) -> u64 {
        let minute = match self.phase {
            MarketPhase::PreOpen => 0,
            MarketPhase::Open => {
                let elapsed = time.saturating_sub(self.phase_since) / self.minute_ms();
                if self.scheduled() { elapsed.min(SESSION_MINUTES - 1) } else { elapsed }
            }
            MarketPhase::ClosingAuction | MarketPhase::Closed => SESSION_MINUTES,
        };
        self.day * SESSION_MINUTES + minute
    }

    pub fn clock(&self) -> u64 {
        self.clock_at(self.now)
    }

    pub fn minute_of_day(&self) -> u64 {
        self.clock() - self.day * SESSION_MINUTES
    }

    pub fn minutes_to_close(&self) -> Option<u64> {
        (self.continuous() && self.next_change_at > self.now).then(|| (self.next_change_at - self.now) / self.minute_ms())
    }

    pub fn closing_soon(&self) -> bool {
        self.minutes_to_close().is_some_and(|left| left <= 15)
    }

    pub fn change_bps(&self, symbol: Symbol, minutes: u64) -> Option<f64> {
        self.market(symbol)?.change_over(self.clock(), minutes)
    }

    pub fn minute_vol(&self, symbol: Symbol, minutes: u64) -> Option<f64> {
        self.market(symbol)?.minute_vol_bps(self.clock(), minutes)
    }

    pub fn daily_vol(&self, symbol: Symbol) -> Option<f64> {
        let market = self.market(symbol)?;
        market
            .daily_vol_bps()
            .or_else(|| Some(market.minute_vol_bps(self.clock(), SESSION_MINUTES)? * (SESSION_MINUTES as f64).sqrt()))
    }

    pub fn note_moves(&mut self, alert_bps: f64) {
        for (symbol, market) in &self.markets {
            let Some(price) = market.mid().or(market.last) else { continue };
            let mark = *self.marks.entry(*symbol).or_insert(price);
            if (price as f64 / mark.max(1) as f64 - 1.0).abs() * 10_000.0 >= alert_bps && !self.wake.moved.contains(symbol) {
                self.wake.moved.push(*symbol);
            }
        }
    }

    pub fn looked(&mut self) {
        for symbol in std::mem::take(&mut self.wake.moved) {
            if let Some(price) = self.markets.get(&symbol).and_then(|m| m.mid().or(m.last)) {
                self.marks.insert(symbol, price);
            }
        }
        self.wake = Wake::default();
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
                if status.phase != self.phase {
                    self.wake.phase = true;
                }
                if status.day != self.day {
                    self.wake.day = true;
                    self.yesterday = std::mem::take(&mut self.today);
                    if let Some(started) = self.day_started.filter(|s| status.since > *s && status.day == self.day + 1) {
                        self.cycle_ms = Some(status.since - started);
                    }
                    self.day_started = Some(status.since);
                }
                self.phase = status.phase;
                self.day = status.day;
                self.phase_since = status.since;
                self.next_change_at = status.next_change_at;
                if status.phase == MarketPhase::Open && status.next_change_at > status.since {
                    self.open_ms = status.next_change_at - status.since;
                }
                self.news.pace(self.minute_ms(), self.day_ms());
            }
            ServerMessage::Book(book) => {
                let market = self.markets.entry(book.symbol).or_default();
                market.bid = book.bids.first().map(|l| l.price);
                market.ask = book.asks.first().map(|l| l.price);
                market.bids = book.bids.clone();
                market.asks = book.asks.clone();
                self.now = self.now.max(book.timestamp);
            }
            ServerMessage::Trade(trade) => {
                let minute = self.clock_at(trade.timestamp);
                let market = self.markets.entry(trade.symbol).or_default();
                market.record(trade.price);
                market.sample(minute, trade.price, trade.qty);
                self.now = self.now.max(trade.timestamp);
            }
            ServerMessage::DayHistory(history) => {
                self.markets.entry(history.symbol).or_default().days = history.days.clone();
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
                    let value = report.last_qty * report.last_price;
                    self.fills += 1;
                    self.traded += value;
                    self.today.fills += 1;
                    self.today.traded += value;
                    self.wake.fill = true;
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
            ServerMessage::News(item) => {
                if self.news.absorb(item) {
                    self.wake.news = true;
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
        market.paused = info.status == InstrumentStatus::Paused;
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
                    paused: false,
                    reference: *price,
                    last: Some(*price),
                    bid: Some(price - 5),
                    ask: Some(price + 5),
                    prices: VecDeque::from(vec![*price; 50]),
                    bids: vec![Level { price: price - 5, qty: 1_000_000, orders: 1 }],
                    asks: vec![Level { price: price + 5, qty: 1_000_000, orders: 1 }],
                    bars: VecDeque::new(),
                    days: Vec::new(),
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

    pub fn bars(state: &mut AgentState, symbol: Symbol, closes: &[Price]) {
        let clock = state.clock();
        let market = state.markets.get_mut(&symbol).unwrap();
        market.bars = closes
            .iter()
            .enumerate()
            .map(|(i, close)| Bar { minute: clock + 1 + i as u64 - closes.len() as u64, close: *close, volume: 100 })
            .collect();
        if let Some(last) = closes.last() {
            market.last = Some(*last);
            market.bid = Some(last - 5);
            market.ask = Some(last + 5);
            market.bids = vec![Level { price: last - 5, qty: 1_000_000, orders: 1 }];
            market.asks = vec![Level { price: last + 5, qty: 1_000_000, orders: 1 }];
        }
    }

    pub fn days(state: &mut AgentState, symbol: Symbol, closes: &[Price]) {
        state.markets.get_mut(&symbol).unwrap().days = closes
            .iter()
            .enumerate()
            .map(|(i, close)| DayBar { day: i as u64 + 1, open: *close, high: *close, low: *close, close: *close, volume: 1_000, trades: 10, turnover: close * 1_000 })
            .collect();
    }

    #[test]
    fn the_trading_clock_runs_in_session_minutes_whatever_the_day_length() {
        let mut state = AgentState::default();
        let status = |phase, day, since, next| {
            ServerMessage::MarketStatus(ome_protocol::MarketStatus { phase, day, since, next_change_at: next })
        };
        state.apply(&status(MarketPhase::Open, 3, 1_000_000, 1_540_000));
        assert_eq!((state.minute_ms(), state.clock()), (1_440, 3 * SESSION_MINUTES));
        state.now = 1_270_000;
        assert_eq!(state.minute_of_day(), 187, "half way through a ten-minute day is lunchtime");
        assert_eq!(state.minutes_to_close(), Some(187));
        assert!(!state.closing_soon());
        state.now = 1_530_000;
        assert!(state.closing_soon());
        state.apply(&status(MarketPhase::Closed, 3, 1_560_000, 1_570_000));
        assert_eq!(state.clock(), 4 * SESSION_MINUTES, "the clock stops overnight");
        state.apply(&status(MarketPhase::PreOpen, 4, 1_570_000, 1_600_000));
        state.apply(&status(MarketPhase::Open, 4, 1_600_000, 2_140_000));
        assert_eq!(state.clock(), 4 * SESSION_MINUTES);
        assert!(state.wake.day && state.wake.phase);
        state.looked();
        assert!(!state.wake.any());
    }

    #[test]
    fn bars_days_and_price_alerts_measure_the_market_in_trading_time() {
        let mut state = state_with(&[(acme(), 10_000)], 0);
        let closes: Vec<Price> = (0..60).map(|i| 10_000 + i * 10).collect();
        bars(&mut state, acme(), &closes);
        let clock = state.clock();
        let market = state.market(acme()).unwrap();
        assert!((market.change_over(clock, 30).unwrap() - 291.5).abs() < 1.0);
        assert_eq!(market.change_over(clock, 500), None, "no history that far back");
        assert!(market.minute_vol_bps(clock, 30).unwrap() > 5.0);
        assert!(market.z_over(clock, 30).unwrap() > 1.0);
        assert_eq!(market.prior_high(clock, 30), Some(10_580));
        days(&mut state, acme(), &[9_000, 9_200, 9_100, 9_500, 9_900]);
        let market = state.market(acme()).unwrap();
        assert!(market.day_change(2).unwrap() > 1_000.0);
        assert!(market.daily_vol_bps().unwrap() > 200.0);
        state.note_moves(50.0);
        assert!(state.wake.moved.is_empty(), "the first look sets the mark");
        bars(&mut state, acme(), &[10_700]);
        state.note_moves(50.0);
        assert_eq!(state.wake.moved, vec![acme()]);
        state.looked();
        state.note_moves(50.0);
        assert!(state.wake.moved.is_empty());
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
