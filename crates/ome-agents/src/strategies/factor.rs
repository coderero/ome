use super::{Params, Strategy, affordable};
use crate::action::Action;
use crate::state::AgentState;
use ome_core::symbol::Symbol;
use ome_core::types::{Side, Timestamp};
use ome_protocol::AlgoStrategy;
use rand::rngs::StdRng;
use std::time::Duration;

pub(super) fn rebalance(
    state: &AgentState,
    weights: &[(Symbol, f64)],
    min_trade_pct: f64,
    duration_ms: u64,
) -> Vec<Action> {
    rebalance_with(state, weights, min_trade_pct, AlgoStrategy::Vwap { duration_ms })
}

pub(super) fn rebalance_with(
    state: &AgentState,
    weights: &[(Symbol, f64)],
    min_trade_pct: f64,
    strategy: AlgoStrategy,
) -> Vec<Action> {
    let equity = state.equity() as f64;
    let mut actions = Vec::new();
    for (symbol, weight) in weights {
        if state.algo_in(*symbol) || !state.trading(*symbol) {
            continue;
        }
        let Some(price) = state.market(*symbol).and_then(|m| m.fair()) else {
            continue;
        };
        let current = state.holding(*symbol).total() as f64 * price as f64;
        let delta = equity * weight - current;
        if delta.abs() < equity * min_trade_pct {
            continue;
        }
        let qty = (delta.abs() / price as f64) as u64;
        let (side, qty) = if delta > 0.0 {
            (Side::Buy, affordable(state.cash, price, qty))
        } else {
            (Side::Sell, qty.min(state.holding(*symbol).available))
        };
        if qty > 0 {
            actions.push(Action::Algo {
                symbol: *symbol,
                side,
                qty,
                limit: None,
                strategy,
            });
        }
    }
    actions.sort_by_key(|a| {
        matches!(
            a,
            Action::Algo {
                side: Side::Buy,
                ..
            }
        )
    });
    actions
}

pub struct FactorModel {
    symbols: Vec<Symbol>,
    pub invest_pct: f64,
    pub momentum_weight: f64,
    pub reversal_weight: f64,
    pub volatility_weight: f64,
    pub rebalance_ms: Timestamp,
    last_rebalance: Option<Timestamp>,
}

impl FactorModel {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        Self {
            symbols,
            invest_pct: params.f64("invest_pct", 0.7),
            momentum_weight: params.f64("momentum", 1.0),
            reversal_weight: params.f64("reversal", 0.5),
            volatility_weight: params.f64("volatility", 2.0),
            rebalance_ms: params.u64("rebalance_ms", 90_000),
            last_rebalance: None,
        }
    }

    pub fn score(&self, state: &AgentState, symbol: Symbol) -> Option<f64> {
        let m = state.market(symbol)?;
        let momentum = m.change_bps(200)?;
        let reversal = m.change_bps(20).unwrap_or(0.0);
        let volatility = m.volatility_bps(100).unwrap_or(0.0);
        Some(
            self.momentum_weight * momentum
                - self.reversal_weight * reversal
                - self.volatility_weight * volatility,
        )
    }

    pub fn weights(&self, state: &AgentState) -> Vec<(Symbol, f64)> {
        let scores: Vec<(Symbol, f64)> = self
            .symbols
            .iter()
            .filter_map(|s| Some((*s, self.score(state, *s)?)))
            .collect();
        let Some(floor) = scores.iter().map(|(_, s)| *s).reduce(f64::min) else {
            return vec![];
        };
        let raw: Vec<(Symbol, f64)> = scores
            .iter()
            .map(|(sym, s)| (*sym, s - floor + 1.0))
            .collect();
        let total: f64 = raw.iter().map(|(_, w)| w).sum();
        raw.into_iter()
            .map(|(s, w)| (s, w / total * self.invest_pct))
            .collect()
    }
}

impl Strategy for FactorModel {
    fn name(&self) -> &'static str {
        "factor model"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(2_000)
    }

    fn on_tick(&mut self, state: &AgentState, _rng: &mut StdRng) -> Vec<Action> {
        let due = self
            .last_rebalance
            .is_none_or(|at| state.now >= at + self.rebalance_ms);
        if !due || state.busy() || !state.continuous() {
            return vec![];
        }
        let weights = self.weights(state);
        if weights.len() < self.symbols.len() {
            return vec![];
        }
        self.last_rebalance = Some(state.now);
        rebalance(state, &weights, 0.02, 60_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{acme, nova, state_with};
    use rand::SeedableRng;

    #[test]
    fn trending_low_volatility_names_get_more_weight_and_trades_go_through_vwap() {
        let mut rng = StdRng::seed_from_u64(10);
        let mut state = state_with(&[(acme(), 10_000), (nova(), 5_000)], 10_000_00);
        state.markets.get_mut(&acme()).unwrap().prices = (0..250).map(|i| 9_000 + i * 4).collect();
        state.markets.get_mut(&nova()).unwrap().prices =
            (0..250).map(|i| 5_500 - i * 2 + (i % 2) * 30).collect();
        let mut model = FactorModel::new(vec![acme(), nova()], &Params::default());
        let weights = model.weights(&state);
        assert!(weights[0].1 > weights[1].1, "{weights:?}");
        assert!((weights.iter().map(|w| w.1).sum::<f64>() - 0.7).abs() < 1e-9);
        let trades = model.on_tick(&state, &mut rng);
        assert!(!trades.is_empty());
        assert!(trades.iter().all(|a| matches!(
            a,
            Action::Algo {
                side: Side::Buy,
                strategy: AlgoStrategy::Vwap { .. },
                ..
            }
        )));
        assert!(
            model.on_tick(&state, &mut rng).is_empty(),
            "waits for the next rebalance"
        );
    }
}
