use crate::economy::Economy;
use ome_core::price::{Price, parse_price};
use ome_core::symbol::Symbol;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanySpec {
    pub symbol: Symbol,
    pub name: String,
    pub sector: String,
    pub price: Price,
    pub shares: u64,
    pub eps_cents: i64,
    pub margin_bps: i64,
    pub growth_bps: i64,
    pub beta_pct: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fundamentals {
    pub revenue: u64,
    pub margin_bps: i64,
    pub growth_bps: i64,
    pub eps_e4: i64,
    pub consensus_e4: i64,
    pub quarters: u64,
}

pub fn quarterly_eps_e4(revenue: u64, margin_bps: i64, shares: u64) -> i64 {
    (revenue as i128 * margin_bps as i128 / shares.max(1) as i128) as i64
}

fn percent_bps(text: &str) -> Option<i64> {
    let value: f64 = text.parse().ok()?;
    Some((value * 100.0).round() as i64)
}

pub fn parse_companies(text: &str) -> Result<Vec<CompanySpec>, String> {
    let mut companies = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |why: &str| format!("line {}: {why}", n + 1);
        let (head, name) = line
            .split_once(" name=")
            .ok_or_else(|| at("name=... must come last"))?;
        let mut words = head.split_whitespace();
        if words.next() != Some("company") {
            return Err(at("expected 'company SYMBOL key=value ... name=Full Name'"));
        }
        let symbol = words
            .next()
            .and_then(Symbol::parse)
            .ok_or_else(|| at("invalid symbol"))?;
        let mut spec = CompanySpec {
            symbol,
            name: name.trim().to_string(),
            sector: "general".into(),
            price: 0,
            shares: 0,
            eps_cents: 0,
            margin_bps: 1_000,
            growth_bps: 500,
            beta_pct: 100,
        };
        for pair in words {
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| at(&format!("expected key=value, got '{pair}'")))?;
            let bad = || at(&format!("invalid {key} '{value}'"));
            match key {
                "sector" => spec.sector = value.to_string(),
                "price" => spec.price = parse_price(value).ok_or_else(bad)?,
                "shares" => spec.shares = value.parse().map_err(|_| bad())?,
                "eps" => spec.eps_cents = parse_price(value).ok_or_else(bad)? as i64,
                "margin" => spec.margin_bps = percent_bps(value).ok_or_else(bad)?,
                "growth" => spec.growth_bps = percent_bps(value).ok_or_else(bad)?,
                "beta" => spec.beta_pct = percent_bps(value).ok_or_else(bad)?,
                _ => return Err(at(&format!("unknown key '{key}'"))),
            }
        }
        if spec.price == 0 || spec.shares == 0 || spec.eps_cents <= 0 || spec.margin_bps <= 0 {
            return Err(at("price, shares, eps and margin must be positive"));
        }
        if companies.iter().any(|c: &CompanySpec| c.symbol == symbol) {
            return Err(at(&format!("{symbol} listed twice")));
        }
        companies.push(spec);
    }
    Ok(companies)
}

impl CompanySpec {
    pub fn baseline(&self) -> Fundamentals {
        let annual_profit = self.eps_cents as u128 * self.shares as u128 / 100;
        let revenue = ((annual_profit * 10_000 / self.margin_bps as u128 / 4) as u64).max(1);
        let eps_e4 = quarterly_eps_e4(revenue, self.margin_bps, self.shares);
        Fundamentals {
            revenue,
            margin_bps: self.margin_bps,
            growth_bps: self.growth_bps,
            eps_e4,
            consensus_e4: eps_e4 + eps_e4 * self.growth_bps / 40_000,
            quarters: 0,
        }
    }

    pub fn fair(&self, now: &Fundamentals, economy: &Economy, start: &Economy) -> Price {
        let base = self.baseline();
        let forward =
            |f: &Fundamentals| f.eps_e4 as f64 * 4.0 * (1.0 + f.growth_bps as f64 / 10_000.0);
        let base_forward = forward(&base).max(1.0);
        let earnings = (forward(now) / base_forward).max(0.05);
        let capped = |g: i64| g.clamp(-2_000, 3_000) as f64 / 10_000.0;
        let beta = self.beta_pct as f64 / 100.0;
        let rerating = -6.0 * (economy.rate_bps - start.rate_bps) as f64 / 10_000.0
            + 1.5 * (capped(now.growth_bps) - capped(base.growth_bps))
            + 0.15 * beta * (economy.risk_appetite - start.risk_appetite) as f64 / 100.0;
        let fair = self.price as f64 * earnings * rerating.clamp(-1.5, 1.0).exp();
        fair.round().max(1.0) as Price
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn acme() -> CompanySpec {
        parse_companies(
            "company ACME sector=robotics price=25.00 shares=52000 eps=1.25 margin=12 growth=9 beta=1.2 name=Acme Robotics",
        )
        .unwrap()
        .remove(0)
    }

    #[test]
    fn companies_parse_from_a_config_file() {
        let spec = acme();
        assert_eq!(spec.name, "Acme Robotics");
        assert_eq!((spec.price, spec.eps_cents), (25_00, 125));
        assert_eq!(
            (spec.margin_bps, spec.growth_bps, spec.beta_pct),
            (1_200, 900, 120)
        );
        assert!(parse_companies("company ACME price=1 name=X").is_err());
        assert!(parse_companies("company ACME price=1 shares=1 eps=1").is_err());
        assert!(parse_companies("# comment\n\n").unwrap().is_empty());
        let twice = "company A price=1 shares=10 eps=0.10 name=A\ncompany A price=1 shares=10 eps=0.10 name=A";
        assert!(parse_companies(twice).is_err());
    }

    #[test]
    fn the_baseline_reproduces_the_listing_eps_and_fair_value() {
        let spec = acme();
        let base = spec.baseline();
        assert!(
            (base.eps_e4 * 4 - 12_500).abs() <= 12,
            "annual eps {}",
            base.eps_e4 * 4
        );
        let economy = Economy::default();
        let fair = spec.fair(&base, &economy, &economy);
        assert!((fair as i64 - 25_00).abs() <= 2, "{fair}");
    }

    #[test]
    fn fair_value_follows_earnings_rates_and_risk_appetite() {
        let spec = acme();
        let start = Economy::default();
        let base = spec.baseline();
        let fair = |f: &Fundamentals, e: &Economy| spec.fair(f, e, &start);
        let better = Fundamentals {
            eps_e4: base.eps_e4 * 11 / 10,
            ..base
        };
        assert!(fair(&better, &start) > 27_00);
        let hiked = Economy {
            rate_bps: start.rate_bps + 100,
            ..start
        };
        assert!(fair(&base, &hiked) < 24_00 && fair(&base, &hiked) > 23_00);
        let fearful = Economy {
            risk_appetite: -80,
            ..start
        };
        assert!(fair(&base, &fearful) < fair(&base, &start));
        let loss = Fundamentals {
            eps_e4: -500,
            ..base
        };
        assert!(fair(&loss, &start) >= 1);
    }
}
