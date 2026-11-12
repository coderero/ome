use crate::price::{Price, format_price};
use crate::types::{OrderKind, OrderRequest, Qty};
use std::fmt;

#[derive(Debug, Clone, Copy)]
pub struct RiskContext {
    pub reference_price: Price,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    ZeroQuantity,
    QuantityTooLarge {
        qty: Qty,
        max: Qty,
    },
    PriceOutsideBand {
        price: Price,
        low: Price,
        high: Price,
    },
    NotionalTooLarge {
        notional: u64,
        max: u64,
    },
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rejection::ZeroQuantity => write!(f, "quantity must be greater than zero"),
            Rejection::QuantityTooLarge { qty, max } => {
                write!(f, "quantity {qty} exceeds max {max}")
            }
            Rejection::PriceOutsideBand { price, low, high } => write!(
                f,
                "price {} outside band {} - {}",
                format_price(*price),
                format_price(*low),
                format_price(*high)
            ),
            Rejection::NotionalTooLarge { notional, max } => write!(
                f,
                "notional {} exceeds max {}",
                format_price(*notional),
                format_price(*max)
            ),
        }
    }
}

pub trait RiskCheck: Send {
    fn check(&self, request: &OrderRequest, ctx: &RiskContext) -> Result<(), Rejection>;
}

pub struct PositiveQuantity;

impl RiskCheck for PositiveQuantity {
    fn check(&self, request: &OrderRequest, _: &RiskContext) -> Result<(), Rejection> {
        if request.qty == 0 {
            return Err(Rejection::ZeroQuantity);
        }
        Ok(())
    }
}

pub struct MaxQuantity(pub Qty);

impl RiskCheck for MaxQuantity {
    fn check(&self, request: &OrderRequest, _: &RiskContext) -> Result<(), Rejection> {
        if request.qty > self.0 {
            return Err(Rejection::QuantityTooLarge {
                qty: request.qty,
                max: self.0,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PriceBand {
    pub bps: u64,
}

impl PriceBand {
    pub fn limits(&self, reference: Price) -> (Price, Price) {
        let delta = reference * self.bps / 10_000;
        (reference.saturating_sub(delta).max(1), reference + delta)
    }
}

impl RiskCheck for PriceBand {
    fn check(&self, request: &OrderRequest, ctx: &RiskContext) -> Result<(), Rejection> {
        let OrderKind::Limit(price) = request.kind else {
            return Ok(());
        };
        let (low, high) = self.limits(ctx.reference_price);
        if price < low || price > high {
            return Err(Rejection::PriceOutsideBand { price, low, high });
        }
        Ok(())
    }
}

pub struct MaxNotional(pub u64);

impl RiskCheck for MaxNotional {
    fn check(&self, request: &OrderRequest, ctx: &RiskContext) -> Result<(), Rejection> {
        let price = match request.kind {
            OrderKind::Limit(price) => price,
            OrderKind::Market => ctx.reference_price,
        };
        let notional = price.saturating_mul(request.qty);
        if notional > self.0 {
            return Err(Rejection::NotionalTooLarge {
                notional,
                max: self.0,
            });
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct RiskEngine {
    checks: Vec<Box<dyn RiskCheck>>,
}

impl RiskEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, check: impl RiskCheck + 'static) -> Self {
        self.checks.push(Box::new(check));
        self
    }

    pub fn check(&self, request: &OrderRequest, ctx: &RiskContext) -> Result<(), Rejection> {
        self.checks.iter().try_for_each(|c| c.check(request, ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Side;

    fn limit(price: Price, qty: Qty) -> OrderRequest {
        OrderRequest {
            side: Side::Buy,
            kind: OrderKind::Limit(price),
            qty,
        }
    }

    fn engine() -> RiskEngine {
        RiskEngine::new()
            .with(PositiveQuantity)
            .with(MaxQuantity(1_000))
            .with(PriceBand { bps: 1_000 })
            .with(MaxNotional(5_000_000))
    }

    const CTX: RiskContext = RiskContext {
        reference_price: 10_000,
    };

    #[test]
    fn band_is_symmetric_around_reference() {
        assert_eq!(PriceBand { bps: 1_000 }.limits(10_000), (9_000, 11_000));
        assert_eq!(PriceBand { bps: 500 }.limits(10_000), (9_500, 10_500));
    }

    #[test]
    fn accepts_sane_orders() {
        assert_eq!(engine().check(&limit(10_050, 10), &CTX), Ok(()));
        assert_eq!(engine().check(&limit(11_000, 10), &CTX), Ok(()));
    }

    #[test]
    fn rejects_absurd_prices_on_both_sides() {
        for price in [100_000, 11_001, 8_999, 1] {
            assert!(matches!(
                engine().check(&limit(price, 10), &CTX),
                Err(Rejection::PriceOutsideBand { .. })
            ));
        }
    }

    #[test]
    fn rejects_bad_quantities_and_notional() {
        assert_eq!(
            engine().check(&limit(10_000, 0), &CTX),
            Err(Rejection::ZeroQuantity)
        );
        assert_eq!(
            engine().check(&limit(10_000, 5_000), &CTX),
            Err(Rejection::QuantityTooLarge {
                qty: 5_000,
                max: 1_000
            })
        );
        assert!(matches!(
            engine().check(&limit(10_000, 600), &CTX),
            Err(Rejection::NotionalTooLarge { .. })
        ));
    }

    #[test]
    fn market_orders_skip_band_but_not_notional() {
        let mut market = limit(0, 600);
        market.kind = OrderKind::Market;
        assert!(matches!(
            engine().check(&market, &CTX),
            Err(Rejection::NotionalTooLarge { .. })
        ));
        market.qty = 10;
        assert_eq!(engine().check(&market, &CTX), Ok(()));
    }
}
