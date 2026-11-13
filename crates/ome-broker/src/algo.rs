use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, OrderKind, OrderOptions, Qty, Side, TimeInForce, Timestamp};
use ome_protocol::{AlgoReport, AlgoStatus, AlgoStrategy};

const VWAP_SLICES: u64 = 20;
const POV_CHECK_MS: Timestamp = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Algo {
    pub algo_id: u64,
    pub account: AccountId,
    pub client_order_id: u64,
    pub symbol: Symbol,
    pub side: Side,
    pub qty: Qty,
    pub limit: Option<Price>,
    pub strategy: AlgoStrategy,
    pub started_at: Timestamp,
    pub volume_base: Qty,
    pub filled: Qty,
    pub notional: u128,
    pub children: u64,
    pub child: Option<u64>,
}

pub fn vwap_curve(x_permille: u64) -> u64 {
    let x = x_permille.min(1_000) as i128;
    let centered = 2 * x - 1_000;
    let cubic = centered * centered * centered / 1_000_000;
    ((3 * x + cubic + 1_000) / 5) as u64
}

impl Algo {
    pub fn remaining(&self) -> Qty {
        self.qty - self.filled
    }

    pub fn end_at(&self) -> Option<Timestamp> {
        match self.strategy {
            AlgoStrategy::Twap { duration_ms, .. }
            | AlgoStrategy::Vwap { duration_ms }
            | AlgoStrategy::Pov { duration_ms, .. } => Some(self.started_at + duration_ms),
            AlgoStrategy::Iceberg { .. } => None,
        }
    }

    pub fn expired(&self, now: Timestamp) -> bool {
        self.end_at().is_some_and(|end| now >= end)
    }

    fn slice_target(&self, now: Timestamp, slices: u64, curve: impl Fn(u64) -> u64) -> Qty {
        let duration = self.end_at().unwrap_or(self.started_at) - self.started_at;
        let slices = slices.max(1);
        let interval = (duration / slices).max(1);
        let step = (now.saturating_sub(self.started_at) / interval + 1).min(slices);
        let fraction = curve(step * 1_000 / slices);
        (self.qty as u128 * fraction as u128 / 1_000) as Qty
    }

    pub fn target(&self, now: Timestamp, market_volume: Qty) -> Qty {
        let target = match self.strategy {
            AlgoStrategy::Twap { slices, .. } => self.slice_target(now, slices, |x| x),
            AlgoStrategy::Vwap { .. } => self.slice_target(now, VWAP_SLICES, vwap_curve),
            AlgoStrategy::Pov { rate_bps, .. } => {
                let others = market_volume
                    .saturating_sub(self.volume_base)
                    .saturating_sub(self.filled);
                let rate = rate_bps.min(9_999) as u128;
                (others as u128 * rate / (10_000 - rate)) as Qty
            }
            AlgoStrategy::Iceberg { .. } => self.qty,
        };
        target.min(self.qty)
    }

    pub fn next_child(
        &self,
        now: Timestamp,
        market_volume: Qty,
    ) -> Option<(OrderKind, OrderOptions, Qty)> {
        if self.child.is_some() || self.filled >= self.qty {
            return None;
        }
        let wanted = self.target(now, market_volume).saturating_sub(self.filled);
        let minimum = match self.strategy {
            AlgoStrategy::Pov { .. } => (self.qty / 100).max(1).min(self.remaining()),
            _ => 1,
        };
        if wanted < minimum {
            return None;
        }
        if let AlgoStrategy::Iceberg { clip } = self.strategy {
            return Some((
                OrderKind::Limit(self.limit?),
                OrderOptions::tif(TimeInForce::Gtc),
                clip.min(wanted),
            ));
        }
        let kind = self.limit.map_or(OrderKind::Market, OrderKind::Limit);
        Some((kind, OrderOptions::tif(TimeInForce::Ioc), wanted))
    }

    pub fn check_interval(&self) -> Timestamp {
        match self.strategy {
            AlgoStrategy::Pov { .. } => POV_CHECK_MS,
            _ => 0,
        }
    }

    pub fn report(&self, status: AlgoStatus, reason: &str) -> AlgoReport {
        AlgoReport {
            algo_id: self.algo_id,
            client_order_id: self.client_order_id,
            account: self.account,
            symbol: self.symbol,
            side: self.side,
            qty: self.qty,
            filled: self.filled,
            avg_price: if self.filled == 0 {
                0
            } else {
                (self.notional / self.filled as u128) as Price
            },
            children: self.children,
            status,
            strategy: self.strategy,
            reason: reason.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn algo(strategy: AlgoStrategy) -> Algo {
        Algo {
            algo_id: 1,
            account: 1,
            client_order_id: 0,
            symbol: Symbol::parse("ACME").unwrap(),
            side: Side::Buy,
            qty: 1_000,
            limit: None,
            strategy,
            started_at: 10_000,
            volume_base: 5_000,
            filled: 0,
            notional: 0,
            children: 0,
            child: None,
        }
    }

    #[test]
    fn twap_releases_equal_slices_one_interval_apart() {
        let a = algo(AlgoStrategy::Twap {
            duration_ms: 10_000,
            slices: 4,
        });
        assert_eq!(a.target(10_000, 0), 250);
        assert_eq!(a.target(12_499, 0), 250);
        assert_eq!(a.target(12_500, 0), 500);
        assert_eq!(a.target(19_999, 0), 1_000);
        assert_eq!(a.target(99_999, 0), 1_000);
        assert!(!a.expired(19_999));
        assert!(a.expired(20_000));
        let (kind, options, qty) = a.next_child(10_000, 0).unwrap();
        assert_eq!(
            (kind, options.tif, qty),
            (OrderKind::Market, TimeInForce::Ioc, 250)
        );
    }

    #[test]
    fn vwap_front_and_back_loads_like_an_intraday_volume_curve() {
        assert_eq!(vwap_curve(0), 0);
        assert_eq!(vwap_curve(1_000), 1_000);
        assert_eq!(vwap_curve(500), 500);
        let first_tenth = vwap_curve(100);
        let middle_tenth = vwap_curve(550) - vwap_curve(450);
        assert!(
            first_tenth > 2 * middle_tenth,
            "{first_tenth} vs {middle_tenth}"
        );
        let a = algo(AlgoStrategy::Vwap {
            duration_ms: 20_000,
        });
        assert!(
            a.target(10_000, 0) > 50,
            "first slice is heavier than an even 1/20"
        );
    }

    #[test]
    fn pov_follows_other_participants_volume() {
        let mut a = algo(AlgoStrategy::Pov {
            rate_bps: 2_000,
            duration_ms: 60_000,
        });
        assert_eq!(a.target(11_000, 5_000), 0);
        assert_eq!(a.next_child(11_000, 5_005), None, "below the minimum clip");
        assert_eq!(a.target(11_000, 5_400), 100);
        a.filled = 100;
        assert_eq!(a.target(12_000, 5_500), 100);
        assert_eq!(a.target(12_000, 6_100), 250);
        assert_eq!(a.target(12_000, 100_000), 1_000);
    }

    #[test]
    fn iceberg_slicer_rests_clips_at_the_limit_one_at_a_time() {
        let mut a = algo(AlgoStrategy::Iceberg { clip: 300 });
        assert_eq!(a.next_child(0, 0), None, "an iceberg needs a limit price");
        a.limit = Some(25_00);
        assert_eq!(
            a.next_child(0, 0),
            Some((
                OrderKind::Limit(25_00),
                OrderOptions::tif(TimeInForce::Gtc),
                300
            ))
        );
        a.child = Some(9);
        assert_eq!(a.next_child(0, 0), None);
        a.child = None;
        a.filled = 900;
        assert_eq!(a.next_child(0, 0).unwrap().2, 100);
        assert!(!a.expired(u64::MAX));
    }
}
