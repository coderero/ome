use crate::company::{CompanySpec, Fundamentals, quarterly_eps_e4};
use crate::economy::{Economy, Release, calendar};
use crate::events::{self, EventType, Scope};
use crate::writer::{Story, Writer};
use ome_core::price::{Price, format_price};
use ome_core::symbol::Symbol;
use ome_core::types::Timestamp;
use ome_net::auth::SecretHash;
use ome_protocol::{CompanyInfo, MacroInfo, NewsItem, NewsKind, NewsroomStatus};
use rand::Rng;
use rand::rngs::StdRng;
use std::collections::{BTreeMap, VecDeque};

const RETAINED: usize = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Company(Symbol, Fundamentals),
    Economy(Economy),
    Reading(Release, i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact {
    SubscriberRegistered {
        name: String,
        secret: SecretHash,
    },
    DayStarted {
        day: u64,
        economy: Economy,
    },
    Published {
        item: NewsItem,
        changes: Vec<Change>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filing {
    pub symbol: Symbol,
    pub reference: u64,
    pub name: String,
}

pub struct Newsroom {
    specs: Vec<CompanySpec>,
    fundamentals: BTreeMap<Symbol, Fundamentals>,
    economy: Economy,
    start: Economy,
    readings: BTreeMap<u8, i64>,
    day: u64,
    quarter_days: u64,
    news: VecDeque<NewsItem>,
    next_seq: u64,
    subscribers: BTreeMap<String, SecretHash>,
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

impl Newsroom {
    pub fn new(specs: Vec<CompanySpec>, quarter_days: u64) -> Self {
        let fundamentals = specs.iter().map(|s| (s.symbol, s.baseline())).collect();
        Self {
            specs,
            fundamentals,
            economy: Economy::default(),
            start: Economy::default(),
            readings: BTreeMap::new(),
            day: 0,
            quarter_days: quarter_days.max(1),
            news: VecDeque::new(),
            next_seq: 1,
            subscribers: BTreeMap::new(),
            filed: BTreeMap::new(),
        }
    }

    pub fn apply(&mut self, fact: &Fact) {
        match fact {
            Fact::SubscriberRegistered { name, secret } => {
                self.subscribers.insert(name.clone(), *secret);
            }
            Fact::DayStarted { day, economy } => {
                self.day = *day;
                self.economy = *economy;
            }
            Fact::Published { item, changes } => {
                for change in changes {
                    match change {
                        Change::Company(symbol, f) => {
                            self.fundamentals.insert(*symbol, *f);
                        }
                        Change::Economy(economy) => self.economy = *economy,
                        Change::Reading(release, value) => {
                            self.readings.insert(release_to(*release), *value);
                        }
                    }
                }
                if item.kind == NewsKind::Prospectus
                    && let Some(symbol) = item.symbol
                {
                    self.filed.insert(symbol, item.seq);
                }
                self.next_seq = item.seq + 1;
                if self.news.len() == RETAINED {
                    self.news.pop_front();
                }
                self.news.push_back(item.clone());
            }
        }
    }

    pub fn day(&self) -> u64 {
        self.day
    }

    pub fn economy(&self) -> Economy {
        self.economy
    }

    pub fn subscriber(&self, name: &str) -> Option<SecretHash> {
        self.subscribers.get(name).copied()
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

    pub fn fair(&self, symbol: Symbol) -> Option<Price> {
        self.fair_with(symbol, &self.fundamentals, &self.economy)
    }

    fn fair_with(
        &self,
        symbol: Symbol,
        fundamentals: &BTreeMap<Symbol, Fundamentals>,
        economy: &Economy,
    ) -> Option<Price> {
        let spec = self.spec(symbol)?;
        Some(spec.fair(fundamentals.get(&symbol)?, economy, &self.start))
    }

    pub fn fairs(&self) -> Vec<(Symbol, Price)> {
        self.specs
            .iter()
            .filter_map(|s| Some((s.symbol, self.fair(s.symbol)?)))
            .collect()
    }

    pub fn since(&self, last_seq: u64) -> Vec<NewsItem> {
        self.news
            .iter()
            .filter(|n| n.seq > last_seq)
            .cloned()
            .collect()
    }

    pub fn oldest_retained(&self) -> u64 {
        self.news.front().map_or(self.next_seq, |n| n.seq)
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    pub fn recent(&self, limit: usize, symbol: Option<Symbol>) -> Vec<NewsItem> {
        let sector = symbol.and_then(|s| self.spec(s)).map(|s| s.sector.clone());
        let mut items: Vec<NewsItem> = self
            .news
            .iter()
            .rev()
            .filter(|n| match (symbol, &sector) {
                (Some(s), Some(sector)) => n.about(s, sector),
                (Some(_), None) => false,
                (None, _) => true,
            })
            .take(limit)
            .cloned()
            .collect();
        items.reverse();
        items
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

    pub fn status(&self, subscribers: u32, writer: String) -> NewsroomStatus {
        NewsroomStatus {
            day: self.day,
            quarter: self.day / self.quarter_days + 1,
            published: self.next_seq - 1,
            subscribers,
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
            writer,
        }
    }

    pub fn decide_register(&self, name: &str, secret: SecretHash) -> Result<Fact, String> {
        if self.subscribers.contains_key(name) {
            return Err(format!("{name} already subscribes"));
        }
        Ok(Fact::SubscriberRegistered {
            name: name.to_string(),
            secret,
        })
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
        let mut economy = self.economy;
        for change in changes {
            match change {
                Change::Company(symbol, f) => {
                    fundamentals.insert(*symbol, *f);
                }
                Change::Economy(e) => economy = *e,
                Change::Reading(..) => {}
            }
        }
        let moves: Vec<f64> = before
            .iter()
            .filter_map(|(symbol, was)| {
                let now = self.fair_with(*symbol, &fundamentals, &economy)?;
                Some((now as f64 / (*was).max(1) as f64 - 1.0) * 10_000.0)
            })
            .collect();
        if moves.is_empty() {
            return 0;
        }
        (moves.iter().sum::<f64>() / moves.len() as f64).round() as i64
    }

    #[allow(clippy::too_many_arguments)]
    fn publish(
        &self,
        story: Story,
        affected: &[Symbol],
        changes: Vec<Change>,
        now: Timestamp,
        rng: &mut StdRng,
        writer: &mut dyn Writer,
    ) -> Fact {
        let before: Vec<(Symbol, Price)> = affected
            .iter()
            .filter_map(|s| Some((*s, self.fair(*s)?)))
            .collect();
        let impact_bps = self.impact(&before, &changes);
        let copy = writer.write(&story, rng);
        Fact::Published {
            item: NewsItem {
                seq: self.next_seq,
                time: now,
                day: self.day,
                kind: story.kind,
                symbol: story.symbol,
                sentiment: story.sentiment.clamp(-100, 100),
                impact_bps,
                sector: story.sector.clone(),
                headline: copy.headline,
                body: copy.body,
                data: story.data(),
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

    pub fn decide_prospectus(
        &self,
        symbol: Symbol,
        now: Timestamp,
        rng: &mut StdRng,
        writer: &mut dyn Writer,
    ) -> Option<Fact> {
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
            ("price", format_price(spec.price)),
        ]);
        Some(self.publish(story, &[], vec![], now, rng, writer))
    }

    pub fn decide_earnings(
        &self,
        symbol: Symbol,
        now: Timestamp,
        rng: &mut StdRng,
        writer: &mut dyn Writer,
    ) -> Option<Fact> {
        let spec = self.spec(symbol)?;
        let f = self.fundamentals(symbol)?;
        let cycle = spec.beta_pct * (self.economy.growth_bps - 200) / 400;
        let growth_q = f.growth_bps / 4 + cycle + Economy::noisy(rng, 150.0);
        let revenue = ((f.revenue as i128 * (10_000 + growth_q) as i128 / 10_000).max(1)) as u64;
        let margin_bps = (f.margin_bps + Economy::noisy(rng, 40.0)).clamp(100, 4_000);
        let eps_e4 = quarterly_eps_e4(revenue, margin_bps, spec.shares);
        let surprise_bps = (eps_e4 - f.consensus_e4) * 10_000 / f.consensus_e4.abs().max(1);
        let growth_bps = f.growth_bps + (growth_q * 4 - f.growth_bps) / 4;
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
        Some(self.publish(
            story,
            &[symbol],
            vec![Change::Company(symbol, next)],
            now,
            rng,
            writer,
        ))
    }

    pub fn decide_release(
        &self,
        release: Release,
        now: Timestamp,
        rng: &mut StdRng,
        writer: &mut dyn Writer,
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
        self.publish(story, &all, changes, now, rng, writer)
    }

    pub fn decide_event(
        &self,
        event: &EventType,
        symbol: Symbol,
        now: Timestamp,
        rng: &mut StdRng,
        writer: &mut dyn Writer,
    ) -> Option<Fact> {
        let spec = self.spec(symbol)?;
        let draw = event.draw(rng);
        let affected: Vec<Symbol> = match event.scope {
            Scope::Company => vec![symbol],
            Scope::Sector => self
                .specs
                .iter()
                .filter(|s| s.sector == spec.sector)
                .map(|s| s.symbol)
                .collect(),
        };
        let changes: Vec<Change> = affected
            .iter()
            .filter(|_| draw.growth_bps != 0 || draw.margin_rel_bps != 0)
            .filter_map(|s| {
                let f = self.fundamentals(*s)?;
                let margin = f.margin_bps + f.margin_bps * draw.margin_rel_bps / 10_000;
                let margin_bps = margin.clamp(100, 4_000);
                let growth_bps = (f.growth_bps + draw.growth_bps).clamp(-3_000, 5_000);
                let shares = self.spec(*s)?.shares;
                Some(Change::Company(
                    *s,
                    Fundamentals {
                        margin_bps,
                        growth_bps,
                        eps_e4: quarterly_eps_e4(f.revenue, margin_bps, shares),
                        ..f
                    },
                ))
            })
            .collect();
        let (kind, story_symbol) = match event.scope {
            Scope::Company => (NewsKind::Company, Some(symbol)),
            Scope::Sector => (NewsKind::Sector, None),
        };
        let mut story = self.story(kind, event.name, story_symbol, draw.sentiment);
        story.sector = spec.sector.clone();
        story.facts = facts(&[
            ("event", event.name.to_string()),
            ("growth_change", signed_pct(draw.growth_bps)),
            ("margin_change", signed_pct(draw.margin_rel_bps)),
        ]);
        Some(self.publish(story, &affected, changes, now, rng, writer))
    }

    pub fn decide_shock(
        &self,
        symbol: Symbol,
        move_bps: i64,
        now: Timestamp,
        rng: &mut StdRng,
        writer: &mut dyn Writer,
    ) -> Option<Fact> {
        let spec = self.spec(symbol)?;
        let f = self.fundamentals(symbol)?;
        let margin_bps = (f.margin_bps * (10_000 + move_bps) / 10_000).max(1);
        let next = Fundamentals {
            margin_bps,
            eps_e4: quarterly_eps_e4(f.revenue, margin_bps, spec.shares),
            ..f
        };
        let mut story = self.story(
            NewsKind::Company,
            "shock",
            Some(symbol),
            (move_bps / 10).clamp(-90, 90),
        );
        story.facts = facts(&[("event", "shock".into()), ("move", signed_pct(move_bps))]);
        Some(self.publish(
            story,
            &[symbol],
            vec![Change::Company(symbol, next)],
            now,
            rng,
            writer,
        ))
    }

    pub fn decide_random(
        &self,
        now: Timestamp,
        rng: &mut StdRng,
        writer: &mut dyn Writer,
    ) -> Option<Fact> {
        if self.specs.is_empty() {
            return None;
        }
        let event = events::pick(rng);
        let symbol = self.specs[rng.random_range(0..self.specs.len())].symbol;
        self.decide_event(event, symbol, now, rng, writer)
    }

    pub fn inject(
        &self,
        symbol: Option<Symbol>,
        name: &str,
        now: Timestamp,
        rng: &mut StdRng,
        writer: &mut dyn Writer,
    ) -> Result<Fact, String> {
        if let Some(release) = Release::parse(name) {
            return Ok(self.decide_release(release, now, rng, writer));
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
                .decide_shock(symbol, move_bps, now, rng, writer)
                .ok_or(format!("nothing to publish for {symbol}"));
        }
        let fact = match name {
            "earnings" => self.decide_earnings(symbol, now, rng, writer),
            "prospectus" => self.decide_prospectus(symbol, now, rng, writer),
            _ => {
                let event = events::find(name).ok_or_else(|| {
                    let names: Vec<&str> = events::CATALOGUE.iter().map(|e| e.name).collect();
                    format!(
                        "unknown event '{name}'; try earnings, cpi, rates, gdp or {}",
                        names.join(", ")
                    )
                })?;
                self.decide_event(event, symbol, now, rng, writer)
            }
        };
        fact.ok_or(format!("nothing to publish for {symbol}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::company::parse_companies;
    use crate::writer::TemplateWriter;
    use rand::SeedableRng;

    pub fn newsroom() -> Newsroom {
        let specs = parse_companies(
            "company ACME sector=robotics price=25.00 shares=52000 eps=1.25 margin=12 growth=9 beta=1.2 name=Acme Robotics
company NOVA sector=energy price=12.50 shares=104000 eps=0.80 margin=8 growth=4 beta=0.9 name=Nova Energy
company BOLT sector=robotics price=40.00 shares=20000 eps=2.00 margin=15 growth=12 beta=1.4 name=Bolt Motion",
        )
        .unwrap();
        Newsroom::new(specs, 5)
    }

    fn acme() -> Symbol {
        Symbol::parse("ACME").unwrap()
    }

    fn run(room: &mut Newsroom, decide: impl FnOnce(&Newsroom) -> Fact) -> NewsItem {
        let fact = decide(room);
        room.apply(&fact);
        match fact {
            Fact::Published { item, .. } => item,
            other => panic!("expected news, got {other:?}"),
        }
    }

    #[test]
    fn prospectuses_are_filed_once_with_the_financial_baseline() {
        let mut room = newsroom();
        let mut rng = StdRng::seed_from_u64(1);
        let mut w = TemplateWriter;
        assert_eq!(room.unfiled().len(), 3);
        let item = run(&mut room, |r| {
            r.decide_prospectus(acme(), 5, &mut rng, &mut w).unwrap()
        });
        assert_eq!(
            (item.seq, item.kind, item.symbol),
            (1, NewsKind::Prospectus, Some(acme()))
        );
        assert!(
            item.body.contains("1.24") || item.body.contains("1.25"),
            "{}",
            item.body
        );
        assert_eq!(item.value("price"), Some("25.00"));
        assert!(
            room.decide_prospectus(acme(), 6, &mut rng, &mut w)
                .is_none()
        );
        assert_eq!(room.unfiled().len(), 2);
        assert_eq!(room.prospectuses()[0].reference, 1);
    }

    #[test]
    fn earnings_follow_a_staggered_quarterly_calendar_and_move_fair_value_with_the_surprise() {
        let mut room = newsroom();
        let mut rng = StdRng::seed_from_u64(2);
        let mut w = TemplateWriter;
        let mut reported = BTreeMap::new();
        let mut beats = 0;
        for day in 1..=40 {
            let started = room.decide_day(day, &mut rng).unwrap();
            room.apply(&started);
            assert!(room.decide_day(day, &mut rng).is_none());
            for symbol in room.earnings_due() {
                let before = room.fair(symbol).unwrap();
                let item = run(&mut room, |r| {
                    r.decide_earnings(symbol, 0, &mut rng, &mut w).unwrap()
                });
                *reported.entry(symbol).or_insert(0) += 1;
                assert_eq!(item.day, day);
                let after = room.fair(symbol).unwrap();
                let moved = (after as f64 / before as f64 - 1.0) * 10_000.0;
                assert!((moved - item.impact_bps as f64).abs() <= 1.0);
                if item.sentiment > 0 {
                    beats += 1;
                }
                assert!(item.value("eps").is_some() && item.value("consensus").is_some());
            }
        }
        assert_eq!(reported.len(), 3, "every company reports");
        assert!(reported.values().all(|n| *n == 8), "{reported:?}");
        assert!((5..20).contains(&beats), "beats {beats}/24");
        assert!(room.next_report_day(acme()) > 40);
    }

    #[test]
    fn macro_releases_move_rates_risk_and_every_fair_value() {
        let mut room = newsroom();
        let mut rng = StdRng::seed_from_u64(3);
        let mut w = TemplateWriter;
        room.apply(&Fact::DayStarted {
            day: 2,
            economy: Economy {
                inflation_bps: 420,
                ..Economy::default()
            },
        });
        let cpi = run(&mut room, |r| {
            r.decide_release(Release::Inflation, 0, &mut rng, &mut w)
        });
        assert!(
            cpi.sentiment < 0 && cpi.headline.contains("hot") || cpi.headline.contains("faster")
        );
        assert!(room.economy().risk_appetite < 0);
        let before = room.fair(acme()).unwrap();
        let mut hikes = 0;
        for _ in 0..20 {
            let rates = run(&mut room, |r| {
                r.decide_release(Release::RateDecision, 0, &mut rng, &mut w)
            });
            if rates.value("move") == Some("25") {
                hikes += 1;
                assert!(rates.sentiment < 0);
            }
        }
        assert!(hikes >= 14, "hot inflation brings hikes: {hikes}");
        assert!(room.economy().rate_bps > 450 + 14 * 25 - 6 * 25);
        assert!(room.fair(acme()).unwrap() < before);
        assert_eq!(room.status(0, "t".into()).economy.inflation_bps, 420);
    }

    #[test]
    fn company_and_sector_events_change_fundamentals_and_injection_validates() {
        let mut room = newsroom();
        let mut rng = StdRng::seed_from_u64(4);
        let mut w = TemplateWriter;
        let before = room.fundamentals(acme()).unwrap();
        let lawsuit = run(&mut room, |r| {
            r.inject(Some(acme()), "lawsuit", 0, &mut rng, &mut w)
                .unwrap()
        });
        assert!(
            lawsuit.sentiment < 0 && lawsuit.impact_bps < 0,
            "{lawsuit:?}"
        );
        assert!(room.fundamentals(acme()).unwrap().margin_bps < before.margin_bps);
        let bolt = Symbol::parse("BOLT").unwrap();
        let bolt_growth = room.fundamentals(bolt).unwrap().growth_bps;
        let boom = run(&mut room, |r| {
            r.inject(Some(acme()), "demand-boom", 0, &mut rng, &mut w)
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
            r.inject(Some(acme()), "upgrade", 0, &mut rng, &mut w)
                .unwrap()
        });
        assert!(upgrade.sentiment > 0);
        assert_eq!(upgrade.impact_bps, 0, "opinions do not change fundamentals");
        assert!(room.inject(None, "lawsuit", 0, &mut rng, &mut w).is_err());
        assert!(
            room.inject(Some(acme()), "nope", 0, &mut rng, &mut w)
                .is_err()
        );
        assert!(room.inject(None, "gdp", 0, &mut rng, &mut w).is_ok());
        let fair = room.fair(acme()).unwrap();
        let shock = run(&mut room, |r| {
            r.inject(Some(acme()), "shock:-800", 0, &mut rng, &mut w)
                .unwrap()
        });
        assert!((shock.impact_bps + 800).abs() <= 5, "{shock:?}");
        assert!(room.fair(acme()).unwrap() < fair && shock.sentiment == -80);
        assert!(
            room.inject(Some(acme()), "shock:x", 0, &mut rng, &mut w)
                .is_err()
        );
        assert!(
            room.inject(Some(acme()), "shock:9000", 0, &mut rng, &mut w)
                .is_err()
        );
        assert_eq!(
            room.recent(10, Some(Symbol::parse("NOVA").unwrap())).len(),
            0
        );
        assert_eq!(room.recent(2, None).len(), 2);
        assert_eq!(
            room.recent(10, Some(bolt)).len(),
            1,
            "sector news is about BOLT too"
        );
        assert_eq!(room.since(1).len(), 3);
    }
}
