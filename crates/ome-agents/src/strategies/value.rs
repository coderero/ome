use super::{Params, Strategy, affordable, gaussian, scale};
use crate::action::Action;
use crate::state::AgentState;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Side, Timestamp};
use rand::Rng;
use rand::rngs::StdRng;
use std::collections::BTreeMap;
use std::time::Duration;

const REFRESH_MS: Timestamp = 20_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Traits {
    pub bias: f64,
    pub lag_ms: Timestamp,
    pub margin_bps: f64,
}

pub struct ValueInvestor {
    symbols: Vec<Symbol>,
    pub band_bps: f64,
    pub size_pct: f64,
    pub traits: Option<Traits>,
    anchors: BTreeMap<Symbol, Price>,
    quoted: BTreeMap<Symbol, Price>,
}

impl ValueInvestor {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        Self {
            symbols,
            band_bps: params.f64("band_bps", 150.0),
            size_pct: params.f64("size_pct", 0.04),
            traits: None,
            anchors: BTreeMap::new(),
            quoted: BTreeMap::new(),
        }
    }

    fn traits(&mut self, rng: &mut StdRng) -> Traits {
        *self.traits.get_or_insert_with(|| Traits {
            bias: (gaussian(rng) * 0.04).clamp(-0.1, 0.1),
            lag_ms: rng.random_range(2_000..30_000),
            margin_bps: self.band_bps * rng.random_range(0.6..1.4),
        })
    }

    pub fn belief(&mut self, state: &AgentState, symbol: Symbol, traits: Traits) -> Option<f64> {
        let market = state.market(symbol)?;
        let anchor = *self.anchors.entry(symbol).or_insert(market.reference.max(1));
        let model = match state.world.fair.get(&symbol) {
            Some(operator) => *operator as f64,
            None => state
                .news
                .valuations
                .value(symbol, state.now.saturating_sub(traits.lag_ms), anchor)
                .unwrap_or(anchor as f64),
        };
        Some(model * (1.0 + traits.bias))
    }
}

impl Strategy for ValueInvestor {
    fn name(&self) -> &'static str {
        "value investor"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(1_500)
    }

    fn on_tick(&mut self, state: &AgentState, rng: &mut StdRng) -> Vec<Action> {
        let traits = self.traits(rng);
        let mut actions = Vec::new();
        if state.busy() || self.symbols.is_empty() {
            return actions;
        }
        for symbol in self.symbols.clone() {
            let Some(belief) = self.belief(state, symbol, traits) else {
                continue;
            };
            let belief_price = belief.round().max(1.0) as Price;
            let moved = self
                .quoted
                .get(&symbol)
                .is_none_or(|was| (*was as f64 / belief - 1.0).abs() * 10_000.0 > traits.margin_bps / 3.0);
            let resting: Vec<_> = state.open_orders.iter().filter(|(_, o)| o.symbol == symbol).collect();
            let stale = resting.iter().any(|(_, o)| state.now.saturating_sub(o.placed_at) > REFRESH_MS);
            let paused = state.continuous() && state.market(symbol).is_some_and(|m| m.paused);
            if !state.trading(symbol) && !paused {
                actions.extend(resting.iter().map(|(id, _)| Action::Cancel(**id)));
                continue;
            }
            if !resting.is_empty() && !moved && !stale {
                continue;
            }
            actions.extend(resting.iter().map(|(id, _)| Action::Cancel(**id)));
            self.quoted.insert(symbol, belief_price);
            let caution = 1.0 + 3.0 * (-state.sentiment(symbol)).max(0.0);
            let budget = state.equity() as f64 * self.size_pct;
            let mut buy = scale(belief_price, -traits.margin_bps * caution);
            let mut sell = scale(belief_price, traits.margin_bps);
            if let Some(market) = state.market(symbol) {
                if let Some(ask) = market.ask.or(market.last).filter(|ask| buy > *ask) {
                    buy = buy.min(scale(ask, 100.0));
                }
                if let Some(bid) = market.bid.or(market.last).filter(|bid| sell < *bid) {
                    sell = sell.max(scale(bid, -100.0));
                }
            }
            let buy_qty = affordable(state.cash, buy, (budget / buy as f64) as u64);
            if buy_qty > 0 {
                actions.push(Action::Place { symbol, side: Side::Buy, kind: OrderKind::Limit(buy), qty: buy_qty });
            }
            let sell_qty = ((budget / sell as f64) as u64).min(state.holding(symbol).available);
            if sell_qty > 0 {
                actions.push(Action::Place { symbol, side: Side::Sell, kind: OrderKind::Limit(sell), qty: sell_qty });
            }
        }
        actions
    }

    fn detail(&self) -> String {
        self.traits
            .map(|t| format!("values {:+.1}% vs consensus, reads news {}s late", t.bias * 100.0, t.lag_ms / 1_000))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{acme, hold, state_with};
    use ome_protocol::{NewsItem, NewsKind};
    use rand::SeedableRng;

    fn neutral(value: &mut ValueInvestor) {
        value.traits = Some(Traits { bias: 0.0, lag_ms: 0, margin_bps: 150.0 });
    }

    fn sides(actions: &[Action]) -> Vec<(Side, Price)> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Place { side, kind: OrderKind::Limit(p), .. } => Some((*side, *p)),
                _ => None,
            })
            .collect()
    }

    fn story(data: &str, time: Timestamp) -> NewsItem {
        NewsItem {
            seq: time + 1,
            time,
            day: 1,
            kind: NewsKind::Company,
            symbol: Some(acme()),
            sentiment: 0,
            impact_bps: 0,
            sector: String::new(),
            headline: String::new(),
            body: String::new(),
            data: data.into(),
        }
    }

    #[test]
    fn rests_bids_below_and_offers_above_its_own_valuation_from_the_news() {
        let mut rng = StdRng::seed_from_u64(12);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        hold(&mut state, acme(), 500);
        let mut value = ValueInvestor::new(vec![acme()], &Params::default());
        neutral(&mut value);
        let quotes = sides(&value.on_tick(&state, &mut rng));
        assert_eq!(quotes, [(Side::Buy, 9_850), (Side::Sell, 10_150)], "quotes around the reference until news arrives");
        state.open_orders.insert(1, crate::state::OpenOrder { symbol: acme(), side: Side::Buy, price: 9_850, leaves: 40, placed_at: state.now });
        assert!(value.on_tick(&state, &mut rng).is_empty(), "resting orders stay while its view is unchanged");
        state.now += REFRESH_MS + 1;
        assert!(matches!(value.on_tick(&state, &mut rng)[0], Action::Cancel(1)), "and are refreshed when they get old");

        state.news.absorb(&story("fund=ACME:2500:0:0", 9_000));
        state.news.absorb(&story("fund=ACME:2750:0:0", 9_500));
        state.markets.get_mut(&acme()).unwrap().ask = Some(11_200);
        state.open_orders.clear();
        let quotes = sides(&value.on_tick(&state, &mut rng));
        assert_eq!(quotes, [(Side::Buy, 10_835), (Side::Sell, 11_165)], "10% higher earnings lift its whole ladder");
    }

    #[test]
    fn agents_disagree_and_read_news_late() {
        let mut rng = StdRng::seed_from_u64(3);
        let state = state_with(&[(acme(), 10_000)], 10_000_00);
        let beliefs: Vec<f64> = (0..20)
            .map(|_| {
                let mut v = ValueInvestor::new(vec![acme()], &Params::default());
                let traits = v.traits(&mut rng);
                v.belief(&state, acme(), traits).unwrap()
            })
            .collect();
        let low = beliefs.iter().cloned().fold(f64::MAX, f64::min);
        let high = beliefs.iter().cloned().fold(f64::MIN, f64::max);
        assert!(high / low > 1.03, "different agents value the same company differently: {low:.0}..{high:.0}");

        let mut state = state;
        state.news.absorb(&story("fund=ACME:2500:0:0", 0));
        state.news.absorb(&story("fund=ACME:3000:0:0", 1_000));
        state.now = 1_500;
        let mut slow = ValueInvestor::new(vec![acme()], &Params::default());
        let traits = Traits { bias: 0.0, lag_ms: 5_000, margin_bps: 150.0 };
        assert_eq!(slow.belief(&state, acme(), traits).map(|b| b.round()), Some(10_000.0), "has not read the newer story yet");
        state.now = 6_500;
        assert_eq!(slow.belief(&state, acme(), traits).map(|b| b.round()), Some(12_000.0));
    }

    #[test]
    fn its_anchor_stays_put_when_the_market_runs() {
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        let mut value = ValueInvestor::new(vec![acme()], &Params::default());
        let traits = Traits { bias: 0.0, lag_ms: 0, margin_bps: 150.0 };
        assert_eq!(value.belief(&state, acme(), traits).map(|b| b.round()), Some(10_000.0));
        let market = state.markets.get_mut(&acme()).unwrap();
        market.reference = 14_000;
        market.last = Some(14_500);
        assert_eq!(value.belief(&state, acme(), traits).map(|b| b.round()), Some(10_000.0), "a rally does not raise what it thinks the company is worth");
    }

    #[test]
    fn far_below_its_value_it_takes_the_offer_inside_the_price_band() {
        let mut rng = StdRng::seed_from_u64(2);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        let mut value = ValueInvestor::new(vec![acme()], &Params::default());
        neutral(&mut value);
        let market = state.markets.get_mut(&acme()).unwrap();
        market.last = Some(7_500);
        market.bid = Some(7_490);
        market.ask = Some(7_510);
        let quotes = sides(&value.on_tick(&state, &mut rng));
        assert_eq!(quotes[0], (Side::Buy, 7_585), "bids just through the offer instead of at 98.50, which the band would reject");
    }

    #[test]
    fn an_explicit_operator_value_overrides_its_model() {
        let mut rng = StdRng::seed_from_u64(1);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        let mut value = ValueInvestor::new(vec![acme()], &Params::default());
        neutral(&mut value);
        state.markets.get_mut(&acme()).unwrap().ask = Some(11_200);
        state.world.fair.insert(acme(), 11_000);
        let quotes = sides(&value.on_tick(&state, &mut rng));
        assert_eq!(quotes[0], (Side::Buy, 10_835));
        state.world.sentiment.insert(acme(), -1.0);
        state.open_orders.clear();
        value.quoted.clear();
        let cautious = sides(&value.on_tick(&state, &mut rng));
        assert!(cautious[0].1 < 10_835, "bad news makes it bid lower: {cautious:?}");
    }
}
