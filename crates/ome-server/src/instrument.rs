use ome_core::candles::CandleSeries;
use ome_core::order_book::OrderBook;
use ome_core::price::Price;
use ome_core::risk::PriceBand;
use ome_core::symbol::Symbol;
use ome_core::tape::TradeTape;
use ome_core::types::{Timestamp, Trade};
use ome_protocol::{InstrumentInfo, InstrumentStatus};

pub struct Halt {
    pub reason: String,
    pub auto_resume_at: Option<Timestamp>,
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
    }

    pub fn info(&self, circuit_bps: u64) -> InstrumentInfo {
        let (low_limit, high_limit) = self.circuit_limits(circuit_bps);
        InstrumentInfo {
            symbol: self.symbol,
            status: if self.halt.is_some() {
                InstrumentStatus::Halted
            } else {
                InstrumentStatus::Trading
            },
            reference_price: self.reference_price,
            last_price: self.last_price,
            low_limit,
            high_limit,
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
