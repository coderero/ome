use crate::codec::{DecodeError, Decoder, Encoder};
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NewsKind {
    Company = 1,
    Sector = 2,
    Macro = 3,
    Earnings = 4,
    Prospectus = 5,
}

impl NewsKind {
    pub fn from_u8(code: u8) -> Option<Self> {
        Some(match code {
            1 => NewsKind::Company,
            2 => NewsKind::Sector,
            3 => NewsKind::Macro,
            4 => NewsKind::Earnings,
            5 => NewsKind::Prospectus,
            _ => return None,
        })
    }

    pub fn label(&self) -> &'static str {
        match self {
            NewsKind::Company => "company",
            NewsKind::Sector => "sector",
            NewsKind::Macro => "macro",
            NewsKind::Earnings => "earnings",
            NewsKind::Prospectus => "prospectus",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsItem {
    pub seq: u64,
    pub time: Timestamp,
    pub day: u64,
    pub kind: NewsKind,
    pub symbol: Option<Symbol>,
    pub sentiment: i64,
    pub impact_bps: i64,
    pub sector: String,
    pub headline: String,
    pub body: String,
    pub data: String,
}

impl NewsItem {
    pub fn about(&self, symbol: Symbol, sector: &str) -> bool {
        match self.kind {
            NewsKind::Macro => true,
            NewsKind::Sector => self.sector == sector,
            _ => self.symbol == Some(symbol),
        }
    }

    pub fn value(&self, key: &str) -> Option<&str> {
        self.data
            .split_whitespace()
            .filter_map(|pair| pair.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }
}

pub fn put_news(e: &mut Encoder, item: &NewsItem) {
    e.u64(item.seq)
        .u64(item.time)
        .u64(item.day)
        .u8(item.kind as u8)
        .symbol(item.symbol)
        .i64(item.sentiment)
        .i64(item.impact_bps)
        .end_block()
        .var(&item.sector)
        .var(&item.headline)
        .var(&item.body)
        .var(&item.data);
}

pub fn get_news(d: &mut Decoder) -> Result<NewsItem, DecodeError> {
    let seq = d.u64()?;
    let time = d.u64()?;
    let day = d.u64()?;
    let kind = NewsKind::from_u8(d.u8()?).ok_or(DecodeError::Invalid("news kind"))?;
    let symbol = d.optional_symbol()?;
    let sentiment = d.i64()?;
    let impact_bps = d.i64()?;
    d.end_block()?;
    Ok(NewsItem {
        seq,
        time,
        day,
        kind,
        symbol,
        sentiment,
        impact_bps,
        sector: d.var()?,
        headline: d.var()?,
        body: d.var()?,
        data: d.var()?,
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MacroInfo {
    pub rate_bps: i64,
    pub inflation_bps: i64,
    pub growth_bps: i64,
    pub risk_appetite: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompanyInfo {
    pub symbol: Option<Symbol>,
    pub eps_cents: i64,
    pub consensus_cents: i64,
    pub revenue: u64,
    pub margin_bps: i64,
    pub growth_bps: i64,
    pub fair: Price,
    pub next_report_day: u64,
    pub name: String,
    pub sector: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewsroomStatus {
    pub day: u64,
    pub published: u64,
    pub subscribers: u32,
    pub sources: u32,
    pub received: u64,
    pub writer: String,
}

pub fn put_status(e: &mut Encoder, s: &NewsroomStatus) {
    e.u64(s.day)
        .u64(s.published)
        .u32(s.subscribers)
        .u32(s.sources)
        .u64(s.received)
        .end_block()
        .var(&s.writer);
}

pub fn get_status(d: &mut Decoder) -> Result<NewsroomStatus, DecodeError> {
    let day = d.u64()?;
    let published = d.u64()?;
    let subscribers = d.u32()?;
    let sources = d.u32()?;
    let received = d.u64()?;
    d.end_block()?;
    Ok(NewsroomStatus {
        day,
        published,
        subscribers,
        sources,
        received,
        writer: d.var()?,
    })
}

pub fn put_companies(e: &mut Encoder, companies: &[CompanyInfo]) {
    let mut g = e.begin_group(companies.len());
    for c in companies {
        e.begin_entry(&mut g);
        e.symbol(c.symbol)
            .i64(c.eps_cents)
            .i64(c.consensus_cents)
            .u64(c.revenue)
            .i64(c.margin_bps)
            .i64(c.growth_bps)
            .u64(c.fair)
            .u64(c.next_report_day);
        e.end_entry_block(&mut g);
        e.var(&c.name).var(&c.sector);
    }
}

pub fn get_companies(d: &mut Decoder) -> Result<Vec<CompanyInfo>, DecodeError> {
    let g = d.group()?;
    let mut companies = Vec::with_capacity(g.count);
    for _ in 0..g.count {
        let start = d.entry_start();
        let symbol = d.optional_symbol()?;
        let eps_cents = d.i64()?;
        let consensus_cents = d.i64()?;
        let revenue = d.u64()?;
        let margin_bps = d.i64()?;
        let growth_bps = d.i64()?;
        let fair = d.u64()?;
        let next_report_day = d.u64()?;
        d.end_entry_block(start, &g)?;
        companies.push(CompanyInfo {
            symbol,
            eps_cents,
            consensus_cents,
            revenue,
            margin_bps,
            growth_bps,
            fair,
            next_report_day,
            name: d.var()?,
            sector: d.var()?,
        });
    }
    Ok(companies)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireItem {
    pub seq: u64,
    pub day: u64,
    pub kind: NewsKind,
    pub symbol: Option<Symbol>,
    pub sentiment: i64,
    pub impact_bps: i64,
    pub event: String,
    pub sector: String,
    pub company: String,
    pub facts: Vec<(String, String)>,
    pub data: String,
    pub headline: String,
    pub body: String,
}

pub fn put_wire(e: &mut Encoder, w: &WireItem) {
    let facts: Vec<String> = w.facts.iter().map(|(k, v)| format!("{k}={v}")).collect();
    e.u64(w.seq)
        .u64(w.day)
        .u8(w.kind as u8)
        .symbol(w.symbol)
        .i64(w.sentiment)
        .i64(w.impact_bps)
        .end_block()
        .var(&w.event)
        .var(&w.sector)
        .var(&w.company)
        .var(&facts.join("\n"))
        .var(&w.data)
        .var(&w.headline)
        .var(&w.body);
}

pub fn get_wire(d: &mut Decoder) -> Result<WireItem, DecodeError> {
    let seq = d.u64()?;
    let day = d.u64()?;
    let kind = NewsKind::from_u8(d.u8()?).ok_or(DecodeError::Invalid("news kind"))?;
    let symbol = d.optional_symbol()?;
    let sentiment = d.i64()?;
    let impact_bps = d.i64()?;
    d.end_block()?;
    let event = d.var()?;
    let sector = d.var()?;
    let company = d.var()?;
    let facts = d
        .var()?
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Ok(WireItem {
        seq,
        day,
        kind,
        symbol,
        sentiment,
        impact_bps,
        event,
        sector,
        company,
        facts,
        data: d.var()?,
        headline: d.var()?,
        body: d.var()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn news_items_answer_who_they_are_about_and_expose_their_numbers() {
        let acme = Symbol::parse("ACME").unwrap();
        let mut item = NewsItem {
            seq: 1,
            time: 0,
            day: 3,
            kind: NewsKind::Earnings,
            symbol: Some(acme),
            sentiment: 40,
            impact_bps: 210,
            sector: "robotics".into(),
            headline: String::new(),
            body: String::new(),
            data: "eps=1.32 consensus=1.20".into(),
        };
        assert_eq!(item.value("consensus"), Some("1.20"));
        assert_eq!(item.value("nope"), None);
        assert!(item.about(acme, "robotics"));
        assert!(!item.about(Symbol::parse("NOVA").unwrap(), "robotics"));
        item.kind = NewsKind::Sector;
        assert!(item.about(Symbol::parse("NOVA").unwrap(), "robotics"));
        item.kind = NewsKind::Macro;
        assert!(item.about(Symbol::parse("ZEN").unwrap(), "biotech"));
        assert_eq!(
            NewsKind::from_u8(NewsKind::Prospectus as u8),
            Some(NewsKind::Prospectus)
        );
        assert_eq!(NewsKind::from_u8(9), None);
    }
}
