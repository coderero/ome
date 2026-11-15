use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Side, TimeInForce};
use ome_protocol::Trigger;
use ome_ui_data::OrderRequest;

pub const QTY_STEPS: [u64; 10] = [1, 5, 10, 25, 50, 100, 250, 500, 1_000, 5_000];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Market,
    Limit,
    Stop,
    Bracket,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Market, Kind::Limit, Kind::Stop, Kind::Bracket];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Market => "Market",
            Kind::Limit => "Limit",
            Kind::Stop => "Stop",
            Kind::Bracket => "Bracket",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quote {
    pub bid: Option<Price>,
    pub ask: Option<Price>,
    pub last: Option<Price>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    pub kind: Kind,
    pub qty_step: usize,
    pub price: Option<Price>,
    pub stop_bps: u64,
    pub target_bps: u64,
}

impl Default for Ticket {
    fn default() -> Self {
        Self {
            kind: Kind::Limit,
            qty_step: 2,
            price: None,
            stop_bps: 200,
            target_bps: 300,
        }
    }
}

fn shift(price: Price, bps: i64) -> Price {
    ((price as f64) * (1.0 + bps as f64 / 10_000.0))
        .round()
        .max(1.0) as Price
}

impl Ticket {
    pub fn qty(&self) -> u64 {
        QTY_STEPS[self.qty_step.min(QTY_STEPS.len() - 1)]
    }

    pub fn step_qty(&mut self, up: bool) {
        self.qty_step = if up {
            (self.qty_step + 1).min(QTY_STEPS.len() - 1)
        } else {
            self.qty_step.saturating_sub(1)
        };
    }

    pub fn nudge(&mut self, ticks: i64, quote: Quote) {
        let base = self.price.or(quote.last).or(quote.bid).or(quote.ask);
        self.price = base.map(|p| (p as i64 + ticks).max(1) as Price);
    }

    pub fn follow_book(&mut self) {
        self.price = None;
    }

    pub fn working_price(&self, side: Side, quote: Quote) -> Option<Price> {
        self.price.or(match side {
            Side::Buy => quote.bid.or(quote.last),
            Side::Sell => quote.ask.or(quote.last),
        })
    }

    pub fn request(&self, symbol: Symbol, side: Side, quote: Quote) -> Option<OrderRequest> {
        let qty = self.qty();
        let stop = self.stop_bps as i64;
        Some(match self.kind {
            Kind::Market => OrderRequest::Market { symbol, side, qty },
            Kind::Limit => OrderRequest::Limit {
                symbol,
                side,
                price: self.working_price(side, quote)?,
                qty,
                tif: TimeInForce::Day,
            },
            Kind::Stop => {
                let reference = self.price.or(quote.last)?;
                let trigger = match side {
                    Side::Sell => Trigger::Below(shift(reference, -stop)),
                    Side::Buy => Trigger::Above(shift(reference, stop)),
                };
                OrderRequest::Stop {
                    symbol,
                    side,
                    qty,
                    trigger,
                    kind: OrderKind::Market,
                }
            }
            Kind::Bracket => {
                let entry = self.working_price(side, quote)?;
                let target = self.target_bps as i64;
                let (take_profit, stop_loss) = match side {
                    Side::Buy => (shift(entry, target), shift(entry, -stop)),
                    Side::Sell => (shift(entry, -target), shift(entry, stop)),
                };
                OrderRequest::Bracket {
                    symbol,
                    side,
                    qty,
                    entry: OrderKind::Limit(entry),
                    take_profit,
                    stop_loss,
                }
            }
        })
    }

    pub fn describe(&self, symbol: Symbol, side: Side, quote: Quote) -> String {
        let verb = match side {
            Side::Buy => "Buy",
            Side::Sell => "Sell",
        };
        let fmt = ome_core::price::format_price;
        match self.request(symbol, side, quote) {
            None => format!("{verb} {} {symbol}: no price yet", self.qty()),
            Some(OrderRequest::Market { qty, .. }) => format!("{verb} {qty} {symbol} at market"),
            Some(OrderRequest::Limit { qty, price, .. }) => {
                format!("{verb} {qty} {symbol} limit {} (day)", fmt(price))
            }
            Some(OrderRequest::Stop { qty, trigger, .. }) => {
                format!("{verb} {qty} {symbol} at market {}", trigger.describe())
            }
            Some(OrderRequest::Bracket {
                qty,
                entry,
                take_profit,
                stop_loss,
                ..
            }) => {
                let entry = match entry {
                    OrderKind::Limit(p) => fmt(p),
                    OrderKind::Market => "market".into(),
                };
                format!(
                    "{verb} {qty} {symbol} at {entry}, target {}, stop {}",
                    fmt(take_profit),
                    fmt(stop_loss)
                )
            }
            Some(_) => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acme() -> Symbol {
        Symbol::parse("ACME").unwrap()
    }

    const QUOTE: Quote = Quote {
        bid: Some(24_95),
        ask: Some(25_05),
        last: Some(25_00),
    };

    #[test]
    fn tickets_turn_into_the_order_types_a_retail_client_may_use() {
        let mut ticket = Ticket::default();
        assert_eq!(ticket.qty(), 10);
        ticket.step_qty(true);
        assert_eq!(ticket.qty(), 25);
        for _ in 0..20 {
            ticket.step_qty(true);
        }
        assert_eq!(ticket.qty(), 5_000);
        ticket.qty_step = 2;

        assert_eq!(
            ticket.request(acme(), Side::Buy, QUOTE),
            Some(OrderRequest::Limit {
                symbol: acme(),
                side: Side::Buy,
                price: 24_95,
                qty: 10,
                tif: TimeInForce::Day
            }),
            "limits follow the book until a price is set"
        );
        ticket.nudge(3, QUOTE);
        assert_eq!(ticket.working_price(Side::Sell, QUOTE), Some(25_03));
        ticket.follow_book();

        ticket.kind = Kind::Stop;
        assert!(matches!(
            ticket.request(acme(), Side::Sell, QUOTE),
            Some(OrderRequest::Stop {
                trigger: Trigger::Below(24_50),
                ..
            })
        ));
        ticket.kind = Kind::Bracket;
        assert!(matches!(
            ticket.request(acme(), Side::Buy, QUOTE),
            Some(OrderRequest::Bracket {
                entry: OrderKind::Limit(24_95),
                take_profit: 25_70,
                stop_loss: 24_45,
                ..
            })
        ));
        assert!(
            ticket
                .describe(acme(), Side::Buy, QUOTE)
                .contains("target 25.70")
        );
        let empty = Quote {
            bid: None,
            ask: None,
            last: None,
        };
        assert_eq!(ticket.request(acme(), Side::Buy, empty), None);
        ticket.kind = Kind::Market;
        assert!(
            ticket
                .describe(acme(), Side::Sell, empty)
                .contains("at market")
        );
    }
}
