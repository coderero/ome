use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_protocol::{Scenario, SimCommand};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct WorldView {
    pub fair: BTreeMap<Symbol, Price>,
    pub shock: BTreeMap<Symbol, f64>,
    pub sentiment: BTreeMap<Symbol, f64>,
    pub activity: f64,
    pub drought: bool,
}

impl Default for WorldView {
    fn default() -> Self {
        Self {
            fair: BTreeMap::new(),
            shock: BTreeMap::new(),
            sentiment: BTreeMap::new(),
            activity: 1.0,
            drought: false,
        }
    }
}

impl WorldView {
    pub fn sentiment(&self, symbol: Symbol) -> f64 {
        self.sentiment.get(&symbol).copied().unwrap_or(0.0)
    }

    pub fn fair_value(&self, symbol: Symbol, anchor: Price) -> Price {
        match self.fair.get(&symbol) {
            Some(fair) => *fair,
            None => {
                let shock = self.shock.get(&symbol).copied().unwrap_or(1.0);
                (anchor as f64 * shock).round().max(1.0) as Price
            }
        }
    }
}

struct Fair {
    price: f64,
    drift_bps: f64,
    set: Instant,
}

struct Mood {
    level: f64,
    start: Instant,
    until: Instant,
}

#[derive(Default)]
pub struct World {
    fair: BTreeMap<Symbol, Fair>,
    shock: BTreeMap<Symbol, f64>,
    sentiment: BTreeMap<Symbol, Mood>,
    busy_until: Option<Instant>,
    drought_until: Option<Instant>,
}

impl World {
    pub fn apply(&mut self, command: &SimCommand, now: Instant) {
        match command {
            SimCommand::SetFair {
                symbol,
                price,
                drift_bps,
            } => {
                self.shock.remove(symbol);
                self.fair.insert(
                    *symbol,
                    Fair {
                        price: *price as f64,
                        drift_bps: *drift_bps as f64,
                        set: now,
                    },
                );
            }
            SimCommand::Scenario(scenario) => {
                let until = now + Duration::from_secs(scenario.seconds());
                match *scenario {
                    Scenario::FlashCrash { symbol, .. } => {
                        self.mood(symbol, -1.0, now, until);
                        self.busy_until = Some(until);
                        self.drought_until = Some(until);
                    }
                    Scenario::NewsShock {
                        symbol, move_bps, ..
                    } => {
                        let factor = 1.0 + move_bps as f64 / 10_000.0;
                        match self.fair.get_mut(&symbol) {
                            Some(fair) => {
                                fair.price = Self::drifted(fair, now) * factor;
                                fair.set = now;
                            }
                            None => *self.shock.entry(symbol).or_insert(1.0) *= factor,
                        }
                        if move_bps != 0 {
                            self.mood(symbol, 0.8 * (move_bps.signum() as f64), now, until);
                        }
                    }
                    Scenario::LiquidityDrought { .. } => self.drought_until = Some(until),
                    Scenario::VolatilityBurst { .. } => self.busy_until = Some(until),
                }
            }
            _ => {}
        }
    }

    fn mood(&mut self, symbol: Symbol, level: f64, start: Instant, until: Instant) {
        self.sentiment.insert(
            symbol,
            Mood {
                level,
                start,
                until,
            },
        );
    }

    fn drifted(fair: &Fair, now: Instant) -> f64 {
        let minutes = now.duration_since(fair.set).as_secs_f64() / 60.0;
        fair.price * (1.0 + fair.drift_bps / 10_000.0).powf(minutes)
    }

    pub fn view(&self, now: Instant) -> WorldView {
        let live = |until: Option<Instant>| until.is_some_and(|u| u > now);
        WorldView {
            fair: self
                .fair
                .iter()
                .map(|(s, f)| (*s, Self::drifted(f, now).round().max(1.0) as Price))
                .collect(),
            shock: self.shock.clone(),
            sentiment: self
                .sentiment
                .iter()
                .filter(|(_, m)| m.until > now)
                .map(|(s, m)| {
                    let total = m.until.duration_since(m.start).as_secs_f64().max(0.001);
                    let left = m.until.duration_since(now).as_secs_f64();
                    (*s, m.level * (left / total).clamp(0.0, 1.0).sqrt())
                })
                .collect(),
            activity: if live(self.busy_until) { 3.0 } else { 1.0 },
            drought: live(self.drought_until),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acme() -> Symbol {
        Symbol::parse("ACME").unwrap()
    }

    #[test]
    fn scenarios_shape_sentiment_activity_liquidity_and_fair_value() {
        let t0 = Instant::now();
        let mut world = World::default();
        world.apply(
            &SimCommand::Scenario(Scenario::FlashCrash {
                symbol: acme(),
                seconds: 10,
            }),
            t0,
        );
        let view = world.view(t0);
        assert_eq!((view.sentiment(acme()), view.activity), (-1.0, 3.0));
        assert!(
            view.drought,
            "liquidity providers step back in a flash crash"
        );
        let later = world.view(t0 + Duration::from_secs(5));
        assert!(later.sentiment(acme()) > -1.0 && later.sentiment(acme()) < -0.5);
        assert_eq!(
            world.view(t0 + Duration::from_secs(11)).sentiment(acme()),
            0.0
        );

        world.apply(
            &SimCommand::Scenario(Scenario::NewsShock {
                symbol: acme(),
                move_bps: -1_000,
                seconds: 30,
            }),
            t0,
        );
        assert_eq!(world.view(t0).fair_value(acme(), 25_00), 22_50);
        world.apply(
            &SimCommand::SetFair {
                symbol: acme(),
                price: 30_00,
                drift_bps: 0,
            },
            t0,
        );
        assert_eq!(world.view(t0).fair_value(acme(), 25_00), 30_00);
        world.apply(
            &SimCommand::Scenario(Scenario::LiquidityDrought { seconds: 5 }),
            t0,
        );
        assert!(world.view(t0).drought);
        assert!(!world.view(t0 + Duration::from_secs(6)).drought);
    }
}
