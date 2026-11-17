use super::{Params, Strategy, affordable, gaussian, scale};
use crate::action::Action;
use ome_core::types::{OrderKind, Side};
use crate::state::AgentState;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::Qty;
use rand::Rng;
use rand::rngs::StdRng;
use std::collections::BTreeMap;
use std::time::Duration;

const OBLIGATION_HALF_SPREAD_BPS: f64 = 90.0;
const OBLIGATION_SIZE: Qty = 100;

#[derive(Default)]
struct Book {
    quoted: Option<(Price, Price)>,
    target: Option<Qty>,
}

pub struct MarketMaker {
    symbols: Vec<Symbol>,
    pub gamma: f64,
    pub base_spread_bps: f64,
    pub min_spread_bps: f64,
    pub max_spread_bps: f64,
    pub size: Qty,
    pub size_pct: f64,
    pub requote_bps: f64,
    pub obligation: bool,
    pub levels: u64,
    pub level_gap_bps: f64,
    books: BTreeMap<Symbol, Book>,
    mmp_armed: bool,
    cooldown: u32,
}

impl MarketMaker {
    pub fn new(symbols: Vec<Symbol>, params: &Params, obligation: bool) -> Self {
        Self {
            symbols,
            gamma: params.f64("gamma", 0.5),
            base_spread_bps: params.f64("spread_bps", 20.0),
            min_spread_bps: params.f64("min_spread_bps", 6.0),
            max_spread_bps: params.f64("max_spread_bps", 160.0),
            size: params.u64("size", 100),
            size_pct: params.f64("size_pct", 0.01),
            requote_bps: params.f64("requote_bps", 4.0),
            obligation,
            levels: params.u64("levels", 3).clamp(1, 8),
            level_gap_bps: params.f64("level_gap_bps", 0.0),
            books: BTreeMap::new(),
            mmp_armed: false,
            cooldown: 0,
        }
    }

    pub fn half_spread_bps(&self, volatility_bps: f64) -> f64 {
        let half = (self.base_spread_bps / 2.0 + self.gamma * volatility_bps)
            .clamp(self.min_spread_bps / 2.0, self.max_spread_bps / 2.0);
        if self.obligation {
            half.min(OBLIGATION_HALF_SPREAD_BPS)
        } else {
            half
        }
    }

    fn protection(&self) -> Action {
        Action::SetMmp {
            window_ms: 2_000,
            max_qty: self.size * 8 * self.symbols.len().max(1) as Qty,
        }
    }
}

impl Strategy for MarketMaker {
    fn name(&self) -> &'static str {
        if self.obligation {
            "designated market maker"
        } else {
            "market maker"
        }
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(150)
    }

    fn on_tick(&mut self, state: &AgentState, rng: &mut StdRng) -> Vec<Action> {
        if !self.mmp_armed {
            self.mmp_armed = true;
            return vec![self.protection()];
        }
        if state.mmp_tripped {
            for book in self.books.values_mut() {
                book.quoted = None;
            }
            if self.cooldown == 0 {
                self.cooldown = 20;
            }
            self.cooldown -= 1;
            return if self.cooldown == 0 {
                vec![self.protection()]
            } else {
                vec![]
            };
        }
        if state.busy() {
            return vec![];
        }
        let budget = state.cash / self.symbols.len().max(1) as u64;
        let mut actions = Vec::new();
        for symbol in self.symbols.clone() {
            let open = state.open_in(symbol);
            let Some(market) = state.market(symbol) else {
                continue;
            };
            let volatility = market.volatility_bps(100).unwrap_or(5.0);
            let drought = state.world.drought;
            let half = self.half_spread_bps(volatility) * if drought { 4.0 } else { 1.0 };
            let book = self.books.entry(symbol).or_default();
            if !state.trading(symbol) {
                if open > 0 || book.quoted.is_some() {
                    book.quoted = None;
                    actions.push(Action::Quote {
                        symbol,
                        bid: None,
                        ask: None,
                    });
                }
                continue;
            }
            let Some(fair) = market.mid().or(market.fair()) else {
                continue;
            };
            let held = state.holding(symbol);
            let inventory = held.total();
            let target = *book.target.get_or_insert(inventory.max(1));
            let lots = (inventory as f64 - target as f64) / self.size.max(1) as f64;
            let skew = (-lots * self.gamma * volatility.max(2.0)).clamp(-half, half);
            let jitter = gaussian(rng) * half * 0.1;
            let bid = scale(fair, -half + skew + jitter);
            let ask = scale(fair, half + skew + jitter).max(bid + 1);

            let close_enough = book.quoted.is_some_and(|(b, a)| {
                let moved = |old: Price, new: Price| {
                    (old as f64 - new as f64).abs() / new as f64 * 10_000.0
                };
                moved(b, bid) < self.requote_bps && moved(a, ask) < self.requote_bps
            });
            if open >= 2 && close_enough {
                continue;
            }
            let old = book.quoted;
            for (id, order) in state.open_orders.iter().filter(|(_, o)| o.symbol == symbol) {
                let quote_leg = old.is_some_and(|(b, a)| order.price == b || order.price == a);
                if !quote_leg {
                    actions.push(Action::Cancel(*id));
                }
            }
            let capital = (state.equity() as f64 * self.size_pct / fair.max(1) as f64) as Qty;
            let mut size = (self.size.max(capital) as f64 * rng.random_range(0.7..1.3)).round() as Qty;
            if drought {
                size = (size / 4).max(1);
            } else if self.obligation {
                size = size.max(OBLIGATION_SIZE);
            }
            let lean = (lots * 0.25).clamp(-0.8, 0.8);
            let bid_size = ((size as f64 * (1.0 - lean)).round() as Qty).max(1);
            let ask_size = ((size as f64 * (1.0 + lean)).round() as Qty).max(1);
            let bid_qty = affordable(budget, bid, bid_size);
            let ask_qty = ask_size.min(held.available);
            book.quoted = Some((bid, ask));
            actions.push(Action::Quote {
                symbol,
                bid: (bid_qty > 0).then_some((bid, bid_qty)),
                ask: (ask_qty > 0).then_some((ask, ask_qty)),
            });
            let gap = if self.level_gap_bps > 0.0 { self.level_gap_bps } else { (half * 1.5).max(15.0) };
            let (mut cash_left, mut shares_left) = (budget.saturating_sub(bid.saturating_mul(bid_qty)), held.available.saturating_sub(ask_qty));
            for level in 1..self.levels {
                let depth = 1.0 + 0.5 * level as f64;
                let away = gap * level as f64;
                let bid_level = scale(bid, -away);
                let ask_level = scale(ask, away);
                let qty = |base: Qty| ((base as f64 * depth).round() as Qty).max(1);
                let buy = affordable(cash_left, bid_level, qty(bid_size));
                if buy > 0 && market.ask.is_none_or(|a| bid_level < a) {
                    cash_left -= bid_level * buy;
                    actions.push(Action::Place { symbol, side: Side::Buy, kind: OrderKind::Limit(bid_level), qty: buy });
                }
                let sell = qty(ask_size).min(shares_left);
                if sell > 0 && market.bid.is_none_or(|b| ask_level > b) {
                    shares_left -= sell;
                    actions.push(Action::Place { symbol, side: Side::Sell, kind: OrderKind::Limit(ask_level), qty: sell });
                }
            }
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{acme, hold, nova, state_with};
    use rand::SeedableRng;

    type QuoteLeg = Option<(Price, Qty)>;

    fn quotes(actions: &[Action]) -> Vec<(Symbol, QuoteLeg, QuoteLeg)> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Quote { symbol, bid, ask } => Some((*symbol, *bid, *ask)),
                _ => None,
            })
            .collect()
    }

    fn armed(mm: &mut MarketMaker, state: &crate::state::AgentState, rng: &mut StdRng) {
        assert!(matches!(mm.on_tick(state, rng)[0], Action::SetMmp { .. }));
    }

    #[test]
    fn quote_size_follows_capital_and_leans_against_inventory() {
        let mut rng = StdRng::seed_from_u64(5);
        let mut state = state_with(&[(acme(), 10_000)], 5_000_000_00);
        hold(&mut state, acme(), 4_000);
        let mut mm = MarketMaker::new(vec![acme()], &Params::default(), false);
        armed(&mut mm, &state, &mut rng);
        let (_, bid, ask) = quotes(&mm.on_tick(&state, &mut rng))[0];
        let (bid, ask) = (bid.unwrap().1, ask.unwrap().1);
        assert!(bid > 300 && ask > 300, "a well funded desk quotes more than the minimum: {bid} x {ask}");
        hold(&mut state, acme(), 6_000);
        let (_, bid, ask) = quotes(&mm.on_tick(&state, &mut rng))[0];
        let (bid, ask) = (bid.unwrap().1, ask.unwrap().1);
        assert!(bid * 2 < ask, "long inventory: it bids for less and offers more: {bid} x {ask}");
    }

    #[test]
    fn quotes_every_symbol_around_fair_and_skews_away_from_inventory() {
        let mut rng = StdRng::seed_from_u64(1);
        let mut state = state_with(&[(acme(), 10_000), (nova(), 5_000)], 10_000_000);
        hold(&mut state, acme(), 1_000);
        hold(&mut state, nova(), 1_000);
        let mut mm = MarketMaker::new(vec![acme(), nova()], &Params::default(), false);
        armed(&mut mm, &state, &mut rng);
        let first = quotes(&mm.on_tick(&state, &mut rng));
        assert_eq!(first.len(), 2);
        let (_, bid, ask) = first[0];
        assert!(bid.unwrap().0 < 10_000 && ask.unwrap().0 > 10_000);

        hold(&mut state, acme(), 1_600);
        let mut skewed = MarketMaker::new(vec![acme()], &Params::default(), false);
        armed(&mut skewed, &state, &mut rng);
        skewed.books.entry(acme()).or_default().target = Some(1_000);
        let (_, bid, ask) = quotes(&skewed.on_tick(&state, &mut rng))[0];
        let mid = (bid.unwrap().0 + ask.unwrap().0) / 2;
        assert!(mid < 10_000, "long inventory lowers both quotes: {mid}");
    }

    #[test]
    fn deeper_levels_rest_behind_the_quote_and_are_replaced_on_a_requote() {
        let mut rng = StdRng::seed_from_u64(9);
        let mut state = state_with(&[(acme(), 10_000)], 50_000_000_00);
        hold(&mut state, acme(), 50_000);
        let mut mm = MarketMaker::new(vec![acme()], &Params::default(), false);
        armed(&mut mm, &state, &mut rng);
        let actions = mm.on_tick(&state, &mut rng);
        let (_, bid, ask) = quotes(&actions)[0];
        let ladder: Vec<(Side, Price, Qty)> = actions
            .iter()
            .filter_map(|a| match a {
                Action::Place { side, kind: OrderKind::Limit(p), qty, .. } => Some((*side, *p, *qty)),
                _ => None,
            })
            .collect();
        assert_eq!(ladder.len(), 4, "three levels: the quote plus two resting levels a side");
        let bids: Vec<_> = ladder.iter().filter(|l| l.0 == Side::Buy).collect();
        assert!(bids[0].1 < bid.unwrap().0 && bids[1].1 < bids[0].1, "deeper bids sit lower");
        assert!(bids[1].2 > bids[0].2 && bids[0].2 >= bid.unwrap().1, "and carry more size");
        assert!(ladder.iter().filter(|l| l.0 == Side::Sell).all(|l| l.1 > ask.unwrap().0));
        state.open_orders.insert(77, crate::state::OpenOrder { symbol: acme(), side: Side::Buy, price: bids[0].1, leaves: 10, placed_at: 0 });
        state.markets.get_mut(&acme()).unwrap().prices.push_back(10_400);
        state.markets.get_mut(&acme()).unwrap().bid = Some(10_390);
        state.markets.get_mut(&acme()).unwrap().ask = Some(10_410);
        let requote = mm.on_tick(&state, &mut rng);
        assert!(requote.contains(&Action::Cancel(77)), "the old ladder is pulled when the quote moves");
        let flat = MarketMaker::new(vec![acme()], &Params([("levels".to_string(), "1".to_string())].into()), false);
        assert_eq!(flat.levels, 1);
    }

    #[test]
    fn volatility_widens_the_spread_but_the_obligation_caps_it() {
        let wide = Params([("max_spread_bps".to_string(), "400".to_string())].into());
        let free = MarketMaker::new(vec![acme()], &wide, false);
        let dmm = MarketMaker::new(vec![acme()], &wide, true);
        assert!(free.half_spread_bps(60.0) > free.half_spread_bps(5.0));
        assert!(free.half_spread_bps(500.0) > 90.0);
        assert!(dmm.half_spread_bps(500.0) <= 90.0);
    }

    #[test]
    fn designated_maker_quotes_round_lots_and_pulls_quotes_when_trading_stops() {
        let mut rng = StdRng::seed_from_u64(2);
        let mut state = state_with(&[(acme(), 10_000)], 10_000_000);
        hold(&mut state, acme(), 5_000);
        let params = Params([("size".to_string(), "20".to_string())].into());
        let mut dmm = MarketMaker::new(vec![acme()], &params, true);
        armed(&mut dmm, &state, &mut rng);
        let (_, bid, ask) = quotes(&dmm.on_tick(&state, &mut rng))[0];
        assert!(bid.unwrap().1 >= 100 && ask.unwrap().1 >= 100);
        assert!(ask.unwrap().0 - bid.unwrap().0 <= 200);
        state.markets.get_mut(&acme()).unwrap().trading = false;
        assert_eq!(
            quotes(&dmm.on_tick(&state, &mut rng)),
            vec![(acme(), None, None)]
        );
    }
}
