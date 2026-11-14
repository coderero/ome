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
    pub weight: u32,
}

const fn company(
    name: &'static str,
    sentiment: RangeInclusive<i64>,
    growth_bps: RangeInclusive<i64>,
    margin_rel_bps: RangeInclusive<i64>,
    weight: u32,
) -> EventType {
    EventType {
        name,
        scope: Scope::Company,
        sentiment,
        growth_bps,
        margin_rel_bps,
        weight,
    }
}

const fn sector(
    name: &'static str,
    sentiment: RangeInclusive<i64>,
    growth_bps: RangeInclusive<i64>,
    margin_rel_bps: RangeInclusive<i64>,
    weight: u32,
) -> EventType {
    EventType {
        name,
        scope: Scope::Sector,
        sentiment,
        growth_bps,
        margin_rel_bps,
        weight,
    }
}

pub const CATALOGUE: &[EventType] = &[
    company("contract-win", 30..=70, 50..=150, 0..=0, 10),
    company("product-launch", 20..=60, 30..=100, 0..=0, 8),
    company("lawsuit", -60..=-25, 0..=0, -500..=-200, 6),
    company("recall", -70..=-30, -50..=0, -800..=-300, 4),
    company("exec-departure", -40..=-10, -40..=0, 0..=0, 5),
    company("buyback", 20..=50, 0..=0, 100..=300, 5),
    company("guidance-raise", 40..=80, 80..=200, 0..=0, 4),
    company("guidance-cut", -80..=-40, -200..=-80, 0..=0, 4),
    company("upgrade", 20..=50, 0..=0, 0..=0, 8),
    company("downgrade", -50..=-20, 0..=0, 0..=0, 8),
    company("merger-rumour", 50..=90, 0..=0, 0..=0, 2),
    sector("regulation", -60..=-20, -80..=-30, 0..=0, 3),
    sector("demand-boom", 30..=70, 50..=120, 0..=0, 3),
    sector("supply-shock", -50..=-20, 0..=0, -500..=-150, 3),
    sector("subsidy", 20..=60, 0..=0, 150..=400, 2),
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
}

impl EventType {
    pub fn draw(&self, rng: &mut StdRng) -> Draw {
        Draw {
            sentiment: rng.random_range(self.sentiment.clone()),
            growth_bps: rng.random_range(self.growth_bps.clone()),
            margin_rel_bps: rng.random_range(self.margin_rel_bps.clone()),
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
                draw.sentiment.signum() * (draw.growth_bps + draw.margin_rel_bps) >= 0,
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
