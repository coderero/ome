use ome_core::price::{Price, format_price, parse_price};
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Side, TimeInForce};
use ome_protocol::Trigger;
use ome_ui_data::OrderRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Market,
    Limit,
    Stop,
    Bracket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quote {
    pub bid: Option<Price>,
    pub ask: Option<Price>,
    pub last: Option<Price>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderForm {
    pub side: Side,
    pub kind: Kind,
    pub tif: TimeInForce,
    pub qty: String,
    pub price: String,
    pub target_pct: String,
    pub stop_pct: String,
}

impl Default for OrderForm {
    fn default() -> Self {
        Self {
            side: Side::Buy,
            kind: Kind::Limit,
            tif: TimeInForce::Day,
            qty: "10".into(),
            price: String::new(),
            target_pct: "3".into(),
            stop_pct: "2".into(),
        }
    }
}

fn percent(text: &str, what: &str) -> Result<f64, String> {
    let value: f64 = text
        .trim()
        .parse()
        .map_err(|_| format!("{what} must be a number"))?;
    if value <= 0.0 || value >= 50.0 {
        return Err(format!("{what} must be between 0 and 50%"));
    }
    Ok(value)
}

fn shift(price: Price, pct: f64) -> Price {
    ((price as f64) * (1.0 + pct / 100.0)).round().max(1.0) as Price
}

impl OrderForm {
    pub fn qty(&self) -> Result<u64, String> {
        match self.qty.trim().replace(',', "").parse::<u64>() {
            Ok(q) if q > 0 => Ok(q),
            _ => Err("quantity must be a whole number above zero".into()),
        }
    }

    pub fn price(&self) -> Result<Price, String> {
        match parse_price(self.price.trim()) {
            Some(p) if p > 0 => Ok(p),
            _ => Err("enter a price such as 25.10".into()),
        }
    }

    pub fn fill_price(&mut self, quote: Quote) {
        let price = match self.side {
            Side::Buy => quote.ask.or(quote.last),
            Side::Sell => quote.bid.or(quote.last),
        };
        if let Some(p) = price {
            self.price = format_price(p);
        }
    }

    pub fn nudge(&mut self, cents: i64, quote: Quote) {
        let base = self.price().ok().or(quote.last).unwrap_or(0) as i64;
        if base > 0 {
            self.price = format_price((base + cents).max(1) as Price);
        }
    }

    pub fn build(&self, symbol: Symbol) -> Result<OrderRequest, String> {
        let qty = self.qty()?;
        let side = self.side;
        Ok(match self.kind {
            Kind::Market => OrderRequest::Market { symbol, side, qty },
            Kind::Limit => OrderRequest::Limit {
                symbol,
                side,
                price: self.price()?,
                qty,
                tif: self.tif,
            },
            Kind::Stop => {
                let level = self.price()?;
                OrderRequest::Stop {
                    symbol,
                    side,
                    qty,
                    trigger: match side {
                        Side::Sell => Trigger::Below(level),
                        Side::Buy => Trigger::Above(level),
                    },
                    kind: OrderKind::Market,
                }
            }
            Kind::Bracket => {
                let entry = self.price()?;
                let target = percent(&self.target_pct, "target")?;
                let stop = percent(&self.stop_pct, "stop")?;
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
}

pub fn describe(request: &OrderRequest) -> String {
    let verb = |side: Side| match side {
        Side::Buy => "BUY",
        Side::Sell => "SELL",
    };
    match *request {
        OrderRequest::Market { symbol, side, qty } => {
            format!("{} {qty} {symbol} MKT", verb(side))
        }
        OrderRequest::Limit {
            symbol,
            side,
            price,
            qty,
            tif,
        } => format!(
            "{} {qty} {symbol} LMT {} {}",
            verb(side),
            format_price(price),
            match tif {
                TimeInForce::Gtc => "GTC",
                _ => "DAY",
            }
        ),
        OrderRequest::Stop {
            symbol,
            side,
            qty,
            trigger,
            ..
        } => format!(
            "{} {qty} {symbol} STP, market {}",
            verb(side),
            trigger.describe()
        ),
        OrderRequest::Bracket {
            symbol,
            side,
            qty,
            entry,
            take_profit,
            stop_loss,
        } => format!(
            "{} {qty} {symbol} LMT {} with target {} and stop {}",
            verb(side),
            match entry {
                OrderKind::Limit(p) => format_price(p),
                OrderKind::Market => "MKT".into(),
            },
            format_price(take_profit),
            format_price(stop_loss)
        ),
        OrderRequest::Cancel(id) => format!("cancel order #{id}"),
        OrderRequest::CancelConditional(id) => format!("cancel conditional {id}"),
        OrderRequest::CancelAll(_) => "cancel all".into(),
    }
}

pub fn priced(request: &OrderRequest, quote: Quote) -> Option<(Side, u64, Price)> {
    match *request {
        OrderRequest::Market { side, qty, .. } | OrderRequest::Stop { side, qty, .. } => {
            Some((side, qty, quote.last?))
        }
        OrderRequest::Limit {
            side, qty, price, ..
        } => Some((side, qty, price)),
        OrderRequest::Bracket {
            side,
            qty,
            entry: OrderKind::Limit(price),
            ..
        } => Some((side, qty, price)),
        _ => None,
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
    fn the_order_form_validates_typed_fields_and_builds_retail_orders() {
        let mut form = OrderForm::default();
        assert!(form.build(acme()).unwrap_err().contains("price"));
        form.fill_price(QUOTE);
        assert_eq!(form.price, "25.05", "buyers start at the ask");
        form.side = Side::Sell;
        form.fill_price(QUOTE);
        assert_eq!(form.price, "24.95");
        form.nudge(3, QUOTE);
        assert_eq!(form.price, "24.98");
        assert_eq!(
            form.build(acme()),
            Ok(OrderRequest::Limit {
                symbol: acme(),
                side: Side::Sell,
                price: 24_98,
                qty: 10,
                tif: TimeInForce::Day
            })
        );
        form.qty = "1,000".into();
        assert_eq!(form.qty(), Ok(1_000));
        form.qty = "0".into();
        assert!(form.build(acme()).is_err());
        form.qty = "10".into();

        form.kind = Kind::Stop;
        form.price = "24.50".into();
        assert!(matches!(
            form.build(acme()),
            Ok(OrderRequest::Stop {
                trigger: Trigger::Below(24_50),
                ..
            })
        ));
        form.side = Side::Buy;
        form.kind = Kind::Bracket;
        form.price = "25.00".into();
        let bracket = form.build(acme()).unwrap();
        assert!(matches!(
            bracket,
            OrderRequest::Bracket {
                take_profit: 25_75,
                stop_loss: 24_50,
                ..
            }
        ));
        assert!(describe(&bracket).contains("target 25.75"));
        assert_eq!(priced(&bracket, QUOTE), Some((Side::Buy, 10, 25_00)));
        form.stop_pct = "x".into();
        assert!(form.build(acme()).unwrap_err().contains("stop"));
        form.kind = Kind::Market;
        assert_eq!(
            priced(&form.build(acme()).unwrap(), QUOTE),
            Some((Side::Buy, 10, 25_00))
        );
    }
}
