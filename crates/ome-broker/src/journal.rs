use crate::broker::{Fact, QuoteLeg};
use ome_core::types::Timestamp;
use ome_protocol::Asset;
use ome_protocol::{
    DecodeError, Decoder, Encoder, ServerMessage, asset_symbol, get_charges, get_order_options,
    put_charges, put_order_options, role_from, side_from_u8, side_to_u8,
};
use ome_store::Entry;

mod template {
    pub const ACCOUNT_OPENED: u16 = 300;
    pub const BALANCE_SET: u16 = 301;
    pub const ORDER_ACCEPTED: u16 = 302;
    pub const EXCHANGE_REPORT: u16 = 303;
    pub const ALLOCATED: u16 = 304;
    pub const ORDER_ABANDONED: u16 = 305;
    pub const SETTLEMENT_RECEIVED: u16 = 306;
    pub const QUOTE_ACCEPTED: u16 = 307;
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
            Fact::ExchangeReport { .. } => template::EXCHANGE_REPORT,
            Fact::Allocated(_) => template::ALLOCATED,
            Fact::OrderAbandoned { .. } => template::ORDER_ABANDONED,
            Fact::SettlementReceived(_) => template::SETTLEMENT_RECEIVED,
            Fact::QuoteAccepted { .. } => template::QUOTE_ACCEPTED,
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
                options,
                qty,
                reserved,
                fee_reserve,
            } => {
                e.u64(*order_ref)
                    .u64(*account)
                    .u64(*client_order_id)
                    .symbol(Some(*symbol))
                    .u8(side_to_u8(*side))
                    .u64(*price);
                put_order_options(&mut e, options);
                e.u64(*qty).u64(*reserved).u64(*fee_reserve).end_block();
            }
            Fact::ExchangeReport { report, charges } => {
                put_charges(&mut e, charges);
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
            Fact::QuoteAccepted {
                account,
                symbol,
                bid,
                ask,
            } => {
                e.u64(*account).symbol(Some(*symbol));
                for leg in [bid, ask] {
                    let l = leg.unwrap_or(QuoteLeg {
                        order_ref: 0,
                        price: 0,
                        qty: 0,
                        reserved: 0,
                        fee_reserve: 0,
                    });
                    e.u8(leg.is_some() as u8)
                        .u64(l.order_ref)
                        .u64(l.price)
                        .u64(l.qty)
                        .u64(l.reserved)
                        .u64(l.fee_reserve);
                }
                e.end_block();
            }
            Fact::SettlementReceived(report) => {
                e.end_block()
                    .var_bytes(&ServerMessage::SettlementReport(report.clone()).encode());
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
                let options = get_order_options(&mut d)?;
                let qty = d.u64()?;
                let reserved = d.u64()?;
                let fee_reserve = d.u64()?;
                d.end_block()?;
                Fact::OrderAccepted {
                    order_ref,
                    account,
                    client_order_id,
                    symbol,
                    side,
                    price,
                    options,
                    qty,
                    reserved,
                    fee_reserve,
                }
            }
            template::EXCHANGE_REPORT => {
                let charges = get_charges(&mut d)?;
                d.end_block()?;
                match ServerMessage::decode(d.var_bytes()?)? {
                    ServerMessage::Execution(report) => Fact::ExchangeReport { report, charges },
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
            template::QUOTE_ACCEPTED => {
                let account = d.u64()?;
                let symbol = d.symbol()?;
                let mut legs = [None, None];
                for leg in &mut legs {
                    let present = d.u8()? != 0;
                    let read = QuoteLeg {
                        order_ref: d.u64()?,
                        price: d.u64()?,
                        qty: d.u64()?,
                        reserved: d.u64()?,
                        fee_reserve: d.u64()?,
                    };
                    *leg = present.then_some(read);
                }
                d.end_block()?;
                let [bid, ask] = legs;
                Fact::QuoteAccepted {
                    account,
                    symbol,
                    bid,
                    ask,
                }
            }
            template::SETTLEMENT_RECEIVED => {
                d.end_block()?;
                match ServerMessage::decode(d.var_bytes()?)? {
                    ServerMessage::SettlementReport(report) => Fact::SettlementReceived(report),
                    _ => return Err(DecodeError::Invalid("embedded settlement report")),
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
    use crate::broker::Broker;
    use crate::broker::tests::sym;
    use crate::profile::{Profile, ProfileKind};
    use ome_core::types::{Side, TimeInForce};
    use ome_net::auth::hash_secret;
    use ome_protocol::{Allocation, ExecutionReport, OrderStatus, Role};

    fn facts() -> Vec<Fact> {
        let report = |seq, status, last: (u64, u64), filled| Fact::ExchangeReport {
            charges: ome_protocol::Charges {
                commission: last.1,
                exchange_fee: -1,
                ..Default::default()
            },
            report: ExecutionReport {
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
                cycle: 1,
                timestamp: 3,
                reason: String::new(),
            },
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
                member: 3,
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
                options: ome_core::types::OrderOptions::tif(TimeInForce::Gtc),
                qty: 10,
                reserved: 250_00,
                fee_reserve: 9,
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
                options: ome_core::types::OrderOptions::tif(TimeInForce::Ioc),
                qty: 5,
                reserved: 5,
                fee_reserve: 0,
            },
            Fact::OrderAbandoned { order_ref: 2 },
            Fact::QuoteAccepted {
                account: 2,
                symbol: sym(),
                bid: Some(QuoteLeg {
                    order_ref: 3,
                    price: 24_90,
                    qty: 5,
                    reserved: 124_50,
                    fee_reserve: 1,
                }),
                ask: None,
            },
            Fact::SettlementReceived(ome_protocol::SettlementReport {
                seq: 1,
                cycle: 1,
                member: 3,
                cash: -99_60,
                collateral: 1_000_000_00,
                margin_required: 0,
                positions: vec![],
            }),
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
        assert_eq!(fund.cash.reserved, 25_00 * 6 + 6 + 124_50 + 1);
    }
}
