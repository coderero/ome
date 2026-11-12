use crate::state::BotState;
use ome_core::price::Price;
use ome_core::types::{OrderId, OrderKind, Qty, Side, Timestamp};
use rand::Rng;
use rand::rngs::StdRng;
use std::collections::VecDeque;
use std::f64::consts::TAU;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Place {
        side: Side,
        kind: OrderKind,
        qty: Qty,
    },
    Cancel(OrderId),
    CancelAll,
}

pub trait Strategy: Send {
    fn name(&self) -> &'static str;
    fn interval(&self) -> Duration;
    fn on_tick(&mut self, state: &BotState, rng: &mut StdRng) -> Vec<Action>;
}

pub fn create(name: &str) -> Option<Box<dyn Strategy>> {
    Some(match name {
        "mm" | "market-maker" => Box::new(MarketMaker::default()),
        "momentum" => Box::new(Momentum::default()),
        "meanrev" | "mean-reversion" => Box::new(MeanReversion::default()),
        "noise" | "retail" => Box::new(NoiseTrader::default()),
        _ => return None,
    })
}

fn gaussian(rng: &mut StdRng) -> f64 {
    let u1: f64 = rng.random_range(f64::EPSILON..1.0);
    let u2: f64 = rng.random();
    (-2.0 * u1.ln()).sqrt() * (TAU * u2).cos()
}

fn scale(price: Price, bps: f64) -> Price {
    ((price as f64) * (1.0 + bps / 10_000.0)).round().max(1.0) as Price
}

fn affordable(cash: u64, price: Price, qty: Qty) -> Qty {
    cash.checked_div(price).map_or(0, |max| qty.min(max))
}

fn cancel_stale(state: &BotState, ttl: Timestamp) -> Vec<Action> {
    state
        .open_orders
        .iter()
        .filter(|(_, o)| state.now.saturating_sub(o.placed_at) > ttl)
        .map(|(id, _)| Action::Cancel(*id))
        .collect()
}

pub struct MarketMaker {
    pub half_spread_bps: f64,
    pub quote_size: Qty,
    pub max_inventory_skew: Qty,
    pub requote_bps: f64,
    target_inventory: Option<Qty>,
    quoted: Option<(Price, Price)>,
}

impl Default for MarketMaker {
    fn default() -> Self {
        Self {
            half_spread_bps: 12.0,
            quote_size: 25,
            max_inventory_skew: 400,
            requote_bps: 4.0,
            target_inventory: None,
            quoted: None,
        }
    }
}

impl Strategy for MarketMaker {
    fn name(&self) -> &'static str {
        "market-maker"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(120)
    }

    fn on_tick(&mut self, state: &BotState, rng: &mut StdRng) -> Vec<Action> {
        if !state.trading {
            self.quoted = None;
            return if state.open_orders.is_empty() {
                vec![]
            } else {
                vec![Action::CancelAll]
            };
        }
        if !state.pending.is_empty() {
            return vec![];
        }
        let Some(fair) = state.fair_price() else {
            return vec![];
        };

        let inventory = state.total_shares();
        let target = *self.target_inventory.get_or_insert(inventory.max(1));
        let imbalance = (inventory as f64 - target as f64) / self.max_inventory_skew as f64;
        let skew_bps = -imbalance.clamp(-1.0, 1.0) * self.half_spread_bps * 1.5;
        let jitter = gaussian(rng) * self.half_spread_bps * 0.15;

        let bid = scale(fair, -self.half_spread_bps + skew_bps + jitter);
        let ask = scale(fair, self.half_spread_bps + skew_bps + jitter).max(bid + 1);

        let both_live = state.open_orders.len() == 2;
        let close_enough = self.quoted.is_some_and(|(b, a)| {
            let moved =
                |old: Price, new: Price| (old as f64 - new as f64).abs() / new as f64 * 10_000.0;
            moved(b, bid) < self.requote_bps && moved(a, ask) < self.requote_bps
        });
        if both_live && close_enough {
            return vec![];
        }

        let size = (self.quote_size as f64 * rng.random_range(0.6..1.4))
            .round()
            .max(1.0) as Qty;
        let bid_qty = affordable(state.cash + state.cash_reserved, bid, size);
        let ask_qty = size.min(inventory);
        let mut actions = vec![Action::CancelAll];
        if bid_qty > 0 {
            actions.push(Action::Place {
                side: Side::Buy,
                kind: OrderKind::Limit(bid),
                qty: bid_qty,
            });
        }
        if ask_qty > 0 {
            actions.push(Action::Place {
                side: Side::Sell,
                kind: OrderKind::Limit(ask),
                qty: ask_qty,
            });
        }
        self.quoted = Some((bid, ask));
        actions
    }
}

pub struct Momentum {
    pub fast_alpha: f64,
    pub slow_alpha: f64,
    pub threshold_bps: f64,
    pub trade_size: Qty,
    pub max_position: Qty,
    fast: Option<f64>,
    slow: Option<f64>,
    cooldown: u32,
}

impl Default for Momentum {
    fn default() -> Self {
        Self {
            fast_alpha: 0.25,
            slow_alpha: 0.03,
            threshold_bps: 10.0,
            trade_size: 40,
            max_position: 600,
            fast: None,
            slow: None,
            cooldown: 0,
        }
    }
}

impl Strategy for Momentum {
    fn name(&self) -> &'static str {
        "momentum"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(250)
    }

    fn on_tick(&mut self, state: &BotState, rng: &mut StdRng) -> Vec<Action> {
        let Some(price) = state.last_price else {
            return vec![];
        };
        let p = price as f64;
        let fast = self.fast.map_or(p, |f| f + self.fast_alpha * (p - f));
        let slow = self.slow.map_or(p, |s| s + self.slow_alpha * (p - s));
        self.fast = Some(fast);
        self.slow = Some(slow);

        if self.cooldown > 0 {
            self.cooldown -= 1;
            return vec![];
        }
        if !state.trading || !state.pending.is_empty() {
            return vec![];
        }

        let signal_bps = (fast - slow) / slow * 10_000.0;
        let size = (self.trade_size as f64 * rng.random_range(0.5..1.5))
            .round()
            .max(1.0) as Qty;
        if signal_bps > self.threshold_bps && state.total_shares() < self.max_position {
            let qty = affordable(state.cash, scale(price, 1_100.0), size);
            if qty > 0 {
                self.cooldown = 6;
                return vec![Action::Place {
                    side: Side::Buy,
                    kind: OrderKind::Market,
                    qty,
                }];
            }
        }
        if signal_bps < -self.threshold_bps && state.shares > 0 {
            self.cooldown = 6;
            return vec![Action::Place {
                side: Side::Sell,
                kind: OrderKind::Market,
                qty: size.min(state.shares),
            }];
        }
        vec![]
    }
}

pub struct MeanReversion {
    pub window: usize,
    pub entry_z: f64,
    pub order_size: Qty,
    pub max_position: Qty,
    pub order_ttl: Timestamp,
}

impl Default for MeanReversion {
    fn default() -> Self {
        Self {
            window: 150,
            entry_z: 1.6,
            order_size: 30,
            max_position: 500,
            order_ttl: 6_000,
        }
    }
}

impl MeanReversion {
    fn z_score(&self, prices: &VecDeque<Price>) -> Option<f64> {
        if prices.len() < self.window / 3 {
            return None;
        }
        let window: Vec<f64> = prices
            .iter()
            .rev()
            .take(self.window)
            .map(|&p| p as f64)
            .collect();
        let n = window.len() as f64;
        let mean = window.iter().sum::<f64>() / n;
        let var = window.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / n;
        let sd = var.sqrt();
        (sd > 0.0).then(|| (window[0] - mean) / sd)
    }
}

impl Strategy for MeanReversion {
    fn name(&self) -> &'static str {
        "mean-reversion"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(500)
    }

    fn on_tick(&mut self, state: &BotState, _rng: &mut StdRng) -> Vec<Action> {
        let mut actions = cancel_stale(state, self.order_ttl);
        if !state.trading || !state.pending.is_empty() || !state.open_orders.is_empty() {
            return actions;
        }
        let Some(z) = self.z_score(&state.prices) else {
            return actions;
        };
        if z < -self.entry_z && state.total_shares() < self.max_position {
            if let Some(bid) = state.best_bid.or(state.last_price) {
                let qty = affordable(state.cash, bid, self.order_size);
                if qty > 0 {
                    actions.push(Action::Place {
                        side: Side::Buy,
                        kind: OrderKind::Limit(bid),
                        qty,
                    });
                }
            }
        } else if z > self.entry_z
            && state.shares > 0
            && let Some(ask) = state.best_ask.or(state.last_price)
        {
            let qty = self.order_size.min(state.shares);
            actions.push(Action::Place {
                side: Side::Sell,
                kind: OrderKind::Limit(ask),
                qty,
            });
        }
        actions
    }
}

pub struct NoiseTrader {
    pub activity: f64,
    pub max_size: Qty,
    pub order_ttl: Timestamp,
    sentiment: f64,
}

impl Default for NoiseTrader {
    fn default() -> Self {
        Self {
            activity: 0.6,
            max_size: 25,
            order_ttl: 8_000,
            sentiment: 0.0,
        }
    }
}

impl Strategy for NoiseTrader {
    fn name(&self) -> &'static str {
        "noise"
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(300)
    }

    fn on_tick(&mut self, state: &BotState, rng: &mut StdRng) -> Vec<Action> {
        self.sentiment = (self.sentiment * 0.985 + gaussian(rng) * 0.06).clamp(-1.0, 1.0);
        if rng.random_bool(0.0015) {
            self.sentiment = if rng.random_bool(0.5) { 1.0 } else { -1.0 };
        }

        let mut actions = cancel_stale(state, self.order_ttl);
        if !state.trading || state.pending.len() > 2 || !rng.random_bool(self.activity) {
            return actions;
        }
        let Some(fair) = state.fair_price() else {
            return actions;
        };

        let mut side = if rng.random_bool((0.5 + self.sentiment * 0.3).clamp(0.05, 0.95)) {
            Side::Buy
        } else {
            Side::Sell
        };
        let qty = rng.random_range(1..=self.max_size);
        if side == Side::Sell && state.shares < qty {
            side = Side::Buy;
        }
        let market = rng.random_bool(0.55);
        let price = match side {
            Side::Buy => scale(fair, -rng.random_range(0.0..35.0)),
            Side::Sell => scale(fair, rng.random_range(0.0..35.0)),
        };
        let cost_basis = if market { scale(fair, 1_100.0) } else { price };
        if side == Side::Buy && affordable(state.cash, cost_basis, qty) < qty {
            return actions;
        }
        let kind = if market {
            OrderKind::Market
        } else {
            OrderKind::Limit(price)
        };
        actions.push(Action::Place { side, kind, qty });
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::OpenOrder;
    use crate::state::tests::state_at;
    use rand::SeedableRng;

    fn rng() -> StdRng {
        StdRng::seed_from_u64(1)
    }

    fn places(actions: &[Action]) -> Vec<(Side, OrderKind, Qty)> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Place { side, kind, qty } => Some((*side, *kind, *qty)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn market_maker_quotes_both_sides_around_fair() {
        let mut mm = MarketMaker::default();
        let state = state_at(10_000, 10_000_000, 500);

        let actions = mm.on_tick(&state, &mut rng());

        assert_eq!(actions[0], Action::CancelAll);
        let quotes = places(&actions);
        assert_eq!(quotes.len(), 2);
        let (OrderKind::Limit(bid), OrderKind::Limit(ask)) = (quotes[0].1, quotes[1].1) else {
            panic!("expected limits");
        };
        assert!(bid < 10_000 && ask > 10_000 && ask > bid);
    }

    #[test]
    fn market_maker_skews_quotes_against_inventory() {
        let mut long = MarketMaker {
            target_inventory: Some(100),
            ..MarketMaker::default()
        };
        let mut flat = MarketMaker {
            target_inventory: Some(500),
            ..MarketMaker::default()
        };
        let state = state_at(10_000, 10_000_000, 500);

        let mid = |actions: &[Action]| {
            let q = places(actions);
            match (q[0].1, q[1].1) {
                (OrderKind::Limit(b), OrderKind::Limit(a)) => (b + a) / 2,
                _ => unreachable!(),
            }
        };
        assert!(mid(&long.on_tick(&state, &mut rng())) < mid(&flat.on_tick(&state, &mut rng())));
    }

    #[test]
    fn market_maker_pulls_quotes_when_halted() {
        let mut mm = MarketMaker::default();
        let mut state = state_at(10_000, 10_000_000, 500);
        state.trading = false;
        state.open_orders.insert(
            1,
            OpenOrder {
                side: Side::Buy,
                price: 9_990,
                leaves: 5,
                placed_at: 0,
            },
        );

        assert_eq!(mm.on_tick(&state, &mut rng()), vec![Action::CancelAll]);
    }

    #[test]
    fn momentum_buys_breakouts_and_sells_breakdowns() {
        let mut bot = Momentum::default();
        let mut state = state_at(10_000, 10_000_000, 0);
        let mut r = rng();
        bot.on_tick(&state, &mut r);
        let mut bought = false;
        for step in 1..20 {
            state.last_price = Some(10_000 + step * 40);
            bought |= places(&bot.on_tick(&state, &mut r))
                .iter()
                .any(|p| p.0 == Side::Buy);
        }
        assert!(bought);

        state.shares = 100;
        let mut sold = false;
        for step in 1..40 {
            state.last_price = Some(10_800 - step * 60);
            sold |= places(&bot.on_tick(&state, &mut r))
                .iter()
                .any(|p| p.0 == Side::Sell);
        }
        assert!(sold);
    }

    #[test]
    fn mean_reversion_buys_dips() {
        let mut bot = MeanReversion::default();
        let mut state = state_at(10_000, 10_000_000, 0);
        for i in 0..100 {
            state.prices.push_back(10_000 + (i % 5) * 2);
        }
        state.prices.push_back(9_900);
        state.last_price = Some(9_900);

        let orders = places(&bot.on_tick(&state, &mut rng()));
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].0, Side::Buy);
    }

    #[test]
    fn strategies_cancel_stale_orders() {
        let mut bot = NoiseTrader {
            activity: 0.0,
            ..NoiseTrader::default()
        };
        let mut state = state_at(10_000, 0, 0);
        state.now = 100_000;
        state.open_orders.insert(
            7,
            OpenOrder {
                side: Side::Buy,
                price: 9_990,
                leaves: 5,
                placed_at: 0,
            },
        );

        assert_eq!(bot.on_tick(&state, &mut rng()), vec![Action::Cancel(7)]);
    }

    #[test]
    fn noise_trader_never_sells_shares_it_does_not_have() {
        let mut bot = NoiseTrader {
            activity: 1.0,
            ..NoiseTrader::default()
        };
        let state = state_at(10_000, 100_000_000, 0);
        let mut r = rng();
        for _ in 0..500 {
            for (side, _, _) in places(&bot.on_tick(&state, &mut r)) {
                assert_eq!(side, Side::Buy);
            }
        }
    }

    #[test]
    fn every_named_strategy_exists() {
        for name in ["mm", "momentum", "meanrev", "noise"] {
            assert!(create(name).is_some());
        }
        assert!(create("nope").is_none());
    }
}
