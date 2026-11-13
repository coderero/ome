use crate::price::Price;
use crate::types::{AccountId, Order, OrderId, Qty, Side, TimeInForce, Trade};
use std::collections::{BTreeMap, HashMap, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    pub price: Price,
    pub qty: Qty,
    pub orders: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Indication {
    pub price: Price,
    pub volume: Qty,
    pub imbalance: i64,
}

#[derive(Debug, Default)]
pub struct Execution {
    pub trades: Vec<Trade>,
    pub rested: Qty,
    pub expired: Qty,
    pub self_trade_cancelled: Vec<Order>,
}

#[derive(Default)]
pub struct OrderBook {
    bids: BTreeMap<Price, VecDeque<Order>>,
    asks: BTreeMap<Price, VecDeque<Order>>,
    index: HashMap<OrderId, (Side, Price)>,
}

impl OrderBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn submit(&mut self, mut incoming: Order, tif: TimeInForce) -> Execution {
        let mut execution = Execution::default();

        while incoming.qty > 0 {
            let best_level = match incoming.side {
                Side::Buy => self.asks.first_entry(),
                Side::Sell => self.bids.last_entry(),
            };
            let Some(mut level) = best_level else {
                break;
            };

            let level_price = *level.key();
            let crosses = match incoming.side {
                Side::Buy => level_price <= incoming.price,
                Side::Sell => level_price >= incoming.price,
            };
            if !crosses {
                break;
            }

            let queue = level.get_mut();
            while incoming.qty > 0 {
                let Some(resting) = queue.front_mut() else {
                    break;
                };

                if resting.account == incoming.account {
                    if let Some(cancelled) = queue.pop_front() {
                        self.index.remove(&cancelled.id);
                        execution.self_trade_cancelled.push(cancelled);
                    }
                    continue;
                }

                let qty = incoming.qty.min(resting.qty);
                incoming.qty -= qty;
                resting.qty -= qty;

                execution.trades.push(Trade {
                    maker_order: resting.id,
                    taker_order: incoming.id,
                    maker_account: resting.account,
                    taker_account: incoming.account,
                    taker_side: incoming.side,
                    price: level_price,
                    qty,
                    timestamp: incoming.timestamp,
                });

                if resting.qty == 0 {
                    let filled_id = resting.id;
                    queue.pop_front();
                    self.index.remove(&filled_id);
                }
            }

            if queue.is_empty() {
                level.remove();
            }
        }

        if incoming.qty > 0 {
            match tif {
                TimeInForce::Gtc | TimeInForce::Day => {
                    execution.rested = incoming.qty;
                    self.index
                        .insert(incoming.id, (incoming.side, incoming.price));
                    self.levels_mut(incoming.side)
                        .entry(incoming.price)
                        .or_default()
                        .push_back(incoming);
                }
                TimeInForce::Ioc => execution.expired = incoming.qty,
            }
        }
        execution
    }

    pub fn rest(&mut self, order: Order) {
        self.index.insert(order.id, (order.side, order.price));
        self.levels_mut(order.side)
            .entry(order.price)
            .or_default()
            .push_back(order);
    }

    pub fn indicative(&self, reference: Price) -> Option<Indication> {
        let (best_bid, best_ask) = (self.best_bid()?, self.best_ask()?);
        if best_bid < best_ask {
            return None;
        }
        let candidates: std::collections::BTreeSet<Price> = self
            .bids
            .range(best_ask..=best_bid)
            .chain(self.asks.range(best_ask..=best_bid))
            .map(|(p, _)| *p)
            .collect();
        let volume = |levels: &BTreeMap<Price, VecDeque<Order>>,
                      range: (std::ops::Bound<Price>, std::ops::Bound<Price>)|
         -> Qty {
            levels
                .range(range)
                .flat_map(|(_, q)| q.iter())
                .map(|o| o.qty)
                .sum()
        };
        candidates
            .into_iter()
            .map(|price| {
                use std::ops::Bound::{Included, Unbounded};
                let buy = volume(&self.bids, (Included(price), Unbounded));
                let sell = volume(&self.asks, (Unbounded, Included(price)));
                Indication {
                    price,
                    volume: buy.min(sell),
                    imbalance: buy as i64 - sell as i64,
                }
            })
            .min_by_key(|i| {
                (
                    std::cmp::Reverse(i.volume),
                    i.imbalance.unsigned_abs(),
                    i.price.abs_diff(reference),
                    i.price,
                )
            })
            .filter(|i| i.volume > 0)
    }

    pub fn uncross(&mut self, price: Price, timestamp: crate::types::Timestamp) -> Execution {
        let mut execution = Execution::default();
        while let (Some(mut bid_level), Some(mut ask_level)) =
            (self.bids.last_entry(), self.asks.first_entry())
        {
            if *bid_level.key() < price || *ask_level.key() > price {
                break;
            }
            let (bids, asks) = (bid_level.get_mut(), ask_level.get_mut());
            let (Some(bid), Some(ask)) = (bids.front_mut(), asks.front_mut()) else {
                break;
            };
            if bid.account == ask.account {
                let newer = if bid.id > ask.id {
                    bids.pop_front()
                } else {
                    asks.pop_front()
                };
                if let Some(cancelled) = newer {
                    self.index.remove(&cancelled.id);
                    execution.self_trade_cancelled.push(cancelled);
                }
            } else {
                let qty = bid.qty.min(ask.qty);
                bid.qty -= qty;
                ask.qty -= qty;
                let (maker, taker, taker_side) = if bid.id < ask.id {
                    (&*bid, &*ask, Side::Sell)
                } else {
                    (&*ask, &*bid, Side::Buy)
                };
                execution.trades.push(Trade {
                    maker_order: maker.id,
                    taker_order: taker.id,
                    maker_account: maker.account,
                    taker_account: taker.account,
                    taker_side,
                    price,
                    qty,
                    timestamp,
                });
                if bid.qty == 0 {
                    let id = bid.id;
                    bids.pop_front();
                    self.index.remove(&id);
                }
                if ask.qty == 0 {
                    let id = ask.id;
                    asks.pop_front();
                    self.index.remove(&id);
                }
            }
            if bids.is_empty() {
                bid_level.remove();
            }
            if asks.is_empty() {
                ask_level.remove();
            }
        }
        execution
    }

    pub fn orders(&self) -> impl Iterator<Item = &Order> {
        self.bids.values().chain(self.asks.values()).flatten()
    }

    pub fn cancel(&mut self, id: OrderId) -> Option<Order> {
        let (side, price) = self.index.remove(&id)?;
        let levels = self.levels_mut(side);
        let queue = levels.get_mut(&price)?;
        let position = queue.iter().position(|o| o.id == id)?;
        let order = queue.remove(position);
        if queue.is_empty() {
            levels.remove(&price);
        }
        order
    }

    pub fn get(&self, id: OrderId) -> Option<&Order> {
        let (side, price) = self.index.get(&id)?;
        self.levels(*side).get(price)?.iter().find(|o| o.id == id)
    }

    pub fn best_bid(&self) -> Option<Price> {
        self.bids.keys().next_back().copied()
    }

    pub fn best_ask(&self) -> Option<Price> {
        self.asks.keys().next().copied()
    }

    pub fn mid(&self) -> Option<Price> {
        Some((self.best_bid()? + self.best_ask()?) / 2)
    }

    pub fn depth(&self, side: Side, max_levels: usize) -> Vec<Level> {
        let summarize = |(price, queue): (&Price, &VecDeque<Order>)| Level {
            price: *price,
            qty: queue.iter().map(|o| o.qty).sum(),
            orders: queue.len() as u32,
        };
        match side {
            Side::Buy => self
                .bids
                .iter()
                .rev()
                .take(max_levels)
                .map(summarize)
                .collect(),
            Side::Sell => self.asks.iter().take(max_levels).map(summarize).collect(),
        }
    }

    pub fn orders_of(&self, account: AccountId) -> impl Iterator<Item = &Order> {
        self.bids
            .values()
            .chain(self.asks.values())
            .flatten()
            .filter(move |o| o.account == account)
    }

    pub fn order_count(&self) -> usize {
        self.index.len()
    }

    fn levels(&self, side: Side) -> &BTreeMap<Price, VecDeque<Order>> {
        match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        }
    }

    fn levels_mut(&mut self, side: Side) -> &mut BTreeMap<Price, VecDeque<Order>> {
        match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    fn order_for(account: AccountId, side: Side, price: Price, qty: Qty) -> Order {
        Order {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            account,
            side,
            price,
            qty,
            timestamp: 0,
        }
    }

    fn order(side: Side, price: Price, qty: Qty) -> Order {
        let account = match side {
            Side::Buy => 1,
            Side::Sell => 2,
        };
        order_for(account, side, price, qty)
    }

    fn volume(book: &OrderBook, side: Side, price: Price) -> Qty {
        book.levels(side)
            .get(&price)
            .map_or(0, |q| q.iter().map(|o| o.qty).sum())
    }

    fn gtc(book: &mut OrderBook, order: Order) -> Vec<Trade> {
        book.submit(order, TimeInForce::Gtc).trades
    }

    #[test]
    fn sell_matches_highest_bid_first() {
        let mut book = OrderBook::new();
        gtc(&mut book, order(Side::Buy, 10000, 5));
        gtc(&mut book, order(Side::Buy, 10500, 5));

        let trades = gtc(&mut book, order(Side::Sell, 10100, 5));

        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].price, 10500);
        assert_eq!(volume(&book, Side::Buy, 10000), 5);
        assert_eq!(volume(&book, Side::Buy, 10500), 0);
    }

    #[test]
    fn buy_matches_lowest_ask_first() {
        let mut book = OrderBook::new();
        gtc(&mut book, order(Side::Sell, 10500, 5));
        gtc(&mut book, order(Side::Sell, 10000, 5));

        let trades = gtc(&mut book, order(Side::Buy, 10200, 5));

        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].price, 10000);
        assert_eq!(volume(&book, Side::Sell, 10500), 5);
    }

    #[test]
    fn same_price_fills_in_arrival_order() {
        let mut book = OrderBook::new();
        let first = order(Side::Sell, 10000, 3);
        let second = order(Side::Sell, 10000, 3);
        let (first_id, second_id) = (first.id, second.id);
        gtc(&mut book, first);
        gtc(&mut book, second);

        let trades = gtc(&mut book, order(Side::Buy, 10000, 4));

        assert_eq!(trades.len(), 2);
        assert_eq!((trades[0].maker_order, trades[0].qty), (first_id, 3));
        assert_eq!((trades[1].maker_order, trades[1].qty), (second_id, 1));
        assert_eq!(volume(&book, Side::Sell, 10000), 2);
    }

    #[test]
    fn unfilled_remainder_rests_at_its_own_price() {
        let mut book = OrderBook::new();
        gtc(&mut book, order(Side::Sell, 10000, 4));

        let execution = book.submit(order(Side::Buy, 10200, 10), TimeInForce::Gtc);

        assert_eq!(execution.trades.len(), 1);
        assert_eq!(execution.rested, 6);
        assert_eq!(volume(&book, Side::Buy, 10200), 6);
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn sweeps_multiple_levels_and_stops_at_limit() {
        let mut book = OrderBook::new();
        gtc(&mut book, order(Side::Sell, 10000, 2));
        gtc(&mut book, order(Side::Sell, 10100, 2));
        gtc(&mut book, order(Side::Sell, 10300, 2));

        let trades = gtc(&mut book, order(Side::Buy, 10200, 10));

        let prices: Vec<Price> = trades.iter().map(|t| t.price).collect();
        assert_eq!(prices, vec![10000, 10100]);
        assert_eq!(volume(&book, Side::Buy, 10200), 6);
        assert_eq!(volume(&book, Side::Sell, 10300), 2);
    }

    #[test]
    fn non_crossing_orders_both_rest() {
        let mut book = OrderBook::new();
        gtc(&mut book, order(Side::Buy, 9900, 5));

        let trades = gtc(&mut book, order(Side::Sell, 10000, 5));

        assert!(trades.is_empty());
        assert_eq!(book.best_bid(), Some(9900));
        assert_eq!(book.best_ask(), Some(10000));
        assert_eq!(book.mid(), Some(9950));
    }

    #[test]
    fn ioc_remainder_expires_instead_of_resting() {
        let mut book = OrderBook::new();
        gtc(&mut book, order(Side::Sell, 10000, 3));

        let execution = book.submit(order(Side::Buy, 10000, 5), TimeInForce::Ioc);

        assert_eq!(execution.trades.len(), 1);
        assert_eq!(execution.expired, 2);
        assert_eq!(execution.rested, 0);
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn self_trade_cancels_resting_order_instead_of_trading() {
        let mut book = OrderBook::new();
        let own = order_for(7, Side::Sell, 10000, 5);
        let own_id = own.id;
        gtc(&mut book, own);
        gtc(&mut book, order_for(8, Side::Sell, 10000, 5));

        let execution = book.submit(order_for(7, Side::Buy, 10000, 5), TimeInForce::Gtc);

        assert_eq!(execution.self_trade_cancelled.len(), 1);
        assert_eq!(execution.self_trade_cancelled[0].id, own_id);
        assert_eq!(execution.trades.len(), 1);
        assert_eq!(execution.trades[0].maker_account, 8);
        assert!(book.get(own_id).is_none());
    }

    #[test]
    fn cancel_removes_order_and_empty_level() {
        let mut book = OrderBook::new();
        let resting = order(Side::Buy, 10000, 5);
        let id = resting.id;
        gtc(&mut book, resting);

        let cancelled = book.cancel(id).unwrap();

        assert_eq!(cancelled.qty, 5);
        assert_eq!(book.best_bid(), None);
        assert_eq!(book.order_count(), 0);
        assert!(book.cancel(id).is_none());
    }

    #[test]
    fn cancel_keeps_queue_position_of_others() {
        let mut book = OrderBook::new();
        let a = order(Side::Sell, 10000, 1);
        let b = order(Side::Sell, 10000, 1);
        let c = order(Side::Sell, 10000, 1);
        let (a_id, b_id, c_id) = (a.id, b.id, c.id);
        for o in [a, b, c] {
            gtc(&mut book, o);
        }

        book.cancel(b_id);
        let trades = gtc(&mut book, order(Side::Buy, 10000, 2));

        let makers: Vec<OrderId> = trades.iter().map(|t| t.maker_order).collect();
        assert_eq!(makers, vec![a_id, c_id]);
    }

    #[test]
    fn filled_orders_leave_the_index() {
        let mut book = OrderBook::new();
        let resting = order(Side::Sell, 10000, 2);
        let id = resting.id;
        gtc(&mut book, resting);

        gtc(&mut book, order(Side::Buy, 10000, 2));

        assert!(book.get(id).is_none());
        assert!(book.cancel(id).is_none());
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn depth_reports_best_levels_first() {
        let mut book = OrderBook::new();
        gtc(&mut book, order(Side::Buy, 9800, 1));
        gtc(&mut book, order(Side::Buy, 9900, 2));
        gtc(&mut book, order(Side::Buy, 9900, 3));

        let depth = book.depth(Side::Buy, 5);

        assert_eq!(
            depth[0],
            Level {
                price: 9900,
                qty: 5,
                orders: 2
            }
        );
        assert_eq!(
            depth[1],
            Level {
                price: 9800,
                qty: 1,
                orders: 1
            }
        );
    }

    #[test]
    fn orders_of_filters_by_account() {
        let mut book = OrderBook::new();
        gtc(&mut book, order_for(3, Side::Buy, 9800, 1));
        gtc(&mut book, order_for(4, Side::Buy, 9900, 1));
        gtc(&mut book, order_for(3, Side::Sell, 10100, 1));

        assert_eq!(book.orders_of(3).count(), 2);
        assert_eq!(book.orders_of(4).count(), 1);
    }

    fn rested(
        book: &mut OrderBook,
        account: AccountId,
        side: Side,
        price: Price,
        qty: Qty,
    ) -> OrderId {
        let o = order_for(account, side, price, qty);
        let id = o.id;
        book.rest(o);
        id
    }

    #[test]
    fn resting_orders_may_cross_without_trading() {
        let mut book = OrderBook::new();
        rested(&mut book, 1, Side::Buy, 10_100, 5);
        rested(&mut book, 2, Side::Sell, 9_900, 5);
        assert_eq!(
            (book.best_bid(), book.best_ask()),
            (Some(10_100), Some(9_900))
        );
        assert_eq!(book.order_count(), 2);
    }

    #[test]
    fn indicative_price_maximizes_volume_then_minimizes_imbalance() {
        let mut book = OrderBook::new();
        rested(&mut book, 1, Side::Buy, 10_200, 10);
        rested(&mut book, 1, Side::Buy, 10_000, 10);
        rested(&mut book, 2, Side::Sell, 9_900, 5);
        rested(&mut book, 2, Side::Sell, 10_100, 10);
        let i = book.indicative(10_000).unwrap();
        assert_eq!((i.price, i.volume, i.imbalance), (10_100, 10, -5));

        let mut tie = OrderBook::new();
        rested(&mut tie, 1, Side::Buy, 10_200, 5);
        rested(&mut tie, 2, Side::Sell, 9_800, 5);
        assert_eq!(tie.indicative(10_000).unwrap().price, 9_800);
        assert_eq!(tie.indicative(10_200).unwrap().price, 10_200);

        let mut apart = OrderBook::new();
        rested(&mut apart, 1, Side::Buy, 9_900, 5);
        rested(&mut apart, 2, Side::Sell, 10_000, 5);
        assert!(apart.indicative(10_000).is_none());
    }

    #[test]
    fn uncross_fills_everything_crossing_at_one_price_in_time_priority() {
        let mut book = OrderBook::new();
        let early = rested(&mut book, 1, Side::Buy, 10_200, 6);
        let late = rested(&mut book, 3, Side::Buy, 10_200, 6);
        rested(&mut book, 2, Side::Sell, 9_900, 8);
        rested(&mut book, 2, Side::Sell, 10_300, 8);

        let price = book.indicative(10_000).unwrap().price;
        let execution = book.uncross(price, 5);

        assert!(execution.trades.iter().all(|t| t.price == price));
        let filled: Qty = execution.trades.iter().map(|t| t.qty).sum();
        assert_eq!(filled, 8);
        assert_eq!(execution.trades[0].maker_order, early);
        assert_eq!(book.get(late).unwrap().qty, 4);
        assert!(book.best_bid().unwrap() < book.best_ask().unwrap());
    }

    #[test]
    fn uncross_cancels_the_newer_order_of_a_self_match() {
        let mut book = OrderBook::new();
        let old = rested(&mut book, 7, Side::Sell, 10_000, 5);
        let new = rested(&mut book, 7, Side::Buy, 10_000, 5);
        let execution = book.uncross(10_000, 0);
        assert!(execution.trades.is_empty());
        assert_eq!(execution.self_trade_cancelled[0].id, new);
        assert!(book.get(old).is_some());
    }
}
