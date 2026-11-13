use super::{Params, Strategy, affordable, cancel_stale, scale};
use crate::action::Action;
use crate::state::AgentState;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Side};
use rand::rngs::StdRng;
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropSignal {
    Momentum,
    Breakout,
    MeanReversion,
}

#[derive(Default)]
struct Tracker {
    fast: Option<f64>,
    slow: Option<f64>,
    cooldown: u32,
}

pub struct PropDesk {
    signal: PropSignal,
    symbols: Vec<Symbol>,
    pub size_pct: f64,
    pub take_profit_bps: f64,
    pub stop_bps: f64,
    pub threshold: f64,
    pub window: usize,
    trackers: BTreeMap<Symbol, Tracker>,
}

impl PropDesk {
    pub fn new(signal: PropSignal, symbols: Vec<Symbol>, params: &Params) -> Self {
        let (threshold, window) = match signal {
            PropSignal::Momentum => (10.0, 0),
            PropSignal::Breakout => (3.0, 120),
            PropSignal::MeanReversion => (1.6, 150),
        };
        Self {
            signal,
            symbols,
            size_pct: params.f64("size_pct", 0.08),
            take_profit_bps: params.f64("take_profit_bps", 60.0),
            stop_bps: params.f64("stop_bps", 40.0),
            threshold: params.f64("threshold", threshold),
            window: params.u64("window", window as u64) as usize,
            trackers: BTreeMap::new(),
        }
    }

    fn entry(&self, state: &AgentState, symbol: Symbol, tracker: &Tracker) -> Option<OrderKind> {
        let market = state.market(symbol)?;
        let last = market.last? as f64;
        match self.signal {
            PropSignal::Momentum => {
                let (fast, slow) = (tracker.fast?, tracker.slow?);
                ((fast - slow) / slow * 10_000.0 > self.threshold).then_some(OrderKind::Market)
            }
            PropSignal::Breakout => {
                let history = market.recent(self.window + 1);
                if history.len() < self.window / 2 {
                    return None;
                }
                let prior = history[..history.len() - 1]
                    .iter()
                    .cloned()
                    .fold(f64::MIN, f64::max);
                (last > prior * (1.0 + self.threshold / 10_000.0)).then_some(OrderKind::Market)
            }
            PropSignal::MeanReversion => {
                let z = market.z_score(self.window)?;
                (z < -self.threshold).then_some(OrderKind::Limit(market.bid?))
            }
        }
    }
}

impl Strategy for PropDesk {
    fn name(&self) -> &'static str {
        match self.signal {
            PropSignal::Momentum => "prop momentum",
            PropSignal::Breakout => "prop breakout",
            PropSignal::MeanReversion => "prop mean reversion",
        }
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(500)
    }

    fn on_tick(&mut self, state: &AgentState, _rng: &mut StdRng) -> Vec<Action> {
        for symbol in &self.symbols {
            let tracker = self.trackers.entry(*symbol).or_default();
            if let Some(price) = state.market(*symbol).and_then(|m| m.last) {
                let p = price as f64;
                tracker.fast = Some(tracker.fast.map_or(p, |f| f + 0.25 * (p - f)));
                tracker.slow = Some(tracker.slow.map_or(p, |s| s + 0.03 * (p - s)));
            }
            tracker.cooldown = tracker.cooldown.saturating_sub(1);
        }
        let mut actions = cancel_stale(state, None, 5_000);
        if state.busy() {
            return actions;
        }
        for symbol in self.symbols.clone() {
            if !state.trading(symbol) {
                continue;
            }
            let held = state.holding(symbol);
            let armed = state.armed_in(symbol);
            let open = state.open_in(symbol);
            let Some(last) = state.market(symbol).and_then(|m| m.fair()) else {
                continue;
            };
            if held.available > 0 && armed == 0 && open == 0 {
                actions.push(Action::Oco {
                    symbol,
                    side: Side::Sell,
                    qty: held.available,
                    take_profit: scale(last, self.take_profit_bps),
                    stop: scale(last, -self.stop_bps),
                });
                return actions;
            }
            let tracker = &self.trackers[&symbol];
            if held.total() > 0 || armed > 0 || open > 0 || tracker.cooldown > 0 {
                continue;
            }
            let Some(kind) = self.entry(state, symbol, tracker) else {
                continue;
            };
            let budget = (state.equity() as f64 * self.size_pct) as u64;
            let worst = match kind {
                OrderKind::Market => scale(last, 1_100.0),
                OrderKind::Limit(p) => p,
            };
            let qty = affordable(state.cash, worst, budget / worst.max(1));
            if qty == 0 {
                continue;
            }
            if let Some(t) = self.trackers.get_mut(&symbol) {
                t.cooldown = 20;
            }
            actions.push(Action::Place {
                symbol,
                side: Side::Buy,
                kind,
                qty,
            });
            return actions;
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
    fn breakout_enters_on_a_new_high_and_then_protects_the_position_with_an_oco() {
        let mut rng = StdRng::seed_from_u64(7);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        let m = state.markets.get_mut(&acme()).unwrap();
        m.prices = (0..120).map(|i| 10_000 + (i % 7) * 3).collect();
        m.record(10_100);
        let mut desk = PropDesk::new(PropSignal::Breakout, vec![acme()], &Params::default());
        let entry = desk.on_tick(&state, &mut rng);
        assert!(matches!(
            entry[..],
            [Action::Place {
                side: Side::Buy,
                kind: OrderKind::Market,
                ..
            }]
        ));
        hold(&mut state, acme(), 70);
        let exit = desk.on_tick(&state, &mut rng);
        let [
            Action::Oco {
                qty: 70,
                take_profit,
                stop,
                ..
            },
        ] = exit[..]
        else {
            panic!("expected an OCO exit, got {exit:?}");
        };
        assert!(take_profit > 10_100 && stop < 10_100);
    }

    #[test]
    fn mean_reversion_bids_into_weakness_and_momentum_needs_a_trend() {
        let mut rng = StdRng::seed_from_u64(8);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        let m = state.markets.get_mut(&acme()).unwrap();
        m.prices = (0..150).map(|i| 10_000 + (i % 5) * 2).collect();
        m.record(9_900);
        let mut meanrev =
            PropDesk::new(PropSignal::MeanReversion, vec![acme()], &Params::default());
        assert!(matches!(
            meanrev.on_tick(&state, &mut rng)[..],
            [Action::Place {
                kind: OrderKind::Limit(9_995),
                ..
            }]
        ));
        let flat = state_with(&[(acme(), 10_000)], 10_000_00);
        let mut momentum = PropDesk::new(PropSignal::Momentum, vec![acme()], &Params::default());
        assert!(momentum.on_tick(&flat, &mut rng).is_empty());
    }
}
