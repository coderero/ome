use ome_core::types::Timestamp;
use ome_protocol::sim::{get_command, put_command};
use ome_protocol::{DecodeError, Decoder, Encoder, SimCommand};
use ome_store::Entry;

const COMMAND: u16 = 700;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub timestamp: Timestamp,
    pub command: SimCommand,
}

impl Entry for Record {
    fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new(COMMAND);
        e.u64(self.timestamp);
        put_command(&mut e, &self.command);
        e.finish()
    }

    fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(bytes)?;
        if d.template_id != COMMAND {
            return Err(DecodeError::UnknownTemplate(d.template_id));
        }
        let timestamp = d.u64()?;
        Ok(Record {
            timestamp,
            command: get_command(&mut d)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip() {
        let record = Record {
            timestamp: 42,
            command: SimCommand::SetParam {
                group: "noise".into(),
                key: "activity".into(),
                value: "0.9".into(),
            },
        };
        assert_eq!(Record::decode(&record.encode()), Ok(record));
    }
}
