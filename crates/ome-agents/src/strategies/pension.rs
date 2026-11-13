use super::factor::rebalance;
use super::{Params, Strategy};
use crate::action::Action;
use crate::state::AgentState;
use ome_core::symbol::Symbol;
use ome_core::types::Timestamp;
use rand::rngs::StdRng;
use std::time::Duration;

pub struct Pension {
    symbols: Vec<Symbol>,
    pub invest_pct: f64,
    pub drift_pct: f64,
    pub rebalance_ms: Timestamp,
    pub vwap_ms: u64,
    last_check: Option<Timestamp>,
}

impl Pension {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        Self {
            symbols,
            invest_pct: params.f64("invest_pct", 0.8),
            drift_pct: params.f64("drift_pct", 0.03),
            rebalance_ms: params.u64("rebalance_ms", 120_000),
            vwap_ms: params.u64("vwap_ms", 300_000),
            last_check: None,
        }
    }

    pub fn weights(&self) -> Vec<(Symbol, f64)> {
        let each = self.invest_pct / self.symbols.len().max(1) as f64;
        self.symbols.iter().map(|s| (*s, each)).collect()
    }
}

impl Strategy for Pension {
    fn name(&self) -> &'static str {
        "pension rebalancer"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(5_000)
    }

    fn on_tick(&mut self, state: &AgentState, _rng: &mut StdRng) -> Vec<Action> {
        let due = self
            .last_check
            .is_none_or(|at| state.now >= at + self.rebalance_ms);
        if !due || state.busy() || !state.continuous() {
            return vec![];
        }
        self.last_check = Some(state.now);
        rebalance(state, &self.weights(), self.drift_pct, self.vwap_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{acme, hold, nova, state_with};
    use ome_core::types::Side;
    use ome_protocol::AlgoStrategy;
    use rand::SeedableRng;

    #[test]
    fn rebalances_toward_equal_weights_only_when_drift_is_large() {
        let mut rng = StdRng::seed_from_u64(11);
        let mut state = state_with(&[(acme(), 10_000), (nova(), 5_000)], 2_000_000);
        hold(&mut state, acme(), 1_200);
        hold(&mut state, nova(), 0);
        let mut fund = Pension::new(vec![acme(), nova()], &Params::default());
        let trades = fund.on_tick(&state, &mut rng);
        assert!(matches!(
            trades[..],
            [
                Action::Algo {
                    side: Side::Sell,
                    strategy: AlgoStrategy::Vwap {
                        duration_ms: 300_000
                    },
                    ..
                },
                Action::Algo {
                    side: Side::Buy,
                    ..
                }
            ]
        ));
        let mut balanced = state_with(&[(acme(), 10_000), (nova(), 5_000)], 2_000_000);
        hold(&mut balanced, acme(), 400);
        hold(&mut balanced, nova(), 800);
        let mut calm = Pension::new(vec![acme(), nova()], &Params::default());
        assert!(calm.on_tick(&balanced, &mut rng).is_empty());
    }
}
