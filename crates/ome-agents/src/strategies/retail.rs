use super::{Params, Strategy, affordable, pick, scale};
use crate::action::Action;
use crate::edge::taking_cost_bps;
use crate::state::{AgentState, SESSION_MINUTES, Wake};
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
    pub budget_pct: f64,
    pub stop_pct: f64,
    pub target_pct: f64,
    pub dip_pct: f64,
    pub trail_pct: u64,
    pub fee_bps: f64,
    pub hurdle_bps: f64,
    pub look_minutes: u64,
    pub hold_days: u64,
    pub sip_days: u64,
    entries: BTreeMap<Symbol, Entry>,
    next_sip: Option<u64>,
    dipped: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Entry {
    price: Price,
    day: u64,
    target_bps: f64,
    stop_bps: f64,
}

impl RetailPersona {
    pub fn new(persona: Persona, symbols: Vec<Symbol>, params: &Params) -> Self {
        let (budget, stop, target, dip, hurdle) = match persona {
            Persona::DayTrader => (0.15, 1.0, 1.5, 0.0, 10.0),
            Persona::Swing => (0.20, 2.0, 3.0, 0.0, 20.0),
            Persona::Investor => (0.10, 0.0, 0.0, 2.0, 0.0),
        };
        Self {
            persona,
            symbols,
            budget_pct: params.f64("budget_pct", budget),
            stop_pct: params.f64("stop_pct", stop),
            target_pct: params.f64("target_pct", target),
            dip_pct: params.f64("dip_pct", dip),
            trail_pct: params.u64("trail_pct", 12),
            fee_bps: params.f64("fee_bps", 5.0),
            hurdle_bps: params.f64("hurdle_bps", hurdle),
            look_minutes: params.u64("look_minutes", 15),
            hold_days: params.u64("hold_days", 5),
            sip_days: params.u64("sip_days", 5),
            entries: BTreeMap::new(),
            next_sip: None,
            dipped: None,
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

    fn exit(state: &AgentState, symbol: Symbol, held: Qty) -> Vec<Action> {
        let mut actions: Vec<Action> = Self::armed(state, symbol, Side::Sell).into_iter().map(Action::CancelConditional).collect();
        actions.extend(state.open_orders.iter().filter(|(_, o)| o.symbol == symbol).map(|(id, _)| Action::Cancel(*id)));
        actions.push(Action::Place { symbol, side: Side::Sell, kind: OrderKind::Market, qty: held });
        actions
    }

    fn day_trader(&mut self, state: &AgentState) -> Vec<Action> {
        let mut actions = Vec::new();
        for symbol in self.symbols.clone() {
            let held = state.holding(symbol).available;
            let Some(last) = state.market(symbol).and_then(|m| m.fair()) else {
                continue;
            };
            if held == 0 {
                if state.holding(symbol).total() == 0 {
                    self.entries.remove(&symbol);
                }
                continue;
            }
            if !state.trading(symbol) {
                continue;
            }
            let entry = *self.entries.entry(symbol).or_insert(Entry {
                price: last,
                day: state.day,
                target_bps: self.target_pct * 100.0,
                stop_bps: self.stop_pct * 100.0,
            });
            let target_hit = last >= scale(entry.price, entry.target_bps);
            if state.sentiment(symbol) < -0.5 || state.closing_soon() || target_hit {
                return Self::exit(state, symbol, held);
            }
            if Self::armed(state, symbol, Side::Sell).is_empty() && state.open_in(symbol) == 0 {
                actions.push(Action::Conditional {
                    symbol,
                    side: Side::Sell,
                    qty: held,
                    trigger: Trigger::Below(scale(entry.price, -entry.stop_bps).min(last.saturating_sub(1)).max(1)),
                    kind: OrderKind::Market,
                });
                return actions;
            }
        }
        if state.closing_soon() {
            return actions;
        }
        let look = self.look_minutes;
        let mut best: Option<(f64, Symbol, Price, Qty, f64)> = None;
        for symbol in &self.symbols {
            if !state.trading(*symbol) || state.holding(*symbol).total() > 0 || state.open_in(*symbol) > 0 {
                continue;
            }
            let (Some(change), Some(vol)) = (state.change_bps(*symbol, look), state.minute_vol(*symbol, 60)) else { continue };
            let noise = vol * (look as f64).sqrt();
            if change <= 1.5 * noise / state.world.activity.max(0.1) {
                continue;
            }
            let Some(price) = state.market(*symbol).and_then(|m| m.ask.or(m.fair())) else { continue };
            let qty = self.size(state, scale(price, 200.0));
            let Some(cost) = taking_cost_bps(state, *symbol, Side::Buy, qty.max(1), self.fee_bps) else { continue };
            let edge = change / 2.0 - 2.0 * cost - self.hurdle_bps;
            if qty > 0 && edge > 0.0 && best.is_none_or(|b| edge > b.0) {
                best = Some((edge, *symbol, price, qty, noise));
            }
        }
        if let Some((_, symbol, price, qty, noise)) = best {
            self.entries.insert(
                symbol,
                Entry {
                    price,
                    day: state.day,
                    target_bps: (3.0 * noise).max(self.target_pct * 100.0 / 3.0),
                    stop_bps: (2.0 * noise).max(self.stop_pct * 100.0 / 3.0),
                },
            );
            actions.push(Action::Place { symbol, side: Side::Buy, kind: OrderKind::Market, qty });
        }
        actions
    }

    fn dip_bps(state: &AgentState, symbol: Symbol) -> Option<(f64, f64)> {
        let market = state.market(symbol)?;
        match (market.day_change(3), market.daily_vol_bps()) {
            (Some(change), Some(vol)) => Some((-change, vol)),
            _ => {
                let clock = state.clock();
                let vol = market.minute_vol_bps(clock, SESSION_MINUTES)? * (SESSION_MINUTES as f64).sqrt();
                let mean = market.mean_over(clock, SESSION_MINUTES)?;
                Some(((1.0 - market.fair()? as f64 / mean) * 10_000.0, vol))
            }
        }
    }

    fn swing(&mut self, state: &AgentState) -> Vec<Action> {
        for symbol in self.symbols.clone() {
            let held = state.holding(symbol);
            if held.total() == 0 {
                if state.armed_in(symbol) == 0 && state.open_in(symbol) == 0 {
                    self.entries.remove(&symbol);
                }
                continue;
            }
            let entered = self.entries.entry(symbol).or_insert(Entry { price: 0, day: state.day, target_bps: 0.0, stop_bps: 0.0 }).day;
            if state.trading(symbol) && held.total() > 0 && state.day >= entered + self.hold_days {
                let mut actions = Self::exit(state, symbol, held.total());
                actions.pop();
                if held.available > 0 && actions.is_empty() {
                    actions.push(Action::Place { symbol, side: Side::Sell, kind: OrderKind::Market, qty: held.available });
                }
                return actions;
            }
        }
        let mut best: Option<(f64, Symbol, Price, Qty, f64)> = None;
        for symbol in &self.symbols {
            if !state.trading(*symbol) || state.holding(*symbol).total() > 0 || state.armed_in(*symbol) > 0 || state.open_in(*symbol) > 0 {
                continue;
            }
            let Some((dip, vol)) = Self::dip_bps(state, *symbol) else { continue };
            if dip < vol {
                continue;
            }
            let Some(entry) = state.market(*symbol).and_then(|m| m.bid.or(m.fair())) else { continue };
            let qty = self.size(state, entry);
            let edge = dip / 2.0 - 2.0 * self.fee_bps - self.hurdle_bps;
            if qty > 0 && edge > 0.0 && best.is_none_or(|b| edge > b.0) {
                best = Some((edge, *symbol, entry, qty, vol));
            }
        }
        let Some((_, symbol, entry, qty, vol)) = best else { return vec![] };
        let target_bps = (1.5 * vol).max(self.target_pct * 100.0 / 3.0);
        let stop_bps = vol.max(self.stop_pct * 100.0 / 3.0);
        self.entries.insert(symbol, Entry { price: entry, day: state.day, target_bps, stop_bps });
        vec![Action::Bracket {
            symbol,
            side: Side::Buy,
            qty,
            entry: OrderKind::Limit(entry),
            take_profit: scale(entry, target_bps).max(entry + 1),
            stop_loss: scale(entry, -stop_bps).min(entry - 1),
        }]
    }

    fn lightest(&self, state: &AgentState) -> Option<Symbol> {
        self.symbols
            .iter()
            .copied()
            .filter(|s| state.market(*s).and_then(|m| m.fair()).is_some())
            .min_by_key(|s| state.holding(*s).total() * state.market(*s).and_then(|m| m.fair()).unwrap_or(0))
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
        let clock = state.clock();
        let due = *self.next_sip.get_or_insert_with(|| clock + rng.random_range(0..self.sip_days.max(1) * SESSION_MINUTES));
        if clock >= due {
            let Some(symbol) = self.lightest(state) else { return actions };
            let Some(market) = state.market(symbol) else { return actions };
            let Some(last) = market.fair() else { return actions };
            if state.phase == MarketPhase::Closed {
                let limit = scale(last, 100.0);
                let qty = self.size(state, limit);
                self.next_sip = Some(clock + self.sip_days * SESSION_MINUTES);
                if qty > 0 {
                    actions.push(Action::Conditional {
                        symbol,
                        side: Side::Buy,
                        qty,
                        trigger: Trigger::MarketOpens,
                        kind: OrderKind::Limit(limit),
                    });
                }
                return actions;
            }
            if state.trading(symbol) && state.open_in(symbol) == 0 {
                self.next_sip = Some(clock + self.sip_days * SESSION_MINUTES);
                let ask = market.ask.unwrap_or(last);
                let qty = self.size(state, ask);
                let patient = taking_cost_bps(state, symbol, Side::Buy, qty.max(1), self.fee_bps).is_none_or(|cost| cost > 25.0);
                let limit = if patient { market.bid.unwrap_or(last) } else { ask };
                if qty > 0 {
                    actions.push(Action::Place { symbol, side: Side::Buy, kind: OrderKind::Limit(limit), qty });
                }
                return actions;
            }
        }
        if self.dipped != Some(state.day) && state.continuous() {
            self.dipped = Some(state.day);
            let Some(symbol) = pick(&self.symbols, rng) else { return actions };
            if !state.trading(symbol) || !Self::armed(state, symbol, Side::Buy).is_empty() {
                return actions;
            }
            let Some(last) = state.market(symbol).and_then(|m| m.fair()) else { return actions };
            let dip = state.daily_vol(symbol).map_or(self.dip_pct * 100.0, |vol| (1.5 * vol).max(self.dip_pct * 100.0 / 2.0));
            let level = scale(last, -dip);
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
            Persona::Swing => 3_000,
            Persona::Investor => 5_000,
        })
    }

    fn alert_bps(&self) -> Option<f64> {
        Some(match self.persona {
            Persona::DayTrader => 30.0,
            Persona::Swing => 100.0,
            Persona::Investor => 200.0,
        })
    }

    fn wakes(&self, wake: &Wake) -> bool {
        match self.persona {
            Persona::DayTrader => wake.any(),
            Persona::Swing | Persona::Investor => wake.day || wake.phase || wake.fill || !wake.moved.is_empty(),
        }
    }

    fn on_tick(&mut self, state: &AgentState, rng: &mut StdRng) -> Vec<Action> {
        if state.busy() {
            return vec![];
        }
        match self.persona {
            Persona::DayTrader => self.day_trader(state),
            Persona::Swing => self.swing(state),
            Persona::Investor => self.investor(state, rng),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{acme, bars, days, hold, state_with};
    use ome_protocol::{ConditionalReport, ConditionalStatus};
    use rand::SeedableRng;

    fn rising(state: &mut AgentState) {
        let closes: Vec<Price> = (0..60).map(|i| 10_000 + (i % 2) * 2 + if i > 45 { (i - 45) * 12 } else { 0 }).collect();
        bars(state, acme(), &closes);
    }

    #[test]
    fn day_trader_buys_a_move_that_stands_out_protects_it_and_is_flat_by_the_close() {
        let mut rng = StdRng::seed_from_u64(4);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        let mut trader = RetailPersona::new(Persona::DayTrader, vec![acme()], &Params::default());
        let quiet: Vec<Price> = (0..60).map(|i| 10_000 + (i % 2) * 2).collect();
        bars(&mut state, acme(), &quiet);
        assert!(trader.on_tick(&state, &mut rng).is_empty(), "nothing stands out of the noise");
        rising(&mut state);
        let buy = trader.on_tick(&state, &mut rng);
        assert!(matches!(buy[..], [Action::Place { side: Side::Buy, kind: OrderKind::Market, .. }]), "{buy:?}");

        let mut costly = RetailPersona::new(Persona::DayTrader, vec![acme()], &Params::default());
        let market = state.markets.get_mut(&acme()).unwrap();
        let (bid, ask) = (market.bids[0].price, market.asks[0].price);
        market.asks[0].price = ask + 150;
        assert!(costly.on_tick(&state, &mut rng).is_empty(), "the same move is not worth a wide spread");
        let market = state.markets.get_mut(&acme()).unwrap();
        market.asks[0].price = ask;
        assert_eq!(market.bids[0].price, bid);

        hold(&mut state, acme(), 10);
        let stop = trader.on_tick(&state, &mut rng);
        assert!(matches!(stop[..], [Action::Conditional { side: Side::Sell, qty: 10, trigger: Trigger::Below(_), .. }]));
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
        assert!(trader.on_tick(&state, &mut rng).is_empty(), "protected: it waits");
        state.next_change_at = state.now + 5_000;
        let flatten = trader.on_tick(&state, &mut rng);
        assert_eq!(flatten[0], Action::CancelConditional(1));
        assert!(matches!(flatten[1], Action::Place { side: Side::Sell, qty: 10, .. }));
    }

    #[test]
    fn swing_trader_buys_a_multi_day_dip_with_a_bracket_and_gives_up_after_some_days() {
        let mut rng = StdRng::seed_from_u64(5);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        let mut swing = RetailPersona::new(Persona::Swing, vec![acme()], &Params::default());
        days(&mut state, acme(), &[10_000, 10_100, 10_050, 10_150, 10_100]);
        assert!(swing.on_tick(&state, &mut rng).is_empty(), "no dip, no trade");
        bars(&mut state, acme(), &[9_700]);
        let actions = swing.on_tick(&state, &mut rng);
        let [Action::Bracket { entry: OrderKind::Limit(entry), take_profit, stop_loss, .. }] = actions[..] else {
            panic!("expected a bracket, got {actions:?}");
        };
        assert!(stop_loss < entry && entry < take_profit);
        hold(&mut state, acme(), 20);
        assert!(swing.on_tick(&state, &mut rng).is_empty());
        state.day += 5;
        let exit = swing.on_tick(&state, &mut rng);
        assert!(matches!(exit[..], [Action::Place { side: Side::Sell, qty: 20, .. }]), "{exit:?}");
    }

    #[test]
    fn investor_saves_on_a_schedule_of_trading_days_and_keeps_a_trailing_stop() {
        let mut rng = StdRng::seed_from_u64(6);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        let mut investor = RetailPersona::new(Persona::Investor, vec![acme()], &Params::default());
        investor.next_sip = Some(state.clock() + 100);
        let gtt = investor.on_tick(&state, &mut rng);
        assert!(matches!(gtt[..], [Action::Conditional { side: Side::Buy, trigger: Trigger::Below(9_800), kind: OrderKind::Limit(9_800), .. }]), "{gtt:?}");
        assert!(investor.on_tick(&state, &mut rng).is_empty(), "one dip order a day, and the next saving is not due");
        state.now += 101 * state.minute_ms();
        let sip = investor.on_tick(&state, &mut rng);
        assert!(matches!(sip[..], [Action::Place { side: Side::Buy, kind: OrderKind::Limit(10_005), .. }]), "{sip:?}");
        assert_eq!(investor.next_sip, Some(state.clock() + 5 * SESSION_MINUTES));
        state.phase = MarketPhase::Closed;
        investor.next_sip = Some(0);
        let amo = investor.on_tick(&state, &mut rng);
        assert!(matches!(amo[..], [Action::Conditional { trigger: Trigger::MarketOpens, .. }]));
        state.phase = MarketPhase::Open;
        hold(&mut state, acme(), 40);
        let trail = investor.on_tick(&state, &mut rng);
        assert!(matches!(trail[..], [Action::Conditional { side: Side::Sell, qty: 40, trigger: Trigger::TrailingBelow(1_200), .. }]));
    }
}
