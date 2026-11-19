use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::Timestamp;
use crate::world::company::Fundamentals;
use crate::world::economy::{Economy, Release};
use crate::world::{Change, WorldFact};
use ome_protocol::news::{get_wire, put_wire};
use ome_protocol::sim::{get_command, put_command};
use ome_protocol::{DecodeError, Decoder, Encoder, SimCommand};
use ome_store::Entry;

const COMMAND: u16 = 700;
const ACCELERATED: u16 = 701;
const RECORDING_STARTED: u16 = 702;
const RECORDING_STOPPED: u16 = 703;
const FORWARDED: u16 = 704;
const WORLD_DAY: u16 = 706;
const HAPPENED: u16 = 707;
const PROVISIONED: u16 = 708;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub symbol: Symbol,
    pub last: Price,
    pub volume: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact {
    Command(SimCommand),
    Accelerated([u64; 4]),
    RecordingStarted { name: String, marks: Vec<Mark> },
    RecordingStopped { marks: Vec<Mark> },
    Forwarded(SimCommand),
    World(WorldFact),
    Provisioned { key: String },
}

fn release_code(release: Release) -> u64 {
    match release {
        Release::Inflation => 1,
        Release::RateDecision => 2,
        Release::Growth => 3,
    }
}

fn release_of(code: u64) -> Result<Release, DecodeError> {
    Ok(match code {
        1 => Release::Inflation,
        2 => Release::RateDecision,
        3 => Release::Growth,
        _ => return Err(DecodeError::Invalid("release")),
    })
}

fn put_changes(e: &mut Encoder, changes: &[Change]) {
    let mut g = e.begin_group(changes.len());
    for change in changes {
        e.begin_entry(&mut g);
        let (code, symbol, x, a, b, c, d, y, z) = match change {
            Change::Company(symbol, f) => (1, Some(*symbol), f.revenue, f.margin_bps, f.growth_bps, f.eps_e4, f.consensus_e4, f.quarters, f.momentum_bps),
            Change::Economy(m) => (2, None, 0, m.rate_bps, m.inflation_bps, m.growth_bps, m.risk_appetite, 0, m.cycle),
            Change::Reading(release, value) => (3, None, release_code(*release), *value, 0, 0, 0, 0, 0),
            Change::Cash(symbol, cash) => (4, Some(*symbol), 0, *cash, 0, 0, 0, 0, 0),
        };
        e.u8(code).symbol(symbol).u64(x).i64(a).i64(b).i64(c).i64(d).u64(y).i64(z);
        e.end_entry_block(&mut g);
    }
}

fn get_changes(d: &mut Decoder) -> Result<Vec<Change>, DecodeError> {
    let g = d.group()?;
    let mut changes = Vec::with_capacity(g.count);
    for _ in 0..g.count {
        let start = d.entry_start();
        let code = d.u8()?;
        let symbol = d.optional_symbol()?;
        let x = d.u64()?;
        let (a, b, c, e4) = (d.i64()?, d.i64()?, d.i64()?, d.i64()?);
        let y = d.u64()?;
        let z = d.i64()?;
        let change = match (code, symbol) {
            (1, Some(symbol)) => Change::Company(
                symbol,
                Fundamentals { revenue: x, margin_bps: a, growth_bps: b, eps_e4: c, consensus_e4: e4, quarters: y, momentum_bps: z },
            ),
            (2, _) => Change::Economy(Economy { rate_bps: a, inflation_bps: b, growth_bps: c, risk_appetite: e4, cycle: z }),
            (3, _) => Change::Reading(release_of(x)?, a),
            (4, Some(symbol)) => Change::Cash(symbol, a),
            _ => return Err(DecodeError::Invalid("world change")),
        };
        d.end_entry_block(start, &g)?;
        changes.push(change);
    }
    Ok(changes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub timestamp: Timestamp,
    pub fact: Fact,
}

fn put_marks(e: &mut Encoder, marks: &[Mark]) {
    let mut g = e.begin_group(marks.len());
    for m in marks {
        e.begin_entry(&mut g);
        e.symbol(Some(m.symbol)).u64(m.last).u64(m.volume);
        e.end_entry_block(&mut g);
    }
}

fn get_marks(d: &mut Decoder) -> Result<Vec<Mark>, DecodeError> {
    let g = d.group()?;
    let mut marks = Vec::with_capacity(g.count);
    for _ in 0..g.count {
        let start = d.entry_start();
        let symbol = d.symbol()?;
        let last = d.u64()?;
        let volume = d.u64()?;
        d.end_entry_block(start, &g)?;
        marks.push(Mark {
            symbol,
            last,
            volume,
        });
    }
    Ok(marks)
}

impl Entry for Record {
    fn encode(&self) -> Vec<u8> {
        let template = match self.fact {
            Fact::Command(_) => COMMAND,
            Fact::Accelerated(_) => ACCELERATED,
            Fact::RecordingStarted { .. } => RECORDING_STARTED,
            Fact::RecordingStopped { .. } => RECORDING_STOPPED,
            Fact::Forwarded(_) => FORWARDED,
            Fact::World(WorldFact::DayStarted { .. }) => WORLD_DAY,
            Fact::World(WorldFact::Happened { .. }) => HAPPENED,
            Fact::Provisioned { .. } => PROVISIONED,
        };
        let mut e = Encoder::new(template);
        e.u64(self.timestamp);
        match &self.fact {
            Fact::Command(command) | Fact::Forwarded(command) => put_command(&mut e, command),
            Fact::Accelerated(seconds) => {
                for s in seconds {
                    e.u64(*s);
                }
                e.end_block();
            }
            Fact::RecordingStarted { name, marks } => {
                e.end_block();
                put_marks(&mut e, marks);
                e.var(name);
            }
            Fact::RecordingStopped { marks } => {
                e.end_block();
                put_marks(&mut e, marks);
            }
            Fact::World(WorldFact::DayStarted { day, economy }) => {
                e.u64(*day)
                    .i64(economy.rate_bps)
                    .i64(economy.inflation_bps)
                    .i64(economy.growth_bps)
                    .i64(economy.risk_appetite)
                    .i64(economy.cycle)
                    .end_block();
            }
            Fact::World(WorldFact::Happened { wire, changes }) => {
                e.end_block();
                put_changes(&mut e, changes);
                let mut payload = Encoder::new(0);
                put_wire(&mut payload, wire);
                e.var_bytes(&payload.finish());
            }
            Fact::Provisioned { key } => {
                e.end_block().var(key);
            }
        }
        e.finish()
    }

    fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(bytes)?;
        let template = d.template_id;
        let timestamp = d.u64()?;
        let fact = match template {
            COMMAND => Fact::Command(get_command(&mut d)?),
            FORWARDED => Fact::Forwarded(get_command(&mut d)?),
            ACCELERATED => {
                let seconds = [d.u64()?, d.u64()?, d.u64()?, d.u64()?];
                d.end_block()?;
                Fact::Accelerated(seconds)
            }
            RECORDING_STARTED => {
                d.end_block()?;
                let marks = get_marks(&mut d)?;
                Fact::RecordingStarted {
                    name: d.var()?,
                    marks,
                }
            }
            RECORDING_STOPPED => {
                d.end_block()?;
                Fact::RecordingStopped {
                    marks: get_marks(&mut d)?,
                }
            }
            WORLD_DAY => {
                let day = d.u64()?;
                let economy = Economy {
                    rate_bps: d.i64()?,
                    inflation_bps: d.i64()?,
                    growth_bps: d.i64()?,
                    risk_appetite: d.i64()?,
                    cycle: d.i64()?,
                };
                d.end_block()?;
                Fact::World(WorldFact::DayStarted { day, economy })
            }
            HAPPENED => {
                d.end_block()?;
                let changes = get_changes(&mut d)?;
                let payload = d.var_bytes()?;
                let mut inner = Decoder::new(payload)?;
                Fact::World(WorldFact::Happened { wire: get_wire(&mut inner)?, changes })
            }
            PROVISIONED => {
                d.end_block()?;
                Fact::Provisioned { key: d.var()? }
            }
            other => return Err(DecodeError::UnknownTemplate(other)),
        };
        Ok(Record { timestamp, fact })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip() {
        let marks = vec![Mark {
            symbol: Symbol::parse("ACME").unwrap(),
            last: 25_10,
            volume: 1_200,
        }];
        let facts = [
            Fact::Command(SimCommand::SetParam {
                group: "noise".into(),
                key: "activity".into(),
                value: "0.9".into(),
            }),
            Fact::Accelerated([3, 30, 3, 3]),
            Fact::RecordingStarted {
                name: "drill".into(),
                marks: marks.clone(),
            },
            Fact::RecordingStopped { marks },
            Fact::Forwarded(SimCommand::Pause { group: "x".into() }),
            Fact::World(WorldFact::DayStarted { day: 4, economy: Economy::default() }),
            Fact::World(WorldFact::Happened {
                wire: ome_protocol::WireItem {
                    seq: 9,
                    day: 4,
                    kind: ome_protocol::NewsKind::Earnings,
                    symbol: Symbol::parse("ACME"),
                    sentiment: 30,
                    impact_bps: 120,
                    event: "earnings".into(),
                    sector: "robotics".into(),
                    company: "Acme Robotics".into(),
                    facts: vec![("eps".into(), "0.32".into())],
                    data: "fund=ACME:3115:900:0".into(),
                    headline: String::new(),
                    body: String::new(),
                },
                changes: vec![
                    Change::Company(
                        Symbol::parse("ACME").unwrap(),
                        Fundamentals { revenue: 135_000, margin_bps: 1_200, growth_bps: 900, eps_e4: 3_115, consensus_e4: -20, quarters: 3, momentum_bps: -45 },
                    ),
                    Change::Economy(Economy { risk_appetite: -12, cycle: -30, ..Economy::default() }),
                    Change::Reading(Release::Growth, -40),
                    Change::Cash(Symbol::parse("ACME").unwrap(), -35),
                ],
            }),
            Fact::Provisioned { key: "ipo:ACME".into() },
        ];
        for fact in facts {
            let record = Record {
                timestamp: 42,
                fact,
            };
            assert_eq!(Record::decode(&record.encode()), Ok(record));
        }
    }
}
