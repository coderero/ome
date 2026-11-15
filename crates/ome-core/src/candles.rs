use crate::price::Price;
use crate::types::{Qty, Timestamp};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candle {
    pub start: Timestamp,
    pub open: Price,
    pub high: Price,
    pub low: Price,
    pub close: Price,
    pub volume: u64,
    pub trades: u64,
}

impl Candle {
    pub fn new(start: Timestamp, price: Price, qty: Qty) -> Self {
        Self {
            start,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: qty,
            trades: 1,
        }
    }

    fn flat(start: Timestamp, price: Price) -> Self {
        Self {
            volume: 0,
            trades: 0,
            ..Self::new(start, price, 0)
        }
    }

    pub fn update(&mut self, price: Price, qty: Qty) {
        self.high = self.high.max(price);
        self.low = self.low.min(price);
        self.close = price;
        self.volume += qty;
        self.trades += 1;
    }

    pub fn is_bullish(&self) -> bool {
        self.close >= self.open
    }
}

pub struct CandleSeries {
    interval: Timestamp,
    capacity: usize,
    candles: VecDeque<Candle>,
}

impl CandleSeries {
    pub fn new(interval: Timestamp, capacity: usize) -> Self {
        assert!(interval > 0 && capacity > 0);
        Self {
            interval,
            capacity,
            candles: VecDeque::with_capacity(capacity),
        }
    }

    pub fn from_candles(
        interval: Timestamp,
        capacity: usize,
        candles: impl IntoIterator<Item = Candle>,
    ) -> Self {
        let mut series = Self::new(interval, capacity);
        for candle in candles {
            series.push(candle);
        }
        series
    }

    pub fn record(&mut self, price: Price, qty: Qty, timestamp: Timestamp) {
        self.roll_to(timestamp);
        let bucket = self.bucket(timestamp);
        match self.candles.back_mut() {
            Some(last) if bucket <= last.start => last.update(price, qty),
            _ => self.push(Candle::new(bucket, price, qty)),
        }
    }

    pub fn roll_to(&mut self, now: Timestamp) {
        let Some(last) = self.candles.back().copied() else {
            return;
        };
        let bucket = self.bucket(now);
        if bucket <= last.start {
            return;
        }
        let missing = (bucket - last.start) / self.interval;
        let skip = missing.saturating_sub(self.capacity as u64);
        let mut start = last.start + (skip + 1) * self.interval;
        while start <= bucket {
            self.push(Candle::flat(start, last.close));
            start += self.interval;
        }
    }

    pub fn candles(&self) -> &VecDeque<Candle> {
        &self.candles
    }

    pub fn interval(&self) -> Timestamp {
        self.interval
    }

    fn bucket(&self, timestamp: Timestamp) -> Timestamp {
        timestamp - timestamp % self.interval
    }

    fn push(&mut self, candle: Candle) {
        if self.candles.len() == self.capacity {
            self.candles.pop_front();
        }
        self.candles.push_back(candle);
    }
}

pub fn aggregate(candles: &[Candle], interval: Timestamp) -> Vec<Candle> {
    let interval = interval.max(1);
    let mut out: Vec<Candle> = Vec::new();
    for c in candles {
        let start = c.start / interval * interval;
        match out.last_mut() {
            Some(last) if last.start == start => {
                if c.trades > 0 {
                    if last.trades == 0 {
                        last.open = c.open;
                        last.high = c.high;
                        last.low = c.low;
                    } else {
                        last.high = last.high.max(c.high);
                        last.low = last.low.min(c.low);
                    }
                    last.close = c.close;
                }
                last.volume += c.volume;
                last.trades += c.trades;
            }
            _ => out.push(Candle { start, ..*c }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trades_in_same_interval_share_a_candle() {
        let mut series = CandleSeries::new(1_000, 10);
        series.record(100, 1, 0);
        series.record(105, 2, 400);
        series.record(98, 3, 999);
        series.record(101, 4, 999);

        let candles = series.candles();
        assert_eq!(candles.len(), 1);
        let c = candles[0];
        assert_eq!((c.open, c.high, c.low, c.close), (100, 105, 98, 101));
        assert_eq!((c.volume, c.trades), (10, 4));
    }

    #[test]
    fn new_interval_starts_new_candle_and_fills_gaps() {
        let mut series = CandleSeries::new(1_000, 10);
        series.record(100, 1, 500);
        series.record(110, 1, 3_200);

        let candles: Vec<Candle> = series.candles().iter().copied().collect();
        assert_eq!(candles.len(), 4);
        assert_eq!(candles[1].start, 1_000);
        assert_eq!((candles[1].open, candles[1].trades), (100, 0));
        assert_eq!(candles[3].start, 3_000);
        assert_eq!(candles[3].close, 110);
    }

    #[test]
    fn capacity_drops_oldest_candles() {
        let mut series = CandleSeries::new(1_000, 3);
        for i in 0..10 {
            series.record(100 + i, 1, i * 1_000);
        }
        let starts: Vec<Timestamp> = series.candles().iter().map(|c| c.start).collect();
        assert_eq!(starts, vec![7_000, 8_000, 9_000]);
    }

    #[test]
    fn rolling_far_ahead_stays_within_capacity() {
        let mut series = CandleSeries::new(1_000, 5);
        series.record(100, 1, 0);
        series.roll_to(1_000_000);

        let candles = series.candles();
        assert_eq!(candles.len(), 5);
        assert_eq!(candles.back().unwrap().start, 1_000_000);
    }

    #[test]
    fn candles_aggregate_into_longer_intervals() {
        let mut a = Candle::new(1_000, 10_00, 5);
        a.update(10_20, 1);
        let quiet = Candle::flat(2_000, 10_20);
        let mut b = Candle::new(61_000, 9_90, 2);
        b.update(10_05, 3);
        let minutes = aggregate(&[quiet, a, b], 60_000);
        assert_eq!(minutes.len(), 2);
        assert_eq!(
            (
                minutes[0].open,
                minutes[0].high,
                minutes[0].close,
                minutes[0].volume
            ),
            (10_00, 10_20, 10_20, 6)
        );
        assert_eq!(
            (minutes[1].start, minutes[1].low, minutes[1].close),
            (60_000, 9_90, 10_05)
        );
    }
}
