use crate::state::{AgentState, SESSION_MINUTES};
use crate::strategies::gaussian;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use rand::Rng;
use rand::rngs::StdRng;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reader {
    Institution,
    Retail,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Outlook {
    pub bias: f64,
    pub lag_minutes: f64,
    pub digest_minutes: f64,
    pub reflex: f64,
    pub memory_days: f64,
    pub reading: f64,
    pub macro_weight: f64,
}

impl Outlook {
    pub const NEUTRAL: Outlook =
        Outlook { bias: 0.0, lag_minutes: 0.0, digest_minutes: 0.0, reflex: 0.0, memory_days: 10.0, reading: 0.0, macro_weight: 0.0 };

    pub fn draw(reader: Reader, rng: &mut StdRng) -> Self {
        let bias = (gaussian(rng) * 0.04).clamp(-0.1, 0.1);
        match reader {
            Reader::Institution => Outlook {
                bias,
                lag_minutes: rng.random_range(1.0..20.0),
                digest_minutes: rng.random_range(30.0..240.0),
                reflex: rng.random_range(0.05..0.25),
                memory_days: rng.random_range(5.0..20.0),
                reading: 0.3,
                macro_weight: rng.random_range(0.02..0.06),
            },
            Reader::Retail => Outlook {
                bias,
                lag_minutes: rng.random_range(20.0..375.0),
                digest_minutes: rng.random_range(120.0..750.0),
                reflex: rng.random_range(0.2..0.5),
                memory_days: rng.random_range(3.0..10.0),
                reading: 0.6,
                macro_weight: rng.random_range(0.01..0.04),
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct View {
    digested: f64,
    opinion: f64,
    habit: f64,
    seen: u64,
    at: u64,
    value: f64,
}

#[derive(Debug, Clone)]
pub struct Views {
    pub outlook: Outlook,
    views: BTreeMap<Symbol, View>,
    anchors: BTreeMap<Symbol, Price>,
}

fn toward(dt: f64, over: f64) -> f64 {
    if over <= 0.0 { 1.0 } else { 1.0 - (-dt / over).exp() }
}

impl Views {
    pub fn new(outlook: Outlook) -> Self {
        Self { outlook, views: BTreeMap::new(), anchors: BTreeMap::new() }
    }

    pub fn update(&mut self, state: &AgentState, symbols: &[Symbol], rng: &mut StdRng) {
        let o = self.outlook;
        let clock = state.clock();
        let read_until = state.now.saturating_sub((o.lag_minutes * state.minute_ms() as f64) as u64);
        let memory = o.memory_days * SESSION_MINUTES as f64;
        let macro_view = o.macro_weight * state.news.macro_trend(state.now, o.memory_days);
        for symbol in symbols {
            let Some(market) = state.market(*symbol) else { continue };
            let anchor = *self.anchors.entry(*symbol).or_insert(market.reference.max(1));
            let operator = state.world.fair.get(symbol).map(|p| *p as f64);
            let model = operator
                .or_else(|| state.news.valuations.value(*symbol, read_until, anchor))
                .unwrap_or(anchor as f64);
            let price = market.fair().map_or(model, |p| p as f64);
            let view = self.views.entry(*symbol).or_insert(View {
                digested: model,
                opinion: 0.0,
                habit: price,
                seen: state.news.last_seq(),
                at: clock,
                value: model,
            });
            let dt = clock.saturating_sub(view.at) as f64;
            view.at = clock;
            view.digested += (model - view.digested) * if operator.is_some() { 1.0 } else { toward(dt, o.digest_minutes) };
            view.opinion *= 1.0 - toward(dt, memory);
            view.habit += (price - view.habit) * toward(dt, memory);
            for item in state.news.after(view.seen) {
                if item.time > read_until {
                    break;
                }
                view.seen = item.seq;
                if state.news.about(item, *symbol) {
                    let size = (item.impact_bps.abs() as f64 / 10_000.0).max(item.sentiment.abs() as f64 / 100.0 * 0.005);
                    view.opinion = (view.opinion + gaussian(rng) * o.reading * size).clamp(-0.2, 0.2);
                }
            }
            let own = view.digested * (1.0 + o.bias + view.opinion + macro_view);
            view.value = (own * (1.0 - o.reflex) + view.habit * o.reflex).max(1.0);
        }
    }

    pub fn value(&self, symbol: Symbol) -> Option<f64> {
        self.views.get(&symbol).map(|v| v.value)
    }

    pub fn upside(&self, state: &AgentState, symbol: Symbol) -> Option<f64> {
        Some(self.value(symbol)? / state.market(symbol)?.fair()? as f64 - 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::news::tests::story;
    use crate::state::tests::{acme, bars, state_with};
    use ome_protocol::NewsKind;
    use rand::SeedableRng;

    fn fundamentals(state: &mut AgentState, seq: u64, eps: i64, impact: i64) {
        let mut item = story(seq, NewsKind::Company, "ACME", 40, state.now);
        item.impact_bps = impact;
        item.data = format!("fund=ACME:{eps}:0:0");
        state.news.absorb(&item);
    }

    #[test]
    fn a_neutral_reader_values_the_company_at_its_published_fundamentals() {
        let mut rng = StdRng::seed_from_u64(1);
        let mut state = state_with(&[(acme(), 10_000)], 0);
        let mut views = Views::new(Outlook::NEUTRAL);
        views.update(&state, &[acme()], &mut rng);
        assert_eq!(views.value(acme()).map(f64::round), Some(10_000.0));
        fundamentals(&mut state, 1, 2_500, 0);
        fundamentals(&mut state, 2, 2_750, 1_000);
        views.update(&state, &[acme()], &mut rng);
        assert_eq!(views.value(acme()).map(f64::round), Some(11_000.0), "10% more earnings, 10% more value");
        state.world.fair.insert(acme(), 9_000);
        views.update(&state, &[acme()], &mut rng);
        assert_eq!(views.value(acme()).map(f64::round), Some(9_000.0), "an operator override wins");
    }

    #[test]
    fn readers_digest_news_slowly_disagree_about_it_and_lean_on_the_price_they_see() {
        let mut rng = StdRng::seed_from_u64(2);
        let mut state = state_with(&[(acme(), 10_000)], 0);
        let slow = Outlook { lag_minutes: 30.0, digest_minutes: 120.0, ..Outlook::NEUTRAL };
        let mut views = Views::new(slow);
        fundamentals(&mut state, 1, 2_500, 0);
        views.update(&state, &[acme()], &mut rng);
        fundamentals(&mut state, 2, 3_000, 2_000);
        state.now += 10 * state.minute_ms();
        views.update(&state, &[acme()], &mut rng);
        assert_eq!(views.value(acme()).map(f64::round), Some(10_000.0), "not read yet");
        state.now += 60 * state.minute_ms();
        views.update(&state, &[acme()], &mut rng);
        let partly = views.value(acme()).unwrap();
        assert!(partly > 10_300.0 && partly < 11_700.0, "half digested after an hour: {partly}");
        state.now += 2_000 * state.minute_ms();
        views.update(&state, &[acme()], &mut rng);
        assert!((views.value(acme()).unwrap() - 12_000.0).abs() < 5.0);

        let mut values = Vec::new();
        for seed in 0..12 {
            let mut rng = StdRng::seed_from_u64(seed);
            let mut readers = Views::new(Outlook { reading: 0.6, ..Outlook::NEUTRAL });
            let mut state = state_with(&[(acme(), 10_000)], 0);
            readers.update(&state, &[acme()], &mut rng);
            fundamentals(&mut state, 1, 2_500, 800);
            readers.update(&state, &[acme()], &mut rng);
            values.push(readers.value(acme()).unwrap());
        }
        let spread = values.iter().cloned().fold(f64::MIN, f64::max) / values.iter().cloned().fold(f64::MAX, f64::min);
        assert!(spread > 1.03, "the same story is read differently: {values:?}");

        let mut rng = StdRng::seed_from_u64(3);
        let mut state = state_with(&[(acme(), 10_000)], 0);
        let mut follower = Views::new(Outlook { reflex: 0.5, memory_days: 1.0, ..Outlook::NEUTRAL });
        follower.update(&state, &[acme()], &mut rng);
        bars(&mut state, acme(), &[12_000]);
        state.now += 3 * SESSION_MINUTES * state.minute_ms();
        follower.update(&state, &[acme()], &mut rng);
        let pulled = follower.value(acme()).unwrap();
        assert!(pulled > 10_800.0 && pulled < 11_000.0, "a price that stays 20% higher pulls its value up: {pulled}");
    }
}
