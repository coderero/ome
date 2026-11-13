use crate::exchange::Command;
use crate::session::Schedule;
use ome_core::types::Timestamp;
use ome_protocol::{
    DecodeError, Decoder, Encoder, get_order_kind, get_order_options, get_quote_side,
    member_kind_from, phase_from, put_order_kind, put_order_options, put_quote_side, side_from_u8,
    side_to_u8,
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
    pub const SET_PHASE: u16 = 210;
    pub const SET_SCHEDULE: u16 = 211;
    pub const EXPIRE_GTD: u16 = 212;
    pub const SET_MEMBER_STATUS: u16 = 213;
    pub const SET_CANCEL_ON_DISCONNECT: u16 = 214;
    pub const QUOTE: u16 = 215;
    pub const SET_MMP: u16 = 216;
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
            Command::SetPhase { .. } => template::SET_PHASE,
            Command::SetSchedule { .. } => template::SET_SCHEDULE,
            Command::ExpireGtd { .. } => template::EXPIRE_GTD,
            Command::SetMemberStatus { .. } => template::SET_MEMBER_STATUS,
            Command::SetCancelOnDisconnect { .. } => template::SET_CANCEL_ON_DISCONNECT,
            Command::Quote { .. } => template::QUOTE,
            Command::SetMmp { .. } => template::SET_MMP,
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
                options,
                qty,
            } => {
                e.u64(*member)
                    .u64(*member_order_id)
                    .u64(*account)
                    .symbol(Some(*symbol))
                    .u8(side_to_u8(*side));
                put_order_kind(&mut e, *kind);
                put_order_options(&mut e, options);
                e.u64(*qty).end_block();
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
            Command::ExpireGtd { at } => {
                e.u64(*at).end_block();
            }
            Command::SetMemberStatus { member, enabled }
            | Command::SetCancelOnDisconnect { member, enabled } => {
                e.u64(*member).u8(*enabled as u8).end_block();
            }
            Command::Quote {
                member,
                account,
                symbol,
                bid_ref,
                bid,
                ask_ref,
                ask,
            } => {
                e.u64(*member)
                    .u64(*account)
                    .symbol(Some(*symbol))
                    .u64(*bid_ref);
                put_quote_side(&mut e, bid);
                e.u64(*ask_ref);
                put_quote_side(&mut e, ask);
                e.end_block();
            }
            Command::SetMmp {
                member,
                account,
                window_ms,
                max_qty,
            } => {
                e.u64(*member)
                    .u64(*account)
                    .u64(*window_ms)
                    .u64(*max_qty)
                    .end_block();
            }
            Command::SetPhase { phase } => {
                e.u8(*phase as u8).end_block();
            }
            Command::SetSchedule { schedule } => {
                let s = schedule.unwrap_or(Schedule {
                    pre_open_ms: 0,
                    continuous_ms: 0,
                    closing_ms: 0,
                    closed_ms: 0,
                });
                e.u8(schedule.is_some() as u8)
                    .u64(s.pre_open_ms)
                    .u64(s.continuous_ms)
                    .u64(s.closing_ms)
                    .u64(s.closed_ms)
                    .end_block();
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
                let options = get_order_options(&mut d)?;
                let qty = d.u64()?;
                d.end_block()?;
                Command::NewOrder {
                    member,
                    member_order_id,
                    account,
                    symbol,
                    side,
                    kind,
                    options,
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
            template::SET_MEMBER_STATUS | template::SET_CANCEL_ON_DISCONNECT => {
                let member = d.u64()?;
                let enabled = d.u8()? != 0;
                d.end_block()?;
                if d.template_id == template::SET_MEMBER_STATUS {
                    Command::SetMemberStatus { member, enabled }
                } else {
                    Command::SetCancelOnDisconnect { member, enabled }
                }
            }
            template::QUOTE => {
                let member = d.u64()?;
                let account = d.u64()?;
                let symbol = d.symbol()?;
                let bid_ref = d.u64()?;
                let bid = get_quote_side(&mut d)?;
                let ask_ref = d.u64()?;
                let ask = get_quote_side(&mut d)?;
                d.end_block()?;
                Command::Quote {
                    member,
                    account,
                    symbol,
                    bid_ref,
                    bid,
                    ask_ref,
                    ask,
                }
            }
            template::SET_MMP => {
                let member = d.u64()?;
                let account = d.u64()?;
                let window_ms = d.u64()?;
                let max_qty = d.u64()?;
                d.end_block()?;
                Command::SetMmp {
                    member,
                    account,
                    window_ms,
                    max_qty,
                }
            }
            template::EXPIRE_GTD => {
                let at = d.u64()?;
                d.end_block()?;
                Command::ExpireGtd { at }
            }
            template::SET_PHASE => {
                let phase = phase_from(d.u8()?)?;
                d.end_block()?;
                Command::SetPhase { phase }
            }
            template::SET_SCHEDULE => {
                let enabled = d.u8()? != 0;
                let schedule = Schedule {
                    pre_open_ms: d.u64()?,
                    continuous_ms: d.u64()?,
                    closing_ms: d.u64()?,
                    closed_ms: d.u64()?,
                };
                d.end_block()?;
                Command::SetSchedule {
                    schedule: enabled.then_some(schedule),
                }
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
    use ome_core::types::{OrderKind, OrderOptions, Side, TimeInForce};
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
                options: if i % 4 == 0 {
                    OrderOptions::tif(TimeInForce::Ioc)
                } else if i % 7 == 0 {
                    OrderOptions {
                        display_qty: Some(1),
                        ..OrderOptions::tif(TimeInForce::Gtd(1_000_000))
                    }
                } else {
                    OrderOptions::tif(TimeInForce::Gtc)
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
        commands.push(Command::SetSchedule {
            schedule: Some(Schedule {
                pre_open_ms: 5,
                continuous_ms: 40,
                closing_ms: 5,
                closed_ms: 10,
            }),
        });
        commands.push(Command::SetPhase {
            phase: ome_protocol::MarketPhase::Closed,
        });
        commands.push(Command::SetPhase {
            phase: ome_protocol::MarketPhase::PreOpen,
        });
        commands.push(Command::SetPhase {
            phase: ome_protocol::MarketPhase::Open,
        });
        commands.push(Command::SetSchedule { schedule: None });
        commands.push(Command::ExpireGtd { at: 2_000_000 });
        commands.push(Command::SetMmp {
            member: 3,
            account: 2,
            window_ms: 1_000,
            max_qty: 5,
        });
        commands.push(Command::Quote {
            member: 3,
            account: 2,
            symbol: sym("ACME"),
            bid_ref: 900,
            bid: ome_protocol::QuoteSide {
                price: 24_95,
                qty: 4,
            },
            ask_ref: 901,
            ask: ome_protocol::QuoteSide {
                price: 25_05,
                qty: 4,
            },
        });
        commands.push(Command::SetCancelOnDisconnect {
            member: 2,
            enabled: true,
        });
        commands.push(Command::SetMemberStatus {
            member: 3,
            enabled: false,
        });
        commands.push(Command::SetMemberStatus {
            member: 3,
            enabled: true,
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
