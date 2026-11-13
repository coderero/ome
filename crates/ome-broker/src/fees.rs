use ome_core::price::Price;
use ome_core::types::{Qty, Side};
use ome_protocol::{Charges, Role};

const PPM: u128 = 1_000_000;
const PER_SHARE_SCALE: u128 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommissionPlan {
    pub rate_ppm: u64,
    pub cap_per_order: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Taxes {
    pub sell_tax_ppm: u64,
    pub buy_stamp_ppm: u64,
    pub regulatory_ppm: u64,
    pub gst_ppm: u64,
}

impl Default for Taxes {
    fn default() -> Self {
        Self {
            sell_tax_ppm: 1_000,
            buy_stamp_ppm: 150,
            regulatory_ppm: 1,
            gst_ppm: 180_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeEngine {
    pub taxes: Taxes,
    pub exchange_fee_ceiling_per_share: u64,
}

impl Default for FeeEngine {
    fn default() -> Self {
        Self {
            taxes: Taxes::default(),
            exchange_fee_ceiling_per_share: 50,
        }
    }
}

fn ppm_of(amount: u64, ppm: u64) -> u64 {
    ((amount as u128 * ppm as u128 + PPM / 2) / PPM) as u64
}

impl FeeEngine {
    pub fn plan(role: Role) -> CommissionPlan {
        let (rate_ppm, cap_per_order) = match role {
            Role::Retail => (300, 20_00),
            Role::Quant | Role::Prop => (50, 0),
            Role::Hft => (5, 0),
            Role::Issuer => (20_000, 0),
            Role::LpDesk | Role::BrokerAdmin => (0, 0),
        };
        CommissionPlan {
            rate_ppm,
            cap_per_order,
        }
    }

    pub fn charges(
        &self,
        role: Role,
        side: Side,
        price: Price,
        qty: Qty,
        exchange_fee: i64,
        commission_so_far: u64,
    ) -> Charges {
        let notional = price.saturating_mul(qty);
        let plan = Self::plan(role);
        let mut commission = ppm_of(notional, plan.rate_ppm);
        if plan.cap_per_order > 0 {
            commission = commission.min(plan.cap_per_order.saturating_sub(commission_so_far));
        }
        let (transaction_tax, stamp_duty) = match side {
            Side::Sell => (ppm_of(notional, self.taxes.sell_tax_ppm), 0),
            Side::Buy => (0, ppm_of(notional, self.taxes.buy_stamp_ppm)),
        };
        let regulatory_fee = ppm_of(notional, self.taxes.regulatory_ppm);
        let gst_base = commission + regulatory_fee + exchange_fee.max(0) as u64;
        Charges {
            commission,
            exchange_fee,
            transaction_tax,
            stamp_duty,
            regulatory_fee,
            gst: ppm_of(gst_base, self.taxes.gst_ppm),
        }
    }

    pub fn reserve(&self, role: Role, side: Side, price: Price, qty: Qty) -> u64 {
        if side == Side::Sell {
            return 0;
        }
        let ceiling = ((qty as u128 * self.exchange_fee_ceiling_per_share as u128)
            .div_ceil(PER_SHARE_SCALE)) as i64;
        let charges = self.charges(role, side, price, qty, ceiling, 0);
        charges.total().max(0) as u64 + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retail_buy_pays_brokerage_stamp_and_gst() {
        let fees = FeeEngine::default();
        let c = fees.charges(Role::Retail, Side::Buy, 25_00, 10, 3, 0);
        assert_eq!(c.commission, 8);
        assert_eq!(c.stamp_duty, 4);
        assert_eq!(c.transaction_tax, 0);
        assert_eq!(c.gst, 2);
        assert_eq!(c.total(), 8 + 3 + 4 + 2);
    }

    #[test]
    fn retail_sell_pays_transaction_tax() {
        let c = FeeEngine::default().charges(Role::Retail, Side::Sell, 25_00, 10, 3, 0);
        assert_eq!(c.transaction_tax, 25);
        assert_eq!(c.stamp_duty, 0);
    }

    #[test]
    fn retail_commission_is_capped_per_order() {
        let fees = FeeEngine::default();
        let first = fees.charges(Role::Retail, Side::Buy, 100_00, 1_000, 0, 0);
        assert_eq!(first.commission, 20_00);
        let second = fees.charges(Role::Retail, Side::Buy, 100_00, 1_000, 0, 15_00);
        assert_eq!(second.commission, 5_00);
    }

    #[test]
    fn rebates_reduce_charges_and_are_not_taxed() {
        let c = FeeEngine::default().charges(Role::LpDesk, Side::Buy, 25_00, 100, -25, 0);
        assert_eq!(c.commission, 0);
        assert_eq!(c.gst, 0);
        assert_eq!(c.total(), -25 + 38);
    }

    #[test]
    fn reserve_is_an_upper_bound_for_any_fill() {
        let fees = FeeEngine::default();
        for role in [Role::Retail, Role::Quant, Role::Hft, Role::LpDesk] {
            for qty in [1, 7, 100, 999] {
                let reserve = fees.reserve(role, Side::Buy, 25_00, qty);
                let worst = fees.charges(role, Side::Buy, 25_00, qty, 30 * qty as i64 / 100 + 1, 0);
                assert!(reserve as i64 >= worst.total(), "{role:?} {qty}");
            }
        }
        assert_eq!(fees.reserve(Role::Retail, Side::Sell, 25_00, 10), 0);
    }

    #[test]
    fn issuers_pay_underwriting_commission() {
        let c = FeeEngine::default().charges(Role::Issuer, Side::Sell, 20_00, 1_000, -15, 0);
        assert_eq!(c.commission, 400_00);
    }
}
