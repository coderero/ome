use crate::dark::DarkOrder;
use crate::venue::Fact;
use ome_core::types::Timestamp;
use ome_protocol::{DecodeError, Decoder, Encoder, ServerMessage, side_from_u8, side_to_u8};
use ome_store::Entry;

mod template {
    pub const MEMBER_REGISTERED: u16 = 500;
    pub const FILLED: u16 = 501;
    pub const DECLINED: u16 = 502;
    pub const HEDGE_SENT: u16 = 503;
    pub const EXCHANGE_REPORT: u16 = 504;
    pub const HEDGE_ABANDONED: u16 = 505;
    pub const DARK_ACCEPTED: u16 = 506;
    pub const DARK_MATCHED: u16 = 507;
    pub const DARK_CANCELLED: u16 = 508;
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
            Fact::Filled { .. } => template::FILLED,
            Fact::Declined { .. } => template::DECLINED,
            Fact::HedgeSent { .. } => template::HEDGE_SENT,
            Fact::ExchangeReport(_) => template::EXCHANGE_REPORT,
            Fact::HedgeAbandoned { .. } => template::HEDGE_ABANDONED,
            Fact::DarkAccepted(_) => template::DARK_ACCEPTED,
            Fact::DarkMatched { .. } => template::DARK_MATCHED,
            Fact::DarkCancelled { .. } => template::DARK_CANCELLED,
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
            Fact::Filled {
                fill_id,
                member,
                member_order_id,
                account,
                symbol,
                side,
                price,
                qty,
                pfof,
                cycle,
                timestamp,
            } => {
                e.u64(*fill_id)
                    .u64(*member)
                    .u64(*member_order_id)
                    .u64(*account)
                    .symbol(Some(*symbol))
                    .u8(side_to_u8(*side))
                    .u64(*price)
                    .u64(*qty)
                    .u64(*pfof)
                    .u64(*cycle)
                    .u64(*timestamp)
                    .end_block();
            }
            Fact::Declined {
                member,
                member_order_id,
                account,
                symbol,
                side,
                qty,
                reason,
                timestamp,
            } => {
                e.u64(*member)
                    .u64(*member_order_id)
                    .u64(*account)
                    .symbol(Some(*symbol))
                    .u8(side_to_u8(*side))
                    .u64(*qty)
                    .u64(*timestamp)
                    .end_block()
                    .var(reason);
            }
            Fact::HedgeSent {
                order_ref,
                symbol,
                side,
                price,
                qty,
            } => {
                e.u64(*order_ref)
                    .symbol(Some(*symbol))
                    .u8(side_to_u8(*side))
                    .u64(*price)
                    .u64(*qty)
                    .end_block();
            }
            Fact::ExchangeReport(report) => {
                e.end_block()
                    .var_bytes(&ServerMessage::Execution(report.clone()).encode());
            }
            Fact::HedgeAbandoned { order_ref } => {
                e.u64(*order_ref).end_block();
            }
            Fact::DarkAccepted(o) => {
                e.u64(o.order_id)
                    .u64(o.member)
                    .u64(o.member_order_id)
                    .u64(o.account)
                    .symbol(Some(o.symbol))
                    .u8(side_to_u8(o.side))
                    .u64(o.limit.unwrap_or(0))
                    .u64(o.qty)
                    .u64(o.min_qty)
                    .u64(o.timestamp)
                    .end_block();
            }
            Fact::DarkMatched {
                buy_id,
                sell_id,
                price,
                qty,
                cycle,
                timestamp,
            } => {
                e.u64(*buy_id)
                    .u64(*sell_id)
                    .u64(*price)
                    .u64(*qty)
                    .u64(*cycle)
                    .u64(*timestamp)
                    .end_block();
            }
            Fact::DarkCancelled {
                order_id,
                reason,
                timestamp,
            } => {
                e.u64(*order_id).u64(*timestamp).end_block().var(reason);
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
                    secret,
                    name: d.var()?,
                }
            }
            template::FILLED => {
                let fact = Fact::Filled {
                    fill_id: d.u64()?,
                    member: d.u64()?,
                    member_order_id: d.u64()?,
                    account: d.u64()?,
                    symbol: d.symbol()?,
                    side: side_from_u8(d.u8()?)?,
                    price: d.u64()?,
                    qty: d.u64()?,
                    pfof: d.u64()?,
                    cycle: d.u64()?,
                    timestamp: d.u64()?,
                };
                d.end_block()?;
                fact
            }
            template::DECLINED => {
                let member = d.u64()?;
                let member_order_id = d.u64()?;
                let account = d.u64()?;
                let symbol = d.symbol()?;
                let side = side_from_u8(d.u8()?)?;
                let qty = d.u64()?;
                let timestamp = d.u64()?;
                d.end_block()?;
                Fact::Declined {
                    member,
                    member_order_id,
                    account,
                    symbol,
                    side,
                    qty,
                    timestamp,
                    reason: d.var()?,
                }
            }
            template::HEDGE_SENT => {
                let fact = Fact::HedgeSent {
                    order_ref: d.u64()?,
                    symbol: d.symbol()?,
                    side: side_from_u8(d.u8()?)?,
                    price: d.u64()?,
                    qty: d.u64()?,
                };
                d.end_block()?;
                fact
            }
            template::EXCHANGE_REPORT => {
                d.end_block()?;
                match ServerMessage::decode(d.var_bytes()?)? {
                    ServerMessage::Execution(report) => Fact::ExchangeReport(report),
                    _ => return Err(DecodeError::Invalid("embedded execution report")),
                }
            }
            template::HEDGE_ABANDONED => {
                let order_ref = d.u64()?;
                d.end_block()?;
                Fact::HedgeAbandoned { order_ref }
            }
            template::DARK_ACCEPTED => {
                let order = DarkOrder {
                    order_id: d.u64()?,
                    member: d.u64()?,
                    member_order_id: d.u64()?,
                    account: d.u64()?,
                    symbol: d.symbol()?,
                    side: side_from_u8(d.u8()?)?,
                    limit: Some(d.u64()?).filter(|&p| p > 0),
                    qty: d.u64()?,
                    min_qty: d.u64()?,
                    filled: 0,
                    timestamp: d.u64()?,
                };
                d.end_block()?;
                Fact::DarkAccepted(order)
            }
            template::DARK_MATCHED => {
                let fact = Fact::DarkMatched {
                    buy_id: d.u64()?,
                    sell_id: d.u64()?,
                    price: d.u64()?,
                    qty: d.u64()?,
                    cycle: d.u64()?,
                    timestamp: d.u64()?,
                };
                d.end_block()?;
                fact
            }
            template::DARK_CANCELLED => {
                let order_id = d.u64()?;
                let timestamp = d.u64()?;
                d.end_block()?;
                Fact::DarkCancelled {
                    order_id,
                    timestamp,
                    reason: d.var()?,
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
    use ome_core::symbol::Symbol;
    use ome_core::types::Side;
    use ome_protocol::{ExecutionReport, OrderStatus};

    #[test]
    fn every_fact_round_trips() {
        let acme = Symbol::parse("ACME").unwrap();
        let facts = vec![
            Fact::MemberRegistered {
                member: 2,
                name: "retail-co".into(),
                secret: [7; 32],
            },
            Fact::Filled {
                fill_id: 1,
                member: 2,
                member_order_id: 3,
                account: 4,
                symbol: acme,
                side: Side::Buy,
                price: 25_06,
                qty: 100,
                pfof: 20,
                cycle: 2,
                timestamp: 9,
            },
            Fact::Declined {
                member: 2,
                member_order_id: 4,
                account: 4,
                symbol: acme,
                side: Side::Sell,
                qty: 9,
                reason: "declined: inventory".into(),
                timestamp: 10,
            },
            Fact::HedgeSent {
                order_ref: 5,
                symbol: acme,
                side: Side::Buy,
                price: 27_50,
                qty: 600,
            },
            Fact::ExchangeReport(ExecutionReport {
                seq: 3,
                order_id: 1,
                client_order_id: 5,
                account: 1,
                symbol: acme,
                side: Side::Buy,
                status: OrderStatus::Filled,
                price: 27_50,
                qty: 600,
                filled: 600,
                leaves: 0,
                last_price: 25_10,
                last_qty: 600,
                fees: 180,
                cycle: 2,
                timestamp: 11,
                reason: String::new(),
            }),
            Fact::HedgeAbandoned { order_ref: 5 },
            Fact::DarkAccepted(DarkOrder {
                order_id: 6,
                member: 3,
                member_order_id: 7,
                account: 8,
                symbol: acme,
                side: Side::Sell,
                limit: Some(25_20),
                qty: 900,
                min_qty: 100,
                filled: 0,
                timestamp: 12,
            }),
            Fact::DarkMatched {
                buy_id: 6,
                sell_id: 7,
                price: 25_00,
                qty: 100,
                cycle: 2,
                timestamp: 13,
            },
            Fact::DarkCancelled {
                order_id: 6,
                reason: "cancelled".into(),
                timestamp: 14,
            },
        ];
        for (i, fact) in facts.into_iter().enumerate() {
            let record = Record {
                timestamp: i as u64,
                fact,
            };
            assert_eq!(Record::decode(&record.encode()), Ok(record));
        }
    }
}
