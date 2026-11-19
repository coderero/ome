use super::{Params, Strategy, affordable, cancel_stale, scale};
use crate::action::Action;
use crate::edge::taking_cost_bps;
use crate::state::{AgentState, Wake};
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Side};
use ome_protocol::{NewsItem, NewsKind};
use rand::rngs::StdRng;
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trade {
    pub side: Side,
    pub qty: u64,
    pub at: u64,
    pub target: Price,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Idea {
    expected_bps: f64,
    seen_at: Price,
    clock: u64,
}

pub struct NewsFund {
    symbols: Vec<Symbol>,
    pub size_pct: f64,
    pub hold_minutes: u64,
    pub max_age_minutes: u64,
    pub cross_bps: f64,
    pub fee_bps: f64,
    pub hurdle_bps: f64,
    seen: Option<u64>,
    ideas: BTreeMap<Symbol, Idea>,
    pub open: BTreeMap<Symbol, Trade>,
}

pub fn expected_move_bps(item: &NewsItem) -> f64 {
    let relevance = match item.kind {
        NewsKind::Macro => 0.5,
        NewsKind::Sector => 0.7,
        NewsKind::Prospectus => 0.0,
        _ => 1.0,
    };
    relevance * (item.impact_bps as f64 + item.sentiment as f64 * 0.5)
}

impl NewsFund {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        Self {
            symbols,
            size_pct: params.f64("size_pct", 0.06),
            hold_minutes: params.u64("hold_minutes", 120),
            max_age_minutes: params.u64("max_age_minutes", 30),
            cross_bps: params.f64("cross_bps", 10.0),
            fee_bps: params.f64("fee_bps", 2.0),
            hurdle_bps: params.f64("hurdle_bps", 10.0),
            seen: None,
            ideas: BTreeMap::new(),
            open: BTreeMap::new(),
        }
    }

    fn read(&mut self, state: &AgentState) {
        let after = *self.seen.get_or_insert(state.news.last_seq());
        self.seen = Some(state.news.last_seq());
        let fresh = self.max_age_minutes * state.minute_ms();
        for symbol in &self.symbols {
            let expected: f64 = state
                .news
                .after(after)
                .filter(|i| state.now.saturating_sub(i.time) <= fresh && state.news.about(i, *symbol))
                .map(expected_move_bps)
                .sum();
            let Some(price) = state.market(*symbol).and_then(|m| m.mid().or(m.fair())) else { continue };
            if expected != 0.0 {
                let idea = self.ideas.entry(*symbol).or_insert(Idea { expected_bps: 0.0, seen_at: price, clock: state.clock() });
                idea.expected_bps += expected;
            }
        }
        let clock = state.clock();
        let age = self.max_age_minutes;
        self.ideas.retain(|_, idea| clock <= idea.clock + age);
    }

    pub fn remaining_bps(&self, state: &AgentState, symbol: Symbol) -> Option<f64> {
        let idea = self.ideas.get(&symbol)?;
        let price = state.market(symbol).and_then(|m| m.mid().or(m.fair()))? as f64;
        Some(idea.expected_bps - (price / idea.seen_at as f64 - 1.0) * 10_000.0)
    }

    fn order(&self, state: &AgentState, symbol: Symbol, side: Side, qty: u64) -> Option<Action> {
        let market = state.market(symbol)?;
        let price = match side {
            Side::Buy => scale(market.ask.or(market.fair())?, self.cross_bps),
            Side::Sell => scale(market.bid.or(market.fair())?, -self.cross_bps),
        };
        let qty = match side {
            Side::Buy => affordable(state.cash, price, qty),
            Side::Sell => qty.min(state.holding(symbol).available),
        };
        (qty > 0).then_some(Action::Place {
            symbol,
            side,
            kind: OrderKind::Limit(price),
            qty,
        })
    }
}

impl Strategy for NewsFund {
    fn name(&self) -> &'static str {
        "news-driven fund"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(1_000)
    }

    fn alert_bps(&self) -> Option<f64> {
        Some(40.0)
    }

    fn wakes(&self, wake: &Wake) -> bool {
        wake.news || wake.fill || (!wake.moved.is_empty() && !self.open.is_empty())
    }

    fn on_tick(&mut self, state: &AgentState, _rng: &mut StdRng) -> Vec<Action> {
        let mut actions = cancel_stale(state, None, 5 * state.minute_ms());
        self.read(state);
        if state.busy() {
            return actions;
        }
        let clock = state.clock();
        for symbol in self.symbols.clone() {
            if !state.trading(symbol) || state.open_in(symbol) > 0 {
                continue;
            }
            let Some(price) = state.market(symbol).and_then(|m| m.fair()) else {
                continue;
            };
            if let Some(trade) = self.open.get(&symbol).copied() {
                let reached = match trade.side {
                    Side::Buy => price >= trade.target,
                    Side::Sell => price <= trade.target,
                };
                if reached || clock >= trade.at + self.hold_minutes {
                    self.open.remove(&symbol);
                    self.ideas.remove(&symbol);
                    let back = match trade.side {
                        Side::Buy => Side::Sell,
                        Side::Sell => Side::Buy,
                    };
                    actions.extend(self.order(state, symbol, back, trade.qty));
                }
                continue;
            }
            let Some(remaining) = self.remaining_bps(state, symbol) else { continue };
            let side = if remaining > 0.0 { Side::Buy } else { Side::Sell };
            let budget = state.equity() as f64 * self.size_pct * (remaining.abs() / 50.0).min(2.0);
            let qty = (budget / price as f64) as u64;
            let Some(Action::Place { qty, .. }) = self.order(state, symbol, side, qty) else { continue };
            let Some(cost) = taking_cost_bps(state, symbol, side, qty, self.fee_bps) else { continue };
            if remaining.abs() <= 2.0 * cost + self.hurdle_bps {
                continue;
            }
            if let Some(action) = self.order(state, symbol, side, qty) {
                let target = scale(price, remaining * 0.8);
                self.open.insert(symbol, Trade { side, qty, at: clock, target });
                actions.push(action);
            }
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::news::tests::story;
    use crate::state::tests::{acme, bars, hold, state_with};
    use rand::SeedableRng;

    #[test]
    fn trades_what_is_left_of_a_story_after_costs_and_unwinds_when_it_is_priced_in() {
        let mut rng = StdRng::seed_from_u64(1);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_000);
        hold(&mut state, acme(), 500);
        let mut fund = NewsFund::new(vec![acme()], &Params::default());
        assert!(fund.on_tick(&state, &mut rng).is_empty());
        let now = state.now;
        state.news.absorb(&story(1, NewsKind::Earnings, "ACME", 60, now - 900_000));
        assert!(fund.on_tick(&state, &mut rng).is_empty(), "stale stories are ignored");

        state.news.absorb(&story(2, NewsKind::Earnings, "ACME", 60, now));
        let expected = fund_expectation(&state);
        assert!((expected - 210.0).abs() < 1e-9, "impact 180 bps plus tone: {expected}");
        let buy = fund.on_tick(&state, &mut rng);
        let [Action::Place { side: Side::Buy, kind: OrderKind::Limit(price), qty, .. }] = buy[..] else {
            panic!("{buy:?}");
        };
        assert!(price > 10_005 && qty > 0);
        assert!(fund.on_tick(&state, &mut rng).is_empty(), "each story trades once");

        bars(&mut state, acme(), &[10_180]);
        let unwind = fund.on_tick(&state, &mut rng);
        assert!(matches!(unwind[..], [Action::Place { side: Side::Sell, qty: q, .. }] if q == qty), "priced in: {unwind:?}");

        let now = state.now;
        state.news.absorb(&story(3, NewsKind::Company, "ACME", -70, now));
        let sell = fund.on_tick(&state, &mut rng);
        assert!(matches!(sell[..], [Action::Place { side: Side::Sell, .. }]), "{sell:?}");
        state.now += 121 * state.minute_ms();
        let back = fund.on_tick(&state, &mut rng);
        assert!(matches!(back[..], [Action::Place { side: Side::Buy, .. }]), "after its holding period it buys back: {back:?}");
    }

    fn fund_expectation(state: &AgentState) -> f64 {
        state.news.after(1).map(expected_move_bps).sum()
    }

    #[test]
    fn a_story_the_market_already_moved_on_or_a_small_one_is_left_alone() {
        let mut rng = StdRng::seed_from_u64(2);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_000);
        let mut fund = NewsFund::new(vec![acme()], &Params::default());
        fund.on_tick(&state, &mut rng);
        let now = state.now;
        state.news.absorb(&story(1, NewsKind::Company, "ACME", 4, now));
        assert!(fund.on_tick(&state, &mut rng).is_empty(), "14 bps is not worth the spread and fees");

        let mut fund = NewsFund::new(vec![acme()], &Params::default());
        fund.on_tick(&state, &mut rng);
        state.pending.insert(9, state.now);
        state.news.absorb(&story(2, NewsKind::Earnings, "ACME", 60, now));
        assert!(fund.on_tick(&state, &mut rng).is_empty(), "busy when the story broke");
        state.pending.clear();
        bars(&mut state, acme(), &[10_205]);
        assert!(fund.remaining_bps(&state, acme()).unwrap().abs() < 15.0);
        assert!(fund.on_tick(&state, &mut rng).is_empty(), "the price got there first");
    }
}
