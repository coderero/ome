use crate::newsroom::Fact;
use ome_core::types::Timestamp;
use ome_protocol::news::{get_news, put_news};
use ome_protocol::{DecodeError, Decoder, Encoder};
use ome_store::Entry;

const SUBSCRIBER_REGISTERED: u16 = 800;
const REWRITTEN: u16 = 803;
const PUBLISHED: u16 = 804;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub timestamp: Timestamp,
    pub fact: Fact,
}

impl Entry for Record {
    fn encode(&self) -> Vec<u8> {
        let template = match self.fact {
            Fact::SubscriberRegistered { .. } => SUBSCRIBER_REGISTERED,
            Fact::Published { .. } => PUBLISHED,
            Fact::Rewritten { .. } => REWRITTEN,
        };
        let mut e = Encoder::new(template);
        e.u64(self.timestamp);
        match &self.fact {
            Fact::SubscriberRegistered { name, secret } => {
                e.end_block().var(name).var_bytes(secret);
            }
            Fact::Rewritten { seq, headline, body } => {
                e.u64(*seq).end_block().var(headline).var(body);
            }
            Fact::Published { item, source, source_seq } => {
                e.u64(*source_seq).end_block().var(source);
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
                Fact::Rewritten { seq, headline: d.var()?, body: d.var()? }
            }
            PUBLISHED => {
                let source_seq = d.u64()?;
                d.end_block()?;
                let source = d.var()?;
                let payload = d.var_bytes()?;
                let mut inner = Decoder::new(payload)?;
                Fact::Published { item: get_news(&mut inner)?, source, source_seq }
            }
            other => return Err(DecodeError::UnknownTemplate(other)),
        };
        Ok(Record { timestamp, fact })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ome_core::symbol::Symbol;
    use ome_protocol::{NewsItem, NewsKind};

    #[test]
    fn facts_round_trip() {
        let facts = [
            Fact::SubscriberRegistered { name: "retail-co".into(), secret: [7; 32] },
            Fact::Rewritten { seq: 9, headline: "Acme tops forecasts".into(), body: "Acme earned 0.32 a share.".into() },
            Fact::Published {
                item: NewsItem {
                    seq: 9,
                    time: 100,
                    day: 4,
                    kind: NewsKind::Earnings,
                    symbol: Symbol::parse("ACME"),
                    sentiment: 30,
                    impact_bps: 120,
                    sector: "robotics".into(),
                    headline: "Acme beats".into(),
                    body: "EPS 0.32".into(),
                    data: "eps=0.32".into(),
                },
                source: "world".into(),
                source_seq: 12,
            },
        ];
        for fact in facts {
            let record = Record { timestamp: 77, fact };
            assert_eq!(Record::decode(&record.encode()), Ok(record));
        }
    }
}
