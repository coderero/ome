use ome_core::candles::CandleSeries;
use ome_core::order_book::OrderBook;
use ome_core::price::Price;
use ome_core::risk::PriceBand;
use ome_core::symbol::Symbol;
use ome_core::tape::TradeTape;
use ome_core::types::{Timestamp, Trade};
use std::collections::VecDeque;
use ome_protocol::{DayBar, DayHistory, InstrumentInfo, InstrumentStatus, MarketStats, Venue};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DayStats {
    pub open: Price,
    pub high: Price,
    pub low: Price,
    pub last: Price,
    pub trades: u64,
    pub turnover: u64,
    pub volume: [u64; 4],
    pub pauses: u32,
}

impl DayStats {
    fn record(&mut self, price: Price, qty: u64, venue: u8) {
        if self.trades == 0 {
            self.open = price;
            self.high = price;
            self.low = price;
        }
        self.high = self.high.max(price);
        self.low = self.low.min(price);
        self.last = price;
        self.trades += 1;
        self.turnover += price * qty;
        self.volume[(venue as usize).min(3)] += qty;
    }
}

pub const LULD_MIN_TRADES: usize = 5;
pub const DAY_HISTORY: usize = 260;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltKind {
    Manual,
    Circuit,
    Luld,
}

pub struct Halt {
    pub reason: String,
    pub auto_resume_at: Option<Timestamp>,
    pub kind: HaltKind,
}

pub struct Instrument {
    pub symbol: Symbol,
    pub name: String,
    pub book: OrderBook,
    pub candles: CandleSeries,
    pub tape: TradeTape,
    pub reference_price: Price,
    pub last_price: Option<Price>,
    pub volume: u64,
    pub halt: Option<Halt>,
    pub day: DayStats,
    pub history: VecDeque<DayBar>,
    pub recent: VecDeque<(Timestamp, Price)>,
    pub luld_anchor: Option<Price>,
}

impl Instrument {
    pub fn new(
        symbol: Symbol,
        name: String,
        ipo_price: Price,
        candle_interval: Timestamp,
        candle_capacity: usize,
        tape_capacity: usize,
    ) -> Self {
        Self {
            symbol,
            name,
            book: OrderBook::new(),
            candles: CandleSeries::new(candle_interval, candle_capacity),
            tape: TradeTape::new(tape_capacity),
            reference_price: ipo_price,
            last_price: None,
            volume: 0,
            halt: None,
            day: DayStats::default(),
            history: VecDeque::new(),
            recent: VecDeque::new(),
            luld_anchor: None,
        }
    }

    pub fn band(&self, bps: u64) -> (Price, Price) {
        PriceBand { bps }.limits(self.last_price.unwrap_or(self.reference_price))
    }

    pub fn circuit_limits(&self, bps: u64) -> (Price, Price) {
        PriceBand { bps }.limits(self.reference_price)
    }

    pub fn breached_circuit(&self, bps: u64) -> bool {
        let (low, high) = self.circuit_limits(bps);
        self.last_price.is_some_and(|p| p < low || p > high)
    }

    pub fn record_trade(&mut self, trade: &Trade) {
        self.last_price = Some(trade.price);
        self.volume += trade.qty;
        self.candles.record(trade.price, trade.qty, trade.timestamp);
        self.tape.push(trade.clone());
        self.day.record(trade.price, trade.qty, trade.venue);
        self.recent.push_back((trade.timestamp, trade.price));
    }

    pub fn luld_reference(&mut self, now: Timestamp, window: Timestamp) -> Option<Price> {
        while self.recent.front().is_some_and(|(at, _)| now.saturating_sub(*at) > window) {
            self.recent.pop_front();
        }
        if self.recent.len() < LULD_MIN_TRADES + 1 {
            return self.luld_anchor;
        }
        let all: Vec<Price> = self.recent.iter().map(|(_, p)| *p).collect();
        let before_last = &all[..all.len() - 1];
        Some(before_last.iter().sum::<Price>() / before_last.len() as Price)
    }

    pub fn paused(&self) -> bool {
        self.halt.as_ref().is_some_and(|h| h.kind == HaltKind::Luld)
    }

    pub fn archive_day(&mut self, day: u64) -> bool {
        if self.day.trades == 0 || self.history.back().is_some_and(|b| b.day >= day) {
            return false;
        }
        if self.history.len() == DAY_HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(DayBar {
            day,
            open: self.day.open,
            high: self.day.high,
            low: self.day.low,
            close: self.day.last,
            volume: self.day.volume.iter().sum(),
            trades: self.day.trades,
            turnover: self.day.turnover,
        });
        true
    }

    pub fn day_history(&self) -> DayHistory {
        DayHistory { symbol: self.symbol, days: self.history.iter().copied().collect() }
    }

    pub fn stats(&self) -> MarketStats {
        let d = &self.day;
        let volume: u64 = d.volume.iter().sum();
        MarketStats {
            symbol: self.symbol,
            open: d.open,
            high: d.high,
            low: d.low,
            last: d.last,
            vwap: d.turnover.checked_div(volume).unwrap_or(0),
            trades: d.trades,
            turnover: d.turnover,
            exchange_volume: d.volume[Venue::Exchange as usize],
            internal_volume: d.volume[Venue::Internal as usize],
            wholesaler_volume: d.volume[Venue::Wholesaler as usize],
            ats_volume: d.volume[Venue::Ats as usize],
            pauses: d.pauses,
        }
    }

    pub fn info(&self, band_bps: u64, circuit_bps: u64) -> InstrumentInfo {
        let (low_limit, high_limit) = self.circuit_limits(circuit_bps);
        InstrumentInfo {
            symbol: self.symbol,
            status: if self.paused() {
                InstrumentStatus::Paused
            } else if self.halt.is_some() {
                InstrumentStatus::Halted
            } else {
                InstrumentStatus::Trading
            },
            reference_price: self.reference_price,
            last_price: self.last_price,
            low_limit,
            high_limit,
            band_bps,
            volume: self.volume,
            name: self.name.clone(),
            halt_reason: self
                .halt
                .as_ref()
                .map(|h| h.reason.clone())
                .unwrap_or_default(),
        }
    }
}
