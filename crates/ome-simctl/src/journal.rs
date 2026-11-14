use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::Timestamp;
use ome_protocol::sim::{get_command, put_command};
use ome_protocol::{DecodeError, Decoder, Encoder, SimCommand};
use ome_store::Entry;

const COMMAND: u16 = 700;
const ACCELERATED: u16 = 701;
const RECORDING_STARTED: u16 = 702;
const RECORDING_STOPPED: u16 = 703;

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
        };
        let mut e = Encoder::new(template);
        e.u64(self.timestamp);
        match &self.fact {
            Fact::Command(command) => put_command(&mut e, command),
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
        }
        e.finish()
    }

    fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(bytes)?;
        let template = d.template_id;
        let timestamp = d.u64()?;
        let fact = match template {
            COMMAND => Fact::Command(get_command(&mut d)?),
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
