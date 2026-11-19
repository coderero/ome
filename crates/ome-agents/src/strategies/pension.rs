use super::factor::rebalance_with;
use ome_protocol::AlgoStrategy;
use super::{Params, Strategy};
use crate::action::Action;
use crate::state::{AgentState, Wake};
use ome_core::symbol::Symbol;
use rand::rngs::StdRng;
use std::time::Duration;

pub struct Pension {
    symbols: Vec<Symbol>,
    pub invest_pct: f64,
    pub drift_pct: f64,
    pub review_days: u64,
    pub work_minutes: u64,
    pub pov_bps: u64,
    last_check: Option<u64>,
}

impl Pension {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        Self {
            symbols,
            invest_pct: params.f64("invest_pct", 0.8),
            drift_pct: params.f64("drift_pct", 0.03),
            review_days: params.u64("review_days", 20).max(1),
            work_minutes: params.u64("work_minutes", 300),
            pov_bps: params.u64("pov_bps", 500),
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
        Duration::from_millis(10_000)
    }

    fn wakes(&self, wake: &Wake) -> bool {
        wake.day || wake.phase
    }

    fn on_tick(&mut self, state: &AgentState, _rng: &mut StdRng) -> Vec<Action> {
        let due = self.last_check.is_none_or(|day| state.day >= day + self.review_days);
        if !due || state.busy() || !state.continuous() {
            return vec![];
        }
        self.last_check = Some(state.day);
        let duration_ms = super::institution::until_close(state, self.work_minutes * state.minute_ms());
        rebalance_with(state, &self.weights(), self.drift_pct, AlgoStrategy::Pov { rate_bps: self.pov_bps, duration_ms })
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
                    strategy: AlgoStrategy::Pov { rate_bps: 500, .. },
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
        state.day += 19;
        state.algos.clear();
        assert!(fund.on_tick(&state, &mut rng).is_empty(), "it reviews about once a month of trading days");
        state.day += 1;
        assert!(!fund.on_tick(&state, &mut rng).is_empty());
    }
}
