mod engine;
mod utils;

use crate::engine::ohlc::OhlcTracker;
use crate::engine::order_book::OrderBook;
use crate::utils::input::read_order_from_cli;

fn main() {
    let mut book = OrderBook::new();
    let mut ohlc = OhlcTracker::new();

    println!("Enter orders (buy/sell price qty):");
    loop {
        if let Some(order) = read_order_from_cli() {
            book.process_order(order.clone(), &mut ohlc);
            book.print_state();
            ohlc.print();
        }
    }
}
