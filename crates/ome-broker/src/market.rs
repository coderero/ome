use ome_core::candles::CandleSeries;
use ome_core::price::Price;
use ome_core::risk::PriceBand;
use ome_core::symbol::Symbol;
use ome_core::types::Timestamp;
use ome_protocol::{
    BookSnapshot, CandleSnapshot, InstrumentInfo, InstrumentStatus, MarketPhase, MarketStats,
    MarketStatus, ServerMessage, TradeEvent,
};
use std::collections::{BTreeMap, VecDeque};

const CANDLE_CAPACITY: usize = 600;
const TAPE_CAPACITY: usize = 60;

pub struct Quote {
    pub info: InstrumentInfo,
    pub book: Option<BookSnapshot>,
    pub candles: CandleSeries,
    pub tape: VecDeque<TradeEvent>,
    pub stats: Option<MarketStats>,
}

#[derive(Default)]
pub struct MarketCache {
    quotes: BTreeMap<Symbol, Quote>,
    status: Option<MarketStatus>,
}

impl MarketCache {
    pub fn apply(&mut self, message: &ServerMessage) -> Option<Symbol> {
        match message {
            ServerMessage::InstrumentList(list) => {
                for info in list {
                    self.upsert(info);
                }
                None
            }
            ServerMessage::Instrument(info) => self.upsert(info),
            ServerMessage::MarketStatus(status) => {
                self.status = Some(*status);
                None
            }
            ServerMessage::MarketStats(stats) => {
                if let Some(quote) = self.quotes.get_mut(&stats.symbol) {
                    quote.stats = Some(*stats);
                }
                None
            }
            ServerMessage::Book(book) => {
                if let Some(quote) = self.quotes.get_mut(&book.symbol) {
                    quote.book = Some(book.clone());
                }
                None
            }
            ServerMessage::Candles(snapshot) => {
                if let Some(quote) = self.quotes.get_mut(&snapshot.symbol) {
                    quote.candles = CandleSeries::from_candles(
                        snapshot.interval,
                        CANDLE_CAPACITY,
                        snapshot.candles.iter().copied(),
                    );
                }
                None
            }
            ServerMessage::Trade(trade) => {
                if let Some(quote) = self.quotes.get_mut(&trade.symbol) {
                    quote.info.last_price = Some(trade.price);
                    if trade.trade_id != 0 {
                        quote.info.volume += trade.qty;
                        quote
                            .candles
                            .record(trade.price, trade.qty, trade.timestamp);
                    }
                    if quote.tape.len() == TAPE_CAPACITY {
                        quote.tape.pop_front();
                    }
                    quote.tape.push_back(*trade);
                }
                None
            }
            _ => None,
        }
    }

    fn upsert(&mut self, info: &InstrumentInfo) -> Option<Symbol> {
        match self.quotes.get_mut(&info.symbol) {
            Some(quote) => {
                quote.info = info.clone();
                None
            }
            None => {
                self.quotes.insert(
                    info.symbol,
                    Quote {
                        info: info.clone(),
                        book: None,
                        candles: CandleSeries::new(1_000, CANDLE_CAPACITY),
                        tape: VecDeque::new(),
                        stats: None,
                    },
                );
                Some(info.symbol)
            }
        }
    }

    pub fn roll(&mut self, now: Timestamp) {
        for quote in self.quotes.values_mut() {
            quote.candles.roll_to(now);
        }
    }

    pub fn get(&self, symbol: Symbol) -> Option<&Quote> {
        self.quotes.get(&symbol)
    }

    pub fn best(&self, symbol: Symbol) -> Option<(Price, Price)> {
        let book = self.quotes.get(&symbol)?.book.as_ref()?;
        Some((book.bids.first()?.price, book.asks.first()?.price))
    }

    pub fn symbols(&self) -> impl Iterator<Item = Symbol> + '_ {
        self.quotes.keys().copied()
    }

    pub fn instruments(&self) -> Vec<InstrumentInfo> {
        self.quotes.values().map(|q| q.info.clone()).collect()
    }

    pub fn band(&self, symbol: Symbol) -> Option<(Price, Price)> {
        let info = &self.quotes.get(&symbol)?.info;
        Some(
            PriceBand { bps: info.band_bps }
                .limits(info.last_price.unwrap_or(info.reference_price)),
        )
    }

    pub fn phase(&self) -> MarketPhase {
        self.status.map_or(MarketPhase::Open, |s| s.phase)
    }

    pub fn status(&self) -> Option<MarketStatus> {
        self.status
    }

    pub fn is_trading(&self, symbol: Symbol) -> bool {
        self.quotes
            .get(&symbol)
            .is_some_and(|q| q.info.status == InstrumentStatus::Trading)
    }

    pub fn last(&self, symbol: Symbol) -> Option<Price> {
        let quote = self.quotes.get(&symbol)?;
        quote.info.last_price.or(Some(quote.info.reference_price))
    }

    pub fn history(
        &self,
        symbol: Symbol,
        interval: Timestamp,
        limit: usize,
    ) -> Option<CandleSnapshot> {
        let quote = self.quotes.get(&symbol)?;
        let candles: Vec<_> = quote.candles.candles().iter().copied().collect();
        let interval = interval.max(quote.candles.interval());
        let mut candles = ome_core::candles::aggregate(&candles, interval);
        let skip = candles.len().saturating_sub(limit.max(1));
        candles.drain(..skip);
        Some(CandleSnapshot {
            symbol,
            interval,
            candles,
        })
    }

    pub fn snapshot(&self, symbol: Symbol) -> Vec<ServerMessage> {
        let Some(quote) = self.quotes.get(&symbol) else {
            return Vec::new();
        };
        let mut messages = vec![
            ServerMessage::Instrument(quote.info.clone()),
            ServerMessage::Candles(CandleSnapshot {
                symbol,
                interval: quote.candles.interval(),
                candles: quote.candles.candles().iter().copied().collect(),
            }),
        ];
        messages.extend(
            quote
                .tape
                .iter()
                .map(|t| ServerMessage::Trade(TradeEvent { trade_id: 0, ..*t })),
        );
        messages.extend(quote.book.clone().map(ServerMessage::Book));
        messages.extend(quote.stats.map(ServerMessage::MarketStats));
        messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ome_core::types::Side;

    fn info(symbol: &str) -> InstrumentInfo {
        InstrumentInfo {
            symbol: Symbol::parse(symbol).unwrap(),
            status: InstrumentStatus::Trading,
            reference_price: 100_00,
            last_price: None,
            low_limit: 70_00,
            high_limit: 130_00,
            band_bps: 1_000,
            volume: 0,
            name: String::new(),
            halt_reason: String::new(),
        }
    }

    #[test]
    fn new_instruments_are_reported_once_and_trades_move_the_band() {
        let mut cache = MarketCache::default();
        let acme = Symbol::parse("ACME").unwrap();
        assert_eq!(
            cache.apply(&ServerMessage::Instrument(info("ACME"))),
            Some(acme)
        );
        assert_eq!(cache.apply(&ServerMessage::Instrument(info("ACME"))), None);
        assert_eq!(cache.band(acme), Some((90_00, 110_00)));

        cache.apply(&ServerMessage::Trade(TradeEvent {
            symbol: acme,
            trade_id: 1,
            price: 105_00,
            qty: 3,
            aggressor: Side::Buy,
            timestamp: 10,
            venue: ome_protocol::Venue::Exchange,
        }));
        assert_eq!(cache.band(acme), Some((94_50, 115_50)));
        assert_eq!(cache.snapshot(acme).len(), 3);
    }
}
