use ome_core::types::Qty;
use ome_protocol::MemberKind;

pub const PER_SHARE_SCALE: i64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub maker_per_share: i64,
    pub taker_per_share: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeSchedule {
    pub broker: Rate,
    pub liquidity_provider: Rate,
}

impl Default for FeeSchedule {
    fn default() -> Self {
        Self {
            broker: Rate {
                maker_per_share: -15,
                taker_per_share: 30,
            },
            liquidity_provider: Rate {
                maker_per_share: -25,
                taker_per_share: 30,
            },
        }
    }
}

impl FeeSchedule {
    pub fn fee(&self, kind: MemberKind, maker: bool, qty: Qty) -> i64 {
        let rate = match kind {
            MemberKind::LiquidityProvider => self.liquidity_provider,
            MemberKind::Broker | MemberKind::Admin => self.broker,
        };
        let per_share = if maker {
            rate.maker_per_share
        } else {
            rate.taker_per_share
        };
        let raw = per_share * qty as i64;
        let half = PER_SHARE_SCALE / 2;
        if raw >= 0 {
            (raw + half) / PER_SHARE_SCALE
        } else {
            (raw - half) / PER_SHARE_SCALE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takers_pay_and_makers_earn_rebates_per_share() {
        let fees = FeeSchedule::default();
        assert_eq!(fees.fee(MemberKind::Broker, false, 100), 30);
        assert_eq!(fees.fee(MemberKind::Broker, true, 100), -15);
        assert_eq!(fees.fee(MemberKind::LiquidityProvider, true, 100), -25);
        assert_eq!(fees.fee(MemberKind::Broker, false, 1), 0);
        assert_eq!(fees.fee(MemberKind::Broker, false, 5), 2);
        assert_eq!(fees.fee(MemberKind::LiquidityProvider, true, 2), -1);
    }
}
