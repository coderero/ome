use crate::company::Fundamentals;
use crate::economy::Economy;
use crate::newsroom::{Change, Fact, release_from, release_to};
use ome_core::types::Timestamp;
use ome_protocol::news::{get_news, put_news};
use ome_protocol::{DecodeError, Decoder, Encoder};
use ome_store::Entry;

const SUBSCRIBER_REGISTERED: u16 = 800;
const DAY_STARTED: u16 = 801;
const PUBLISHED: u16 = 802;
const REWRITTEN: u16 = 803;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub timestamp: Timestamp,
    pub fact: Fact,
}

fn put_economy(e: &mut Encoder, economy: &Economy) {
    e.i64(economy.rate_bps)
        .i64(economy.inflation_bps)
        .i64(economy.growth_bps)
        .i64(economy.risk_appetite);
}

fn get_economy(d: &mut Decoder) -> Result<Economy, DecodeError> {
    Ok(Economy {
        rate_bps: d.i64()?,
        inflation_bps: d.i64()?,
        growth_bps: d.i64()?,
        risk_appetite: d.i64()?,
    })
}

impl Entry for Record {
    fn encode(&self) -> Vec<u8> {
        let template = match self.fact {
            Fact::SubscriberRegistered { .. } => SUBSCRIBER_REGISTERED,
            Fact::DayStarted { .. } => DAY_STARTED,
            Fact::Published { .. } => PUBLISHED,
            Fact::Rewritten { .. } => REWRITTEN,
        };
        let mut e = Encoder::new(template);
        e.u64(self.timestamp);
        match &self.fact {
            Fact::SubscriberRegistered { name, secret } => {
                e.end_block().var(name).var_bytes(secret);
            }
            Fact::Rewritten {
                seq,
                headline,
                body,
            } => {
                e.u64(*seq).end_block().var(headline).var(body);
            }
            Fact::DayStarted { day, economy } => {
                e.u64(*day);
                put_economy(&mut e, economy);
                e.end_block();
            }
            Fact::Published { item, changes } => {
                e.end_block();
                let mut g = e.begin_group(changes.len());
                for change in changes {
                    e.begin_entry(&mut g);
                    let (code, symbol, x, a, b, c, d, y) = match change {
                        Change::Company(symbol, f) => (
                            1,
                            Some(*symbol),
                            f.revenue,
                            f.margin_bps,
                            f.growth_bps,
                            f.eps_e4,
                            f.consensus_e4,
                            f.quarters,
                        ),
                        Change::Economy(m) => (
                            2,
                            None,
                            0,
                            m.rate_bps,
                            m.inflation_bps,
                            m.growth_bps,
                            m.risk_appetite,
                            0,
                        ),
                        Change::Reading(release, value) => {
                            (3, None, release_to(*release) as u64, *value, 0, 0, 0, 0)
                        }
                        Change::Cash(symbol, cash) => (4, Some(*symbol), 0, *cash, 0, 0, 0, 0),
                    };
                    e.u8(code)
                        .symbol(symbol)
                        .u64(x)
                        .i64(a)
                        .i64(b)
                        .i64(c)
                        .i64(d)
                        .u64(y);
                    e.end_entry_block(&mut g);
                }
                let mut payload = Encoder::new(0);
                put_news(&mut payload, item);
                e.var_bytes(&payload.finish());
            }
        }
        e.finish()
    }

    fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(bytes)?;
        let template = d.template_id;
        let timestamp = d.u64()?;
        let fact = match template {
            SUBSCRIBER_REGISTERED => {
                d.end_block()?;
                let name = d.var()?;
                let secret = d
                    .var_bytes()?
                    .try_into()
                    .map_err(|_| DecodeError::Invalid("secret hash"))?;
                Fact::SubscriberRegistered { name, secret }
            }
            REWRITTEN => {
                let seq = d.u64()?;
                d.end_block()?;
                Fact::Rewritten {
                    seq,
                    headline: d.var()?,
                    body: d.var()?,
                }
            }
            DAY_STARTED => {
                let day = d.u64()?;
                let economy = get_economy(&mut d)?;
                d.end_block()?;
                Fact::DayStarted { day, economy }
            }
            PUBLISHED => {
                d.end_block()?;
                let g = d.group()?;
                let mut changes = Vec::with_capacity(g.count);
                for _ in 0..g.count {
                    let start = d.entry_start();
                    let code = d.u8()?;
                    let symbol = d.optional_symbol()?;
                    let x = d.u64()?;
                    let (a, b, c, e4) = (d.i64()?, d.i64()?, d.i64()?, d.i64()?);
                    let y = d.u64()?;
                    let change = match (code, symbol) {
                        (1, Some(symbol)) => Change::Company(
                            symbol,
                            Fundamentals {
                                revenue: x,
                                margin_bps: a,
                                growth_bps: b,
                                eps_e4: c,
                                consensus_e4: e4,
                                quarters: y,
                            },
                        ),
                        (2, _) => Change::Economy(Economy {
                            rate_bps: a,
                            inflation_bps: b,
                            growth_bps: c,
                            risk_appetite: e4,
                        }),
                        (4, Some(symbol)) => Change::Cash(symbol, a),
                        (3, _) => Change::Reading(
                            release_from(x as u8).ok_or(DecodeError::Invalid("release"))?,
                            a,
                        ),
                        _ => return Err(DecodeError::Invalid("newsroom change")),
                    };
                    d.end_entry_block(start, &g)?;
                    changes.push(change);
                }
                let payload = d.var_bytes()?;
                let mut inner = Decoder::new(payload)?;
                Fact::Published {
                    item: get_news(&mut inner)?,
                    changes,
                }
            }
            other => return Err(DecodeError::UnknownTemplate(other)),
        };
        Ok(Record { timestamp, fact })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::economy::Release;
    use ome_core::symbol::Symbol;
    use ome_protocol::{NewsItem, NewsKind};

    #[test]
    fn facts_round_trip() {
        let acme = Symbol::parse("ACME").unwrap();
        let facts = [
            Fact::SubscriberRegistered {
                name: "retail-co".into(),
                secret: [7; 32],
            },
            Fact::DayStarted {
                day: 4,
                economy: Economy::default(),
            },
            Fact::Rewritten {
                seq: 9,
                headline: "Acme tops forecasts".into(),
                body: "Acme earned 0.32 a share.".into(),
            },
            Fact::Published {
                item: NewsItem {
                    seq: 9,
                    time: 100,
                    day: 4,
                    kind: NewsKind::Earnings,
                    symbol: Some(acme),
                    sentiment: 30,
                    impact_bps: 120,
                    sector: "robotics".into(),
                    headline: "Acme beats".into(),
                    body: "EPS 0.32".into(),
                    data: "eps=0.32".into(),
                },
                changes: vec![
                    Change::Company(
                        acme,
                        Fundamentals {
                            revenue: 135_000,
                            margin_bps: 1_200,
                            growth_bps: 900,
                            eps_e4: 3_115,
                            consensus_e4: -20,
                            quarters: 3,
                        },
                    ),
                    Change::Economy(Economy {
                        risk_appetite: -12,
                        ..Economy::default()
                    }),
                    Change::Reading(Release::Growth, -40),
                ],
            },
        ];
        for fact in facts {
            let record = Record {
                timestamp: 77,
                fact,
            };
            assert_eq!(Record::decode(&record.encode()), Ok(record));
        }
    }
}
