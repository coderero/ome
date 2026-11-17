use rand::Rng;
use rand::rngs::StdRng;
use std::ops::RangeInclusive;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Company,
    Sector,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventType {
    pub name: &'static str,
    pub scope: Scope,
    pub sentiment: RangeInclusive<i64>,
    pub growth_bps: RangeInclusive<i64>,
    pub margin_rel_bps: RangeInclusive<i64>,
    pub one_off_bps: RangeInclusive<i64>,
    pub weight: u32,
}

struct Effect {
    growth_bps: RangeInclusive<i64>,
    margin_rel_bps: RangeInclusive<i64>,
    one_off_bps: RangeInclusive<i64>,
}

const NONE: Effect = Effect { growth_bps: 0..=0, margin_rel_bps: 0..=0, one_off_bps: 0..=0 };

const fn growth(range: RangeInclusive<i64>) -> Effect {
    Effect { growth_bps: range, margin_rel_bps: 0..=0, one_off_bps: 0..=0 }
}

const fn event(name: &'static str, scope: Scope, sentiment: RangeInclusive<i64>, effect: Effect, weight: u32) -> EventType {
    EventType {
        name,
        scope,
        sentiment,
        growth_bps: effect.growth_bps,
        margin_rel_bps: effect.margin_rel_bps,
        one_off_bps: effect.one_off_bps,
        weight,
    }
}

use Scope::{Company as C, Sector as S};

pub const CATALOGUE: &[EventType] = &[
    event("contract-win", C, 30..=70, Effect { growth_bps: 0..=15, margin_rel_bps: 0..=0, one_off_bps: 20..=80 }, 10),
    event("product-launch", C, 20..=60, growth(5..=25), 8),
    event("lawsuit", C, -60..=-25, Effect { growth_bps: 0..=0, margin_rel_bps: 0..=0, one_off_bps: -150..=-25 }, 3),
    event("recall", C, -70..=-30, Effect { growth_bps: -15..=0, margin_rel_bps: 0..=0, one_off_bps: -250..=-50 }, 2),
    event("exec-departure", C, -40..=-10, growth(-15..=0), 5),
    event("buyback", C, 20..=50, NONE, 5),
    event("guidance-raise", C, 40..=80, growth(40..=150), 4),
    event("guidance-cut", C, -80..=-40, growth(-150..=-40), 4),
    event("upgrade", C, 20..=50, NONE, 8),
    event("downgrade", C, -50..=-20, NONE, 8),
    event("merger-rumour", C, 50..=90, NONE, 2),
    event("regulation", S, -60..=-20, Effect { growth_bps: -15..=0, margin_rel_bps: -150..=-50, one_off_bps: 0..=0 }, 3),
    event("demand-boom", S, 30..=70, growth(10..=40), 3),
    event("supply-shock", S, -50..=-20, Effect { growth_bps: 0..=0, margin_rel_bps: -200..=-50, one_off_bps: 0..=0 }, 3),
    event("subsidy", S, 20..=60, Effect { growth_bps: 0..=0, margin_rel_bps: 50..=150, one_off_bps: 0..=0 }, 2),
];

pub fn find(name: &str) -> Option<&'static EventType> {
    CATALOGUE.iter().find(|e| e.name == name)
}

pub fn pick(rng: &mut StdRng) -> &'static EventType {
    let total: u32 = CATALOGUE.iter().map(|e| e.weight).sum();
    let mut roll = rng.random_range(0..total);
    for event in CATALOGUE {
        if roll < event.weight {
            return event;
        }
        roll -= event.weight;
    }
    &CATALOGUE[0]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Draw {
    pub sentiment: i64,
    pub growth_bps: i64,
    pub margin_rel_bps: i64,
    pub one_off_bps: i64,
}

impl EventType {
    pub fn draw(&self, rng: &mut StdRng) -> Draw {
        Draw {
            sentiment: rng.random_range(self.sentiment.clone()),
            growth_bps: rng.random_range(self.growth_bps.clone()),
            margin_rel_bps: rng.random_range(self.margin_rel_bps.clone()),
            one_off_bps: rng.random_range(self.one_off_bps.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn events_are_drawn_by_weight_and_keep_tone_and_effect_consistent() {
        let mut rng = StdRng::seed_from_u64(9);
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..2_000 {
            let event = pick(&mut rng);
            seen.insert(event.name);
            let draw = event.draw(&mut rng);
            assert!(event.sentiment.contains(&draw.sentiment));
            assert!(
                draw.sentiment.signum() * (draw.growth_bps + draw.margin_rel_bps + draw.one_off_bps) >= 0,
                "{} moves fundamentals against its tone",
                event.name
            );
        }
        assert_eq!(seen.len(), CATALOGUE.len());
        assert_eq!(find("recall").unwrap().scope, Scope::Company);
        assert_eq!(find("subsidy").unwrap().scope, Scope::Sector);
        assert!(find("nope").is_none());
    }
}
