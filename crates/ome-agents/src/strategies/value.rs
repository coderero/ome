use super::{Params, Strategy, affordable, cancel_stale};
use crate::action::Action;
use crate::state::AgentState;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Side};
use rand::rngs::StdRng;
use std::collections::BTreeMap;
use std::time::Duration;

pub struct ValueInvestor {
    symbols: Vec<Symbol>,
    pub band_bps: f64,
    pub size_pct: f64,
    anchors: BTreeMap<Symbol, Price>,
    turn: usize,
}

impl ValueInvestor {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        Self {
            symbols,
            band_bps: params.f64("band_bps", 150.0),
            size_pct: params.f64("size_pct", 0.04),
            anchors: BTreeMap::new(),
            turn: 0,
        }
    }

    pub fn deviation_bps(&mut self, state: &AgentState, symbol: Symbol) -> Option<(f64, Price)> {
        let market = state.market(symbol)?;
        let anchor = *self
            .anchors
            .entry(symbol)
            .or_insert(market.reference.max(1));
        let fair = state.world.fair_value(symbol, anchor);
        let last = market.fair()?;
        Some(((last as f64 / fair as f64 - 1.0) * 10_000.0, fair))
    }
}

impl Strategy for ValueInvestor {
    fn name(&self) -> &'static str {
        "value investor"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(1_500)
    }

    fn on_tick(&mut self, state: &AgentState, _rng: &mut StdRng) -> Vec<Action> {
        let mut actions = cancel_stale(state, None, 4_000);
        if state.busy() || self.symbols.is_empty() {
            return actions;
        }
        for _ in 0..self.symbols.len() {
            self.turn = (self.turn + 1) % self.symbols.len();
            let symbol = self.symbols[self.turn];
            let Some((deviation, fair)) = self.deviation_bps(state, symbol) else {
                continue;
            };
            if !state.trading(symbol) || state.open_in(symbol) > 0 {
                continue;
            }
            let conviction = (deviation.abs() / self.band_bps).min(3.0);
            let budget = state.equity() as f64 * self.size_pct * conviction;
            let Some(market) = state.market(symbol) else {
                continue;
            };
            let caution = 1.0 + 4.0 * (-state.sentiment(symbol)).max(0.0);
            if deviation < -self.band_bps * caution {
                let price = market.ask.unwrap_or(fair).min(fair);
                let qty = affordable(state.cash, price, (budget / price as f64) as u64);
                if qty > 0 {
                    actions.push(Action::Place {
                        symbol,
                        side: Side::Buy,
                        kind: OrderKind::Limit(price),
                        qty,
                    });
                    return actions;
                }
            } else if deviation > self.band_bps {
                let price = market.bid.unwrap_or(fair).max(fair);
                let qty = ((budget / price as f64) as u64).min(state.holding(symbol).available);
                if qty > 0 {
                    actions.push(Action::Place {
                        symbol,
                        side: Side::Sell,
                        kind: OrderKind::Limit(price),
                        qty,
                    });
                    return actions;
                }
            }
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{acme, hold, state_with};
    use rand::SeedableRng;

    #[test]
    fn buys_below_fair_value_sells_above_and_follows_the_world_fair_value() {
        let mut rng = StdRng::seed_from_u64(12);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        hold(&mut state, acme(), 500);
        let mut value = ValueInvestor::new(vec![acme()], &Params::default());
        assert!(
            value.on_tick(&state, &mut rng).is_empty(),
            "at fair value it waits"
        );

        state.markets.get_mut(&acme()).unwrap().last = Some(9_500);
        assert!(matches!(
            value.on_tick(&state, &mut rng)[..],
            [Action::Place {
                side: Side::Buy,
                ..
            }]
        ));
        state.markets.get_mut(&acme()).unwrap().last = Some(10_600);
        assert!(matches!(
            value.on_tick(&state, &mut rng)[..],
            [Action::Place {
                side: Side::Sell,
                ..
            }]
        ));
        state.world.fair.insert(acme(), 11_000);
        assert!(
            matches!(
                value.on_tick(&state, &mut rng)[..],
                [Action::Place {
                    side: Side::Buy,
                    ..
                }]
            ),
            "a higher fair value turns the same price into a bargain"
        );
        state.world.sentiment.insert(acme(), -1.0);
        assert!(
            value.on_tick(&state, &mut rng).is_empty(),
            "no catching a falling knife"
        );
    }
}
