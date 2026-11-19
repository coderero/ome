use crate::state::{AgentState, Market};
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{Qty, Side};

pub fn half_spread_bps(market: &Market) -> Option<f64> {
    let (bid, ask) = (market.bid? as f64, market.ask? as f64);
    let mid = (bid + ask) / 2.0;
    (mid > 0.0).then(|| (ask - bid) / 2.0 / mid * 10_000.0)
}

pub fn impact_bps(market: &Market, side: Side, qty: Qty) -> Option<f64> {
    let mid = market.mid()? as f64;
    let levels = match side {
        Side::Buy => &market.asks,
        Side::Sell => &market.bids,
    };
    let (mut left, mut paid) = (qty as f64, 0.0);
    let mut last = levels.first()?.price as f64;
    for level in levels {
        let take = left.min(level.qty as f64);
        paid += take * level.price as f64;
        left -= take;
        last = level.price as f64;
        if left <= 0.0 {
            break;
        }
    }
    if left > 0.0 {
        let shown: f64 = levels.iter().map(|l| l.qty as f64).sum();
        let reach = (last - mid).abs().max(mid * 0.000_1);
        let beyond = reach * (1.0 + left / shown.max(1.0));
        paid += left * if side == Side::Buy { last + beyond } else { (last - beyond).max(1.0) };
    }
    let average = paid / qty.max(1) as f64;
    Some(((average - mid) / mid * 10_000.0 * if side == Side::Buy { 1.0 } else { -1.0 }).max(0.0))
}

pub fn taking_cost_bps(state: &AgentState, symbol: Symbol, side: Side, qty: Qty, fee_bps: f64) -> Option<f64> {
    Some(impact_bps(state.market(symbol)?, side, qty)? + fee_bps)
}

pub fn worth_taking(state: &AgentState, symbol: Symbol, side: Side, qty: Qty, edge_bps: f64, fee_bps: f64, hurdle_bps: f64) -> bool {
    taking_cost_bps(state, symbol, side, qty, fee_bps).is_some_and(|cost| edge_bps > cost + hurdle_bps)
}

pub fn size_within(state: &AgentState, symbol: Symbol, side: Side, wanted: Qty, edge_bps: f64, fee_bps: f64, hurdle_bps: f64) -> Qty {
    if wanted == 0 || !worth_taking(state, symbol, side, 1, edge_bps, fee_bps, hurdle_bps) {
        return 0;
    }
    let (mut low, mut high) = (1, wanted);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if worth_taking(state, symbol, side, middle, edge_bps, fee_bps, hurdle_bps) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low
}

pub fn limit_for(value: f64, side: Side, keep_bps: f64) -> Price {
    let factor = keep_bps / 10_000.0;
    match side {
        Side::Buy => (value / (1.0 + factor)).floor().max(1.0) as Price,
        Side::Sell => (value * (1.0 + factor)).ceil().max(1.0) as Price,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{acme, state_with};
    use ome_core::order_book::Level;

    fn deep(state: &mut AgentState) {
        let market = state.markets.get_mut(&acme()).unwrap();
        let level = |price, qty| Level { price, qty, orders: 1 };
        market.asks = vec![level(10_005, 100), level(10_030, 300), level(10_100, 600)];
        market.bids = vec![level(9_995, 100), level(9_970, 300), level(9_900, 600)];
    }

    #[test]
    fn the_cost_of_a_trade_is_read_from_the_book_for_its_size() {
        let mut state = state_with(&[(acme(), 10_000)], 0);
        deep(&mut state);
        let market = state.market(acme()).unwrap();
        assert!((half_spread_bps(market).unwrap() - 5.0).abs() < 1e-9);
        assert!((impact_bps(market, Side::Buy, 50).unwrap() - 5.0).abs() < 1e-9, "inside the touch it pays half the spread");
        let walk = impact_bps(market, Side::Buy, 400).unwrap();
        assert!(walk > 20.0 && walk < 30.0, "walking the second level costs more: {walk}");
        let beyond = impact_bps(market, Side::Sell, 3_000).unwrap();
        assert!(beyond > 100.0, "an order larger than the visible book pays past the last level: {beyond}");
        assert!(worth_taking(&state, acme(), Side::Buy, 50, 20.0, 5.0, 5.0));
        assert!(!worth_taking(&state, acme(), Side::Buy, 400, 20.0, 5.0, 5.0), "the same idea is not worth it in size");
        let sized = size_within(&state, acme(), Side::Buy, 1_000, 20.0, 5.0, 5.0);
        assert!((100..400).contains(&sized), "it trades as much as the edge pays for: {sized}");
        assert_eq!(size_within(&state, acme(), Side::Buy, 1_000, 8.0, 5.0, 5.0), 0);
        assert_eq!((limit_for(10_000.0, Side::Buy, 100.0), limit_for(10_000.0, Side::Sell, 100.0)), (9_900, 10_100));
    }
}
