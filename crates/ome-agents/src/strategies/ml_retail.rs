use super::{Params, Strategy, affordable, scale};
use crate::action::Action;
use crate::model::{FEATURE_NAMES, FEATURES, Features, OnlineLogit, features};
use crate::state::AgentState;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Side};
use ome_protocol::Trigger;
use rand::Rng;
use rand::rngs::StdRng;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

struct Sample {
    tick: u64,
    x: Features,
    price: Price,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Settings {
    activity: f64,
    budget_pct: f64,
    enter: f64,
    strong: f64,
    exit: f64,
    horizon: u64,
    warmup: u64,
    deadband_bps: f64,
    trail_pct: u64,
    risk_vol: f64,
    learning_rate: f64,
    l2: f64,
    diversity: f64,
}

impl Settings {
    fn from(params: &Params) -> Self {
        let (activity, budget, enter, strong, exit) = match params.text("style", "balanced") {
            "cautious" => (0.10, 0.15, 0.12, 0.20, 0.04),
            "bold" => (0.35, 0.30, 0.05, 0.10, 0.12),
            _ => (0.20, 0.20, 0.08, 0.15, 0.08),
        };
        Self {
            activity: params.f64("activity", activity),
            budget_pct: params.f64("budget_pct", budget),
            enter: params.f64("enter", enter),
            strong: params.f64("strong", strong),
            exit: params.f64("exit", exit),
            horizon: params.u64("horizon", 20).max(1),
            warmup: params.u64("warmup", 60),
            deadband_bps: params.f64("deadband_bps", 3.0),
            trail_pct: params.u64("trail_pct", 6).max(1),
            risk_vol: params.f64("risk_vol", 3.0),
            learning_rate: params.f64("lr", 0.05),
            l2: params.f64("l2", 1e-4),
            diversity: params.f64("diversity", 0.5).clamp(0.0, 0.9),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Quirks {
    horizon: f64,
    learning_rate: f64,
    keep: f64,
}

impl Quirks {
    fn draw(diversity: f64, rng: &mut StdRng) -> Self {
        let mut spread = || 1.0 + diversity * rng.random_range(-1.0..=1.0);
        Self {
            horizon: spread(),
            learning_rate: spread(),
            keep: 1.0 - diversity * rng.random_range(0.0..=0.8),
        }
    }
}

pub struct MlRetail {
    symbols: Vec<Symbol>,
    settings: Settings,
    quirks: Option<Quirks>,
    model: OnlineLogit<FEATURES>,
    samples: BTreeMap<Symbol, VecDeque<Sample>>,
    exiting: BTreeSet<Symbol>,
    tick: u64,
    best_edge: f64,
}

impl MlRetail {
    pub fn new(symbols: Vec<Symbol>, params: &Params) -> Self {
        let settings = Settings::from(params);
        Self {
            symbols,
            settings,
            quirks: None,
            model: OnlineLogit::new(settings.learning_rate, settings.l2),
            samples: BTreeMap::new(),
            exiting: BTreeSet::new(),
            tick: 0,
            best_edge: 0.0,
        }
    }

    pub fn model(&self) -> &OnlineLogit<FEATURES> {
        &self.model
    }

    pub fn ready(&self) -> bool {
        self.model.seen >= self.settings.warmup
    }

    fn quirks(&mut self, rng: &mut StdRng) -> Quirks {
        let diversity = self.settings.diversity;
        let quirks = *self
            .quirks
            .get_or_insert_with(|| Quirks::draw(diversity, rng));
        self.model.learning_rate = self.settings.learning_rate * quirks.learning_rate;
        self.model.l2 = self.settings.l2;
        quirks
    }

    fn observe(&mut self, state: &AgentState, rng: &mut StdRng) -> BTreeMap<Symbol, f64> {
        let quirks = self.quirks(rng);
        let horizon = ((self.settings.horizon as f64 * quirks.horizon).round() as u64).max(1);
        self.tick += 1;
        let mut edges = BTreeMap::new();
        for symbol in self.symbols.clone() {
            let Some(market) = state.market(symbol) else {
                continue;
            };
            let Some(price) = market.fair() else {
                continue;
            };
            let queue = self.samples.entry(symbol).or_default();
            while queue.front().is_some_and(|s| self.tick - s.tick >= horizon) {
                let Some(sample) = queue.pop_front() else {
                    break;
                };
                let moved = (price as f64 / sample.price as f64 - 1.0) * 10_000.0;
                if moved.abs() >= self.settings.deadband_bps {
                    self.model.learn(&sample.x, moved > 0.0);
                }
            }
            let Some(x) = features(market, state.sentiment(symbol)) else {
                continue;
            };
            if !state.trading(symbol) {
                continue;
            }
            if rng.random_bool(quirks.keep) {
                queue.push_back(Sample {
                    tick: self.tick,
                    x,
                    price,
                });
            }
            edges.insert(symbol, self.model.predict(&x) - self.model.base_rate());
        }
        edges
    }

    fn manage(&mut self, state: &AgentState, symbol: Symbol, edge: f64) -> Vec<Action> {
        let held = state.holding(symbol);
        let sells_armed: Vec<u64> = state
            .armed
            .values()
            .filter(|c| c.symbol == symbol && c.side == Side::Sell)
            .map(|c| c.cond_id)
            .collect();
        let sells_open: Vec<u64> = state
            .open_orders
            .iter()
            .filter(|(_, o)| o.symbol == symbol && o.side == Side::Sell)
            .map(|(id, _)| *id)
            .collect();
        if edge <= -self.settings.exit || self.exiting.contains(&symbol) {
            self.exiting.insert(symbol);
            if !sells_armed.is_empty() || !sells_open.is_empty() {
                return sells_armed
                    .into_iter()
                    .map(Action::CancelConditional)
                    .chain(sells_open.into_iter().map(Action::Cancel))
                    .collect();
            }
            if held.available > 0 {
                return vec![Action::Place {
                    symbol,
                    side: Side::Sell,
                    kind: OrderKind::Market,
                    qty: held.available,
                }];
            }
            return vec![];
        }
        if sells_armed.is_empty() && sells_open.is_empty() && held.available > 0 {
            let Some(last) = state.market(symbol).and_then(|m| m.fair()) else {
                return vec![];
            };
            return vec![Action::Conditional {
                symbol,
                side: Side::Sell,
                qty: held.available,
                trigger: Trigger::TrailingBelow((last * self.settings.trail_pct / 100).max(1)),
                kind: OrderKind::Market,
            }];
        }
        vec![]
    }

    fn enter(&self, state: &AgentState, symbol: Symbol, edge: f64) -> Vec<Action> {
        let Some(market) = state.market(symbol) else {
            return vec![];
        };
        let Some(last) = market.fair() else {
            return vec![];
        };
        let confidence = (edge / self.settings.strong.max(0.01)).clamp(0.25, 1.0);
        let budget = state.equity() as f64 * self.settings.budget_pct * confidence;
        let vol_bps = market.volatility_bps(50).unwrap_or(10.0).max(5.0);
        let risk_bps = (vol_bps * self.settings.risk_vol * 4.0).clamp(80.0, 800.0);
        if edge >= self.settings.strong {
            let entry = market.ask.unwrap_or(last);
            let qty = affordable(state.cash, entry, (budget / entry as f64) as u64);
            if qty == 0 {
                return vec![];
            }
            return vec![Action::Bracket {
                symbol,
                side: Side::Buy,
                qty,
                entry: OrderKind::Limit(entry),
                take_profit: scale(entry, risk_bps * 1.5).max(entry + 1),
                stop_loss: scale(entry, -risk_bps).min(entry - 1),
            }];
        }
        let level = scale(market.bid.unwrap_or(last).min(last), -risk_bps / 4.0);
        let qty = affordable(state.cash, level, (budget / level as f64) as u64);
        if qty == 0 {
            return vec![];
        }
        vec![Action::Conditional {
            symbol,
            side: Side::Buy,
            qty,
            trigger: Trigger::Below(level),
            kind: OrderKind::Limit(level),
        }]
    }
}

impl Strategy for MlRetail {
    fn name(&self) -> &'static str {
        "ML retail persona"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(1_500)
    }

    fn retune(&mut self, params: &Params) -> bool {
        self.settings = Settings::from(params);
        if let Some(quirks) = self.quirks {
            self.model.learning_rate = self.settings.learning_rate * quirks.learning_rate;
        }
        self.model.l2 = self.settings.l2;
        true
    }

    fn detail(&self) -> String {
        let (strongest, weight) = self
            .model
            .weights
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .map_or(("none", 0.0), |(i, w)| (FEATURE_NAMES[i], *w));
        format!(
            "model {} samples hit rate {:.0}% base {:.2} best edge {:+.2} strongest {strongest} {weight:+.2}",
            self.model.seen,
            self.model.hit_rate() * 100.0,
            self.model.base_rate(),
            self.best_edge
        )
    }

    fn on_tick(&mut self, state: &AgentState, rng: &mut StdRng) -> Vec<Action> {
        let edges = self.observe(state, rng);
        if let Some((_, edge)) = edges.iter().max_by(|a, b| a.1.total_cmp(b.1)) {
            self.best_edge = *edge;
        }
        self.exiting.retain(|s| state.holding(*s).total() > 0);
        if state.busy() || !self.ready() {
            return vec![];
        }
        for (symbol, edge) in &edges {
            if state.holding(*symbol).total() > 0 || self.exiting.contains(symbol) {
                let actions = self.manage(state, *symbol, *edge);
                if !actions.is_empty() {
                    return actions;
                }
            }
        }
        if !rng.random_bool((self.settings.activity * state.world.activity).min(1.0)) {
            return vec![];
        }
        let best = edges
            .iter()
            .filter(|(s, _)| state.holding(**s).total() == 0)
            .filter(|(s, _)| state.armed_in(**s) == 0 && state.open_in(**s) == 0)
            .filter(|(_, edge)| **edge >= self.settings.enter)
            .max_by(|a, b| a.1.total_cmp(b.1));
        match best {
            Some((symbol, edge)) => self.enter(state, *symbol, *edge),
            None => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{acme, hold, state_with};
    use crate::strategies::gaussian;
    use rand::SeedableRng;

    fn params() -> Params {
        Params(
            [
                ("activity", "1"),
                ("warmup", "40"),
                ("horizon", "5"),
                ("lr", "0.05"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        )
    }

    fn walk(state: &mut AgentState, rng: &mut StdRng, drift_bps: f64) {
        let m = state.markets.get_mut(&acme()).unwrap();
        let last = *m.prices.back().unwrap() as f64;
        let price = (last * (1.0 + (drift_bps + 8.0 * gaussian(rng)) / 10_000.0)) as Price;
        m.record(price);
        m.bid = Some(price - 5);
        m.ask = Some(price + 5);
        state.now += 1_500;
    }

    fn trending_market(persona: &mut MlRetail, state: &mut AgentState, rng: &mut StdRng) {
        let mut drift = 12.0;
        for _ in 0..1_500 {
            if rng.random_bool(1.0 / 60.0) {
                drift = -drift;
            }
            walk(state, rng, drift);
            state.pending.insert(1, state.now);
            persona.on_tick(state, rng);
        }
        state.pending.clear();
    }

    fn regime(persona: &mut MlRetail, state: &mut AgentState, rng: &mut StdRng, drift: f64) {
        for _ in 0..30 {
            walk(state, rng, drift);
            state.pending.insert(1, state.now);
            persona.on_tick(state, rng);
        }
        state.pending.clear();
        walk(state, rng, drift);
    }

    #[test]
    fn learns_momentum_from_the_tape_before_it_trades() {
        let mut rng = StdRng::seed_from_u64(21);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_000);
        let mut persona = MlRetail::new(vec![acme()], &params());
        assert!(persona.on_tick(&state, &mut rng).is_empty(), "no model yet");
        trending_market(&mut persona, &mut state, &mut rng);
        assert!(persona.ready());
        assert!(persona.model().seen > 500);
        assert!(
            persona.model().weights[0] > 0.0,
            "trend-following weight on the short return: {:?}",
            persona.model()
        );
        assert!(persona.model().hit_rate() > 0.7, "{}", persona.detail());
        regime(&mut persona, &mut state, &mut rng, 12.0);
        let entry = persona.on_tick(&state, &mut rng);
        assert!(
            matches!(
                entry[..],
                [Action::Bracket {
                    side: Side::Buy,
                    ..
                }] | [Action::Conditional {
                    side: Side::Buy,
                    trigger: Trigger::Below(_),
                    ..
                }]
            ),
            "{entry:?} with {}",
            persona.detail()
        );
    }

    #[test]
    fn protects_holdings_with_a_trailing_stop_and_exits_when_the_model_turns() {
        let mut rng = StdRng::seed_from_u64(22);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_000);
        let mut persona = MlRetail::new(vec![acme()], &params());
        trending_market(&mut persona, &mut state, &mut rng);
        hold(&mut state, acme(), 50);
        regime(&mut persona, &mut state, &mut rng, 12.0);
        let protect = persona.on_tick(&state, &mut rng);
        assert!(
            matches!(
                protect[..],
                [Action::Conditional {
                    side: Side::Sell,
                    qty: 50,
                    trigger: Trigger::TrailingBelow(_),
                    ..
                }]
            ),
            "{protect:?} with {}",
            persona.detail()
        );
        regime(&mut persona, &mut state, &mut rng, -12.0);
        let mut exit = persona.on_tick(&state, &mut rng);
        for _ in 0..10 {
            if !exit.is_empty() {
                break;
            }
            walk(&mut state, &mut rng, -12.0);
            exit = persona.on_tick(&state, &mut rng);
        }
        assert_eq!(
            exit,
            vec![Action::Place {
                symbol: acme(),
                side: Side::Sell,
                kind: OrderKind::Market,
                qty: 50
            }],
            "{}",
            persona.detail()
        );
    }

    #[test]
    fn retuning_keeps_what_the_model_learned() {
        let mut rng = StdRng::seed_from_u64(23);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_000);
        let mut persona = MlRetail::new(vec![acme()], &params());
        trending_market(&mut persona, &mut state, &mut rng);
        let learned = persona.model().clone();
        let mut bold = params();
        bold.0.insert("style".into(), "bold".into());
        bold.0.insert("enter".into(), "0.9".into());
        assert!(persona.retune(&bold));
        assert_eq!(persona.model().weights, learned.weights);
        assert_eq!(persona.settings.enter, 0.9);
        assert_eq!(persona.settings.budget_pct, 0.30);
        let quirks = persona.quirks.unwrap();
        assert_eq!(
            persona.model().learning_rate,
            0.05 * quirks.learning_rate,
            "an agent keeps its own quirks across retuning"
        );
    }

    #[test]
    fn agents_watching_the_same_tape_learn_different_models() {
        let mut state = state_with(&[(acme(), 10_000)], 10_000_000);
        let mut a = MlRetail::new(vec![acme()], &params());
        let mut b = MlRetail::new(vec![acme()], &params());
        let mut rng_a = StdRng::seed_from_u64(31);
        let mut rng_b = StdRng::seed_from_u64(32);
        let mut tape = StdRng::seed_from_u64(33);
        let mut drift = 12.0;
        for _ in 0..600 {
            if tape.random_bool(1.0 / 60.0) {
                drift = -drift;
            }
            walk(&mut state, &mut tape, drift);
            state.pending.insert(1, state.now);
            a.on_tick(&state, &mut rng_a);
            b.on_tick(&state, &mut rng_b);
        }
        assert_ne!(a.quirks, b.quirks);
        assert_ne!(a.model().weights, b.model().weights);
        assert!(a.model().weights[0] > 0.0 && b.model().weights[0] > 0.0);
    }
}
