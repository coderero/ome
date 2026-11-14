use super::{Params, Strategy, affordable, cancel_stale, scale};
use crate::action::Action;
use crate::state::AgentState;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Side, Timestamp};
use ome_protocol::NewsKind;
use rand::rngs::StdRng;
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trade {
    pub side: Side,
    pub qty: u64,
    pub at: Timestamp,
}

pub struct NewsFund {
    symbols: Vec<Symbol>,
    pub threshold: f64,
    pub size_pct: f64,
    pub hold_ms: Timestamp,
    pub max_age_ms: Timestamp,
    pub cross_bps: f64,
    seen: Option<u64>,
    pub open: BTreeMap<Symbol, Trade>,
}

impl NewsFund {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        Self {
            symbols,
            threshold: params.f64("threshold", 0.35),
            size_pct: params.f64("size_pct", 0.06),
            hold_ms: params.u64("hold_ms", 60_000),
            max_age_ms: params.u64("max_age_ms", 10_000),
            cross_bps: params.f64("cross_bps", 10.0),
            seen: None,
            open: BTreeMap::new(),
        }
    }

    pub fn signal(&self, state: &AgentState, symbol: Symbol, after: u64) -> f64 {
        state
            .news
            .after(after)
            .filter(|i| state.now.saturating_sub(i.time) <= self.max_age_ms)
            .filter(|i| i.kind != NewsKind::Prospectus && state.news.about(i, symbol))
            .map(|i| {
                let relevance = match i.kind {
                    NewsKind::Macro => 0.5,
                    NewsKind::Sector => 0.7,
                    _ => 1.0,
                };
                relevance * (i.sentiment as f64 / 100.0 + i.impact_bps as f64 / 300.0)
            })
            .sum()
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
        Duration::from_millis(500)
    }

    fn on_tick(&mut self, state: &AgentState, _rng: &mut StdRng) -> Vec<Action> {
        let mut actions = cancel_stale(state, None, 3_000);
        let after = *self.seen.get_or_insert(0);
        self.seen = Some(state.news.last_seq());
        if state.busy() {
            return actions;
        }
        for symbol in self.symbols.clone() {
            if !state.trading(symbol) || state.open_in(symbol) > 0 {
                continue;
            }
            if let Some(trade) = self.open.get(&symbol).copied()
                && state.now.saturating_sub(trade.at) >= self.hold_ms
            {
                self.open.remove(&symbol);
                let back = match trade.side {
                    Side::Buy => Side::Sell,
                    Side::Sell => Side::Buy,
                };
                actions.extend(self.order(state, symbol, back, trade.qty));
                continue;
            }
            let signal = self.signal(state, symbol, after);
            if signal.abs() < self.threshold || self.open.contains_key(&symbol) {
                continue;
            }
            let Some(price) = state.market(symbol).and_then(|m| m.fair()) else {
                continue;
            };
            let budget = state.equity() as f64 * self.size_pct * signal.abs().min(2.0);
            let side = if signal > 0.0 { Side::Buy } else { Side::Sell };
            let qty = (budget / price as f64) as u64;
            if let Some(action) = self.order(state, symbol, side, qty) {
                if let Action::Place { qty, .. } = action {
                    self.open.insert(
                        symbol,
                        Trade {
                            side,
                            qty,
                            at: state.now,
                        },
                    );
                }
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
    use crate::state::tests::{acme, hold, state_with};
    use rand::SeedableRng;

    #[test]
    fn trades_fresh_stories_by_tone_and_impact_then_unwinds_after_the_holding_period() {
        let mut rng = StdRng::seed_from_u64(1);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_000);
        hold(&mut state, acme(), 500);
        let mut fund = NewsFund::new(vec![acme()], &Params::default());
        let now = state.now;
        state
            .news
            .absorb(&story(1, NewsKind::Earnings, "ACME", 60, now - 60_000));
        assert!(
            fund.on_tick(&state, &mut rng).is_empty(),
            "stale stories are ignored"
        );

        state
            .news
            .absorb(&story(2, NewsKind::Earnings, "ACME", 60, now));
        let buy = fund.on_tick(&state, &mut rng);
        let [
            Action::Place {
                side: Side::Buy,
                kind: OrderKind::Limit(price),
                qty,
                ..
            },
        ] = buy[..]
        else {
            panic!("{buy:?}");
        };
        assert!(price > 10_005 && qty > 0);
        assert!(
            fund.on_tick(&state, &mut rng).is_empty(),
            "each story trades once"
        );

        state.now += 61_000;
        let unwind = fund.on_tick(&state, &mut rng);
        assert!(matches!(unwind[..], [Action::Place { side: Side::Sell, qty: q, .. }] if q == qty));

        let now = state.now;
        state
            .news
            .absorb(&story(3, NewsKind::Company, "ACME", -70, now));
        let sell = fund.on_tick(&state, &mut rng);
        assert!(
            matches!(
                sell[..],
                [Action::Place {
                    side: Side::Sell,
                    ..
                }]
            ),
            "{sell:?}"
        );
        state
            .news
            .absorb(&story(4, NewsKind::Company, "ACME", 10, now));
        assert!(
            fund.signal(&state, acme(), 3).abs() < fund.threshold,
            "weak stories stay below the threshold"
        );
    }
}
