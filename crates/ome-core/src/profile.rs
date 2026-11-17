use crate::price::{Price, parse_price};
use crate::symbol::Symbol;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Profile {
    pub symbol: Symbol,
    pub shares: Option<u64>,
    pub price: Option<Price>,
    pub float_pct: Option<f64>,
    pub adv: Option<u64>,
    pub spread_bps: Option<f64>,
    pub vol_pct: Option<f64>,
    pub depth: Option<u64>,
}

impl Profile {
    pub fn float_shares(&self, shares: u64) -> u64 {
        let float = self.float_pct.unwrap_or(100.0).clamp(1.0, 100.0);
        ((shares as f64 * float / 100.0).round() as u64).max(1)
    }
}

pub fn parse_profiles(text: &str) -> Result<Vec<Profile>, String> {
    let mut profiles: Vec<Profile> = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let at = |why: String| format!("profile line {}: {why}", n + 1);
        let mut words = line.split_whitespace();
        if words.next() != Some("profile") {
            return Err(at("lines start with 'profile <SYMBOL>'".into()));
        }
        let symbol = words
            .next()
            .and_then(Symbol::parse)
            .ok_or_else(|| at("missing or invalid symbol".into()))?;
        let mut p = Profile { symbol, shares: None, price: None, float_pct: None, adv: None, spread_bps: None, vol_pct: None, depth: None };
        for word in words {
            let (key, value) = word.split_once('=').ok_or_else(|| at(format!("expected key=value, got '{word}'")))?;
            let bad = || at(format!("invalid {key} '{value}'"));
            let number = || value.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0).ok_or_else(bad);
            match key {
                "shares" => p.shares = Some(value.parse().map_err(|_| bad())?),
                "price" => p.price = Some(parse_price(value).ok_or_else(bad)?),
                "free-float" => p.float_pct = Some(number()?),
                "adv" => p.adv = Some(value.parse().map_err(|_| bad())?),
                "spread-bps" => p.spread_bps = Some(number()?),
                "vol-pct" => p.vol_pct = Some(number()?),
                "depth-1pct" => p.depth = Some(value.parse().map_err(|_| bad())?),
                _ => return Err(at(format!("unknown key '{key}'"))),
            }
        }
        if profiles.iter().any(|q| q.symbol == symbol) {
            return Err(at(format!("{symbol} profiled twice")));
        }
        profiles.push(p);
    }
    Ok(profiles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_parse_with_optional_fields_and_free_float() {
        let p = parse_profiles("# mine\nprofile ACME shares=6700000 price=25.00 free-float=55 adv=900000 spread-bps=6 vol-pct=1.8 depth-1pct=40000\nprofile NOVA adv=10")
            .unwrap();
        assert_eq!((p[0].shares, p[0].price, p[0].adv, p[0].depth), (Some(6_700_000), Some(25_00), Some(900_000), Some(40_000)));
        assert_eq!(p[0].float_shares(6_700_000), 3_685_000);
        assert_eq!(p[1].float_shares(1_000), 1_000);
        assert!(parse_profiles("profile ACME colour=red").is_err());
        assert!(parse_profiles("profile ACME vol-pct=-1").is_err());
        assert!(parse_profiles("company ACME").is_err());
        assert!(parse_profiles("profile ACME\nprofile ACME").is_err());
    }
}
