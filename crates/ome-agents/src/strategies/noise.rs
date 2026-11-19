use super::{Params, Strategy, affordable, pick};
use crate::action::Action;
use crate::edge::taking_cost_bps;
use crate::state::{AgentState, SESSION_MINUTES, Wake};
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Qty, Side};
use rand::Rng;
use rand::rngs::StdRng;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Household {
    pub notice: f64,
    pub notice_move_bps: f64,
    pub chase: f64,
    pub panic_pct: f64,
    pub stake: f64,
    pub next_need: u64,
}

pub struct NoiseTrader {
    symbols: Vec<Symbol>,
    pub need_days: f64,
    pub patience_bps: f64,
    pub fee_bps: f64,
    pub order_ttl_minutes: u64,
    pub household: Option<Household>,
    noticed: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Wish {
    symbol: Symbol,
    side: Side,
    share: f64,
}

impl NoiseTrader {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        Self {
            symbols,
            need_days: params.f64("need_days", 4.0),
            patience_bps: params.f64("patience_bps", 15.0),
            fee_bps: params.f64("fee_bps", 5.0),
            order_ttl_minutes: params.u64("order_ttl_minutes", 60),
            household: None,
            noticed: None,
        }
    }

    fn wait(&self, rng: &mut StdRng) -> u64 {
        let u: f64 = rng.random_range(f64::EPSILON..1.0);
        (-u.ln() * self.need_days * SESSION_MINUTES as f64).ceil() as u64
    }

    fn household(&mut self, state: &AgentState, rng: &mut StdRng) -> Household {
        if let Some(h) = self.household {
            return h;
        }
        let h = Household {
            notice: rng.random_range(0.3..1.2),
            notice_move_bps: rng.random_range(100.0..400.0),
            chase: rng.random_range(-0.5..1.0),
            panic_pct: rng.random_range(10.0..25.0),
            stake: rng.random_range(0.05..0.2),
            next_need: state.clock() + self.wait(rng),
        };
        self.household = Some(h);
        h
    }

    fn drawdown_pct(state: &AgentState, symbol: Symbol) -> Option<f64> {
        let market = state.market(symbol)?;
        let price = market.fair()? as f64;
        let peak = market.days.iter().rev().take(5).map(|d| d.high).max()? as f64;
        Some((1.0 - price / peak) * 100.0)
    }

    fn favourite(&self, state: &AgentState, chase: f64, rng: &mut StdRng) -> Option<Symbol> {
        let scored: Vec<(Symbol, f64)> = self
            .symbols
            .iter()
            .filter_map(|s| Some((*s, state.market(*s)?.day_change(5)? * chase.signum())))
            .collect();
        if scored.len() < self.symbols.len() {
            return pick(&self.symbols, rng);
        }
        scored.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(s, _)| s)
    }

    fn wishes(&mut self, state: &AgentState, h: Household, rng: &mut StdRng) -> Vec<Wish> {
        let mut wishes = Vec::new();
        let alert = state.world.activity.max(0.1);
        let follow = |signal: f64| if (signal > 0.0) == (h.chase >= 0.0) { Side::Buy } else { Side::Sell };
        let after = *self.noticed.get_or_insert(state.news.last_seq());
        self.noticed = Some(state.news.last_seq());
        for symbol in &self.symbols {
            let story: f64 = state
                .news
                .after(after)
                .filter(|i| state.news.about(i, *symbol))
                .map(|i| i.sentiment as f64 / 100.0 + i.impact_bps as f64 / 300.0)
                .sum();
            if story.abs() >= h.notice / alert {
                wishes.push(Wish { symbol: *symbol, side: follow(story), share: h.stake });
            }
            if state.wake.moved.contains(symbol)
                && let Some(change) = state.change_bps(*symbol, 60)
                && change.abs() >= h.notice_move_bps / alert
            {
                wishes.push(Wish { symbol: *symbol, side: follow(change), share: h.stake });
            }
            if (state.wake.day || state.wake.moved.contains(symbol))
                && state.holding(*symbol).available > 0
                && Self::drawdown_pct(state, *symbol).is_some_and(|d| d >= h.panic_pct)
            {
                wishes.push(Wish { symbol: *symbol, side: Side::Sell, share: 1.0 });
            }
        }
        if state.clock() >= h.next_need && state.continuous() {
            let wait = self.wait(rng);
            if let Some(h) = self.household.as_mut() {
                h.next_need = state.clock() + wait;
            }
            let equity = state.equity().max(1) as f64;
            let idle = state.cash as f64 / equity;
            let mood: f64 = self.symbols.iter().map(|s| state.sentiment(*s)).sum::<f64>() * h.chase.signum();
            let saving = idle > 0.5 || (idle > 0.15 && mood >= 0.0);
            if saving {
                if let Some(symbol) = self.favourite(state, h.chase, rng) {
                    wishes.push(Wish { symbol, side: Side::Buy, share: h.stake });
                }
            } else if let Some(symbol) = self
                .symbols
                .iter()
                .copied()
                .max_by_key(|s| state.holding(*s).available * state.market(*s).and_then(|m| m.fair()).unwrap_or(0))
            {
                wishes.push(Wish { symbol, side: Side::Sell, share: h.stake * 2.0 });
            }
        }
        wishes
    }
}

impl Strategy for NoiseTrader {
    fn name(&self) -> &'static str {
        "household"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(1_000)
    }

    fn alert_bps(&self) -> Option<f64> {
        Some(self.household.map_or(100.0, |h| h.notice_move_bps / 2.0))
    }

    fn wakes(&self, wake: &Wake) -> bool {
        wake.news || wake.day || !wake.moved.is_empty()
    }

    fn on_tick(&mut self, state: &AgentState, rng: &mut StdRng) -> Vec<Action> {
        let h = self.household(state, rng);
        let ttl = self.order_ttl_minutes * state.minute_ms();
        let mut actions = super::cancel_stale(state, None, ttl);
        let wishes = self.wishes(state, h, rng);
        if state.busy() {
            return actions;
        }
        let equity = state.equity() as f64;
        for wish in wishes {
            if !state.trading(wish.symbol) || state.open_in(wish.symbol) > 0 {
                continue;
            }
            let Some(market) = state.market(wish.symbol) else { continue };
            let Some(price) = market.fair() else { continue };
            let qty: Qty = match wish.side {
                Side::Buy => affordable(state.cash, price + price / 50, (equity * wish.share / price as f64) as u64),
                Side::Sell => ((state.holding(wish.symbol).available as f64 * (wish.share * 2.0).min(1.0)).ceil() as u64)
                    .min(state.holding(wish.symbol).available),
            };
            if qty == 0 {
                continue;
            }
            let cost = taking_cost_bps(state, wish.symbol, wish.side, qty, self.fee_bps);
            let urgent = wish.share >= 1.0;
            let kind = match (cost, wish.side) {
                (Some(cost), _) if urgent || cost <= self.patience_bps => OrderKind::Market,
                (_, Side::Buy) => OrderKind::Limit(market.bid.unwrap_or(price)),
                (_, Side::Sell) => OrderKind::Limit(market.ask.unwrap_or(price)),
            };
            actions.push(Action::Place { symbol: wish.symbol, side: wish.side, kind, qty });
            break;
        }
        actions
    }

    fn detail(&self) -> String {
        self.household
            .map(|h| {
                format!(
                    "{} · notices stories above {:.1} and moves above {:.1}% · sells out {:.0}% below a recent high",
                    if h.chase >= 0.0 { "follows the crowd" } else { "goes against the crowd" },
                    h.notice,
                    h.notice_move_bps / 100.0,
                    h.panic_pct
                )
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::news::tests::story;
    use crate::state::tests::{acme, bars, days, hold, state_with};
    use ome_protocol::NewsKind;
    use rand::SeedableRng;

    fn household(chase: f64) -> Household {
        Household { notice: 0.5, notice_move_bps: 200.0, chase, panic_pct: 15.0, stake: 0.1, next_need: u64::MAX }
    }

    fn placed(actions: &[Action]) -> Vec<(Side, OrderKind)> {
        actions.iter().filter_map(|a| if let Action::Place { side, kind, .. } = a { Some((*side, *kind)) } else { None }).collect()
    }

    #[test]
    fn a_household_sits_still_until_something_gives_it_a_reason() {
        let mut rng = StdRng::seed_from_u64(3);
        let mut state = state_with(&[(acme(), 10_000)], 50_000_00);
        let mut trader = NoiseTrader::new(vec![acme()], &Params::default());
        trader.household = Some(household(1.0));
        for _ in 0..500 {
            state.now += 1_000;
            assert!(trader.on_tick(&state, &mut rng).is_empty(), "no news, no move, no need: no order");
        }
        state.news.absorb(&story(1, NewsKind::Company, "ACME", 20, state.now));
        assert!(trader.on_tick(&state, &mut rng).is_empty(), "a small story is not noticed");
        state.news.absorb(&story(2, NewsKind::Earnings, "ACME", 80, state.now));
        assert_eq!(placed(&trader.on_tick(&state, &mut rng)), [(Side::Buy, OrderKind::Market)], "a big one is, and the crowd buys it");

        let mut contrarian = NoiseTrader::new(vec![acme()], &Params::default());
        contrarian.household = Some(household(-0.3));
        hold(&mut state, acme(), 100);
        contrarian.on_tick(&state, &mut rng);
        state.news.absorb(&story(3, NewsKind::Earnings, "ACME", 80, state.now));
        assert!(matches!(placed(&contrarian.on_tick(&state, &mut rng))[..], [(Side::Sell, _)]), "others sell into good news");
    }

    #[test]
    fn its_own_needs_arrive_in_trading_days_and_wide_spreads_make_it_patient() {
        let mut rng = StdRng::seed_from_u64(5);
        let mut state = state_with(&[(acme(), 10_000)], 50_000_00);
        let mut trader = NoiseTrader::new(vec![acme()], &Params::default());
        trader.household = Some(Household { next_need: state.clock() + 10, ..household(1.0) });
        assert!(trader.on_tick(&state, &mut rng).is_empty());
        state.now += 11 * state.minute_ms();
        assert_eq!(placed(&trader.on_tick(&state, &mut rng)), [(Side::Buy, OrderKind::Market)], "savings go into the market");
        let next = trader.household.unwrap().next_need;
        assert!(next > state.clock(), "and the next need is days away");

        let mut wide = state_with(&[(acme(), 10_000)], 50_000_00);
        let market = wide.markets.get_mut(&acme()).unwrap();
        market.bid = Some(9_900);
        market.ask = Some(10_100);
        market.bids[0].price = 9_900;
        market.asks[0].price = 10_100;
        let mut patient = NoiseTrader::new(vec![acme()], &Params::default());
        patient.household = Some(Household { next_need: 0, ..household(1.0) });
        assert_eq!(placed(&patient.on_tick(&wide, &mut rng)), [(Side::Buy, OrderKind::Limit(9_900))], "crossing 1% is not worth it: it joins the bid");
    }

    #[test]
    fn a_deep_drawdown_makes_it_sell_everything() {
        let mut rng = StdRng::seed_from_u64(7);
        let mut state = state_with(&[(acme(), 10_000)], 1_000_00);
        hold(&mut state, acme(), 80);
        days(&mut state, acme(), &[12_000, 12_400, 11_800]);
        bars(&mut state, acme(), &[10_300]);
        let mut trader = NoiseTrader::new(vec![acme()], &Params::default());
        trader.household = Some(household(1.0));
        state.wake.day = true;
        let actions = trader.on_tick(&state, &mut rng);
        assert!(matches!(actions[..], [Action::Place { side: Side::Sell, kind: OrderKind::Market, qty: 80, .. }]), "{actions:?}");
    }
}
