use crate::codec::{DecodeError, Decoder, Encoder};
use crate::template::*;
use crate::types::*;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, OrderId, OrderKind, OrderOptions, Qty, Side};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    Logon {
        username: String,
        secret: String,
    },
    Heartbeat,
    NewOrder {
        client_order_id: u64,
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
        symbol: Option<Symbol>,
    },
    Quote {
        symbol: Symbol,
        bid: QuoteSide,
        ask: QuoteSide,
    },
    SetMmp {
        window_ms: u64,
        max_qty: Qty,
    },
    Conditional {
        client_order_id: u64,
        symbol: Symbol,
        side: Side,
        qty: Qty,
        trigger: Trigger,
        kind: OrderKind,
        options: OrderOptions,
    },
    Oco {
        client_order_id: u64,
        symbol: Symbol,
        side: Side,
        qty: Qty,
        take_profit: Price,
        stop: Price,
        stop_limit: Option<Price>,
    },
    Bracket {
        client_order_id: u64,
        symbol: Symbol,
        side: Side,
        qty: Qty,
        entry: OrderKind,
        take_profit: Price,
        stop_loss: Price,
    },
    CancelConditional {
        cond_id: u64,
    },
    Subscribe {
        symbol: Symbol,
    },
    Unsubscribe {
        symbol: Symbol,
    },
    QueryAccount,
    QueryInstruments,
    CreateAccount {
        role: Role,
        cash: u64,
        username: String,
        secret: String,
    },
    SetBalance {
        account_id: AccountId,
        asset: Asset,
        amount: u64,
    },
    QueryAccounts,
    QueryFees,
}

impl ClientMessage {
    pub fn template_id(&self) -> u16 {
        match self {
            ClientMessage::Logon { .. } => LOGON,
            ClientMessage::Heartbeat => CLIENT_HEARTBEAT,
            ClientMessage::NewOrder { .. } => NEW_ORDER,
            ClientMessage::Cancel { .. } => CANCEL,
            ClientMessage::CancelAll { .. } => CANCEL_ALL,
            ClientMessage::Quote { .. } => QUOTE,
            ClientMessage::SetMmp { .. } => SET_MMP,
            ClientMessage::Conditional { .. } => CONDITIONAL,
            ClientMessage::Oco { .. } => OCO,
            ClientMessage::Bracket { .. } => BRACKET,
            ClientMessage::CancelConditional { .. } => CANCEL_CONDITIONAL,
            ClientMessage::Subscribe { .. } => SUBSCRIBE,
            ClientMessage::Unsubscribe { .. } => UNSUBSCRIBE,
            ClientMessage::QueryAccount => QUERY_ACCOUNT,
            ClientMessage::QueryInstruments => QUERY_INSTRUMENTS,
            ClientMessage::CreateAccount { .. } => CREATE_ACCOUNT,
            ClientMessage::SetBalance { .. } => SET_BALANCE,
            ClientMessage::QueryAccounts => QUERY_ACCOUNTS,
            ClientMessage::QueryFees => QUERY_FEES,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new(self.template_id());
        match self {
            ClientMessage::Logon { username, secret } => {
                e.end_block().var(username).var(secret);
            }
            ClientMessage::Heartbeat
            | ClientMessage::QueryAccount
            | ClientMessage::QueryInstruments
            | ClientMessage::QueryAccounts
            | ClientMessage::QueryFees => {
                e.end_block();
            }
            ClientMessage::NewOrder {
                client_order_id,
                symbol,
                side,
                kind,
                options,
                qty,
            } => {
                e.u64(*client_order_id)
                    .symbol(Some(*symbol))
                    .u8(side_to_u8(*side));
                put_order_kind(&mut e, *kind);
                e.u64(*qty);
                put_order_options(&mut e, options);
                e.end_block();
            }
            ClientMessage::Cancel { order_id } => {
                e.u64(*order_id).end_block();
            }
            ClientMessage::CancelAll { symbol } => {
                e.symbol(*symbol).end_block();
            }
            ClientMessage::Quote { symbol, bid, ask } => {
                e.symbol(Some(*symbol));
                put_quote_side(&mut e, bid);
                put_quote_side(&mut e, ask);
                e.end_block();
            }
            ClientMessage::Conditional {
                client_order_id,
                symbol,
                side,
                qty,
                trigger,
                kind,
                options,
            } => {
                e.u64(*client_order_id)
                    .symbol(Some(*symbol))
                    .u8(side_to_u8(*side))
                    .u64(*qty);
                put_trigger(&mut e, trigger);
                put_order_kind(&mut e, *kind);
                put_order_options(&mut e, options);
                e.end_block();
            }
            ClientMessage::Oco {
                client_order_id,
                symbol,
                side,
                qty,
                take_profit,
                stop,
                stop_limit,
            } => {
                e.u64(*client_order_id)
                    .symbol(Some(*symbol))
                    .u8(side_to_u8(*side))
                    .u64(*qty)
                    .u64(*take_profit)
                    .u64(*stop)
                    .u64(stop_limit.unwrap_or(0))
                    .end_block();
            }
            ClientMessage::Bracket {
                client_order_id,
                symbol,
                side,
                qty,
                entry,
                take_profit,
                stop_loss,
            } => {
                e.u64(*client_order_id)
                    .symbol(Some(*symbol))
                    .u8(side_to_u8(*side))
                    .u64(*qty);
                put_order_kind(&mut e, *entry);
                e.u64(*take_profit).u64(*stop_loss).end_block();
            }
            ClientMessage::CancelConditional { cond_id } => {
                e.u64(*cond_id).end_block();
            }
            ClientMessage::SetMmp { window_ms, max_qty } => {
                e.u64(*window_ms).u64(*max_qty).end_block();
            }
            ClientMessage::Subscribe { symbol } | ClientMessage::Unsubscribe { symbol } => {
                e.symbol(Some(*symbol)).end_block();
            }
            ClientMessage::CreateAccount {
                role,
                cash,
                username,
                secret,
            } => {
                e.u8(*role as u8)
                    .u64(*cash)
                    .end_block()
                    .var(username)
                    .var(secret);
            }
            ClientMessage::SetBalance {
                account_id,
                asset,
                amount,
            } => {
                e.u64(*account_id)
                    .symbol(asset_symbol(*asset))
                    .u64(*amount)
                    .end_block();
            }
        }
        e.finish()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(bytes)?;
        let message = match d.template_id {
            LOGON => {
                d.end_block()?;
                ClientMessage::Logon {
                    username: d.var()?,
                    secret: d.var()?,
                }
            }
            CLIENT_HEARTBEAT => {
                d.end_block()?;
                ClientMessage::Heartbeat
            }
            QUERY_ACCOUNT => {
                d.end_block()?;
                ClientMessage::QueryAccount
            }
            QUERY_INSTRUMENTS => {
                d.end_block()?;
                ClientMessage::QueryInstruments
            }
            QUERY_ACCOUNTS => {
                d.end_block()?;
                ClientMessage::QueryAccounts
            }
            QUERY_FEES => {
                d.end_block()?;
                ClientMessage::QueryFees
            }
            NEW_ORDER => {
                let client_order_id = d.u64()?;
                let symbol = d.symbol()?;
                let side = side_from_u8(d.u8()?)?;
                let kind = get_order_kind(&mut d)?;
                let qty = d.u64()?;
                let options = get_order_options(&mut d)?;
                d.end_block()?;
                ClientMessage::NewOrder {
                    client_order_id,
                    symbol,
                    side,
                    kind,
                    options,
                    qty,
                }
            }
            CANCEL => {
                let order_id = d.u64()?;
                d.end_block()?;
                ClientMessage::Cancel { order_id }
            }
            QUOTE => {
                let symbol = d.symbol()?;
                let bid = get_quote_side(&mut d)?;
                let ask = get_quote_side(&mut d)?;
                d.end_block()?;
                ClientMessage::Quote { symbol, bid, ask }
            }
            SET_MMP => {
                let window_ms = d.u64()?;
                let max_qty = d.u64()?;
                d.end_block()?;
                ClientMessage::SetMmp { window_ms, max_qty }
            }
            CONDITIONAL => {
                let client_order_id = d.u64()?;
                let symbol = d.symbol()?;
                let side = side_from_u8(d.u8()?)?;
                let qty = d.u64()?;
                let trigger = get_trigger(&mut d)?;
                let kind = get_order_kind(&mut d)?;
                let options = get_order_options(&mut d)?;
                d.end_block()?;
                ClientMessage::Conditional {
                    client_order_id,
                    symbol,
                    side,
                    qty,
                    trigger,
                    kind,
                    options,
                }
            }
            OCO => {
                let client_order_id = d.u64()?;
                let symbol = d.symbol()?;
                let side = side_from_u8(d.u8()?)?;
                let qty = d.u64()?;
                let take_profit = d.u64()?;
                let stop = d.u64()?;
                let stop_limit = Some(d.u64()?).filter(|&p| p > 0);
                d.end_block()?;
                ClientMessage::Oco {
                    client_order_id,
                    symbol,
                    side,
                    qty,
                    take_profit,
                    stop,
                    stop_limit,
                }
            }
            BRACKET => {
                let client_order_id = d.u64()?;
                let symbol = d.symbol()?;
                let side = side_from_u8(d.u8()?)?;
                let qty = d.u64()?;
                let entry = get_order_kind(&mut d)?;
                let take_profit = d.u64()?;
                let stop_loss = d.u64()?;
                d.end_block()?;
                ClientMessage::Bracket {
                    client_order_id,
                    symbol,
                    side,
                    qty,
                    entry,
                    take_profit,
                    stop_loss,
                }
            }
            CANCEL_CONDITIONAL => {
                let cond_id = d.u64()?;
                d.end_block()?;
                ClientMessage::CancelConditional { cond_id }
            }
            CANCEL_ALL => {
                let symbol = d.optional_symbol()?;
                d.end_block()?;
                ClientMessage::CancelAll { symbol }
            }
            SUBSCRIBE => {
                let symbol = d.symbol()?;
                d.end_block()?;
                ClientMessage::Subscribe { symbol }
            }
            UNSUBSCRIBE => {
                let symbol = d.symbol()?;
                d.end_block()?;
                ClientMessage::Unsubscribe { symbol }
            }
            CREATE_ACCOUNT => {
                let role = role_from(d.u8()?)?;
                let cash = d.u64()?;
                d.end_block()?;
                ClientMessage::CreateAccount {
                    role,
                    cash,
                    username: d.var()?,
                    secret: d.var()?,
                }
            }
            SET_BALANCE => {
                let account_id = d.u64()?;
                let asset = d.optional_symbol()?.map_or(Asset::Cash, Asset::Shares);
                let amount = d.u64()?;
                d.end_block()?;
                ClientMessage::SetBalance {
                    account_id,
                    asset,
                    amount,
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
    use ome_core::types::TimeInForce;

    pub fn sym(s: &str) -> Symbol {
        Symbol::parse(s).unwrap()
    }

    pub fn samples() -> Vec<ClientMessage> {
        vec![
            ClientMessage::Logon {
                username: "alice".into(),
                secret: "pw".into(),
            },
            ClientMessage::Heartbeat,
            ClientMessage::NewOrder {
                client_order_id: 9,
                symbol: sym("ACME"),
                side: Side::Buy,
                kind: OrderKind::Limit(2_500),
                options: OrderOptions {
                    tif: TimeInForce::Gtd(1_700_000_000_000),
                    post_only: true,
                    display_qty: Some(2),
                    stp: ome_core::types::StpMode::CancelBoth,
                },
                qty: 10,
            },
            ClientMessage::NewOrder {
                client_order_id: 10,
                symbol: sym("ACME"),
                side: Side::Sell,
                kind: OrderKind::Market,
                options: OrderOptions::tif(TimeInForce::Ioc),
                qty: 3,
            },
            ClientMessage::Cancel { order_id: 77 },
            ClientMessage::CancelAll { symbol: None },
            ClientMessage::Quote {
                symbol: sym("ACME"),
                bid: QuoteSide {
                    price: 2_499,
                    qty: 100,
                },
                ask: QuoteSide::default(),
            },
            ClientMessage::SetMmp {
                window_ms: 1_000,
                max_qty: 500,
            },
            ClientMessage::Conditional {
                client_order_id: 21,
                symbol: sym("ACME"),
                side: Side::Sell,
                qty: 10,
                trigger: Trigger::TrailingBelow(50),
                kind: OrderKind::Market,
                options: OrderOptions::tif(TimeInForce::Ioc),
            },
            ClientMessage::Oco {
                client_order_id: 22,
                symbol: sym("ACME"),
                side: Side::Sell,
                qty: 10,
                take_profit: 2_700,
                stop: 2_300,
                stop_limit: Some(2_290),
            },
            ClientMessage::Bracket {
                client_order_id: 23,
                symbol: sym("ACME"),
                side: Side::Buy,
                qty: 10,
                entry: OrderKind::Limit(2_500),
                take_profit: 2_700,
                stop_loss: 2_400,
            },
            ClientMessage::CancelConditional { cond_id: 5 },
            ClientMessage::CancelAll {
                symbol: Some(sym("NOVA")),
            },
            ClientMessage::Subscribe {
                symbol: sym("ACME"),
            },
            ClientMessage::Unsubscribe {
                symbol: sym("ACME"),
            },
            ClientMessage::QueryAccount,
            ClientMessage::QueryInstruments,
            ClientMessage::CreateAccount {
                role: Role::Hft,
                cash: 1_000_000,
                username: "jump".into(),
                secret: "s3cret".into(),
            },
            ClientMessage::SetBalance {
                account_id: 4,
                asset: Asset::Cash,
                amount: 500,
            },
            ClientMessage::SetBalance {
                account_id: 4,
                asset: Asset::Shares(sym("ACME")),
                amount: 20,
            },
            ClientMessage::QueryAccounts,
            ClientMessage::QueryFees,
        ]
    }

    #[test]
    fn every_client_message_round_trips() {
        for message in samples() {
            assert_eq!(
                ClientMessage::decode(&message.encode()).as_ref(),
                Ok(&message)
            );
        }
    }

    #[test]
    fn new_order_has_fixed_compact_layout() {
        let bytes = samples()[2].encode();
        assert_eq!(bytes.len(), 8 + 8 + 8 + 1 + 1 + 8 + 8 + 19);
        assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), NEW_ORDER);
    }

    #[test]
    fn roles_round_trip_and_parse() {
        for role in Role::ALL {
            assert_eq!(Role::from_u8(role as u8), Some(role));
            assert_eq!(Role::parse(role.label()), Some(role));
        }
    }
}
