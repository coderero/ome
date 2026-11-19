use super::{Params, Strategy, affordable};
use crate::action::Action;
use crate::edge::half_spread_bps;
use crate::state::{AgentState, Wake, mean_sd};
use ome_core::symbol::Symbol;
use ome_core::types::{Qty, Side};
use ome_protocol::AlgoStrategy;
use rand::rngs::StdRng;
use std::collections::VecDeque;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Execution {
    Algo,
    Dark,
}

pub struct PairsTrader {
    a: Symbol,
    b: Symbol,
    pub window: usize,
    pub entry_z: f64,
    pub exit_z: f64,
    pub trade_pct: f64,
    pub execution: Execution,
    pub fee_bps: f64,
    ratios: VecDeque<f64>,
    sampled: Option<u64>,
    position: i8,
    legs: [(Side, Qty); 2],
}

impl PairsTrader {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Option<Self> {
        let [a, b, ..] = symbols[..] else {
            return None;
        };
        Some(Self {
            a,
            b,
            window: params.u64("window", 120) as usize,
            entry_z: params.f64("entry_z", 1.8),
            exit_z: params.f64("exit_z", 0.4),
            trade_pct: params.f64("trade_pct", 0.08),
            execution: if params.text("exec", "algo") == "dark" {
                Execution::Dark
            } else {
                Execution::Algo
            },
            fee_bps: params.f64("fee_bps", 2.0),
            ratios: VecDeque::new(),
            sampled: None,
            position: 0,
            legs: [(Side::Buy, 0), (Side::Buy, 0)],
        })
    }

    fn order(&self, state: &AgentState, symbol: Symbol, side: Side, qty: Qty) -> Action {
        match self.execution {
            Execution::Algo => Action::Algo {
                symbol,
                side,
                qty,
                limit: None,
                strategy: AlgoStrategy::Pov {
                    rate_bps: 1_000,
                    duration_ms: 40 * state.minute_ms(),
                },
            },
            Execution::Dark => Action::Dark {
                symbol,
                side,
                qty,
                limit: None,
                min_qty: (qty / 5).max(1),
            },
        }
    }

    fn trade(&mut self, state: &AgentState, buy: Symbol, sell: Symbol) -> Vec<Action> {
        let price = |s: Symbol| state.market(s).and_then(|m| m.fair()).unwrap_or(1);
        let value = (state.equity() as f64 * self.trade_pct) as u64;
        let buy_qty = affordable(state.cash, price(buy), value / price(buy).max(1));
        let sell_qty = (value / price(sell).max(1)).min(state.holding(sell).available);
        if buy_qty == 0 || sell_qty == 0 {
            return vec![];
        }
        self.legs = [(Side::Buy, buy_qty), (Side::Sell, sell_qty)];
        vec![
            self.order(state, sell, Side::Sell, sell_qty),
            self.order(state, buy, Side::Buy, buy_qty),
        ]
    }

    pub fn pays(&self, state: &AgentState) -> bool {
        let values: Vec<f64> = self.ratios.iter().copied().collect();
        let Some(last) = values.last() else { return false };
        let gap_bps = (last - mean_sd(&values).0).abs() * 10_000.0;
        let leg = |s: Symbol| state.market(s).and_then(half_spread_bps).unwrap_or(f64::MAX / 8.0) + self.fee_bps;
        gap_bps / 2.0 > 2.0 * (leg(self.a) + leg(self.b))
    }

    pub fn z(&self) -> Option<f64> {
        if self.ratios.len() < self.window / 2 {
            return None;
        }
        let values: Vec<f64> = self.ratios.iter().copied().collect();
        let (mean, sd) = mean_sd(&values);
        (sd > 0.0).then(|| (values[values.len() - 1] - mean) / sd)
    }
}

impl Strategy for PairsTrader {
    fn name(&self) -> &'static str {
        "pairs stat-arb"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(1_000)
    }

    fn alert_bps(&self) -> Option<f64> {
        Some(30.0)
    }

    fn wakes(&self, wake: &Wake) -> bool {
        !wake.moved.is_empty()
    }

    fn on_tick(&mut self, state: &AgentState, _rng: &mut StdRng) -> Vec<Action> {
        let (Some(pa), Some(pb)) = (
            state.market(self.a).and_then(|m| m.last),
            state.market(self.b).and_then(|m| m.last),
        ) else {
            return vec![];
        };
        let clock = state.clock();
        if self.sampled != Some(clock) {
            self.sampled = Some(clock);
            if self.ratios.len() == self.window {
                self.ratios.pop_front();
            }
            self.ratios.push_back((pa as f64 / pb as f64).ln());
        }
        let busy = state.busy()
            || state.algo_in(self.a)
            || state.algo_in(self.b)
            || state.open_in(self.a) + state.open_in(self.b) > 0;
        if busy || !state.trading(self.a) || !state.trading(self.b) {
            return vec![];
        }
        let Some(z) = self.z() else {
            return vec![];
        };
        match self.position {
            0 if z.abs() > self.entry_z && !self.pays(state) => vec![],
            0 if z > self.entry_z => {
                let actions = self.trade(state, self.b, self.a);
                if !actions.is_empty() {
                    self.position = -1;
                }
                actions
            }
            0 if z < -self.entry_z => {
                let actions = self.trade(state, self.a, self.b);
                if !actions.is_empty() {
                    self.position = 1;
                }
                actions
            }
            p if p != 0 && z.abs() < self.exit_z => {
                let (bought, sold) = if p > 0 {
                    (self.a, self.b)
                } else {
                    (self.b, self.a)
                };
                self.position = 0;
                let sell_back = self.legs[0].1.min(state.holding(bought).available);
                let price = state.market(sold).and_then(|m| m.fair()).unwrap_or(1);
                let buy_back = affordable(state.cash, price, self.legs[1].1);
                let mut actions = Vec::new();
                if sell_back > 0 {
                    actions.push(self.order(state, bought, Side::Sell, sell_back));
                }
                if buy_back > 0 {
                    actions.push(self.order(state, sold, Side::Buy, buy_back));
                }
                actions
            }
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{acme, hold, nova, state_with};
    use rand::SeedableRng;

    #[test]
    fn rich_leg_is_sold_and_cheap_leg_bought_through_broker_algos_then_unwound() {
        let mut rng = StdRng::seed_from_u64(9);
        let mut state = state_with(&[(acme(), 10_000), (nova(), 5_000)], 10_000_00);
        hold(&mut state, acme(), 500);
        hold(&mut state, nova(), 500);
        let mut pairs = PairsTrader::new(vec![acme(), nova()], &Params::default()).unwrap();
        for i in 0..80 {
            state.now += state.minute_ms();
            state.markets.get_mut(&acme()).unwrap().last = Some(10_000 + (i % 3) * 5);
            assert!(pairs.on_tick(&state, &mut rng).is_empty());
        }
        state.now += state.minute_ms();
        state.markets.get_mut(&acme()).unwrap().last = Some(10_400);
        let trades = pairs.on_tick(&state, &mut rng);
        assert!(matches!(
            trades[..],
            [
                Action::Algo {
                    side: Side::Sell,
                    strategy: AlgoStrategy::Pov { .. },
                    ..
                },
                Action::Algo {
                    side: Side::Buy,
                    ..
                }
            ]
        ));
        if let Action::Algo { symbol, .. } = trades[0] {
            assert_eq!(symbol, acme(), "the rich leg is sold");
        }
        for _ in 0..200 {
            state.now += state.minute_ms();
            state.markets.get_mut(&acme()).unwrap().last = Some(10_005);
            let unwind = pairs.on_tick(&state, &mut rng);
            if !unwind.is_empty() {
                assert!(matches!(
                    unwind[0],
                    Action::Algo {
                        side: Side::Sell,
                        ..
                    }
                ));
                return;
            }
        }
        panic!("pair never unwound");
    }

    #[test]
    fn dark_execution_uses_midpoint_orders_with_a_minimum_size() {
        let params = Params([("exec".to_string(), "dark".to_string())].into());
        let pairs = PairsTrader::new(vec![acme(), nova()], &params).unwrap();
        assert!(matches!(
            pairs.order(&AgentState::default(), acme(), Side::Buy, 500),
            Action::Dark {
                min_qty: 100,
                limit: None,
                ..
            }
        ));
    }
}
