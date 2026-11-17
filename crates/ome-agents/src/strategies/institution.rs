use super::factor::rebalance_with;
use super::{Params, Strategy, affordable, scale};
use crate::action::Action;
use crate::state::AgentState;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{Side, Timestamp};
use ome_protocol::{AlgoStrategy, MarketPhase};
use rand::rngs::StdRng;
use std::time::Duration;

pub fn cap_weights(state: &AgentState, symbols: &[Symbol]) -> Vec<(Symbol, f64)> {
    let caps: Vec<(Symbol, f64)> = symbols
        .iter()
        .filter_map(|s| {
            let price = state.market(*s)?.fair()?;
            let shares = state.news.shares(*s)?;
            Some((*s, price as f64 * shares as f64))
        })
        .collect();
    let total: f64 = caps.iter().map(|(_, c)| c).sum();
    if caps.len() < symbols.len() || total <= 0.0 {
        let each = 1.0 / symbols.len().max(1) as f64;
        return symbols.iter().map(|s| (*s, each)).collect();
    }
    caps.into_iter().map(|(s, c)| (s, c / total)).collect()
}

pub fn until_close(state: &AgentState, cap_ms: u64) -> u64 {
    if state.continuous() && state.next_change_at > state.now {
        cap_ms.min(state.next_change_at - state.now).saturating_sub(1_000).max(2_000)
    } else {
        cap_ms
    }
}

fn value_of(state: &AgentState, symbol: Symbol, lag_ms: Timestamp) -> Option<(Price, f64)> {
    let market = state.market(symbol)?;
    let price = market.fair()?;
    let value = state.news.valuations.value(symbol, state.now.saturating_sub(lag_ms), market.reference)?;
    Some((price, value))
}

fn pov(symbol: Symbol, side: Side, qty: u64, limit: Option<Price>, rate_bps: u64, duration_ms: u64) -> Action {
    Action::Algo { symbol, side, qty, limit, strategy: AlgoStrategy::Pov { rate_bps, duration_ms } }
}

pub struct IndexFund {
    symbols: Vec<Symbol>,
    pub invest_pct: f64,
    pub drift_pct: f64,
    pub limit_bps: f64,
    last_phase: MarketPhase,
}

impl IndexFund {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        Self {
            symbols,
            invest_pct: params.f64("invest_pct", 0.98),
            drift_pct: params.f64("drift_pct", 0.005),
            limit_bps: params.f64("limit_bps", 100.0),
            last_phase: MarketPhase::Closed,
        }
    }
}

impl Strategy for IndexFund {
    fn name(&self) -> &'static str {
        "index fund"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(500)
    }

    fn on_tick(&mut self, state: &AgentState, _rng: &mut StdRng) -> Vec<Action> {
        let entering = state.phase == MarketPhase::ClosingAuction && self.last_phase != MarketPhase::ClosingAuction;
        self.last_phase = state.phase;
        if !entering {
            return vec![];
        }
        let equity = state.equity() as f64;
        let mut actions = Vec::new();
        let mut cash = state.cash;
        for (symbol, weight) in cap_weights(state, &self.symbols) {
            let Some(market) = state.market(symbol) else { continue };
            let Some(price) = market.last.or(market.fair()) else { continue };
            let held = state.holding(symbol);
            let delta = equity * self.invest_pct * weight - held.total() as f64 * price as f64;
            if delta.abs() < equity * self.drift_pct {
                continue;
            }
            let qty = (delta.abs() / price as f64) as u64;
            let (side, qty, limit) = if delta > 0.0 {
                let limit = scale(price, self.limit_bps);
                (Side::Buy, affordable(cash, limit, qty), limit)
            } else {
                (Side::Sell, qty.min(held.available), scale(price, -self.limit_bps))
            };
            if qty > 0 {
                if side == Side::Buy {
                    cash = cash.saturating_sub(limit * qty);
                }
                actions.push(Action::Auction { symbol, side, qty, limit });
            }
        }
        actions
    }
}

pub struct MutualFund {
    symbols: Vec<Symbol>,
    pub cash_target: f64,
    pub premium: f64,
    pub lag_ms: Timestamp,
    pub check_ms: Timestamp,
    pub vwap_ms: u64,
    pub pov_bps: u64,
    last_check: Option<Timestamp>,
}

impl MutualFund {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        Self {
            symbols,
            cash_target: params.f64("cash_target", 0.05),
            premium: params.f64("premium", 0.15),
            lag_ms: params.u64("lag_ms", 10_000),
            check_ms: params.u64("check_ms", 10_000),
            vwap_ms: params.u64("vwap_ms", 600_000),
            pov_bps: params.u64("pov_bps", 800),
            last_check: None,
        }
    }
}

impl Strategy for MutualFund {
    fn name(&self) -> &'static str {
        "domestic mutual fund"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(2_000)
    }

    fn on_tick(&mut self, state: &AgentState, _rng: &mut StdRng) -> Vec<Action> {
        if self.last_check.is_some_and(|at| state.now < at + self.check_ms) || state.busy() || !state.continuous() {
            return vec![];
        }
        self.last_check = Some(state.now);
        let equity = state.equity() as f64;
        let duration = until_close(state, self.vwap_ms);
        let mut actions = Vec::new();
        let mut candidates = Vec::new();
        for symbol in &self.symbols {
            if state.algo_in(*symbol) || !state.trading(*symbol) {
                continue;
            }
            let Some((price, value)) = value_of(state, *symbol, self.lag_ms) else { continue };
            let upside = value / price as f64 - 1.0;
            let held = state.holding(*symbol);
            if upside < -self.premium && held.available > 0 {
                let qty = (held.available / 4).max(1);
                actions.push(pov(*symbol, Side::Sell, qty, Some((value * (1.0 + self.premium / 2.0)) as Price), self.pov_bps, duration));
            } else if upside > -0.02 {
                candidates.push((*symbol, price, value, upside + 0.05));
            }
        }
        let deployable = state.cash as f64 - equity * self.cash_target;
        let total: f64 = candidates.iter().map(|c| c.3.max(0.01)).sum();
        if deployable > equity * 0.01 && total > 0.0 {
            for (symbol, price, value, score) in candidates {
                let budget = deployable * score.max(0.01) / total;
                let limit = (value as Price).min(scale(price, 100.0));
                let qty = affordable(budget as u64, limit, (budget / limit.max(1) as f64) as u64);
                if qty > 0 {
                    actions.push(pov(symbol, Side::Buy, qty, Some(limit), self.pov_bps, duration));
                }
            }
        }
        actions
    }
}

pub struct ForeignFund {
    symbols: Vec<Symbol>,
    pub base: f64,
    pub sensitivity: f64,
    pub half_life_ms: f64,
    pub check_ms: Timestamp,
    pub drift_pct: f64,
    pub vwap_ms: u64,
    pub pov_bps: u64,
    last_check: Option<Timestamp>,
}

impl ForeignFund {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        Self {
            symbols,
            base: params.f64("base", 0.6),
            sensitivity: params.f64("sensitivity", 0.35),
            half_life_ms: params.f64("half_life_ms", 600_000.0),
            check_ms: params.u64("check_ms", 15_000),
            drift_pct: params.f64("drift_pct", 0.03),
            vwap_ms: params.u64("vwap_ms", 600_000),
            pov_bps: params.u64("pov_bps", 800),
            last_check: None,
        }
    }

    pub fn target(&self, state: &AgentState) -> f64 {
        (self.base + self.sensitivity * state.news.macro_mood(state.now, self.half_life_ms)).clamp(0.1, 0.95)
    }
}

impl Strategy for ForeignFund {
    fn name(&self) -> &'static str {
        "foreign institutional fund"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(2_000)
    }

    fn on_tick(&mut self, state: &AgentState, _rng: &mut StdRng) -> Vec<Action> {
        if self.last_check.is_some_and(|at| state.now < at + self.check_ms) || state.busy() || !state.continuous() {
            return vec![];
        }
        self.last_check = Some(state.now);
        let invested = self.target(state);
        let weights: Vec<(Symbol, f64)> = cap_weights(state, &self.symbols).into_iter().map(|(s, w)| (s, w * invested)).collect();
        let strategy = AlgoStrategy::Pov { rate_bps: self.pov_bps, duration_ms: until_close(state, self.vwap_ms) };
        rebalance_with(state, &weights, self.drift_pct, strategy)
    }

    fn detail(&self) -> String {
        format!("target invested {:.0}%", self.base * 100.0)
    }
}

pub struct SovereignFund {
    symbols: Vec<Symbol>,
    pub discount: f64,
    pub premium: f64,
    pub max_weight: f64,
    pub chunk_pct: f64,
    pub check_ms: Timestamp,
    pub vwap_ms: u64,
    pub pov_bps: u64,
    last_check: Option<Timestamp>,
}

impl SovereignFund {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        Self {
            symbols,
            discount: params.f64("discount", 0.12),
            premium: params.f64("premium", 0.5),
            max_weight: params.f64("max_weight", 0.3),
            chunk_pct: params.f64("chunk_pct", 0.02),
            check_ms: params.u64("check_ms", 20_000),
            vwap_ms: params.u64("vwap_ms", 900_000),
            pov_bps: params.u64("pov_bps", 500),
            last_check: None,
        }
    }
}

impl Strategy for SovereignFund {
    fn name(&self) -> &'static str {
        "sovereign fund"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(3_000)
    }

    fn on_tick(&mut self, state: &AgentState, _rng: &mut StdRng) -> Vec<Action> {
        if self.last_check.is_some_and(|at| state.now < at + self.check_ms) || state.busy() || !state.continuous() {
            return vec![];
        }
        self.last_check = Some(state.now);
        let equity = state.equity() as f64;
        let duration = until_close(state, self.vwap_ms);
        let mut actions = Vec::new();
        for symbol in &self.symbols {
            if state.algo_in(*symbol) || !state.trading(*symbol) {
                continue;
            }
            let Some((price, value)) = value_of(state, *symbol, 30_000) else { continue };
            let held = state.holding(*symbol);
            let weight = held.total() as f64 * price as f64 / equity.max(1.0);
            if (price as f64) < value * (1.0 - self.discount) && weight < self.max_weight {
                let limit = (value * (1.0 - self.discount / 2.0)) as Price;
                let qty = affordable(state.cash, limit, (equity * self.chunk_pct / limit.max(1) as f64) as u64);
                if qty > 0 {
                    actions.push(pov(*symbol, Side::Buy, qty, Some(limit), self.pov_bps, duration));
                }
            } else if (price as f64) > value * (1.0 + self.premium) && held.available > 0 {
                actions.push(pov(*symbol, Side::Sell, (held.available / 5).max(1), Some((value * (1.0 + self.premium / 2.0)) as Price), self.pov_bps, duration));
            }
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::news::tests::story;
    use crate::state::tests::{acme, hold, nova, state_with};
    use ome_protocol::NewsKind;
    use rand::SeedableRng;

    fn prospectus(state: &mut AgentState, symbol: Symbol, shares: u64, price: &str, seq: u64) {
        let mut item = story(seq, NewsKind::Prospectus, &symbol.to_string(), 0, 0);
        item.data = format!("shares={shares} price={price} fund={symbol}:3000:900:0");
        state.news.absorb(&item);
    }

    #[test]
    fn index_funds_hold_market_cap_weights_and_trade_only_in_the_closing_auction() {
        let mut rng = StdRng::seed_from_u64(1);
        let mut state = state_with(&[(acme(), 10_000), (nova(), 5_000)], 10_000_000_00);
        prospectus(&mut state, acme(), 30_000, "100.00", 1);
        prospectus(&mut state, nova(), 20_000, "50.00", 2);
        let weights = cap_weights(&state, &[acme(), nova()]);
        assert!((weights[0].1 - 0.75).abs() < 1e-9, "{weights:?}");
        let mut fund = IndexFund::new(vec![acme(), nova()], &Params::default());
        state.phase = MarketPhase::Open;
        assert!(fund.on_tick(&state, &mut rng).is_empty(), "no trading during the day");
        state.phase = MarketPhase::ClosingAuction;
        let orders = fund.on_tick(&state, &mut rng);
        assert_eq!(orders.len(), 2);
        assert!(orders.iter().all(|o| matches!(o, Action::Auction { side: Side::Buy, .. })));
        if let Action::Auction { qty, limit, .. } = orders[0] {
            assert!(qty > 7_000 && limit == 10_100, "about 75% of 10M in ACME at a limit 1% through: {qty} @ {limit}");
        }
        assert!(fund.on_tick(&state, &mut rng).is_empty(), "once per closing auction");
    }

    #[test]
    fn mutual_funds_deploy_cash_below_value_and_trim_far_above_it() {
        let mut rng = StdRng::seed_from_u64(2);
        let mut state = state_with(&[(acme(), 10_000), (nova(), 5_000)], 1_000_000_00);
        state.now = 100_000;
        prospectus(&mut state, acme(), 30_000, "110.00", 1);
        prospectus(&mut state, nova(), 20_000, "30.00", 2);
        hold(&mut state, nova(), 1_000);
        let mut fund = MutualFund::new(vec![acme(), nova()], &Params::default());
        let orders = fund.on_tick(&state, &mut rng);
        assert!(orders.iter().any(|o| matches!(o, Action::Algo { symbol, side: Side::Buy, limit: Some(l), .. } if *symbol == acme() && *l <= 10_100)));
        assert!(orders.iter().any(|o| matches!(o, Action::Algo { symbol, side: Side::Sell, .. } if *symbol == nova())), "NOVA trades far above its value: {orders:?}");
    }

    #[test]
    fn foreign_funds_follow_macro_risk_appetite() {
        let mut state = state_with(&[(acme(), 10_000)], 1_000_000_00);
        state.now = 10_000;
        let fund = ForeignFund::new(vec![acme()], &Params::default());
        let neutral = fund.target(&state);
        let mut hot = story(5, NewsKind::Macro, "", -80, 0);
        hot.time = 10_000;
        state.news.absorb(&hot);
        assert!(fund.target(&state) < neutral - 0.2, "bad macro news pulls foreign money out");
    }

    #[test]
    fn sovereign_funds_buy_only_deep_discounts() {
        let mut rng = StdRng::seed_from_u64(3);
        let mut state = state_with(&[(acme(), 8_000)], 5_000_000_00);
        state.now = 100_000;
        prospectus(&mut state, acme(), 30_000, "100.00", 1);
        let mut fund = SovereignFund::new(vec![acme()], &Params::default());
        let orders = fund.on_tick(&state, &mut rng);
        assert!(matches!(orders[..], [Action::Algo { side: Side::Buy, .. }]), "{orders:?}");
        let calm = state_with(&[(acme(), 9_500)], 5_000_000_00);
        let mut fund = SovereignFund::new(vec![acme()], &Params::default());
        assert!(fund.on_tick(&calm, &mut rng).is_empty());
    }
}
