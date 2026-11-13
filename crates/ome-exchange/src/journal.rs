use crate::exchange::Command;
use ome_core::types::Timestamp;
use ome_protocol::{
    DecodeError, Decoder, Encoder, get_order_kind, member_kind_from, put_order_kind, side_from_u8,
    side_to_u8, tif_from_u8, tif_to_u8,
};
use ome_store::Entry;

mod template {
    pub const CREATE_MEMBER: u16 = 200;
    pub const LIST_INSTRUMENT: u16 = 201;
    pub const HALT: u16 = 202;
    pub const RESUME: u16 = 203;
    pub const NEW_ORDER: u16 = 204;
    pub const CANCEL: u16 = 205;
    pub const CANCEL_ALL: u16 = 206;
    pub const OPEN_CYCLE: u16 = 207;
    pub const SETTLED: u16 = 208;
    pub const SET_CREDIT_LIMIT: u16 = 209;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub timestamp: Timestamp,
    pub command: Command,
}

impl Entry for Record {
    fn encode(&self) -> Vec<u8> {
        let template_id = match &self.command {
            Command::CreateMember { .. } => template::CREATE_MEMBER,
            Command::ListInstrument { .. } => template::LIST_INSTRUMENT,
            Command::Halt { .. } => template::HALT,
            Command::Resume { .. } => template::RESUME,
            Command::NewOrder { .. } => template::NEW_ORDER,
            Command::Cancel { .. } => template::CANCEL,
            Command::CancelAll { .. } => template::CANCEL_ALL,
            Command::OpenCycle { .. } => template::OPEN_CYCLE,
            Command::Settled { .. } => template::SETTLED,
            Command::SetCreditLimit { .. } => template::SET_CREDIT_LIMIT,
        };
        let mut e = Encoder::new(template_id);
        e.u64(self.timestamp);
        match &self.command {
            Command::CreateMember { kind, name, secret } => {
                e.u8(*kind as u8).bytes32(secret).end_block().var(name);
            }
            Command::ListInstrument {
                symbol,
                name,
                ipo_price,
                shares,
                underwriter,
                issuer_account,
            } => {
                e.symbol(Some(*symbol))
                    .u64(*ipo_price)
                    .u64(*shares)
                    .u64(*underwriter)
                    .u64(*issuer_account)
                    .end_block()
                    .var(name);
            }
            Command::Halt { symbol, reason } => {
                e.symbol(Some(*symbol)).end_block().var(reason);
            }
            Command::Resume { symbol } => {
                e.symbol(Some(*symbol)).end_block();
            }
            Command::NewOrder {
                member,
                member_order_id,
                account,
                symbol,
                side,
                kind,
                tif,
                qty,
            } => {
                e.u64(*member)
                    .u64(*member_order_id)
                    .u64(*account)
                    .symbol(Some(*symbol))
                    .u8(side_to_u8(*side));
                put_order_kind(&mut e, *kind);
                e.u8(tif_to_u8(*tif)).u64(*qty).end_block();
            }
            Command::Cancel { member, order_id } => {
                e.u64(*member).u64(*order_id).end_block();
            }
            Command::CancelAll {
                member,
                account,
                symbol,
            } => {
                e.u64(*member)
                    .u64(account.unwrap_or(0))
                    .symbol(*symbol)
                    .end_block();
            }
            Command::OpenCycle { cycle } | Command::Settled { cycle } => {
                e.u64(*cycle).end_block();
            }
            Command::SetCreditLimit { member, limit } => {
                e.u64(*member).u64(*limit).end_block();
            }
        }
        e.finish()
    }

    fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(bytes)?;
        let timestamp = d.u64()?;
        let command = match d.template_id {
            template::CREATE_MEMBER => {
                let kind = member_kind_from(d.u8()?)?;
                let secret = d.bytes32()?;
                d.end_block()?;
                Command::CreateMember {
                    kind,
                    name: d.var()?,
                    secret,
                }
            }
            template::LIST_INSTRUMENT => {
                let symbol = d.symbol()?;
                let ipo_price = d.u64()?;
                let shares = d.u64()?;
                let underwriter = d.u64()?;
                let issuer_account = d.u64()?;
                d.end_block()?;
                Command::ListInstrument {
                    symbol,
                    name: d.var()?,
                    ipo_price,
                    shares,
                    underwriter,
                    issuer_account,
                }
            }
            template::HALT => {
                let symbol = d.symbol()?;
                d.end_block()?;
                Command::Halt {
                    symbol,
                    reason: d.var()?,
                }
            }
            template::RESUME => {
                let symbol = d.symbol()?;
                d.end_block()?;
                Command::Resume { symbol }
            }
            template::NEW_ORDER => {
                let member = d.u64()?;
                let member_order_id = d.u64()?;
                let account = d.u64()?;
                let symbol = d.symbol()?;
                let side = side_from_u8(d.u8()?)?;
                let kind = get_order_kind(&mut d)?;
                let tif = tif_from_u8(d.u8()?)?;
                let qty = d.u64()?;
                d.end_block()?;
                Command::NewOrder {
                    member,
                    member_order_id,
                    account,
                    symbol,
                    side,
                    kind,
                    tif,
                    qty,
                }
            }
            template::CANCEL => {
                let member = d.u64()?;
                let order_id = d.u64()?;
                d.end_block()?;
                Command::Cancel { member, order_id }
            }
            template::CANCEL_ALL => {
                let member = d.u64()?;
                let account = Some(d.u64()?).filter(|&a| a != 0);
                let symbol = d.optional_symbol()?;
                d.end_block()?;
                Command::CancelAll {
                    member,
                    account,
                    symbol,
                }
            }
            template::OPEN_CYCLE => {
                let cycle = d.u64()?;
                d.end_block()?;
                Command::OpenCycle { cycle }
            }
            template::SETTLED => {
                let cycle = d.u64()?;
                d.end_block()?;
                Command::Settled { cycle }
            }
            template::SET_CREDIT_LIMIT => {
                let member = d.u64()?;
                let limit = d.u64()?;
                d.end_block()?;
                Command::SetCreditLimit { member, limit }
            }
            other => return Err(DecodeError::UnknownTemplate(other)),
        };
        Ok(Record { timestamp, command })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::{Exchange, ExchangeConfig};
    use ome_core::symbol::Symbol;
    use ome_core::types::{OrderKind, Side, TimeInForce};
    use ome_net::auth::hash_secret;
    use ome_protocol::MemberKind;
    use ome_store::Journal;

    fn sym(s: &str) -> Symbol {
        Symbol::parse(s).unwrap()
    }

    fn script() -> Vec<Command> {
        let mut commands = vec![
            Command::CreateMember {
                kind: MemberKind::Admin,
                name: "ops".into(),
                secret: hash_secret("ops", "pw"),
            },
            Command::CreateMember {
                kind: MemberKind::Broker,
                name: "broker".into(),
                secret: hash_secret("broker", "pw"),
            },
            Command::CreateMember {
                kind: MemberKind::LiquidityProvider,
                name: "lp".into(),
                secret: hash_secret("lp", "pw"),
            },
            Command::ListInstrument {
                symbol: sym("ACME"),
                name: "Acme".into(),
                ipo_price: 25_00,
                shares: 500,
                underwriter: 2,
                issuer_account: 1,
            },
        ];
        for i in 0..40u64 {
            let member = 2 + i % 2;
            commands.push(Command::NewOrder {
                member,
                member_order_id: i,
                account: 1 + i % 3,
                symbol: sym("ACME"),
                side: if i % 3 == 0 { Side::Sell } else { Side::Buy },
                kind: if i % 5 == 0 {
                    OrderKind::Market
                } else {
                    OrderKind::Limit(24_00 + (i * 7) % 150)
                },
                tif: if i % 4 == 0 {
                    TimeInForce::Ioc
                } else {
                    TimeInForce::Gtc
                },
                qty: 1 + i % 9,
            });
            if i % 11 == 0 {
                commands.push(Command::Cancel {
                    member,
                    order_id: i / 2 + 2,
                });
            }
        }
        commands.push(Command::Halt {
            symbol: sym("ACME"),
            reason: "maintenance".into(),
        });
        commands.push(Command::CancelAll {
            member: 3,
            account: Some(1),
            symbol: None,
        });
        commands.push(Command::OpenCycle { cycle: 1 });
        commands.push(Command::SetCreditLimit {
            member: 2,
            limit: 1_000_000_00,
        });
        commands.push(Command::Settled { cycle: 0 });
        commands.push(Command::Resume {
            symbol: sym("ACME"),
        });
        commands
    }

    #[test]
    fn every_command_round_trips() {
        for (i, command) in script().into_iter().enumerate() {
            let record = Record {
                timestamp: i as u64,
                command,
            };
            assert_eq!(Record::decode(&record.encode()), Ok(record));
        }
    }

    #[test]
    fn replay_rebuilds_identical_state_and_member_feeds() {
        let dir = std::env::temp_dir().join(format!("ome-exchange-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("test.journal");

        let mut live = Exchange::new(ExchangeConfig::default());
        {
            let (mut journal, _) = Journal::<Record>::open(&path).unwrap();
            for (ts, command) in script().into_iter().enumerate() {
                let record = Record {
                    timestamp: ts as u64,
                    command,
                };
                journal.append(&record).unwrap();
                let _ = live.execute(&record.command, record.timestamp);
            }
            journal.flush().unwrap();
        }

        let (_, records) = Journal::<Record>::open(&path).unwrap();
        let mut replayed = Exchange::new(ExchangeConfig::default());
        for record in &records {
            let _ = replayed.execute(&record.command, record.timestamp);
        }

        assert_eq!(live.instrument_list(), replayed.instrument_list());
        for member in 1..=3 {
            let (a, b) = (live.replay(member, 0), replayed.replay(member, 0));
            assert_eq!(a.messages, b.messages);
            assert_eq!(a.next_seq, b.next_seq);
        }
        assert_eq!(live.member_list(|_| false), replayed.member_list(|_| false));
        let _ = std::fs::remove_dir_all(dir);
    }
}
