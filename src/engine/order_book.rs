use crate::engine::ohlc::OhlcTracker;
use crate::engine::types::{Order, Side};
use indexmap::IndexMap;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
struct OrderedF64(f64);

impl Eq for OrderedF64 {}

impl Hash for OrderedF64 {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

impl Ord for OrderedF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.partial_cmp(other).unwrap()
    }
}

#[derive(Default)]
pub struct OrderBook {
    buy: IndexMap<OrderedF64, VecDeque<Order>>, // price -> orders (buy, highest first)
    sell: IndexMap<OrderedF64, VecDeque<Order>>, // price -> orders (sell, lowest first)
    buy_prices: BinaryHeap<Reverse<OrderedF64>>, // We'll use Reverse for both, but interpret differently
    sell_prices: BinaryHeap<Reverse<OrderedF64>>, // Min heap for sell prices
}

impl OrderBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn process_order(&mut self, order: Order, ohlc: &mut OhlcTracker) {
        match order.side {
            Side::Buy => self.match_order(order, false, ohlc),
            Side::Sell => self.match_order(order, true, ohlc),
        }
    }

    fn match_order(&mut self, mut incoming: Order, is_sell: bool, ohlc: &mut OhlcTracker) {
        let (book, price_heap) = if is_sell {
            (&mut self.buy, &mut self.buy_prices)
        } else {
            (&mut self.sell, &mut self.sell_prices)
        };

        while incoming.qty > 0 {
            let best_price_opt = price_heap.peek().map(|Reverse(p)| *p);

            if best_price_opt.is_none() {
                break;
            }

            let best_price = best_price_opt.unwrap();

            // For sell orders matching with buy book, we want highest buy price first (buy_prices is in reverse)
            // For buy orders matching with sell book, we want lowest sell price first (naturally min-heap)
            let price_ok = if is_sell {
                // When a sell order comes in (is_sell = true), we're looking at the buy book
                // We want buy orders with prices >= the incoming sell price
                best_price.0 >= incoming.price
            } else {
                // When a buy order comes in (is_sell = false), we're looking at the sell book
                // We want sell orders with prices <= the incoming buy price
                best_price.0 <= incoming.price
            };

            if !price_ok {
                break;
            }

            let queue = book.get_mut(&best_price).unwrap();

            while let Some(mut existing) = queue.pop_front() {
                let traded_qty = incoming.qty.min(existing.qty);
                println!(
                    "Trade executed: {} @ {} (qty: {})",
                    best_price.0, incoming.id, traded_qty
                );
                ohlc.update(best_price.0, incoming.timestamp);

                incoming.qty -= traded_qty;
                existing.qty -= traded_qty;

                if existing.qty > 0 {
                    queue.push_front(existing);
                    break;
                }
                if incoming.qty == 0 {
                    break;
                }
            }

            if queue.is_empty() {
                book.shift_remove(&best_price); // Using shift_remove instead of deprecated remove
                price_heap.pop();
            }
        }

        if incoming.qty > 0 {
            let target_book = if incoming.side == Side::Buy {
                &mut self.buy
            } else {
                &mut self.sell
            };
            let target_heap = if incoming.side == Side::Buy {
                &mut self.buy_prices
            } else {
                &mut self.sell_prices
            };

            let price_key = OrderedF64(incoming.price);
            target_book
                .entry(price_key)
                .or_default()
                .push_back(incoming);

            // Add price to heap if not already present
            if !target_heap.iter().any(|Reverse(x)| *x == price_key) {
                target_heap.push(Reverse(price_key));
            }
        }
    }

    pub fn print_state(&self) {
        println!("Order Book:");
        println!("SELL:");

        // For sell side, we want to display from highest to lowest price
        let mut sell_prices: Vec<_> = self.sell.keys().collect();
        sell_prices.sort_by(|a, b| b.partial_cmp(a).unwrap()); // Sort in descending order

        for price in sell_prices {
            if let Some(orders) = self.sell.get(price) {
                for order in orders {
                    println!("{:.2} qty:{} id:{}", price.0, order.qty, order.id);
                }
            }
        }

        println!("BUY:");

        // For buy side, we also want to display from highest to lowest price
        let mut buy_prices: Vec<_> = self.buy.keys().collect();
        buy_prices.sort_by(|a, b| b.partial_cmp(a).unwrap()); // Sort in descending order

        for price in buy_prices {
            if let Some(orders) = self.buy.get(price) {
                for order in orders {
                    println!("{:.2} qty:{} id:{}", price.0, order.qty, order.id);
                }
            }
        }

        println!("--------------------------");
    }
}

