use super::{Params, Strategy, affordable, scale};
use crate::action::Action;
use crate::edge::{limit_for, size_within};
use crate::state::{AgentState, Wake};
use crate::view::{Outlook, Reader, Views};
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Side};
use rand::Rng;
use rand::rngs::StdRng;
use std::collections::BTreeMap;
use std::time::Duration;

const LEVELS: u64 = 2;

pub struct ValueInvestor {
    symbols: Vec<Symbol>,
    pub band_bps: f64,
    pub size_pct: f64,
    pub fee_bps: f64,
    reader: Reader,
    pub margin_bps: Option<f64>,
    pub views: Option<Views>,
    quoted: BTreeMap<Symbol, f64>,
}

impl ValueInvestor {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        Self {
            symbols,
            band_bps: params.f64("band_bps", 150.0),
            size_pct: params.f64("size_pct", 0.04),
            fee_bps: params.f64("fee_bps", 2.0),
            reader: if params.text("reader", "institution") == "retail" { Reader::Retail } else { Reader::Institution },
            margin_bps: None,
            views: None,
            quoted: BTreeMap::new(),
        }
    }

    pub fn with_outlook(mut self, outlook: Outlook) -> Self {
        self.views = Some(Views::new(outlook));
        self.margin_bps = Some(self.band_bps);
        self
    }

    fn prepare(&mut self, rng: &mut StdRng) -> f64 {
        if self.views.is_none() {
            self.views = Some(Views::new(Outlook::draw(self.reader, rng)));
        }
        *self.margin_bps.get_or_insert_with(|| self.band_bps * rng.random_range(0.6..1.4))
    }

    pub fn belief(&mut self, state: &AgentState, symbol: Symbol, rng: &mut StdRng) -> Option<f64> {
        self.prepare(rng);
        let views = self.views.as_mut()?;
        views.update(state, &[symbol], rng);
        views.value(symbol)
    }
}

impl Strategy for ValueInvestor {
    fn name(&self) -> &'static str {
        "value investor"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(2_000)
    }

    fn alert_bps(&self) -> Option<f64> {
        Some(self.margin_bps.unwrap_or(self.band_bps) / 2.0)
    }

    fn wakes(&self, wake: &Wake) -> bool {
        wake.any()
    }

    fn on_tick(&mut self, state: &AgentState, rng: &mut StdRng) -> Vec<Action> {
        let margin = self.prepare(rng);
        let symbols = self.symbols.clone();
        let Some(views) = self.views.as_mut() else { return vec![] };
        views.update(state, &symbols, rng);
        let mut actions = Vec::new();
        if state.busy() {
            return actions;
        }
        let mut cash = state.cash;
        for symbol in symbols {
            let Some(value) = views.value(symbol) else { continue };
            let Some(market) = state.market(symbol) else { continue };
            let resting: Vec<_> = state.open_orders.iter().filter(|(_, o)| o.symbol == symbol).collect();
            let paused = state.continuous() && market.paused;
            if !state.trading(symbol) && !paused {
                actions.extend(resting.iter().map(|(id, _)| Action::Cancel(**id)));
                continue;
            }
            let moved = self.quoted.get(&symbol).is_none_or(|was| (was / value - 1.0).abs() * 10_000.0 > margin / 3.0);
            let incomplete = resting.len() < 2 && state.wake.fill;
            if !resting.is_empty() && !moved && !incomplete {
                continue;
            }
            actions.extend(resting.iter().map(|(id, _)| Action::Cancel(**id)));
            self.quoted.insert(symbol, value);
            let caution = 1.0 + 3.0 * (-state.sentiment(symbol)).max(0.0);
            let budget = state.equity() as f64 * self.size_pct;
            let mut held = state.holding(symbol).available;

            if let Some(ask) = market.ask {
                let edge = (value / ask as f64 - 1.0) * 10_000.0;
                let wanted = (budget * (edge / margin).clamp(0.0, 3.0) / ask as f64) as u64;
                let qty = affordable(cash, ask, size_within(state, symbol, Side::Buy, wanted, edge, self.fee_bps, margin * caution));
                if qty > 0 {
                    let limit = limit_for(value, Side::Buy, margin * caution + self.fee_bps).clamp(ask, scale(ask, 100.0));
                    cash = cash.saturating_sub(limit * qty);
                    actions.push(Action::Place { symbol, side: Side::Buy, kind: OrderKind::Limit(limit), qty });
                }
            }
            if let Some(bid) = market.bid {
                let edge = (bid as f64 / value - 1.0) * 10_000.0;
                let wanted = ((budget * (edge / margin).clamp(0.0, 3.0) / bid as f64) as u64).min(held);
                let qty = size_within(state, symbol, Side::Sell, wanted, edge, self.fee_bps, margin);
                if qty > 0 {
                    let limit = limit_for(value, Side::Sell, margin + self.fee_bps).clamp(scale(bid, -100.0), bid);
                    held -= qty;
                    actions.push(Action::Place { symbol, side: Side::Sell, kind: OrderKind::Limit(limit), qty });
                }
            }
            for level in 1..=LEVELS {
                let step = margin * level as f64;
                let size = budget * level as f64 / LEVELS as f64;
                let mut buy = limit_for(value, Side::Buy, step * caution);
                if let Some(ask) = market.ask.or(market.last).filter(|ask| buy >= *ask) {
                    buy = ask.saturating_sub(1).max(1);
                }
                let buy_qty = affordable(cash, buy, (size / buy as f64) as u64);
                if buy_qty > 0 {
                    cash = cash.saturating_sub(buy * buy_qty);
                    actions.push(Action::Place { symbol, side: Side::Buy, kind: OrderKind::Limit(buy), qty: buy_qty });
                }
                let mut sell: Price = limit_for(value, Side::Sell, step);
                if let Some(bid) = market.bid.or(market.last).filter(|bid| sell <= *bid) {
                    sell = bid + 1;
                }
                let sell_qty = ((size / sell as f64) as u64).min(held);
                if sell_qty > 0 {
                    held -= sell_qty;
                    actions.push(Action::Place { symbol, side: Side::Sell, kind: OrderKind::Limit(sell), qty: sell_qty });
                }
            }
        }
        actions
    }

    fn detail(&self) -> String {
        self.views
            .as_ref()
            .map(|v| {
                let o = v.outlook;
                format!(
                    "values {:+.1}% vs the published numbers, reads news {:.0} session minutes late, leans {:.0}% on the price it sees",
                    o.bias * 100.0,
                    o.lag_minutes,
                    o.reflex * 100.0
                )
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{acme, bars, hold, state_with};
    use ome_core::types::Timestamp;
    use ome_protocol::{NewsItem, NewsKind};
    use rand::SeedableRng;

    fn neutral() -> ValueInvestor {
        ValueInvestor::new(vec![acme()], &Params::default()).with_outlook(Outlook::NEUTRAL)
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
    fn rests_a_ladder_around_its_own_valuation_and_leaves_it_alone_until_its_view_changes() {
        let mut rng = StdRng::seed_from_u64(12);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        hold(&mut state, acme(), 500);
        let mut value = neutral();
        let quotes = sides(&value.on_tick(&state, &mut rng));
        assert_eq!(
            quotes,
            [(Side::Buy, 9_852), (Side::Sell, 10_150), (Side::Buy, 9_708), (Side::Sell, 10_300)],
            "more size waits further from its value"
        );
        state.open_orders.insert(1, crate::state::OpenOrder { symbol: acme(), side: Side::Buy, price: 9_852, leaves: 40, placed_at: state.now });
        state.now += 600_000;
        assert!(value.on_tick(&state, &mut rng).is_empty(), "no refreshing on a timer: orders stay while its view is unchanged");

        state.news.absorb(&story("fund=ACME:2500:0:0", 9_000));
        state.news.absorb(&story("fund=ACME:2750:0:0", 9_500));
        bars(&mut state, acme(), &[11_100]);
        let actions = value.on_tick(&state, &mut rng);
        assert_eq!(actions[0], Action::Cancel(1), "10% higher earnings move its whole ladder");
        assert_eq!(sides(&actions)[..2], [(Side::Buy, 10_837), (Side::Sell, 11_165)]);
    }

    #[test]
    fn different_investors_value_the_same_company_differently() {
        let state = state_with(&[(acme(), 10_000)], 10_000_00);
        let mut rng = StdRng::seed_from_u64(3);
        let beliefs: Vec<f64> = (0..20)
            .map(|_| ValueInvestor::new(vec![acme()], &Params::default()).belief(&state, acme(), &mut rng).unwrap())
            .collect();
        let low = beliefs.iter().cloned().fold(f64::MAX, f64::min);
        let high = beliefs.iter().cloned().fold(f64::MIN, f64::max);
        assert!(high / low > 1.03, "{low:.0}..{high:.0}");
    }

    #[test]
    fn far_from_its_value_it_takes_liquidity_but_only_as_much_as_the_book_pays_for() {
        let mut rng = StdRng::seed_from_u64(2);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_000_00);
        let mut value = neutral();
        bars(&mut state, acme(), &[7_500]);
        let level = |price, qty| ome_core::order_book::Level { price, qty, orders: 1 };
        state.markets.get_mut(&acme()).unwrap().asks = vec![level(7_505, 50), level(9_800, 100_000)];
        let actions = value.on_tick(&state, &mut rng);
        let Action::Place { side: Side::Buy, kind: OrderKind::Limit(limit), qty, .. } = actions[0] else { panic!("{actions:?}") };
        assert_eq!(limit, 7_580, "it bids through the offer, at most 1% so the price band accepts it");
        assert!(qty >= 50, "{qty}");
        state.markets.get_mut(&acme()).unwrap().asks = vec![level(7_505, 50), level(10_400, 100_000)];
        let mut value = neutral();
        let Action::Place { qty: thin, .. } = value.on_tick(&state, &mut rng)[0] else { panic!() };
        assert!(thin < qty, "a thin offer limits how much is worth taking: {thin} vs {qty}");

        let mut calm = state_with(&[(acme(), 10_000)], 10_000_00);
        hold(&mut calm, acme(), 500);
        let mut value = neutral();
        assert!(sides(&value.on_tick(&calm, &mut rng)).iter().all(|(side, price)| match side {
            Side::Buy => *price < 9_995,
            Side::Sell => *price > 10_005,
        }), "near its value it only rests orders");
    }

    #[test]
    fn an_explicit_operator_value_overrides_its_model_and_bad_news_makes_it_careful() {
        let mut rng = StdRng::seed_from_u64(1);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_00);
        let mut value = neutral();
        bars(&mut state, acme(), &[11_100]);
        state.world.fair.insert(acme(), 11_000);
        let quotes = sides(&value.on_tick(&state, &mut rng));
        assert_eq!(quotes[0], (Side::Buy, 10_837));
        state.world.sentiment.insert(acme(), -1.0);
        let mut value = neutral();
        let cautious = sides(&value.on_tick(&state, &mut rng));
        assert!(cautious[0].1 < 10_837, "{cautious:?}");
    }
}
