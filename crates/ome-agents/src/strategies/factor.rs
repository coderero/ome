use super::{Params, Strategy, affordable};
use crate::action::Action;
use crate::state::{AgentState, Wake};
use ome_core::symbol::Symbol;
use ome_core::types::Side;
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
    pub tilt: f64,
    pub rebalance_days: u64,
    pub work_minutes: u64,
    last_rebalance: Option<u64>,
}

impl FactorModel {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        Self {
            symbols,
            invest_pct: params.f64("invest_pct", 0.7),
            momentum_weight: params.f64("momentum", 1.0),
            reversal_weight: params.f64("reversal", 0.25),
            volatility_weight: params.f64("volatility", 0.5),
            tilt: params.f64("tilt", 0.4),
            rebalance_days: params.u64("rebalance_days", 1).max(1),
            work_minutes: params.u64("work_minutes", 120),
            last_rebalance: None,
        }
    }

    pub fn score(&self, state: &AgentState, symbol: Symbol, usual_vol: f64) -> Option<f64> {
        let m = state.market(symbol)?;
        let vol = state.daily_vol(symbol).filter(|v| *v > 0.0)?;
        let days = 5.min(m.days.len()).max(1);
        let momentum = m.day_change(days).or_else(|| state.change_bps(symbol, 200))? / (vol * (days as f64).sqrt());
        let reversal = m.day_change(1).or_else(|| state.change_bps(symbol, 20)).unwrap_or(0.0) / vol;
        Some(self.momentum_weight * momentum - self.reversal_weight * reversal - self.volatility_weight * (vol / usual_vol - 1.0))
    }

    pub fn weights(&self, state: &AgentState) -> Vec<(Symbol, f64)> {
        let vols: Vec<f64> = self.symbols.iter().filter_map(|s| state.daily_vol(*s)).collect();
        if vols.is_empty() {
            return vec![];
        }
        let usual = (vols.iter().sum::<f64>() / vols.len() as f64).max(1.0);
        let scores: Vec<(Symbol, f64)> = self
            .symbols
            .iter()
            .filter_map(|s| Some((*s, self.score(state, *s, usual)?)))
            .collect();
        if scores.is_empty() {
            return vec![];
        }
        let mean = scores.iter().map(|(_, s)| s).sum::<f64>() / scores.len() as f64;
        let raw: Vec<(Symbol, f64)> = scores.iter().map(|(sym, s)| (*sym, (1.0 + self.tilt * (s - mean)).clamp(0.2, 1.8))).collect();
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
        Duration::from_millis(5_000)
    }

    fn wakes(&self, wake: &Wake) -> bool {
        wake.day || wake.phase
    }

    fn on_tick(&mut self, state: &AgentState, _rng: &mut StdRng) -> Vec<Action> {
        let due = self.last_rebalance.is_none_or(|day| state.day >= day + self.rebalance_days);
        if !due || state.busy() || !state.continuous() {
            return vec![];
        }
        let weights = self.weights(state);
        if weights.len() < self.symbols.len() {
            return vec![];
        }
        self.last_rebalance = Some(state.day);
        rebalance(state, &weights, 0.03, super::institution::until_close(state, self.work_minutes * state.minute_ms()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{acme, bars, days, nova, state_with};
    use rand::SeedableRng;

    #[test]
    fn trending_low_volatility_names_get_more_weight_and_trades_go_through_vwap() {
        let mut rng = StdRng::seed_from_u64(10);
        let mut state = state_with(&[(acme(), 10_000), (nova(), 5_000)], 10_000_00);
        days(&mut state, acme(), &[9_000, 9_200, 9_400, 9_600, 9_800, 9_950]);
        days(&mut state, nova(), &[5_500, 5_200, 5_450, 5_100, 5_300, 5_050]);
        bars(&mut state, acme(), &[10_000]);
        bars(&mut state, nova(), &[5_000]);
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
            "waits for the next trading day"
        );
        state.day += 1;
        state.algos.clear();
        assert!(!model.on_tick(&state, &mut rng).is_empty());
    }
}
