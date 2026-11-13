use crate::clearing::Fact;
use ome_core::types::Timestamp;
use ome_protocol::{DecodeError, Decoder, Encoder, ServerMessage};
use ome_store::Entry;

mod template {
    pub const MEMBER_REGISTERED: u16 = 400;
    pub const DEPOSITED: u16 = 401;
    pub const TRADE_CLEARED: u16 = 402;
    pub const ALLOCATION_CLEARED: u16 = 403;
    pub const CYCLE_OPENED: u16 = 404;
    pub const CYCLE_SETTLED: u16 = 405;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub timestamp: Timestamp,
    pub fact: Fact,
}

impl Entry for Record {
    fn encode(&self) -> Vec<u8> {
        let template_id = match &self.fact {
            Fact::MemberRegistered { .. } => template::MEMBER_REGISTERED,
            Fact::Deposited { .. } => template::DEPOSITED,
            Fact::TradeCleared(_) => template::TRADE_CLEARED,
            Fact::AllocationCleared(_) => template::ALLOCATION_CLEARED,
            Fact::CycleOpened { .. } => template::CYCLE_OPENED,
            Fact::CycleSettled { .. } => template::CYCLE_SETTLED,
        };
        let mut e = Encoder::new(template_id);
        e.u64(self.timestamp);
        match &self.fact {
            Fact::MemberRegistered {
                member,
                name,
                secret,
            } => {
                e.u64(*member).bytes32(secret).end_block().var(name);
            }
            Fact::Deposited { member, amount } => {
                e.u64(*member).i64(*amount).end_block();
            }
            Fact::TradeCleared(trade) => {
                e.end_block()
                    .var_bytes(&ServerMessage::ClearedTrade(trade.clone()).encode());
            }
            Fact::AllocationCleared(allocation) => {
                e.end_block()
                    .var_bytes(&ServerMessage::Allocation(allocation.clone()).encode());
            }
            Fact::CycleOpened { cycle } | Fact::CycleSettled { cycle } => {
                e.u64(*cycle).end_block();
            }
        }
        e.finish()
    }

    fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(bytes)?;
        let timestamp = d.u64()?;
        let fact = match d.template_id {
            template::MEMBER_REGISTERED => {
                let member = d.u64()?;
                let secret = d.bytes32()?;
                d.end_block()?;
                Fact::MemberRegistered {
                    member,
                    name: d.var()?,
                    secret,
                }
            }
            template::DEPOSITED => {
                let member = d.u64()?;
                let amount = d.i64()?;
                d.end_block()?;
                Fact::Deposited { member, amount }
            }
            template::TRADE_CLEARED => {
                d.end_block()?;
                match ServerMessage::decode(d.var_bytes()?)? {
                    ServerMessage::ClearedTrade(trade) => Fact::TradeCleared(trade),
                    _ => return Err(DecodeError::Invalid("embedded cleared trade")),
                }
            }
            template::ALLOCATION_CLEARED => {
                d.end_block()?;
                match ServerMessage::decode(d.var_bytes()?)? {
                    ServerMessage::Allocation(allocation) => Fact::AllocationCleared(allocation),
                    _ => return Err(DecodeError::Invalid("embedded allocation")),
                }
            }
            template::CYCLE_OPENED => {
                let cycle = d.u64()?;
                d.end_block()?;
                Fact::CycleOpened { cycle }
            }
            template::CYCLE_SETTLED => {
                let cycle = d.u64()?;
                d.end_block()?;
                Fact::CycleSettled { cycle }
            }
            other => return Err(DecodeError::UnknownTemplate(other)),
        };
        Ok(Record { timestamp, fact })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clearing::tests::{sym, trade};
    use crate::clearing::{Clearing, ClearingConfig};
    use ome_protocol::Allocation;

    fn facts() -> Vec<Fact> {
        vec![
            Fact::MemberRegistered {
                member: 2,
                name: "retail".into(),
                secret: [7; 32],
            },
            Fact::MemberRegistered {
                member: 3,
                name: "prime".into(),
                secret: [8; 32],
            },
            Fact::Deposited {
                member: 2,
                amount: 500_000_00,
            },
            Fact::Deposited {
                member: 3,
                amount: 500_000_00,
            },
            Fact::AllocationCleared(Allocation {
                seq: 1,
                allocation_id: 1,
                member: 3,
                symbol: sym(),
                account: 9,
                shares: 500,
            }),
            Fact::CycleOpened { cycle: 1 },
            Fact::TradeCleared(trade(2, 1, 2, 3, 25_00, 30)),
            Fact::CycleOpened { cycle: 2 },
            Fact::TradeCleared(trade(3, 2, 3, 2, 25_10, 10)),
            Fact::CycleOpened { cycle: 3 },
            Fact::CycleSettled { cycle: 1 },
            Fact::Deposited {
                member: 2,
                amount: -1_000_00,
            },
        ]
    }

    #[test]
    fn every_fact_round_trips() {
        for (i, fact) in facts().into_iter().enumerate() {
            let record = Record {
                timestamp: i as u64,
                fact,
            };
            assert_eq!(Record::decode(&record.encode()), Ok(record));
        }
    }

    #[test]
    fn replaying_facts_rebuilds_identical_state_and_feeds() {
        let mut live = Clearing::new(ClearingConfig::default());
        let mut replayed = Clearing::new(ClearingConfig::default());
        for fact in facts() {
            live.apply(&fact);
            let record = Record::decode(&Record { timestamp: 0, fact }.encode()).unwrap();
            replayed.apply(&record.fact);
        }
        assert_eq!(live.status(None), replayed.status(None));
        assert_eq!(live.replay(2, 0).messages, replayed.replay(2, 0).messages);
        assert_eq!(live.last_exchange_seq(), 3);
        assert_eq!(live.total_cash(), 999_000_00);
    }
}
