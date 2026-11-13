mod factor;
mod market_maker;
mod noise;
mod pension;
mod prop;
mod retail;
mod statarb;

use crate::action::Action;
use crate::state::AgentState;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{Qty, Timestamp};
use rand::Rng;
use rand::rngs::StdRng;
use std::collections::BTreeMap;
use std::f64::consts::TAU;
use std::time::Duration;

pub use factor::FactorModel;
pub use market_maker::MarketMaker;
pub use noise::NoiseTrader;
pub use pension::Pension;
pub use prop::{PropDesk, PropSignal};
pub use retail::{Persona, RetailPersona};
pub use statarb::PairsTrader;

pub trait Strategy: Send {
    fn name(&self) -> &'static str;
    fn interval(&self) -> Duration;
    fn on_tick(&mut self, state: &AgentState, rng: &mut StdRng) -> Vec<Action>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Params(pub BTreeMap<String, String>);

impl Params {
    pub fn f64(&self, key: &str, default: f64) -> f64 {
        self.0
            .get(key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    pub fn u64(&self, key: &str, default: u64) -> u64 {
        self.0
            .get(key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    pub fn text<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.0.get(key).map_or(default, String::as_str)
    }
}

pub const STRATEGIES: &[&str] = &[
    "market-maker",
    "dmm",
    "noise",
    "daytrader",
    "swing",
    "investor",
    "momentum",
    "breakout",
    "meanrev",
    "pairs",
    "factor",
    "pension",
];

pub fn create(name: &str, symbols: &[Symbol], params: &Params) -> Option<Box<dyn Strategy>> {
    let symbols = symbols.to_vec();
    Some(match name {
        "market-maker" => Box::new(MarketMaker::new(symbols, params, false)),
        "dmm" => Box::new(MarketMaker::new(symbols, params, true)),
        "noise" => Box::new(NoiseTrader::new(symbols, params)),
        "daytrader" => Box::new(RetailPersona::new(Persona::DayTrader, symbols, params)),
        "swing" => Box::new(RetailPersona::new(Persona::Swing, symbols, params)),
        "investor" => Box::new(RetailPersona::new(Persona::Investor, symbols, params)),
        "momentum" => Box::new(PropDesk::new(PropSignal::Momentum, symbols, params)),
        "breakout" => Box::new(PropDesk::new(PropSignal::Breakout, symbols, params)),
        "meanrev" => Box::new(PropDesk::new(PropSignal::MeanReversion, symbols, params)),
        "pairs" => Box::new(PairsTrader::new(symbols, params)?),
        "factor" => Box::new(FactorModel::new(symbols, params)),
        "pension" => Box::new(Pension::new(symbols, params)),
        _ => return None,
    })
}

pub fn gaussian(rng: &mut StdRng) -> f64 {
    let u1: f64 = rng.random_range(f64::EPSILON..1.0);
    let u2: f64 = rng.random();
    (-2.0 * u1.ln()).sqrt() * (TAU * u2).cos()
}

pub fn scale(price: Price, bps: f64) -> Price {
    ((price as f64) * (1.0 + bps / 10_000.0)).round().max(1.0) as Price
}

pub fn affordable(cash: u64, price: Price, qty: Qty) -> Qty {
    cash.checked_div(price).map_or(0, |max| qty.min(max))
}

pub fn cancel_stale(state: &AgentState, symbol: Option<Symbol>, ttl: Timestamp) -> Vec<Action> {
    state
        .open_orders
        .iter()
        .filter(|(_, o)| symbol.is_none_or(|s| s == o.symbol))
        .filter(|(_, o)| state.now.saturating_sub(o.placed_at) > ttl)
        .map(|(id, _)| Action::Cancel(*id))
        .collect()
}

pub fn pick(symbols: &[Symbol], rng: &mut StdRng) -> Option<Symbol> {
    (!symbols.is_empty()).then(|| symbols[rng.random_range(0..symbols.len())])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_strategy_can_be_created_from_parameters() {
        let symbols = [
            Symbol::parse("ACME").unwrap(),
            Symbol::parse("NOVA").unwrap(),
        ];
        for name in STRATEGIES {
            assert!(
                create(name, &symbols, &Params::default()).is_some(),
                "{name}"
            );
        }
        assert!(create("pairs", &symbols[..1], &Params::default()).is_none());
        assert!(create("nope", &symbols, &Params::default()).is_none());
        let params = Params([("gamma".to_string(), "0.3".to_string())].into());
        assert_eq!(params.f64("gamma", 1.0), 0.3);
        assert_eq!(params.u64("size", 7), 7);
    }
}
