use crate::world::company::{CompanySpec, Fundamentals, quarterly_eps_e4};
use crate::world::economy::{Economy, Release, calendar};
use crate::world::events::{self, Draw, EventType, Scope};
use crate::world::ideas::{Idea, IdeaContext};
use ome_core::price::{Price, format_price};
use ome_core::symbol::Symbol;
use ome_protocol::{CompanyInfo, MacroInfo, NewsKind, WireItem, WorldStatus};
use rand::Rng;
use rand::rngs::StdRng;
use std::collections::{BTreeMap, VecDeque};

const RETAINED: usize = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Company(Symbol, Fundamentals),
    Economy(Economy),
    Reading(Release, i64),
    Cash(Symbol, i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact {
    DayStarted { day: u64, economy: Economy },
    Happened { wire: WireItem, changes: Vec<Change> },
}

struct Story {
    kind: NewsKind,
    event: String,
    symbol: Option<Symbol>,
    company: String,
    sector: String,
    sentiment: i64,
    facts: Vec<(String, String)>,
}

pub struct Written {
    pub headline: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filing {
    pub symbol: Symbol,
    pub reference: u64,
    pub name: String,
}

pub struct World {
    specs: Vec<CompanySpec>,
    fundamentals: BTreeMap<Symbol, Fundamentals>,
    cash: BTreeMap<Symbol, i64>,
    economy: Economy,
    start: Economy,
    readings: BTreeMap<u8, i64>,
    day: u64,
    quarter_days: u64,
    events: VecDeque<WireItem>,
    next_seq: u64,
    filed: BTreeMap<Symbol, u64>,
}

fn pct(bps: i64) -> String {
    format!(
        "{}{}.{:02}%",
        if bps < 0 { "-" } else { "" },
        bps.abs() / 100,
        bps.abs() % 100
    )
}

fn signed_pct(bps: i64) -> String {
    format!("{}{}", if bps >= 0 { "+" } else { "" }, pct(bps))
}

fn dollars_e4(e4: i64) -> String {
    format!(
        "{}{:.2}",
        if e4 < 0 { "-" } else { "" },
        e4.abs() as f64 / 10_000.0
    )
}

fn money(amount: u64) -> String {
    let digits = amount.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    format!("${out}")
}

fn facts(pairs: &[(&str, String)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

pub fn release_to(release: Release) -> u8 {
    match release {
        Release::Inflation => 1,
        Release::RateDecision => 2,
        Release::Growth => 3,
    }
}

pub fn release_from(code: u8) -> Option<Release> {
    Some(match code {
        1 => Release::Inflation,
        2 => Release::RateDecision,
        3 => Release::Growth,
        _ => return None,
    })
}

impl World {
    pub fn new(specs: Vec<CompanySpec>, quarter_days: u64) -> Self {
        let fundamentals = specs.iter().map(|s| (s.symbol, s.baseline())).collect();
        Self {
            specs,
            fundamentals,
            cash: BTreeMap::new(),
            economy: Economy::default(),
            start: Economy::default(),
            readings: BTreeMap::new(),
            day: 0,
            quarter_days: quarter_days.max(1),
            events: VecDeque::new(),
            next_seq: 1,
            filed: BTreeMap::new(),
        }
    }

    pub fn apply(&mut self, fact: &Fact) {
        match fact {
            Fact::DayStarted { day, economy } => {
                self.day = *day;
                self.economy = *economy;
            }
            Fact::Happened { wire, changes } => {
                for change in changes {
                    match change {
                        Change::Company(symbol, f) => {
                            self.fundamentals.insert(*symbol, *f);
                        }
                        Change::Economy(economy) => self.economy = *economy,
                        Change::Reading(release, value) => {
                            self.readings.insert(release_to(*release), *value);
                        }
                        Change::Cash(symbol, cash) => {
                            self.cash.insert(*symbol, *cash);
                        }
                    }
                }
                if wire.kind == NewsKind::Prospectus
                    && let Some(symbol) = wire.symbol
                {
                    self.filed.insert(symbol, wire.seq);
                }
                self.next_seq = wire.seq + 1;
                if self.events.len() == RETAINED {
                    self.events.pop_front();
                }
                self.events.push_back(wire.clone());
            }
        }
    }

    pub fn day(&self) -> u64 {
        self.day
    }

    pub fn economy(&self) -> Economy {
        self.economy
    }

    pub fn symbols(&self) -> Vec<Symbol> {
        self.specs.iter().map(|s| s.symbol).collect()
    }

    pub fn spec(&self, symbol: Symbol) -> Option<&CompanySpec> {
        self.specs.iter().find(|s| s.symbol == symbol)
    }

    pub fn fundamentals(&self, symbol: Symbol) -> Option<Fundamentals> {
        self.fundamentals.get(&symbol).copied()
    }

    pub fn cash(&self, symbol: Symbol) -> i64 {
        self.cash.get(&symbol).copied().unwrap_or(0)
    }

    pub fn fair(&self, symbol: Symbol) -> Option<Price> {
        self.fair_with(symbol, &self.fundamentals, &self.cash, &self.economy)
    }

    fn fair_with(
        &self,
        symbol: Symbol,
        fundamentals: &BTreeMap<Symbol, Fundamentals>,
        cash: &BTreeMap<Symbol, i64>,
        economy: &Economy,
    ) -> Option<Price> {
        let spec = self.spec(symbol)?;
        let cash_e2 = cash.get(&symbol).copied().unwrap_or(0);
        Some(spec.fair(fundamentals.get(&symbol)?, cash_e2, economy, &self.start))
    }

    pub fn fundamentals_data(&self, symbols: &[Symbol], changes: &[Change]) -> String {
        let mut fundamentals = self.fundamentals.clone();
        let mut cash = self.cash.clone();
        for change in changes {
            match change {
                Change::Company(symbol, f) => {
                    fundamentals.insert(*symbol, *f);
                }
                Change::Cash(symbol, c) => {
                    cash.insert(*symbol, *c);
                }
                _ => {}
            }
        }
        let entries: Vec<String> = symbols
            .iter()
            .filter_map(|s| {
                let f = fundamentals.get(s)?;
                Some(format!("{s}:{}:{}:{}", f.eps_e4, f.growth_bps, cash.get(s).copied().unwrap_or(0)))
            })
            .collect();
        if entries.is_empty() { String::new() } else { format!(" fund={}", entries.join(";")) }
    }

    pub fn fairs(&self) -> Vec<(Symbol, Price)> {
        self.specs
            .iter()
            .filter_map(|s| Some((s.symbol, self.fair(s.symbol)?)))
            .collect()
    }

    pub fn since(&self, next_seq: u64) -> Vec<WireItem> {
        self.events
            .iter()
            .filter(|w| w.seq >= next_seq)
            .cloned()
            .collect()
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    pub fn specs(&self) -> &[CompanySpec] {
        &self.specs
    }

    pub fn next_report_day(&self, symbol: Symbol) -> u64 {
        let Some(index) = self.specs.iter().position(|s| s.symbol == symbol) else {
            return 0;
        };
        (self.day + 1..)
            .find(|d| self.reports_on(index, *d))
            .unwrap_or(0)
    }

    fn reports_on(&self, index: usize, day: u64) -> bool {
        day >= 1 && (day + index as u64).is_multiple_of(self.quarter_days)
    }

    pub fn status(&self) -> WorldStatus {
        WorldStatus {
            day: self.day,
            quarter: self.day / self.quarter_days + 1,
            events: self.next_seq - 1,
            economy: MacroInfo {
                rate_bps: self.economy.rate_bps,
                inflation_bps: self
                    .readings
                    .get(&1)
                    .copied()
                    .unwrap_or(self.start.inflation_bps),
                growth_bps: self
                    .readings
                    .get(&3)
                    .copied()
                    .unwrap_or(self.start.growth_bps),
                risk_appetite: self.economy.risk_appetite,
            },
            companies: self
                .specs
                .iter()
                .map(|s| {
                    let f = self.fundamentals[&s.symbol];
                    CompanyInfo {
                        symbol: Some(s.symbol),
                        eps_cents: f.eps_e4 / 100,
                        consensus_cents: f.consensus_e4 / 100,
                        revenue: f.revenue,
                        margin_bps: f.margin_bps,
                        growth_bps: f.growth_bps,
                        fair: self.fair(s.symbol).unwrap_or(0),
                        next_report_day: self.next_report_day(s.symbol),
                        name: s.name.clone(),
                        sector: s.sector.clone(),
                    }
                })
                .collect(),
            provisioning: String::new(),
            ideas: String::new(),
        }
    }

    pub fn decide_day(&self, day: u64, rng: &mut StdRng) -> Option<Fact> {
        (day > self.day).then(|| Fact::DayStarted {
            day,
            economy: (self.day..day).fold(self.economy, |e, _| e.drift(rng)),
        })
    }

    pub fn releases_due(&self) -> Vec<Release> {
        calendar(self.day)
    }

    pub fn earnings_due(&self) -> Vec<Symbol> {
        self.specs
            .iter()
            .enumerate()
            .filter(|(i, _)| self.reports_on(*i, self.day))
            .map(|(_, s)| s.symbol)
            .collect()
    }

    pub fn prospectuses(&self) -> Vec<Filing> {
        self.filed
            .iter()
            .filter_map(|(symbol, seq)| {
                Some(Filing {
                    symbol: *symbol,
                    reference: *seq,
                    name: self.spec(*symbol)?.name.clone(),
                })
            })
            .collect()
    }

    pub fn unfiled(&self) -> Vec<Symbol> {
        self.specs
            .iter()
            .map(|s| s.symbol)
            .filter(|s| !self.filed.contains_key(s))
            .collect()
    }

    fn impact(&self, before: &[(Symbol, Price)], changes: &[Change]) -> i64 {
        let mut fundamentals = self.fundamentals.clone();
        let mut cash = self.cash.clone();
        let mut economy = self.economy;
        for change in changes {
            match change {
                Change::Company(symbol, f) => {
                    fundamentals.insert(*symbol, *f);
                }
                Change::Cash(symbol, c) => {
                    cash.insert(*symbol, *c);
                }
                Change::Economy(e) => economy = *e,
                Change::Reading(..) => {}
            }
        }
        let moves: Vec<f64> = before
            .iter()
            .filter_map(|(symbol, was)| {
                let now = self.fair_with(*symbol, &fundamentals, &cash, &economy)?;
                Some((now as f64 / (*was).max(1) as f64 - 1.0) * 10_000.0)
            })
            .collect();
        if moves.is_empty() {
            return 0;
        }
        (moves.iter().sum::<f64>() / moves.len() as f64).round() as i64
    }

    fn happen(&self, story: Story, affected: &[Symbol], changes: Vec<Change>, written: Option<Written>) -> Fact {
        let before: Vec<(Symbol, Price)> = affected
            .iter()
            .filter_map(|s| Some((*s, self.fair(*s)?)))
            .collect();
        let impact_bps = self.impact(&before, &changes);
        let data = self.fundamentals_data(affected, &changes).trim().to_string();
        let (headline, body) = written.map(|w| (w.headline, w.body)).unwrap_or_default();
        Fact::Happened {
            wire: WireItem {
                seq: self.next_seq,
                day: self.day,
                kind: story.kind,
                symbol: story.symbol,
                sentiment: story.sentiment.clamp(-100, 100),
                impact_bps,
                event: story.event,
                sector: story.sector,
                company: story.company,
                facts: story.facts,
                data,
                headline,
                body,
            },
            changes,
        }
    }

    fn story(&self, kind: NewsKind, event: &str, symbol: Option<Symbol>, sentiment: i64) -> Story {
        let spec = symbol.and_then(|s| self.spec(s));
        Story {
            kind,
            event: event.to_string(),
            symbol,
            company: spec.map(|s| s.name.clone()).unwrap_or_default(),
            sector: spec.map(|s| s.sector.clone()).unwrap_or_default(),
            sentiment,
            facts: Vec::new(),
        }
    }

    pub fn decide_prospectus(&self, symbol: Symbol) -> Option<Fact> {
        if self.filed.contains_key(&symbol) {
            return None;
        }
        let spec = self.spec(symbol)?;
        let f = self.fundamentals(symbol)?;
        let mut story = self.story(NewsKind::Prospectus, "prospectus", Some(symbol), 0);
        story.facts = facts(&[
            ("revenue", money(f.revenue * 4)),
            ("margin", pct(f.margin_bps)),
            ("eps", dollars_e4(f.eps_e4 * 4)),
            ("growth", pct(f.growth_bps)),
            ("shares", spec.shares.to_string()),
            ("float", spec.float.to_string()),
            ("price", format_price(spec.price)),
        ]);
        Some(self.happen(story, &[symbol], vec![], None))
    }

    pub fn decide_earnings(
        &self,
        symbol: Symbol,
        rng: &mut StdRng,
    ) -> Option<Fact> {
        let spec = self.spec(symbol)?;
        let f = self.fundamentals(symbol)?;
        let cycle = spec.beta_pct * (self.economy.growth_bps - 200) / 400;
        let growth_q = f.growth_bps / 4 + cycle + Economy::noisy(rng, 150.0);
        let revenue = ((f.revenue as i128 * (10_000 + growth_q) as i128 / 10_000).max(1)) as u64;
        let margin_bps = (f.margin_bps + (spec.margin_bps - f.margin_bps) / 4 + Economy::noisy(rng, 40.0)).clamp(100, 4_000);
        let eps_e4 = quarterly_eps_e4(revenue, margin_bps, spec.shares);
        let surprise_bps = (eps_e4 - f.consensus_e4) * 10_000 / f.consensus_e4.abs().max(1);
        let growth_bps = f.growth_bps + (spec.growth_bps - f.growth_bps) / 4 + (growth_q * 4 - f.growth_bps) / 8;
        let analyst_error = Economy::noisy(rng, 150.0);
        let consensus_e4 =
            eps_e4 + eps_e4 * growth_bps / 40_000 + eps_e4.abs() * analyst_error / 10_000;
        let next = Fundamentals {
            revenue,
            margin_bps,
            growth_bps,
            eps_e4,
            consensus_e4,
            quarters: f.quarters + 1,
        };
        let mut story = self.story(
            NewsKind::Earnings,
            "earnings",
            Some(symbol),
            (surprise_bps / 15).clamp(-100, 100),
        );
        story.facts = facts(&[
            ("eps", dollars_e4(eps_e4)),
            ("consensus", dollars_e4(f.consensus_e4)),
            ("surprise", signed_pct(surprise_bps)),
            ("revenue", money(revenue)),
            ("margin", pct(margin_bps)),
            ("growth", pct(growth_bps)),
            ("quarter", next.quarters.to_string()),
        ]);
        Some(self.happen(story, &[symbol], vec![Change::Company(symbol, next)], None))
    }

    pub fn decide_release(
        &self,
        release: Release,
        rng: &mut StdRng,
    ) -> Fact {
        let reading = |r: Release, default: i64| {
            self.readings
                .get(&release_to(r))
                .copied()
                .unwrap_or(default)
        };
        let all = self.symbols();
        let (sentiment, story_facts, changes) = match release {
            Release::Inflation => {
                let expected = reading(Release::Inflation, self.start.inflation_bps);
                let actual = self.economy.inflation_bps;
                let surprise = actual - expected;
                (
                    (-surprise * 2).clamp(-80, 80),
                    facts(&[("actual", pct(actual)), ("expected", pct(expected))]),
                    vec![
                        Change::Economy(self.economy.with_risk(-surprise / 3)),
                        Change::Reading(Release::Inflation, actual),
                    ],
                )
            }
            Release::Growth => {
                let expected = reading(Release::Growth, self.start.growth_bps);
                let actual = self.economy.growth_bps;
                let surprise = actual - expected;
                (
                    surprise.clamp(-80, 80),
                    facts(&[("actual", pct(actual)), ("expected", pct(expected))]),
                    vec![
                        Change::Economy(self.economy.with_risk(surprise / 4)),
                        Change::Reading(Release::Growth, actual),
                    ],
                )
            }
            Release::RateDecision => {
                let expected = Economy::policy_move(
                    reading(Release::Inflation, self.economy.inflation_bps),
                    reading(Release::Growth, self.economy.growth_bps),
                );
                let mut actual =
                    Economy::policy_move(self.economy.inflation_bps, self.economy.growth_bps);
                if rng.random_bool(0.1) {
                    actual = if actual == 0 {
                        25 * [-1, 1][rng.random_range(0..2)]
                    } else {
                        0
                    };
                }
                let rate = (self.economy.rate_bps + actual).max(0);
                let economy = Economy {
                    rate_bps: rate,
                    ..self.economy.with_risk(-(actual - expected))
                };
                let expectation = match expected {
                    0 => "a hold",
                    e if e > 0 => "a hike",
                    _ => "a cut",
                };
                (
                    (-actual - (actual - expected) * 2).clamp(-80, 80),
                    facts(&[
                        ("rate", pct(rate)),
                        ("change", pct(actual.abs())),
                        ("move", actual.to_string()),
                        ("expected", expectation.to_string()),
                    ]),
                    vec![Change::Economy(economy)],
                )
            }
        };
        let mut story = self.story(NewsKind::Macro, release.name(), None, sentiment);
        story.facts = story_facts;
        self.happen(story, &all, changes, None)
    }

    pub fn decide_event(
        &self,
        event: &EventType,
        symbol: Symbol,
        rng: &mut StdRng,
    ) -> Option<Fact> {
        let draw = event.draw(rng);
        self.apply_draw(event.scope, event.name, symbol, draw, None)
    }

    pub fn decide_idea(&self, symbol: Symbol, idea: &Idea) -> Option<Fact> {
        let draw = Draw {
            sentiment: idea.sentiment,
            growth_bps: idea.growth_bps,
            margin_rel_bps: idea.margin_rel_bps,
            one_off_bps: idea.one_off_bps,
        };
        let scope = if idea.sector_wide { Scope::Sector } else { Scope::Company };
        let copy = Written { headline: idea.headline.clone(), body: idea.body.clone() };
        self.apply_draw(scope, &idea.event, symbol, draw, Some(copy))
    }

    pub fn idea_context(&self, symbol: Symbol) -> Option<IdeaContext> {
        let spec = self.spec(symbol)?;
        let f = self.fundamentals(symbol)?;
        let recent = self
            .events
            .iter()
            .rev()
            .filter(|n| n.symbol == Some(symbol) || (n.kind == NewsKind::Sector && n.sector == spec.sector))
            .take(8)
            .map(|n| {
                if n.headline.is_empty() {
                    format!("{} ({})", n.event, if n.company.is_empty() { &n.sector } else { &n.company })
                } else {
                    n.headline.clone()
                }
            })
            .collect();
        let e = self.economy;
        Some(IdeaContext {
            company: spec.name.clone(),
            symbol: symbol.to_string(),
            sector: spec.sector.clone(),
            revenue: f.revenue * 4,
            margin_bps: f.margin_bps,
            growth_bps: f.growth_bps,
            recent,
            economy: format!(
                "policy rate {}, inflation {}, growth {}, risk appetite {}",
                pct(e.rate_bps),
                pct(e.inflation_bps),
                pct(e.growth_bps),
                e.risk_appetite
            ),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_draw(
        &self,
        scope: Scope,
        name: &str,
        symbol: Symbol,
        draw: Draw,
        written: Option<Written>,
    ) -> Option<Fact> {
        let spec = self.spec(symbol)?;
        let affected: Vec<Symbol> = match scope {
            Scope::Company => vec![symbol],
            Scope::Sector => self
                .specs
                .iter()
                .filter(|s| s.sector == spec.sector)
                .map(|s| s.symbol)
                .collect(),
        };
        let mut changes: Vec<Change> = Vec::new();
        for s in &affected {
            let (Some(f), Some(company)) = (self.fundamentals(*s), self.spec(*s)) else { continue };
            if draw.growth_bps != 0 || draw.margin_rel_bps != 0 {
                let margin_bps = (f.margin_bps + f.margin_bps * draw.margin_rel_bps / 10_000).clamp(100, 4_000);
                let growth_bps = (f.growth_bps + draw.growth_bps).clamp(-1_000, 3_000);
                let eps_e4 = quarterly_eps_e4(f.revenue, margin_bps, company.shares);
                changes.push(Change::Company(
                    *s,
                    Fundamentals {
                        margin_bps,
                        growth_bps,
                        eps_e4,
                        consensus_e4: eps_e4 + eps_e4 * growth_bps / 40_000,
                        ..f
                    },
                ));
            }
            if draw.one_off_bps != 0 {
                let per_share = company.revenue_per_share_e2(&f) * draw.one_off_bps / 10_000;
                changes.push(Change::Cash(*s, self.cash(*s) + per_share));
            }
        }
        let (kind, story_symbol) = match scope {
            Scope::Company => (NewsKind::Company, Some(symbol)),
            Scope::Sector => (NewsKind::Sector, None),
        };
        let mut story = self.story(kind, name, story_symbol, draw.sentiment);
        story.sector = spec.sector.clone();
        story.facts = facts(&[
            ("event", name.to_string()),
            ("growth_change", signed_pct(draw.growth_bps)),
            ("margin_change", signed_pct(draw.margin_rel_bps)),
            ("one_off", signed_pct(draw.one_off_bps)),
        ]);
        if written.is_some() {
            story.facts.push(("source".into(), "llm".into()));
        }
        Some(self.happen(story, &affected, changes, written))
    }

    pub fn decide_shock(&self, symbol: Symbol, move_bps: i64) -> Option<Fact> {
        let spec = self.spec(symbol)?;
        let f = self.fundamentals(symbol)?;
        let margin_bps = (f.margin_bps * (10_000 + move_bps) / 10_000).max(1);
        let eps_e4 = quarterly_eps_e4(f.revenue, margin_bps, spec.shares);
        let next = Fundamentals {
            margin_bps,
            eps_e4,
            consensus_e4: eps_e4 + eps_e4 * f.growth_bps / 40_000,
            ..f
        };
        let mut story = self.story(
            NewsKind::Company,
            "shock",
            Some(symbol),
            (move_bps / 10).clamp(-90, 90),
        );
        story.facts = facts(&[("event", "shock".into()), ("move", signed_pct(move_bps))]);
        Some(self.happen(story, &[symbol], vec![Change::Company(symbol, next)], None))
    }

    pub fn decide_random(
        &self,
        rng: &mut StdRng,
    ) -> Option<Fact> {
        if self.specs.is_empty() {
            return None;
        }
        let event = events::pick(rng);
        let symbol = self.specs[rng.random_range(0..self.specs.len())].symbol;
        self.decide_event(event, symbol, rng)
    }

    pub fn inject(
        &self,
        symbol: Option<Symbol>,
        name: &str,
        rng: &mut StdRng,
    ) -> Result<Fact, String> {
        if let Some(release) = Release::parse(name) {
            return Ok(self.decide_release(release, rng));
        }
        let symbol = symbol.ok_or(format!("'{name}' needs a symbol"))?;
        if self.spec(symbol).is_none() {
            return Err(format!("{symbol} is not covered by the newsroom"));
        }
        if let Some(bps) = name.strip_prefix("shock:") {
            let move_bps: i64 = bps.parse().map_err(|_| format!("invalid shock '{bps}'"))?;
            if !(-5_000..=5_000).contains(&move_bps) || move_bps == 0 {
                return Err("a shock moves earnings by -50% to +50%".into());
            }
            return self
                .decide_shock(symbol, move_bps)
                .ok_or(format!("nothing to publish for {symbol}"));
        }
        let fact = match name {
            "earnings" => self.decide_earnings(symbol, rng),
            "prospectus" => self.decide_prospectus(symbol),
            _ => {
                let event = events::find(name).ok_or_else(|| {
                    let names: Vec<&str> = events::CATALOGUE.iter().map(|e| e.name).collect();
                    format!(
                        "unknown event '{name}'; try earnings, cpi, rates, gdp or {}",
                        names.join(", ")
                    )
                })?;
                self.decide_event(event, symbol, rng)
            }
        };
        fact.ok_or(format!("nothing to publish for {symbol}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::company::parse_companies;
    use rand::SeedableRng;

    pub fn world() -> World {
        let specs = parse_companies(
            "company ACME sector=robotics price=25.00 shares=52000 eps=1.25 margin=12 growth=9 beta=1.2 name=Acme Robotics
company NOVA sector=energy price=12.50 shares=104000 eps=0.80 margin=8 growth=4 beta=0.9 name=Nova Energy
company BOLT sector=robotics price=40.00 shares=20000 eps=2.00 margin=15 growth=12 beta=1.4 name=Bolt Motion",
        )
        .unwrap();
        World::new(specs, 5)
    }

    fn acme() -> Symbol {
        Symbol::parse("ACME").unwrap()
    }

    fn run(room: &mut World, decide: impl FnOnce(&World) -> Fact) -> WireItem {
        let fact = decide(room);
        room.apply(&fact);
        match fact {
            Fact::Happened { wire, .. } => wire,
            other => panic!("expected an event, got {other:?}"),
        }
    }

    fn fact<'a>(wire: &'a WireItem, key: &str) -> Option<&'a str> {
        wire.facts.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn an_invented_event_is_published_with_its_own_text_and_checked_economics() {
        let mut room = world();
        let ctx = room.idea_context(acme()).unwrap();
        assert_eq!((ctx.company.as_str(), ctx.sector.as_str()), ("Acme Robotics", "robotics"));
        let idea = Idea {
            sector_wide: false,
            event: "warehouse-deal".into(),
            headline: "Acme Robotics lands a warehouse automation deal".into(),
            body: "Acme Robotics will automate the warehouses of a regional grocer.".into(),
            sentiment: 40,
            growth_bps: 30,
            margin_rel_bps: 0,
            one_off_bps: 40,
        };
        let before = room.fundamentals(acme()).unwrap();
        let item = run(&mut room, |r| r.decide_idea(acme(), &idea).unwrap());
        assert_eq!(item.headline, idea.headline, "the model's own words travel with the event");
        assert!(fact(&item, "source") == Some("llm") && item.event == "warehouse-deal" && item.data.starts_with("fund=ACME:"));
        assert_eq!(room.fundamentals(acme()).unwrap().growth_bps, before.growth_bps + 30);
        assert!(room.cash(acme()) > 0 && item.impact_bps > 0 && item.impact_bps < 100, "{}", item.impact_bps);
        assert!(room.idea_context(acme()).unwrap().recent.contains(&idea.headline), "the next prompt sees it as recent coverage");
    }

    #[test]
    fn story_impacts_come_from_company_economics_and_stay_mostly_small() {
        let mut room = world();
        let mut rng = StdRng::seed_from_u64(21);
        let start = room.fair(acme()).unwrap();
        let symbols = room.symbols();
        let mut impacts = Vec::new();
        for story in 0..2_000 {
            let item = run(&mut room, |r| r.decide_random(&mut StdRng::seed_from_u64(rng.random())).unwrap());
            impacts.push(item.impact_bps.abs());
            if story % 60 == 59 {
                for symbol in &symbols {
                    run(&mut room, |r| r.decide_earnings(*symbol, &mut rng).unwrap());
                }
            }
        }
        impacts.sort();
        let at = |q: f64| impacts[((impacts.len() - 1) as f64 * q) as usize];
        assert!(at(0.5) <= 60, "median story moves value under 0.6%: {}", at(0.5));
        assert!(at(0.9) <= 200, "nine in ten under 2%: {}", at(0.9));
        assert!(impacts[impacts.len() - 1] <= 600, "no single story beyond 6%: {}", impacts[impacts.len() - 1]);
        let drifted = room.fair(acme()).unwrap() as f64 / start as f64;
        let compounding = 1.09_f64.powf(33.0 / 4.0);
        let relative = drifted / compounding;
        assert!(relative > 0.6 && relative < 1.6, "after 33 quarters value follows the company's 9% growth ({compounding:.2}x), not a random walk: {drifted:.2}x");
    }

    #[test]
    fn prospectuses_are_filed_once_with_the_financial_baseline() {
        let mut room = world();
        assert_eq!(room.unfiled().len(), 3);
        let item = run(&mut room, |r| r.decide_prospectus(acme()).unwrap());
        assert_eq!(
            (item.seq, item.kind, item.symbol),
            (1, NewsKind::Prospectus, Some(acme()))
        );
        assert!(matches!(fact(&item, "eps"), Some("1.24") | Some("1.25")), "{:?}", item.facts);
        assert_eq!(fact(&item, "price"), Some("25.00"));
        assert_eq!(item.company, "Acme Robotics");
        assert!(room.decide_prospectus(acme()).is_none());
        assert_eq!(room.unfiled().len(), 2);
        assert_eq!(room.prospectuses()[0].reference, 1);
    }

    #[test]
    fn earnings_follow_a_staggered_quarterly_calendar_and_move_fair_value_with_the_surprise() {
        let mut room = world();
        let mut rng = StdRng::seed_from_u64(2);
        let mut reported = BTreeMap::new();
        let mut beats = 0;
        for day in 1..=40 {
            let started = room.decide_day(day, &mut rng).unwrap();
            room.apply(&started);
            assert!(room.decide_day(day, &mut rng).is_none());
            for symbol in room.earnings_due() {
                let before = room.fair(symbol).unwrap();
                let item = run(&mut room, |r| r.decide_earnings(symbol, &mut rng).unwrap());
                *reported.entry(symbol).or_insert(0) += 1;
                assert_eq!(item.day, day);
                let after = room.fair(symbol).unwrap();
                let moved = (after as f64 / before as f64 - 1.0) * 10_000.0;
                assert!((moved - item.impact_bps as f64).abs() <= 1.0);
                if item.sentiment > 0 {
                    beats += 1;
                }
                assert!(fact(&item, "eps").is_some() && fact(&item, "consensus").is_some());
            }
        }
        assert_eq!(reported.len(), 3, "every company reports");
        assert!(reported.values().all(|n| *n == 8), "{reported:?}");
        assert!((5..20).contains(&beats), "beats {beats}/24");
        assert!(room.next_report_day(acme()) > 40);
    }

    #[test]
    fn macro_releases_move_rates_risk_and_every_fair_value() {
        let mut room = world();
        let mut rng = StdRng::seed_from_u64(3);
        room.apply(&Fact::DayStarted {
            day: 2,
            economy: Economy {
                inflation_bps: 420,
                ..Economy::default()
            },
        });
        let cpi = run(&mut room, |r| r.decide_release(Release::Inflation, &mut rng));
        assert!(cpi.sentiment < 0 && cpi.event == "cpi" && fact(&cpi, "actual") == Some("4.20%"));
        assert!(room.economy().risk_appetite < 0);
        let before = room.fair(acme()).unwrap();
        let mut hikes = 0;
        for _ in 0..20 {
            let rates = run(&mut room, |r| r.decide_release(Release::RateDecision, &mut rng));
            if fact(&rates, "move") == Some("25") {
                hikes += 1;
                assert!(rates.sentiment < 0);
            }
        }
        assert!(hikes >= 14, "hot inflation brings hikes: {hikes}");
        assert!(room.economy().rate_bps > 450 + 14 * 25 - 6 * 25);
        assert!(room.fair(acme()).unwrap() < before);
        assert_eq!(room.status().economy.inflation_bps, 420);
    }

    #[test]
    fn company_and_sector_events_change_fundamentals_and_injection_validates() {
        let mut room = world();
        let mut rng = StdRng::seed_from_u64(4);
        let before = room.fundamentals(acme()).unwrap();
        let lawsuit = run(&mut room, |r| {
            r.inject(Some(acme()), "lawsuit", &mut rng)
                .unwrap()
        });
        assert!(
            lawsuit.sentiment < 0 && lawsuit.impact_bps < 0,
            "{lawsuit:?}"
        );
        let after = room.fundamentals(acme()).unwrap();
        assert_eq!(after, before, "a settlement is a one-off cost, not a lasting change to the business");
        assert!(room.cash(acme()) < 0, "the cost comes off the value per share");
        assert!(lawsuit.impact_bps > -150, "sized against the company's revenue, so the hit is small: {}", lawsuit.impact_bps);
        assert!(lawsuit.data.contains("fund=ACME:"), "the event carries the fundamentals: {}", lawsuit.data);
        let bolt = Symbol::parse("BOLT").unwrap();
        let bolt_growth = room.fundamentals(bolt).unwrap().growth_bps;
        let boom = run(&mut room, |r| {
            r.inject(Some(acme()), "demand-boom", &mut rng)
                .unwrap()
        });
        assert_eq!(
            (boom.kind, boom.symbol, boom.sector.as_str()),
            (NewsKind::Sector, None, "robotics")
        );
        assert!(
            room.fundamentals(bolt).unwrap().growth_bps > bolt_growth,
            "same sector"
        );
        let upgrade = run(&mut room, |r| {
            r.inject(Some(acme()), "upgrade", &mut rng)
                .unwrap()
        });
        assert!(upgrade.sentiment > 0);
        assert_eq!(upgrade.impact_bps, 0, "opinions do not change fundamentals");
        assert!(room.inject(None, "lawsuit", &mut rng).is_err());
        assert!(
            room.inject(Some(acme()), "nope", &mut rng)
                .is_err()
        );
        assert!(room.inject(None, "gdp", &mut rng).is_ok());
        let fair = room.fair(acme()).unwrap();
        let shock = run(&mut room, |r| {
            r.inject(Some(acme()), "shock:-800", &mut rng)
                .unwrap()
        });
        assert!((shock.impact_bps + 800).abs() <= 5, "{shock:?}");
        assert!(room.fair(acme()).unwrap() < fair && shock.sentiment == -80);
        assert!(
            room.inject(Some(acme()), "shock:x", &mut rng)
                .is_err()
        );
        assert!(
            room.inject(Some(acme()), "shock:9000", &mut rng)
                .is_err()
        );
        assert_eq!(room.since(2).len(), 3);
        assert_eq!(room.next_seq(), 5);
    }
}
