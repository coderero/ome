use rand::Rng;
use rand::rngs::StdRng;

pub const INFLATION_TARGET_BPS: i64 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Economy {
    pub rate_bps: i64,
    pub inflation_bps: i64,
    pub growth_bps: i64,
    pub risk_appetite: i64,
}

impl Default for Economy {
    fn default() -> Self {
        Self {
            rate_bps: 450,
            inflation_bps: 280,
            growth_bps: 200,
            risk_appetite: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    Inflation,
    RateDecision,
    Growth,
}

impl Release {
    pub fn name(&self) -> &'static str {
        match self {
            Release::Inflation => "cpi",
            Release::RateDecision => "rates",
            Release::Growth => "gdp",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "cpi" | "inflation" => Release::Inflation,
            "rates" | "rate-decision" => Release::RateDecision,
            "gdp" | "growth" => Release::Growth,
            _ => return None,
        })
    }
}

pub fn calendar(day: u64) -> Vec<Release> {
    let mut due = Vec::new();
    if day % 3 == 1 {
        due.push(Release::Inflation);
    }
    if day % 7 == 4 {
        due.push(Release::Growth);
    }
    if day % 5 == 2 {
        due.push(Release::RateDecision);
    }
    due
}

fn normal(rng: &mut StdRng, sd: f64) -> i64 {
    let u1: f64 = rng.random_range(f64::EPSILON..1.0);
    let u2: f64 = rng.random();
    ((-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos() * sd).round() as i64
}

impl Economy {
    pub fn drift(&self, rng: &mut StdRng) -> Economy {
        Economy {
            rate_bps: self.rate_bps,
            inflation_bps: (self.inflation_bps
                + (250 - self.inflation_bps) / 10
                + normal(rng, 12.0))
            .clamp(-200, 1_500),
            growth_bps: (self.growth_bps + (200 - self.growth_bps) / 10 + normal(rng, 15.0))
                .clamp(-800, 1_000),
            risk_appetite: (self.risk_appetite - self.risk_appetite / 5 + normal(rng, 8.0))
                .clamp(-100, 100),
        }
    }

    pub fn policy_move(inflation_bps: i64, growth_bps: i64) -> i64 {
        if inflation_bps > INFLATION_TARGET_BPS + 100 && growth_bps > 0 {
            25
        } else if inflation_bps < INFLATION_TARGET_BPS - 20 || growth_bps < 50 {
            -25
        } else {
            0
        }
    }

    pub fn with_risk(&self, delta: i64) -> Economy {
        Economy {
            risk_appetite: (self.risk_appetite + delta).clamp(-100, 100),
            ..*self
        }
    }

    pub fn noisy(rng: &mut StdRng, sd: f64) -> i64 {
        normal(rng, sd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn the_economy_mean_reverts_and_stays_in_bounds() {
        let mut rng = StdRng::seed_from_u64(3);
        let mut economy = Economy {
            inflation_bps: 900,
            growth_bps: -600,
            risk_appetite: 100,
            ..Economy::default()
        };
        for _ in 0..200 {
            economy = economy.drift(&mut rng);
        }
        assert!((100..450).contains(&economy.inflation_bps), "{economy:?}");
        assert!((0..400).contains(&economy.growth_bps), "{economy:?}");
        assert!(economy.risk_appetite.abs() < 60, "{economy:?}");
        assert_eq!(economy.rate_bps, 450, "only the central bank moves rates");
    }

    #[test]
    fn the_central_bank_fights_inflation_and_supports_weak_growth() {
        assert_eq!(Economy::policy_move(350, 200), 25);
        assert_eq!(Economy::policy_move(150, 200), -25);
        assert_eq!(Economy::policy_move(250, 20), -25);
        assert_eq!(Economy::policy_move(250, 200), 0);
    }

    #[test]
    fn releases_follow_a_calendar() {
        assert_eq!(calendar(1), vec![Release::Inflation]);
        assert_eq!(calendar(2), vec![Release::RateDecision]);
        assert_eq!(calendar(4), vec![Release::Inflation, Release::Growth]);
        assert!(calendar(3).is_empty());
        assert_eq!(Release::parse("gdp"), Some(Release::Growth));
        assert_eq!(
            Release::parse(Release::RateDecision.name()),
            Some(Release::RateDecision)
        );
    }
}
