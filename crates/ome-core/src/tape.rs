use crate::types::Trade;
use std::collections::VecDeque;

pub struct TradeTape {
    trades: VecDeque<Trade>,
    capacity: usize,
}

impl TradeTape {
    pub fn new(capacity: usize) -> Self {
        Self {
            trades: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, trade: Trade) {
        if self.trades.len() == self.capacity {
            self.trades.pop_back();
        }
        self.trades.push_front(trade);
    }

    pub fn newest_first(&self) -> impl Iterator<Item = &Trade> {
        self.trades.iter()
    }
}
