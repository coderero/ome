use crate::broker::Fact;
use ome_core::types::Timestamp;
use ome_protocol::Asset;
use ome_protocol::{
    DecodeError, Decoder, Encoder, ServerMessage, asset_symbol, role_from, side_from_u8,
    side_to_u8, tif_from_u8, tif_to_u8,
};
use ome_store::Entry;

mod template {
    pub const ACCOUNT_OPENED: u16 = 300;
    pub const BALANCE_SET: u16 = 301;
    pub const ORDER_ACCEPTED: u16 = 302;
    pub const EXCHANGE_REPORT: u16 = 303;
    pub const ALLOCATED: u16 = 304;
    pub const ORDER_ABANDONED: u16 = 305;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub timestamp: Timestamp,
    pub fact: Fact,
}

impl Entry for Record {
    fn encode(&self) -> Vec<u8> {
        let template_id = match &self.fact {
            Fact::AccountOpened { .. } => template::ACCOUNT_OPENED,
            Fact::BalanceSet { .. } => template::BALANCE_SET,
            Fact::OrderAccepted { .. } => template::ORDER_ACCEPTED,
            Fact::ExchangeReport(_) => template::EXCHANGE_REPORT,
            Fact::Allocated(_) => template::ALLOCATED,
            Fact::OrderAbandoned { .. } => template::ORDER_ABANDONED,
        };
        let mut e = Encoder::new(template_id);
        e.u64(self.timestamp);
        match &self.fact {
            Fact::AccountOpened {
                role,
                username,
                secret,
                cash,
            } => {
                e.u8(*role as u8)
                    .u64(*cash)
                    .bytes32(secret)
                    .end_block()
                    .var(username);
            }
            Fact::BalanceSet {
                account,
                asset,
                amount,
            } => {
                e.u64(*account)
                    .symbol(asset_symbol(*asset))
                    .u64(*amount)
                    .end_block();
            }
            Fact::OrderAccepted {
                order_ref,
                account,
                client_order_id,
                symbol,
                side,
                price,
                tif,
                qty,
                reserved,
            } => {
                e.u64(*order_ref)
                    .u64(*account)
                    .u64(*client_order_id)
                    .symbol(Some(*symbol))
                    .u8(side_to_u8(*side))
                    .u64(*price)
                    .u8(tif_to_u8(*tif))
                    .u64(*qty)
                    .u64(*reserved)
                    .end_block();
            }
            Fact::ExchangeReport(report) => {
                e.end_block()
                    .var_bytes(&ServerMessage::Execution(report.clone()).encode());
            }
            Fact::Allocated(allocation) => {
                e.end_block()
                    .var_bytes(&ServerMessage::Allocation(allocation.clone()).encode());
            }
            Fact::OrderAbandoned { order_ref } => {
                e.u64(*order_ref).end_block();
            }
        }
        e.finish()
    }

    fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(bytes)?;
        let timestamp = d.u64()?;
        let fact = match d.template_id {
            template::ACCOUNT_OPENED => {
                let role = role_from(d.u8()?)?;
                let cash = d.u64()?;
                let secret = d.bytes32()?;
                d.end_block()?;
                Fact::AccountOpened {
                    role,
                    username: d.var()?,
                    secret,
                    cash,
                }
            }
            template::BALANCE_SET => {
                let account = d.u64()?;
                let asset = d.optional_symbol()?.map_or(Asset::Cash, Asset::Shares);
                let amount = d.u64()?;
                d.end_block()?;
                Fact::BalanceSet {
                    account,
                    asset,
                    amount,
                }
            }
            template::ORDER_ACCEPTED => {
                let order_ref = d.u64()?;
                let account = d.u64()?;
                let client_order_id = d.u64()?;
                let symbol = d.symbol()?;
                let side = side_from_u8(d.u8()?)?;
                let price = d.u64()?;
                let tif = tif_from_u8(d.u8()?)?;
                let qty = d.u64()?;
                let reserved = d.u64()?;
                d.end_block()?;
                Fact::OrderAccepted {
                    order_ref,
                    account,
                    client_order_id,
                    symbol,
                    side,
                    price,
                    tif,
                    qty,
                    reserved,
                }
            }
            template::EXCHANGE_REPORT => {
                d.end_block()?;
                match ServerMessage::decode(d.var_bytes()?)? {
                    ServerMessage::Execution(report) => Fact::ExchangeReport(report),
                    _ => return Err(DecodeError::Invalid("embedded execution report")),
                }
            }
            template::ALLOCATED => {
                d.end_block()?;
                match ServerMessage::decode(d.var_bytes()?)? {
                    ServerMessage::Allocation(allocation) => Fact::Allocated(allocation),
                    _ => return Err(DecodeError::Invalid("embedded allocation")),
                }
            }
            template::ORDER_ABANDONED => {
                let order_ref = d.u64()?;
                d.end_block()?;
                Fact::OrderAbandoned { order_ref }
            }
            other => return Err(DecodeError::UnknownTemplate(other)),
        };
        Ok(Record { timestamp, fact })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::Broker;
    use crate::broker::tests::sym;
    use crate::profile::{Profile, ProfileKind};
    use ome_core::types::{Side, TimeInForce};
    use ome_net::auth::hash_secret;
    use ome_protocol::{Allocation, ExecutionReport, OrderStatus, Role};

    fn facts() -> Vec<Fact> {
        let report = |seq, status, last: (u64, u64), filled| {
            Fact::ExchangeReport(ExecutionReport {
                seq,
                order_id: 50,
                client_order_id: 1,
                account: 2,
                symbol: sym(),
                side: Side::Buy,
                status,
                price: 25_00,
                qty: 10,
                filled,
                leaves: 10 - filled,
                last_price: last.0,
                last_qty: last.1,
                fees: 0,
                timestamp: 3,
                reason: String::new(),
            })
        };
        vec![
            Fact::AccountOpened {
                role: Role::Issuer,
                username: "acme_ir".into(),
                secret: hash_secret("acme_ir", "pw"),
                cash: 0,
            },
            Fact::AccountOpened {
                role: Role::Quant,
                username: "fund".into(),
                secret: hash_secret("fund", "pw"),
                cash: 1_000_000_00,
            },
            Fact::BalanceSet {
                account: 2,
                asset: Asset::Shares(sym()),
                amount: 30,
            },
            Fact::Allocated(Allocation {
                seq: 1,
                allocation_id: 1,
                symbol: sym(),
                account: 1,
                shares: 500,
            }),
            Fact::OrderAccepted {
                order_ref: 1,
                account: 2,
                client_order_id: 77,
                symbol: sym(),
                side: Side::Buy,
                price: 25_00,
                tif: TimeInForce::Gtc,
                qty: 10,
                reserved: 250_00,
            },
            report(2, OrderStatus::New, (0, 0), 0),
            report(3, OrderStatus::PartiallyFilled, (24_90, 4), 4),
            Fact::OrderAccepted {
                order_ref: 2,
                account: 2,
                client_order_id: 78,
                symbol: sym(),
                side: Side::Sell,
                price: 26_00,
                tif: TimeInForce::Ioc,
                qty: 5,
                reserved: 5,
            },
            Fact::OrderAbandoned { order_ref: 2 },
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
    fn applying_journaled_facts_rebuilds_identical_state() {
        let mut live = Broker::new(Profile::new(ProfileKind::Prime));
        let mut replayed = Broker::new(Profile::new(ProfileKind::Prime));
        for fact in facts() {
            live.apply(&fact);
            let record = Record::decode(&Record { timestamp: 0, fact }.encode()).unwrap();
            replayed.apply(&record.fact);
        }
        assert_eq!(live.account_list(), replayed.account_list());
        assert_eq!(live.account_snapshot(2), replayed.account_snapshot(2));
        assert_eq!(live.last_seq(), 3);
        let fund = live.account_snapshot(2).unwrap();
        assert_eq!(fund.cash.reserved, 25_00 * 6);
    }
}
