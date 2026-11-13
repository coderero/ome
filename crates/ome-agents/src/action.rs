use crate::state::AgentState;
use ome_client::{Client, ClientError};
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderId, OrderKind, OrderOptions, Qty, Side, TimeInForce};
use ome_protocol::{AlgoStrategy, ClientMessage, QuoteSide, Trigger};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Place {
        symbol: Symbol,
        side: Side,
        kind: OrderKind,
        qty: Qty,
    },
    Cancel(OrderId),
    CancelConditional(u64),
    Quote {
        symbol: Symbol,
        bid: Option<(Price, Qty)>,
        ask: Option<(Price, Qty)>,
    },
    SetMmp {
        window_ms: u64,
        max_qty: Qty,
    },
    Conditional {
        symbol: Symbol,
        side: Side,
        qty: Qty,
        trigger: Trigger,
        kind: OrderKind,
    },
    Oco {
        symbol: Symbol,
        side: Side,
        qty: Qty,
        take_profit: Price,
        stop: Price,
    },
    Bracket {
        symbol: Symbol,
        side: Side,
        qty: Qty,
        entry: OrderKind,
        take_profit: Price,
        stop_loss: Price,
    },
    Algo {
        symbol: Symbol,
        side: Side,
        qty: Qty,
        limit: Option<Price>,
        strategy: AlgoStrategy,
    },
    Dark {
        symbol: Symbol,
        side: Side,
        qty: Qty,
        limit: Option<Price>,
        min_qty: Qty,
    },
}

impl Action {
    pub fn to_message(&self, client_order_id: u64) -> ClientMessage {
        match *self {
            Action::Place {
                symbol,
                side,
                kind,
                qty,
            } => ClientMessage::NewOrder {
                client_order_id,
                symbol,
                side,
                kind,
                options: OrderOptions::tif(match kind {
                    OrderKind::Market => TimeInForce::Ioc,
                    OrderKind::Limit(_) => TimeInForce::Gtc,
                }),
                qty,
            },
            Action::Cancel(order_id) => ClientMessage::Cancel { order_id },
            Action::CancelConditional(cond_id) => ClientMessage::CancelConditional { cond_id },
            Action::Quote { symbol, bid, ask } => {
                let side = |q: Option<(Price, Qty)>| {
                    q.map_or(QuoteSide::default(), |(price, qty)| QuoteSide {
                        price,
                        qty,
                    })
                };
                ClientMessage::Quote {
                    symbol,
                    bid: side(bid),
                    ask: side(ask),
                }
            }
            Action::SetMmp { window_ms, max_qty } => ClientMessage::SetMmp { window_ms, max_qty },
            Action::Conditional {
                symbol,
                side,
                qty,
                trigger,
                kind,
            } => ClientMessage::Conditional {
                client_order_id,
                symbol,
                side,
                qty,
                trigger,
                kind,
                options: OrderOptions::default(),
            },
            Action::Oco {
                symbol,
                side,
                qty,
                take_profit,
                stop,
            } => ClientMessage::Oco {
                client_order_id,
                symbol,
                side,
                qty,
                take_profit,
                stop,
                stop_limit: None,
            },
            Action::Bracket {
                symbol,
                side,
                qty,
                entry,
                take_profit,
                stop_loss,
            } => ClientMessage::Bracket {
                client_order_id,
                symbol,
                side,
                qty,
                entry,
                take_profit,
                stop_loss,
            },
            Action::Algo {
                symbol,
                side,
                qty,
                limit,
                strategy,
            } => ClientMessage::AlgoOrder {
                client_order_id,
                symbol,
                side,
                qty,
                limit,
                strategy,
            },
            Action::Dark {
                symbol,
                side,
                qty,
                limit,
                min_qty,
            } => ClientMessage::DarkOrder {
                client_order_id,
                symbol,
                side,
                qty,
                limit,
                min_qty,
            },
        }
    }

    fn tracked(&self) -> bool {
        !matches!(
            self,
            Action::Cancel(_)
                | Action::CancelConditional(_)
                | Action::Quote { .. }
                | Action::SetMmp { .. }
        )
    }
}

pub fn execute(
    client: &mut Client,
    state: &mut AgentState,
    action: &Action,
) -> Result<(), ClientError> {
    if let Action::SetMmp { .. } = action {
        state.mmp_tripped = false;
    }
    let client_order_id = client.next_order_id();
    client.send(&action.to_message(client_order_id))?;
    if action.tracked() {
        state.expect(client_order_id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_map_to_the_matching_client_messages() {
        let acme = Symbol::parse("ACME").unwrap();
        let market = Action::Place {
            symbol: acme,
            side: Side::Buy,
            kind: OrderKind::Market,
            qty: 5,
        };
        assert!(matches!(
            market.to_message(7),
            ClientMessage::NewOrder {
                client_order_id: 7,
                options: OrderOptions {
                    tif: TimeInForce::Ioc,
                    ..
                },
                ..
            }
        ));
        let quote = Action::Quote {
            symbol: acme,
            bid: Some((99, 10)),
            ask: None,
        };
        assert!(matches!(
            quote.to_message(1),
            ClientMessage::Quote { bid: QuoteSide { price: 99, qty: 10 }, ask, .. } if ask.is_empty()
        ));
        assert!(!quote.tracked());
        let algo = Action::Algo {
            symbol: acme,
            side: Side::Sell,
            qty: 500,
            limit: None,
            strategy: AlgoStrategy::Vwap {
                duration_ms: 60_000,
            },
        };
        assert!(matches!(
            algo.to_message(3),
            ClientMessage::AlgoOrder { qty: 500, .. }
        ));
        assert!(algo.tracked());
    }
}
