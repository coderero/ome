use rand::Rng;
use rand::rngs::StdRng;

pub const INFLATION_TARGET_BPS: i64 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Economy {
    pub rate_bps: i64,
    pub inflation_bps: i64,
    pub growth_bps: i64,
    pub risk_appetite: i64,
    pub cycle: i64,
}

impl Default for Economy {
    fn default() -> Self {
        Self {
            rate_bps: 450,
            inflation_bps: 280,
            growth_bps: 200,
            risk_appetite: 0,
            cycle: 0,
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
        let cycle = (self.cycle - self.cycle * 3 / 100 + normal(rng, 6.0)).clamp(-100, 100);
        Economy {
            rate_bps: self.rate_bps,
            inflation_bps: (self.inflation_bps
                + (250 + cycle / 2 - self.inflation_bps) / 10
                + normal(rng, 12.0))
            .clamp(-200, 1_500),
            growth_bps: (self.growth_bps + (200 + 3 * cycle - self.growth_bps) / 10 + normal(rng, 15.0))
                .clamp(-800, 1_000),
            risk_appetite: (self.risk_appetite + (cycle / 2 - self.risk_appetite) / 8 + normal(rng, 8.0))
                .clamp(-100, 100),
            cycle,
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
        assert!((50..500).contains(&economy.inflation_bps), "{economy:?}");
        assert!((-200..600).contains(&economy.growth_bps), "{economy:?}");
        assert!(economy.risk_appetite.abs() < 90, "{economy:?}");
        assert_eq!(economy.rate_bps, 450, "only the central bank moves rates");
    }

    #[test]
    fn the_business_cycle_keeps_its_direction_for_weeks() {
        let mut rng = StdRng::seed_from_u64(11);
        let mut economy = Economy::default();
        let mut path = Vec::new();
        for _ in 0..600 {
            economy = economy.drift(&mut rng);
            path.push((economy.cycle, economy.growth_bps));
        }
        let same_sign = path.windows(11).filter(|w| w[0].0 != 0 && w[0].0.signum() == w[10].0.signum()).count();
        assert!(same_sign * 10 > path.len() * 7, "ten days later the cycle usually points the same way: {same_sign}");
        let boom: Vec<i64> = path.iter().filter(|(c, _)| *c > 20).map(|(_, g)| *g).collect();
        let bust: Vec<i64> = path.iter().filter(|(c, _)| *c < -20).map(|(_, g)| *g).collect();
        if !boom.is_empty() && !bust.is_empty() {
            let mean = |v: &[i64]| v.iter().sum::<i64>() / v.len() as i64;
            assert!(mean(&boom) > mean(&bust) + 50, "growth runs higher in the upswing");
        }
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
