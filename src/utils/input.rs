use crate::engine::types::{Order, Side};
use chrono::Utc;
use std::io::{self, Write};
use uuid::Uuid;

pub fn read_order_from_cli() -> Option<Order> {
    print!(">> ");
    io::stdout().flush().unwrap();
    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return None;
    }
    let tokens: Vec<&str> = input.trim().split_whitespace().collect();
    if tokens.len() != 3 {
        println!("Invalid format. Use: buy/sell price qty");
        return None;
    }
    let side = match tokens[0].to_lowercase().as_str() {
        "buy" => Side::Buy,
        "sell" => Side::Sell,
        _ => return None,
    };

    let price = tokens[1].parse::<f64>().ok()?;
    let qty = tokens[2].parse::<u32>().ok()?;
    Some(Order {
        id: Uuid::new_v4(),
        side,
        price,
        qty,
        timestamp: Utc::now().timestamp_millis(),
    })
}