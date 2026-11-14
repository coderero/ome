use crate::codec::{DecodeError, Decoder, Encoder};
use crate::template::*;
use crate::types::*;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, OrderId, OrderKind, OrderOptions, Qty, Side};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberMessage {
    Logon {
        name: String,
        secret: String,
        last_seq: u64,
    },
    Heartbeat,
    NewOrder {
        member_order_id: u64,
        account: AccountId,
        symbol: Symbol,
        side: Side,
        kind: OrderKind,
        options: OrderOptions,
        qty: Qty,
    },
    Cancel {
        order_id: OrderId,
    },
    CancelAll {
        account: Option<AccountId>,
        symbol: Option<Symbol>,
    },
    Subscribe {
        symbol: Symbol,
    },
    Unsubscribe {
        symbol: Symbol,
    },
    QueryInstruments,
    Quote {
        account: AccountId,
        symbol: Symbol,
        bid_ref: u64,
        bid: QuoteSide,
        ask_ref: u64,
        ask: QuoteSide,
    },
    SetMmp {
        account: AccountId,
        window_ms: u64,
        max_qty: Qty,
    },
    TradeReport {
        report_id: u64,
        symbol: Symbol,
        price: Price,
        qty: Qty,
        venue: Venue,
        buy_member: MemberId,
        sell_member: MemberId,
    },
    DarkOrder {
        member_order_id: u64,
        account: AccountId,
        symbol: Symbol,
        side: Side,
        limit: Option<Price>,
        qty: Qty,
        min_qty: Qty,
    },
    QueryVenue,
    SetDmm {
        symbol: Symbol,
        member: MemberId,
    },
    QueryObligations,
    QueryNewsroom,
    QueryNews {
        limit: u32,
        symbol: Option<Symbol>,
    },
    InjectNews {
        symbol: Option<Symbol>,
        event: String,
    },
    FileProspectus {
        symbol: Symbol,
        reference: u64,
        name: String,
    },
    SetMemberStatus {
        member: MemberId,
        enabled: bool,
    },
    SetCancelOnDisconnect {
        member: MemberId,
        enabled: bool,
    },
    CreateMember {
        kind: MemberKind,
        name: String,
        secret: String,
    },
    ListInstrument {
        symbol: Symbol,
        ipo_price: Price,
        shares: Qty,
        underwriter: MemberId,
        issuer_account: AccountId,
        name: String,
    },
    Halt {
        symbol: Symbol,
        reason: String,
    },
    Resume {
        symbol: Symbol,
    },
    QueryMembers,
    OpenCycle {
        cycle: u64,
    },
    Settled {
        cycle: u64,
    },
    SetCreditLimit {
        member: MemberId,
        limit: u64,
    },
    Deposit {
        member: MemberId,
        amount: i64,
    },
    QueryClearing,
    SetSession {
        phase: MarketPhase,
    },
    SetSchedule {
        pre_open_ms: u64,
        continuous_ms: u64,
        closing_ms: u64,
        closed_ms: u64,
    },
}

impl MemberMessage {
    pub fn template_id(&self) -> u16 {
        match self {
            MemberMessage::Logon { .. } => MEMBER_LOGON,
            MemberMessage::Heartbeat => MEMBER_HEARTBEAT,
            MemberMessage::NewOrder { .. } => MEMBER_NEW_ORDER,
            MemberMessage::Cancel { .. } => MEMBER_CANCEL,
            MemberMessage::CancelAll { .. } => MEMBER_CANCEL_ALL,
            MemberMessage::Subscribe { .. } => MEMBER_SUBSCRIBE,
            MemberMessage::Unsubscribe { .. } => MEMBER_UNSUBSCRIBE,
            MemberMessage::QueryInstruments => MEMBER_QUERY_INSTRUMENTS,
            MemberMessage::Quote { .. } => MEMBER_QUOTE,
            MemberMessage::SetMmp { .. } => MEMBER_SET_MMP,
            MemberMessage::TradeReport { .. } => MEMBER_TRADE_REPORT,
            MemberMessage::DarkOrder { .. } => MEMBER_DARK_ORDER,
            MemberMessage::QueryVenue => QUERY_VENUE,
            MemberMessage::SetDmm { .. } => SET_DMM,
            MemberMessage::QueryObligations => QUERY_OBLIGATIONS,
            MemberMessage::QueryNewsroom => QUERY_NEWSROOM,
            MemberMessage::QueryNews { .. } => QUERY_NEWS,
            MemberMessage::InjectNews { .. } => INJECT_NEWS,
            MemberMessage::FileProspectus { .. } => FILE_PROSPECTUS,
            MemberMessage::SetMemberStatus { .. } => SET_MEMBER_STATUS,
            MemberMessage::SetCancelOnDisconnect { .. } => SET_CANCEL_ON_DISCONNECT,
            MemberMessage::CreateMember { .. } => CREATE_MEMBER,
            MemberMessage::ListInstrument { .. } => LIST_INSTRUMENT,
            MemberMessage::Halt { .. } => HALT,
            MemberMessage::Resume { .. } => RESUME,
            MemberMessage::QueryMembers => QUERY_MEMBERS,
            MemberMessage::OpenCycle { .. } => OPEN_CYCLE,
            MemberMessage::Settled { .. } => SETTLED,
            MemberMessage::SetCreditLimit { .. } => SET_CREDIT_LIMIT,
            MemberMessage::Deposit { .. } => DEPOSIT,
            MemberMessage::QueryClearing => QUERY_CLEARING,
            MemberMessage::SetSession { .. } => SET_SESSION,
            MemberMessage::SetSchedule { .. } => SET_SCHEDULE,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new(self.template_id());
        match self {
            MemberMessage::Logon {
                name,
                secret,
                last_seq,
            } => {
                e.u64(*last_seq).end_block().var(name).var(secret);
            }
            MemberMessage::Heartbeat
            | MemberMessage::QueryInstruments
            | MemberMessage::QueryMembers
            | MemberMessage::QueryClearing
            | MemberMessage::QueryVenue
            | MemberMessage::QueryObligations
            | MemberMessage::QueryNewsroom => {
                e.end_block();
            }
            MemberMessage::QueryNews { limit, symbol } => {
                e.u32(*limit).symbol(*symbol).end_block();
            }
            MemberMessage::InjectNews { symbol, event } => {
                e.symbol(*symbol).end_block().var(event);
            }
            MemberMessage::FileProspectus {
                symbol,
                reference,
                name,
            } => {
                e.symbol(Some(*symbol))
                    .u64(*reference)
                    .end_block()
                    .var(name);
            }
            MemberMessage::SetDmm { symbol, member } => {
                e.symbol(Some(*symbol)).u64(*member).end_block();
            }
            MemberMessage::NewOrder {
                member_order_id,
                account,
                symbol,
                side,
                kind,
                options,
                qty,
            } => {
                e.u64(*member_order_id)
                    .u64(*account)
                    .symbol(Some(*symbol))
                    .u8(side_to_u8(*side));
                put_order_kind(&mut e, *kind);
                put_order_options(&mut e, options);
                e.u64(*qty).end_block();
            }
            MemberMessage::Cancel { order_id } => {
                e.u64(*order_id).end_block();
            }
            MemberMessage::Quote {
                account,
                symbol,
                bid_ref,
                bid,
                ask_ref,
                ask,
            } => {
                e.u64(*account).symbol(Some(*symbol)).u64(*bid_ref);
                put_quote_side(&mut e, bid);
                e.u64(*ask_ref);
                put_quote_side(&mut e, ask);
                e.end_block();
            }
            MemberMessage::SetMmp {
                account,
                window_ms,
                max_qty,
            } => {
                e.u64(*account).u64(*window_ms).u64(*max_qty).end_block();
            }
            MemberMessage::TradeReport {
                report_id,
                symbol,
                price,
                qty,
                venue,
                buy_member,
                sell_member,
            } => {
                e.u64(*report_id)
                    .symbol(Some(*symbol))
                    .u64(*price)
                    .u64(*qty)
                    .u8(*venue as u8)
                    .u64(*buy_member)
                    .u64(*sell_member)
                    .end_block();
            }
            MemberMessage::DarkOrder {
                member_order_id,
                account,
                symbol,
                side,
                limit,
                qty,
                min_qty,
            } => {
                e.u64(*member_order_id)
                    .u64(*account)
                    .symbol(Some(*symbol))
                    .u8(side_to_u8(*side))
                    .u64(limit.unwrap_or(0))
                    .u64(*qty)
                    .u64(*min_qty)
                    .end_block();
            }
            MemberMessage::SetMemberStatus { member, enabled }
            | MemberMessage::SetCancelOnDisconnect { member, enabled } => {
                e.u64(*member).u8(*enabled as u8).end_block();
            }
            MemberMessage::CancelAll { account, symbol } => {
                e.u64(account.unwrap_or(0)).symbol(*symbol).end_block();
            }
            MemberMessage::Subscribe { symbol }
            | MemberMessage::Unsubscribe { symbol }
            | MemberMessage::Resume { symbol } => {
                e.symbol(Some(*symbol)).end_block();
            }
            MemberMessage::CreateMember { kind, name, secret } => {
                e.u8(*kind as u8).end_block().var(name).var(secret);
            }
            MemberMessage::ListInstrument {
                symbol,
                ipo_price,
                shares,
                underwriter,
                issuer_account,
                name,
            } => {
                e.symbol(Some(*symbol))
                    .u64(*ipo_price)
                    .u64(*shares)
                    .u64(*underwriter)
                    .u64(*issuer_account)
                    .end_block()
                    .var(name);
            }
            MemberMessage::Halt { symbol, reason } => {
                e.symbol(Some(*symbol)).end_block().var(reason);
            }
            MemberMessage::OpenCycle { cycle } | MemberMessage::Settled { cycle } => {
                e.u64(*cycle).end_block();
            }
            MemberMessage::SetCreditLimit { member, limit } => {
                e.u64(*member).u64(*limit).end_block();
            }
            MemberMessage::Deposit { member, amount } => {
                e.u64(*member).i64(*amount).end_block();
            }
            MemberMessage::SetSession { phase } => {
                e.u8(*phase as u8).end_block();
            }
            MemberMessage::SetSchedule {
                pre_open_ms,
                continuous_ms,
                closing_ms,
                closed_ms,
            } => {
                e.u64(*pre_open_ms)
                    .u64(*continuous_ms)
                    .u64(*closing_ms)
                    .u64(*closed_ms)
                    .end_block();
            }
        }
        e.finish()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(bytes)?;
        let message = match d.template_id {
            MEMBER_LOGON => {
                let last_seq = d.u64()?;
                d.end_block()?;
                MemberMessage::Logon {
                    name: d.var()?,
                    secret: d.var()?,
                    last_seq,
                }
            }
            MEMBER_HEARTBEAT => {
                d.end_block()?;
                MemberMessage::Heartbeat
            }
            MEMBER_QUERY_INSTRUMENTS => {
                d.end_block()?;
                MemberMessage::QueryInstruments
            }
            QUERY_MEMBERS => {
                d.end_block()?;
                MemberMessage::QueryMembers
            }
            QUERY_CLEARING => {
                d.end_block()?;
                MemberMessage::QueryClearing
            }
            OPEN_CYCLE => {
                let cycle = d.u64()?;
                d.end_block()?;
                MemberMessage::OpenCycle { cycle }
            }
            SETTLED => {
                let cycle = d.u64()?;
                d.end_block()?;
                MemberMessage::Settled { cycle }
            }
            SET_CREDIT_LIMIT => {
                let member = d.u64()?;
                let limit = d.u64()?;
                d.end_block()?;
                MemberMessage::SetCreditLimit { member, limit }
            }
            SET_SESSION => {
                let phase = phase_from(d.u8()?)?;
                d.end_block()?;
                MemberMessage::SetSession { phase }
            }
            SET_SCHEDULE => {
                let message = MemberMessage::SetSchedule {
                    pre_open_ms: d.u64()?,
                    continuous_ms: d.u64()?,
                    closing_ms: d.u64()?,
                    closed_ms: d.u64()?,
                };
                d.end_block()?;
                message
            }
            DEPOSIT => {
                let member = d.u64()?;
                let amount = d.i64()?;
                d.end_block()?;
                MemberMessage::Deposit { member, amount }
            }
            MEMBER_NEW_ORDER => {
                let member_order_id = d.u64()?;
                let account = d.u64()?;
                let symbol = d.symbol()?;
                let side = side_from_u8(d.u8()?)?;
                let kind = get_order_kind(&mut d)?;
                let options = get_order_options(&mut d)?;
                let qty = d.u64()?;
                d.end_block()?;
                MemberMessage::NewOrder {
                    member_order_id,
                    account,
                    symbol,
                    side,
                    kind,
                    options,
                    qty,
                }
            }
            MEMBER_QUOTE => {
                let account = d.u64()?;
                let symbol = d.symbol()?;
                let bid_ref = d.u64()?;
                let bid = get_quote_side(&mut d)?;
                let ask_ref = d.u64()?;
                let ask = get_quote_side(&mut d)?;
                d.end_block()?;
                MemberMessage::Quote {
                    account,
                    symbol,
                    bid_ref,
                    bid,
                    ask_ref,
                    ask,
                }
            }
            MEMBER_TRADE_REPORT => {
                let report_id = d.u64()?;
                let symbol = d.symbol()?;
                let price = d.u64()?;
                let qty = d.u64()?;
                let venue = Venue::from_u8(d.u8()?).ok_or(DecodeError::Invalid("venue"))?;
                let buy_member = d.u64()?;
                let sell_member = d.u64()?;
                d.end_block()?;
                MemberMessage::TradeReport {
                    report_id,
                    symbol,
                    price,
                    qty,
                    venue,
                    buy_member,
                    sell_member,
                }
            }
            MEMBER_DARK_ORDER => {
                let member_order_id = d.u64()?;
                let account = d.u64()?;
                let symbol = d.symbol()?;
                let side = side_from_u8(d.u8()?)?;
                let limit = Some(d.u64()?).filter(|&p| p > 0);
                let qty = d.u64()?;
                let min_qty = d.u64()?;
                d.end_block()?;
                MemberMessage::DarkOrder {
                    member_order_id,
                    account,
                    symbol,
                    side,
                    limit,
                    qty,
                    min_qty,
                }
            }
            QUERY_VENUE => {
                d.end_block()?;
                MemberMessage::QueryVenue
            }
            SET_DMM => {
                let symbol = d.symbol()?;
                let member = d.u64()?;
                d.end_block()?;
                MemberMessage::SetDmm { symbol, member }
            }
            QUERY_OBLIGATIONS => {
                d.end_block()?;
                MemberMessage::QueryObligations
            }
            QUERY_NEWSROOM => {
                d.end_block()?;
                MemberMessage::QueryNewsroom
            }
            QUERY_NEWS => {
                let limit = d.u32()?;
                let symbol = d.optional_symbol()?;
                d.end_block()?;
                MemberMessage::QueryNews { limit, symbol }
            }
            FILE_PROSPECTUS => {
                let symbol = d.symbol()?;
                let reference = d.u64()?;
                d.end_block()?;
                MemberMessage::FileProspectus {
                    symbol,
                    reference,
                    name: d.var()?,
                }
            }
            INJECT_NEWS => {
                let symbol = d.optional_symbol()?;
                d.end_block()?;
                MemberMessage::InjectNews {
                    symbol,
                    event: d.var()?,
                }
            }
            MEMBER_SET_MMP => {
                let account = d.u64()?;
                let window_ms = d.u64()?;
                let max_qty = d.u64()?;
                d.end_block()?;
                MemberMessage::SetMmp {
                    account,
                    window_ms,
                    max_qty,
                }
            }
            SET_MEMBER_STATUS => {
                let member = d.u64()?;
                let enabled = d.u8()? != 0;
                d.end_block()?;
                MemberMessage::SetMemberStatus { member, enabled }
            }
            SET_CANCEL_ON_DISCONNECT => {
                let member = d.u64()?;
                let enabled = d.u8()? != 0;
                d.end_block()?;
                MemberMessage::SetCancelOnDisconnect { member, enabled }
            }
            MEMBER_CANCEL => {
                let order_id = d.u64()?;
                d.end_block()?;
                MemberMessage::Cancel { order_id }
            }
            MEMBER_CANCEL_ALL => {
                let account = Some(d.u64()?).filter(|&a| a != 0);
                let symbol = d.optional_symbol()?;
                d.end_block()?;
                MemberMessage::CancelAll { account, symbol }
            }
            MEMBER_SUBSCRIBE => {
                let symbol = d.symbol()?;
                d.end_block()?;
                MemberMessage::Subscribe { symbol }
            }
            MEMBER_UNSUBSCRIBE => {
                let symbol = d.symbol()?;
                d.end_block()?;
                MemberMessage::Unsubscribe { symbol }
            }
            RESUME => {
                let symbol = d.symbol()?;
                d.end_block()?;
                MemberMessage::Resume { symbol }
            }
            CREATE_MEMBER => {
                let kind = member_kind_from(d.u8()?)?;
                d.end_block()?;
                MemberMessage::CreateMember {
                    kind,
                    name: d.var()?,
                    secret: d.var()?,
                }
            }
            LIST_INSTRUMENT => {
                let symbol = d.symbol()?;
                let ipo_price = d.u64()?;
                let shares = d.u64()?;
                let underwriter = d.u64()?;
                let issuer_account = d.u64()?;
                d.end_block()?;
                MemberMessage::ListInstrument {
                    symbol,
                    ipo_price,
                    shares,
                    underwriter,
                    issuer_account,
                    name: d.var()?,
                }
            }
            HALT => {
                let symbol = d.symbol()?;
                d.end_block()?;
                MemberMessage::Halt {
                    symbol,
                    reason: d.var()?,
                }
            }
            other => return Err(DecodeError::UnknownTemplate(other)),
        };
        Ok(message)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::client::tests::sym;
    use ome_core::types::TimeInForce;

    pub fn samples() -> Vec<MemberMessage> {
        vec![
            MemberMessage::Logon {
                name: "retail-broker".into(),
                secret: "pw".into(),
                last_seq: 42,
            },
            MemberMessage::Heartbeat,
            MemberMessage::NewOrder {
                member_order_id: 7,
                account: 12,
                symbol: sym("ACME"),
                side: Side::Buy,
                kind: OrderKind::Limit(2_510),
                options: OrderOptions::tif(TimeInForce::Ioc),
                qty: 5,
            },
            MemberMessage::NewOrder {
                member_order_id: 8,
                account: 12,
                symbol: sym("ACME"),
                side: Side::Sell,
                kind: OrderKind::Market,
                options: OrderOptions::tif(TimeInForce::Gtc),
                qty: 5,
            },
            MemberMessage::Cancel { order_id: 3 },
            MemberMessage::CancelAll {
                account: None,
                symbol: None,
            },
            MemberMessage::CancelAll {
                account: Some(12),
                symbol: Some(sym("NOVA")),
            },
            MemberMessage::Subscribe {
                symbol: sym("ACME"),
            },
            MemberMessage::Unsubscribe {
                symbol: sym("ACME"),
            },
            MemberMessage::QueryInstruments,
            MemberMessage::CreateMember {
                kind: MemberKind::LiquidityProvider,
                name: "citadel-lp".into(),
                secret: "x".into(),
            },
            MemberMessage::ListInstrument {
                symbol: sym("ZEN"),
                ipo_price: 1_000,
                shares: 50_000,
                underwriter: 3,
                issuer_account: 9,
                name: "Zen Robotics".into(),
            },
            MemberMessage::Halt {
                symbol: sym("ZEN"),
                reason: "news pending".into(),
            },
            MemberMessage::Resume { symbol: sym("ZEN") },
            MemberMessage::QueryMembers,
            MemberMessage::OpenCycle { cycle: 3 },
            MemberMessage::Settled { cycle: 2 },
            MemberMessage::SetCreditLimit {
                member: 2,
                limit: u64::MAX,
            },
            MemberMessage::Deposit {
                member: 2,
                amount: -500,
            },
            MemberMessage::QueryClearing,
            MemberMessage::Quote {
                account: 4,
                symbol: sym("ACME"),
                bid_ref: 10,
                bid: QuoteSide {
                    price: 2_499,
                    qty: 50,
                },
                ask_ref: 11,
                ask: QuoteSide {
                    price: 2_501,
                    qty: 40,
                },
            },
            MemberMessage::SetMmp {
                account: 4,
                window_ms: 1_000,
                max_qty: 200,
            },
            MemberMessage::TradeReport {
                report_id: 7,
                symbol: sym("ACME"),
                price: 2_505,
                qty: 30,
                venue: Venue::Internal,
                buy_member: 0,
                sell_member: 0,
            },
            MemberMessage::DarkOrder {
                member_order_id: 9,
                account: 4,
                symbol: sym("ACME"),
                side: Side::Buy,
                limit: Some(2_510),
                qty: 5_000,
                min_qty: 500,
            },
            MemberMessage::QueryVenue,
            MemberMessage::SetDmm {
                symbol: sym("ACME"),
                member: 5,
            },
            MemberMessage::QueryObligations,
            MemberMessage::QueryNewsroom,
            MemberMessage::QueryNews {
                limit: 20,
                symbol: Some(sym("ACME")),
            },
            MemberMessage::InjectNews {
                symbol: None,
                event: "rate-hike".into(),
            },
            MemberMessage::FileProspectus {
                symbol: sym("ACME"),
                reference: 1,
                name: "Acme Robotics".into(),
            },
            MemberMessage::SetMemberStatus {
                member: 3,
                enabled: false,
            },
            MemberMessage::SetCancelOnDisconnect {
                member: 3,
                enabled: true,
            },
            MemberMessage::SetSession {
                phase: MarketPhase::ClosingAuction,
            },
            MemberMessage::SetSchedule {
                pre_open_ms: 5_000,
                continuous_ms: 45_000,
                closing_ms: 5_000,
                closed_ms: 5_000,
            },
        ]
    }

    #[test]
    fn every_member_message_round_trips() {
        for message in samples() {
            assert_eq!(
                MemberMessage::decode(&message.encode()).as_ref(),
                Ok(&message)
            );
        }
    }

    #[test]
    fn member_and_client_templates_do_not_collide() {
        for m in samples() {
            assert!(crate::ClientMessage::decode(&m.encode()).is_err());
        }
        for c in crate::client::tests::samples() {
            assert!(MemberMessage::decode(&c.encode()).is_err());
        }
    }
}
