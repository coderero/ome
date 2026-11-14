use super::{Params, Strategy, affordable, pick, scale};
use crate::action::Action;
use crate::state::AgentState;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Qty, Side};
use ome_protocol::{MarketPhase, Trigger};
use rand::Rng;
use rand::rngs::StdRng;
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Persona {
    DayTrader,
    Swing,
    Investor,
}

pub struct RetailPersona {
    persona: Persona,
    symbols: Vec<Symbol>,
    pub activity: f64,
    pub budget_pct: f64,
    pub stop_pct: f64,
    pub target_pct: f64,
    pub dip_pct: f64,
    pub trail_pct: u64,
    entries: BTreeMap<Symbol, Price>,
}

impl RetailPersona {
    pub fn new(persona: Persona, symbols: Vec<Symbol>, params: &Params) -> Self {
        let (activity, budget, stop, target, dip) = match persona {
            Persona::DayTrader => (0.25, 0.15, 1.0, 1.5, 0.0),
            Persona::Swing => (0.08, 0.20, 2.0, 3.0, 0.0),
            Persona::Investor => (0.04, 0.25, 0.0, 0.0, 2.0),
        };
        Self {
            persona,
            symbols,
            activity: params.f64("activity", activity),
            budget_pct: params.f64("budget_pct", budget),
            stop_pct: params.f64("stop_pct", stop),
            target_pct: params.f64("target_pct", target),
            dip_pct: params.f64("dip_pct", dip),
            trail_pct: params.u64("trail_pct", 8),
            entries: BTreeMap::new(),
        }
    }

    fn size(&self, state: &AgentState, price: Price) -> Qty {
        let budget = (state.equity() as f64 * self.budget_pct) as u64;
        affordable(state.cash, price, budget / price.max(1))
    }

    fn armed(state: &AgentState, symbol: Symbol, side: Side) -> Vec<u64> {
        state
            .armed
            .values()
            .filter(|c| c.symbol == symbol && c.side == side)
            .map(|c| c.cond_id)
            .collect()
    }

    fn day_trader(&mut self, state: &AgentState, rng: &mut StdRng) -> Vec<Action> {
        let mut actions = Vec::new();
        for symbol in self.symbols.clone() {
            let held = state.holding(symbol).available;
            let Some(last) = state.market(symbol).and_then(|m| m.fair()) else {
                continue;
            };
            if held == 0 {
                self.entries.remove(&symbol);
                continue;
            }
            if !state.trading(symbol) {
                continue;
            }
            let stops = Self::armed(state, symbol, Side::Sell);
            if state.sentiment(symbol) < -0.5 && rng.random_bool(0.5) {
                actions.extend(stops.into_iter().map(Action::CancelConditional));
                actions.push(Action::Place {
                    symbol,
                    side: Side::Sell,
                    kind: OrderKind::Market,
                    qty: held,
                });
                return actions;
            }
            let entry = *self.entries.entry(symbol).or_insert(last);
            let target_hit = last as f64 >= entry as f64 * (1.0 + self.target_pct / 100.0);
            if state.closing_soon() || target_hit {
                actions.extend(stops.into_iter().map(Action::CancelConditional));
                actions.push(Action::Place {
                    symbol,
                    side: Side::Sell,
                    kind: OrderKind::Market,
                    qty: held,
                });
                return actions;
            }
            if stops.is_empty() && state.open_in(symbol) == 0 {
                actions.push(Action::Conditional {
                    symbol,
                    side: Side::Sell,
                    qty: held,
                    trigger: Trigger::Below(scale(entry, -self.stop_pct * 100.0)),
                    kind: OrderKind::Market,
                });
                return actions;
            }
        }
        if state.closing_soon() || !rng.random_bool((self.activity * state.world.activity).min(1.0))
        {
            return actions;
        }
        let candidates: Vec<Symbol> = self
            .symbols
            .iter()
            .copied()
            .filter(|s| state.trading(*s) && state.holding(*s).total() == 0)
            .filter(|s| {
                state
                    .market(*s)
                    .and_then(|m| m.change_bps(20))
                    .is_some_and(|c| c > 8.0)
            })
            .collect();
        if let Some(symbol) = pick(&candidates, rng)
            && let Some(price) = state.market(symbol).and_then(|m| m.fair())
        {
            let qty = self.size(state, scale(price, 1_100.0));
            if qty > 0 {
                self.entries.insert(symbol, price);
                actions.push(Action::Place {
                    symbol,
                    side: Side::Buy,
                    kind: OrderKind::Market,
                    qty,
                });
            }
        }
        actions
    }

    fn swing(&mut self, state: &AgentState, rng: &mut StdRng) -> Vec<Action> {
        if !rng.random_bool(self.activity) {
            return vec![];
        }
        let candidates: Vec<Symbol> = self
            .symbols
            .iter()
            .copied()
            .filter(|s| state.trading(*s) && state.holding(*s).total() == 0)
            .filter(|s| state.armed_in(*s) == 0 && state.open_in(*s) == 0)
            .filter(|s| {
                state
                    .market(*s)
                    .and_then(|m| m.z_score(100))
                    .is_some_and(|z| z < -0.8)
            })
            .collect();
        let Some(symbol) = pick(&candidates, rng) else {
            return vec![];
        };
        let Some(market) = state.market(symbol) else {
            return vec![];
        };
        let Some(entry) = market.bid.or(market.fair()) else {
            return vec![];
        };
        let qty = self.size(state, entry);
        if qty == 0 {
            return vec![];
        }
        vec![Action::Bracket {
            symbol,
            side: Side::Buy,
            qty,
            entry: OrderKind::Limit(entry),
            take_profit: scale(entry, self.target_pct * 100.0).max(entry + 1),
            stop_loss: scale(entry, -self.stop_pct * 100.0).min(entry - 1),
        }]
    }

    fn investor(&mut self, state: &AgentState, rng: &mut StdRng) -> Vec<Action> {
        let mut actions = Vec::new();
        for symbol in self.symbols.clone() {
            let held = state.holding(symbol).available;
            let Some(last) = state.market(symbol).and_then(|m| m.fair()) else {
                continue;
            };
            if held > 0
                && state.trading(symbol)
                && Self::armed(state, symbol, Side::Sell).is_empty()
            {
                actions.push(Action::Conditional {
                    symbol,
                    side: Side::Sell,
                    qty: held,
                    trigger: Trigger::TrailingBelow((last * self.trail_pct / 100).max(1)),
                    kind: OrderKind::Market,
                });
                return actions;
            }
        }
        if !rng.random_bool(self.activity) {
            return actions;
        }
        let Some(symbol) = pick(&self.symbols, rng) else {
            return actions;
        };
        if !Self::armed(state, symbol, Side::Buy).is_empty() {
            return actions;
        }
        let Some(last) = state.market(symbol).and_then(|m| m.fair()) else {
            return actions;
        };
        if state.phase == MarketPhase::Closed {
            let limit = scale(last, 100.0);
            let qty = self.size(state, limit);
            if qty > 0 {
                actions.push(Action::Conditional {
                    symbol,
                    side: Side::Buy,
                    qty,
                    trigger: Trigger::MarketOpens,
                    kind: OrderKind::Limit(limit),
                });
            }
        } else if state.trading(symbol) {
            let level = scale(last, -self.dip_pct * 100.0);
            let qty = self.size(state, level);
            if qty > 0 {
                actions.push(Action::Conditional {
                    symbol,
                    side: Side::Buy,
                    qty,
                    trigger: Trigger::Below(level),
                    kind: OrderKind::Limit(level),
                });
            }
        }
        actions
    }
}

impl Strategy for RetailPersona {
    fn name(&self) -> &'static str {
        match self.persona {
            Persona::DayTrader => "retail day trader",
            Persona::Swing => "retail swing trader",
            Persona::Investor => "retail investor",
        }
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(match self.persona {
            Persona::DayTrader => 1_000,
            Persona::Swing => 2_000,
            Persona::Investor => 3_000,
        })
    }

    fn on_tick(&mut self, state: &AgentState, rng: &mut StdRng) -> Vec<Action> {
        if state.busy() {
            return vec![];
        }
        match self.persona {
            Persona::DayTrader => self.day_trader(state, rng),
            Persona::Swing => self.swing(state, rng),
            Persona::Investor => self.investor(state, rng),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{acme, hold, state_with};
    use ome_protocol::{ConditionalReport, ConditionalStatus};
    use rand::SeedableRng;

    fn always() -> Params {
        Params([("activity".to_string(), "1".to_string())].into())
    }

    fn rising(state: &mut AgentState) {
        let m = state.markets.get_mut(&acme()).unwrap();
        m.prices = (0..30).map(|i| 10_000 + i * 10).collect();
        m.last = Some(10_290);
    }

    #[test]
    fn day_trader_buys_momentum_protects_with_a_stop_and_flattens_before_the_close() {
        let mut rng = StdRng::seed_from_u64(4);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        rising(&mut state);
        let mut trader = RetailPersona::new(Persona::DayTrader, vec![acme()], &always());
        let buy = trader.on_tick(&state, &mut rng);
        assert!(matches!(
            buy[..],
            [Action::Place {
                side: Side::Buy,
                kind: OrderKind::Market,
                ..
            }]
        ));
        hold(&mut state, acme(), 10);
        let stop = trader.on_tick(&state, &mut rng);
        assert!(matches!(
            stop[..],
            [Action::Conditional {
                side: Side::Sell,
                qty: 10,
                trigger: Trigger::Below(_),
                ..
            }]
        ));
        state.armed.insert(
            1,
            ConditionalReport {
                cond_id: 1,
                client_order_id: 0,
                account: 1,
                symbol: acme(),
                side: Side::Sell,
                qty: 10,
                status: ConditionalStatus::Armed,
                trigger: Trigger::Below(10_000),
                level: 10_000,
                reason: String::new(),
            },
        );
        state.next_change_at = state.now + 5_000;
        let flatten = trader.on_tick(&state, &mut rng);
        assert_eq!(flatten[0], Action::CancelConditional(1));
        assert!(matches!(
            flatten[1],
            Action::Place {
                side: Side::Sell,
                qty: 10,
                ..
            }
        ));
    }

    #[test]
    fn swing_trader_buys_dips_with_a_bracket() {
        let mut rng = StdRng::seed_from_u64(5);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        let m = state.markets.get_mut(&acme()).unwrap();
        m.prices = (0..100).map(|i| 10_400 - i * 4).collect();
        let mut swing = RetailPersona::new(Persona::Swing, vec![acme()], &always());
        let actions = swing.on_tick(&state, &mut rng);
        let [
            Action::Bracket {
                entry: OrderKind::Limit(entry),
                take_profit,
                stop_loss,
                ..
            },
        ] = actions[..]
        else {
            panic!("expected a bracket, got {actions:?}");
        };
        assert!(stop_loss < entry && entry < take_profit);
    }

    #[test]
    fn investor_uses_gtt_dips_after_market_orders_and_trailing_stops() {
        let mut rng = StdRng::seed_from_u64(6);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        let mut investor = RetailPersona::new(Persona::Investor, vec![acme()], &always());
        let gtt = investor.on_tick(&state, &mut rng);
        assert!(matches!(
            gtt[..],
            [Action::Conditional {
                side: Side::Buy,
                trigger: Trigger::Below(9_800),
                kind: OrderKind::Limit(9_800),
                ..
            }]
        ));
        state.phase = MarketPhase::Closed;
        let amo = investor.on_tick(&state, &mut rng);
        assert!(matches!(
            amo[..],
            [Action::Conditional {
                trigger: Trigger::MarketOpens,
                ..
            }]
        ));
        state.phase = MarketPhase::Open;
        hold(&mut state, acme(), 40);
        let trail = investor.on_tick(&state, &mut rng);
        assert!(matches!(
            trail[..],
            [Action::Conditional {
                side: Side::Sell,
                qty: 40,
                trigger: Trigger::TrailingBelow(800),
                ..
            }]
        ));
    }
}
