use super::{Params, Strategy, affordable, cancel_stale, gaussian, pick, scale};
use crate::action::Action;
use crate::state::AgentState;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Qty, Side, Timestamp};
use rand::Rng;
use rand::rngs::StdRng;
use std::time::Duration;

pub struct NoiseTrader {
    symbols: Vec<Symbol>,
    pub activity: f64,
    pub max_size: Qty,
    pub order_ttl: Timestamp,
    pub market_share: f64,
    sentiment: f64,
}

impl NoiseTrader {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        Self {
            symbols,
            activity: params.f64("activity", 0.5),
            max_size: params.u64("max_size", 25),
            order_ttl: params.u64("order_ttl_ms", 8_000),
            market_share: params.f64("market_share", 0.55),
            sentiment: 0.0,
        }
    }
}

impl Strategy for NoiseTrader {
    fn name(&self) -> &'static str {
        "noise trader"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(400)
    }

    fn on_tick(&mut self, state: &AgentState, rng: &mut StdRng) -> Vec<Action> {
        self.sentiment = (self.sentiment * 0.985 + gaussian(rng) * 0.06).clamp(-1.0, 1.0);
        if rng.random_bool(0.0015) {
            self.sentiment = if rng.random_bool(0.5) { 1.0 } else { -1.0 };
        }
        let mut actions = cancel_stale(state, None, self.order_ttl);
        let activity = (self.activity * state.world.activity).clamp(0.0, 1.0);
        if state.pending.len() > 2 || !rng.random_bool(activity) {
            return actions;
        }
        let Some(symbol) = pick(&self.symbols, rng) else {
            return actions;
        };
        let mood = (self.sentiment + state.world.sentiment(symbol)).clamp(-1.0, 1.0);
        if !state.trading(symbol) {
            return actions;
        }
        let Some(fair) = state.market(symbol).and_then(|m| m.fair()) else {
            return actions;
        };
        let held = state.holding(symbol).available;
        let mut qty = rng.random_range(1..=self.max_size.max(1));
        let mut side = if rng.random_bool((0.5 + mood * 0.4).clamp(0.05, 0.95)) {
            Side::Buy
        } else {
            Side::Sell
        };
        if side == Side::Sell && held < qty {
            if held > 0 {
                qty = held;
            } else if mood < -0.3 {
                return actions;
            } else {
                side = Side::Buy;
            }
        }
        let market = mood < -0.5 || rng.random_bool(self.market_share.clamp(0.0, 1.0));
        let offset = rng.random_range(0.0..35.0);
        let price = match side {
            Side::Buy => scale(fair, -offset),
            Side::Sell => scale(fair, offset),
        };
        let cost = if market { scale(fair, 1_100.0) } else { price };
        if side == Side::Buy && affordable(state.cash, cost, qty) < qty {
            return actions;
        }
        actions.push(Action::Place {
            symbol,
            side,
            kind: if market {
                OrderKind::Market
            } else {
                OrderKind::Limit(price)
            },
            qty,
        });
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{acme, state_with};
    use rand::SeedableRng;

    #[test]
    fn trades_only_what_it_can_pay_for_and_never_sells_short() {
        let mut rng = StdRng::seed_from_u64(3);
        let state = state_with(&[(acme(), 10_000)], 50_000_00);
        let params = Params([("activity".to_string(), "1".to_string())].into());
        let mut noise = NoiseTrader::new(vec![acme()], &params);
        let mut orders = 0;
        for _ in 0..200 {
            for action in noise.on_tick(&state, &mut rng) {
                if let Action::Place { side, .. } = action {
                    assert_eq!(side, Side::Buy, "holds no shares, so it only buys");
                    orders += 1;
                }
            }
        }
        assert!(orders > 100);
        let broke = state_with(&[(acme(), 10_000)], 0);
        assert!(
            (0..50)
                .flat_map(|_| noise.on_tick(&broke, &mut rng))
                .all(|a| !matches!(a, Action::Place { .. }))
        );
    }
}
