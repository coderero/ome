use super::{Params, Strategy, affordable, cancel_stale, scale};
use crate::action::Action;
use crate::edge::taking_cost_bps;
use crate::state::{AgentState, Wake};
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

pub struct PropDesk {
    signal: PropSignal,
    symbols: Vec<Symbol>,
    pub size_pct: f64,
    pub take_profit_bps: f64,
    pub stop_bps: f64,
    pub threshold: f64,
    pub look_minutes: u64,
    pub cooldown_minutes: u64,
    pub fee_bps: f64,
    pub hurdle_bps: f64,
    resting_until: BTreeMap<Symbol, u64>,
}

impl PropDesk {
    pub fn new(signal: PropSignal, symbols: Vec<Symbol>, params: &Params) -> Self {
        let (threshold, look) = match signal {
            PropSignal::Momentum => (1.5, 30),
            PropSignal::Breakout => (0.5, 120),
            PropSignal::MeanReversion => (1.6, 150),
        };
        Self {
            signal,
            symbols,
            size_pct: params.f64("size_pct", 0.08),
            take_profit_bps: params.f64("take_profit_bps", 60.0),
            stop_bps: params.f64("stop_bps", 40.0),
            threshold: params.f64("threshold", threshold),
            look_minutes: params.u64("look_minutes", look),
            cooldown_minutes: params.u64("cooldown_minutes", 30),
            fee_bps: params.f64("fee_bps", 2.0),
            hurdle_bps: params.f64("hurdle_bps", 5.0),
            resting_until: BTreeMap::new(),
        }
    }

    fn noise(&self, state: &AgentState, symbol: Symbol) -> Option<f64> {
        Some(state.minute_vol(symbol, self.look_minutes.max(60))? * (self.look_minutes as f64).sqrt())
    }

    pub fn idea(&self, state: &AgentState, symbol: Symbol) -> Option<(OrderKind, f64)> {
        let market = state.market(symbol)?;
        let clock = state.clock();
        let last = market.fair()? as f64;
        let noise = self.noise(state, symbol)?;
        match self.signal {
            PropSignal::Momentum => {
                let change = market.change_over(clock, self.look_minutes)?;
                (change > self.threshold * noise).then_some((OrderKind::Market, change / 2.0))
            }
            PropSignal::Breakout => {
                let prior = market.prior_high(clock, self.look_minutes)? as f64;
                let excess = (last / prior - 1.0) * 10_000.0;
                (excess > self.threshold * noise / (self.look_minutes as f64).sqrt()).then_some((OrderKind::Market, excess + noise / 2.0))
            }
            PropSignal::MeanReversion => {
                let z = market.z_over(clock, self.look_minutes)?;
                let mean = market.mean_over(clock, self.look_minutes)?;
                (z < -self.threshold).then_some((OrderKind::Limit(market.bid?), (mean / last - 1.0) * 10_000.0 / 2.0))
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
        Duration::from_millis(1_000)
    }

    fn alert_bps(&self) -> Option<f64> {
        Some(20.0)
    }

    fn wakes(&self, wake: &Wake) -> bool {
        wake.fill || !wake.moved.is_empty()
    }

    fn on_tick(&mut self, state: &AgentState, _rng: &mut StdRng) -> Vec<Action> {
        let mut actions = cancel_stale(state, None, 60 * state.minute_ms());
        if state.busy() {
            return actions;
        }
        let clock = state.clock();
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
                let noise = self.noise(state, symbol).unwrap_or(0.0);
                actions.push(Action::Oco {
                    symbol,
                    side: Side::Sell,
                    qty: held.available,
                    take_profit: scale(last, self.take_profit_bps.max(1.5 * noise)),
                    stop: scale(last, -self.stop_bps.max(noise)),
                });
                return actions;
            }
            let resting = self.resting_until.get(&symbol).is_some_and(|until| clock < *until);
            if held.total() > 0 || armed > 0 || open > 0 || resting {
                continue;
            }
            let Some((kind, edge)) = self.idea(state, symbol) else {
                continue;
            };
            let budget = (state.equity() as f64 * self.size_pct) as u64;
            let worst = match kind {
                OrderKind::Market => scale(last, 200.0),
                OrderKind::Limit(p) => p,
            };
            let qty = affordable(state.cash, worst, budget / worst.max(1));
            if qty == 0 {
                continue;
            }
            let cost = match kind {
                OrderKind::Market => taking_cost_bps(state, symbol, Side::Buy, qty, self.fee_bps).unwrap_or(f64::MAX),
                OrderKind::Limit(_) => self.fee_bps,
            };
            if edge <= 2.0 * cost + self.hurdle_bps {
                continue;
            }
            self.resting_until.insert(symbol, clock + self.cooldown_minutes);
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
    use crate::state::tests::{acme, bars, hold, state_with};
    use ome_core::price::Price;
    use rand::SeedableRng;

    #[test]
    fn breakout_enters_on_a_new_high_and_then_protects_the_position_with_an_oco() {
        let mut rng = StdRng::seed_from_u64(7);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        let mut closes: Vec<Price> = (0..120).map(|i| 10_000 + (i % 7) * 3).collect();
        bars(&mut state, acme(), &closes);
        let mut desk = PropDesk::new(PropSignal::Breakout, vec![acme()], &Params::default());
        assert!(desk.on_tick(&state, &mut rng).is_empty(), "inside the range");
        closes.push(10_100);
        bars(&mut state, acme(), &closes);
        let entry = desk.on_tick(&state, &mut rng);
        assert!(matches!(entry[..], [Action::Place { side: Side::Buy, kind: OrderKind::Market, .. }]), "{entry:?}");
        assert!(desk.on_tick(&state, &mut rng).is_empty(), "then it rests for a while");
        hold(&mut state, acme(), 70);
        let exit = desk.on_tick(&state, &mut rng);
        let [Action::Oco { qty: 70, take_profit, stop, .. }] = exit[..] else {
            panic!("expected an OCO exit, got {exit:?}");
        };
        assert!(take_profit > 10_100 && stop < 10_100);
    }

    #[test]
    fn mean_reversion_bids_into_weakness_and_momentum_needs_a_trend_that_pays_for_its_costs() {
        let mut rng = StdRng::seed_from_u64(8);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        let mut closes: Vec<Price> = (0..150).map(|i| 10_000 + (i % 5) * 2).collect();
        closes.push(9_900);
        bars(&mut state, acme(), &closes);
        let mut meanrev = PropDesk::new(PropSignal::MeanReversion, vec![acme()], &Params::default());
        assert!(matches!(meanrev.on_tick(&state, &mut rng)[..], [Action::Place { kind: OrderKind::Limit(9_895), .. }]));

        let mut flat = state_with(&[(acme(), 10_000)], 10_000_00);
        let quiet: Vec<Price> = (0..90).map(|i| 10_000 + (i % 2) * 2).collect();
        bars(&mut flat, acme(), &quiet);
        let mut momentum = PropDesk::new(PropSignal::Momentum, vec![acme()], &Params::default());
        assert!(momentum.on_tick(&flat, &mut rng).is_empty());
        let trend: Vec<Price> = (0..90).map(|i| 10_000 + (i % 2) * 2 + if i > 60 { (i - 60) * 8 } else { 0 }).collect();
        bars(&mut flat, acme(), &trend);
        assert!(momentum.idea(&flat, acme()).is_some());
        let market = flat.markets.get_mut(&acme()).unwrap();
        market.asks[0].price += 200;
        assert!(momentum.on_tick(&flat, &mut rng).is_empty(), "a wide spread eats the edge");
        flat.markets.get_mut(&acme()).unwrap().asks[0].price -= 200;
        assert!(matches!(momentum.on_tick(&flat, &mut rng)[..], [Action::Place { kind: OrderKind::Market, .. }]));
    }
}
