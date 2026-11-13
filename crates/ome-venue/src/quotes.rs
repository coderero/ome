use ome_core::price::Price;
use ome_core::risk::PriceBand;
use ome_core::symbol::Symbol;
use ome_protocol::{InstrumentInfo, InstrumentStatus, MarketPhase, ServerMessage};
use std::collections::BTreeMap;

struct Quote {
    info: InstrumentInfo,
    bid: Option<Price>,
    ask: Option<Price>,
}

#[derive(Default)]
pub struct Quotes {
    quotes: BTreeMap<Symbol, Quote>,
    phase: Option<MarketPhase>,
    next_change_at: u64,
    cycle: u64,
}

impl Quotes {
    pub fn apply(&mut self, message: &ServerMessage) -> Vec<Symbol> {
        match message {
            ServerMessage::InstrumentList(list) => {
                list.iter().filter_map(|i| self.upsert(i)).collect()
            }
            ServerMessage::Instrument(info) => self.upsert(info).into_iter().collect(),
            ServerMessage::Book(book) => {
                if let Some(q) = self.quotes.get_mut(&book.symbol) {
                    q.bid = book.bids.first().map(|l| l.price);
                    q.ask = book.asks.first().map(|l| l.price);
                }
                vec![]
            }
            ServerMessage::Trade(trade) => {
                if let Some(q) = self.quotes.get_mut(&trade.symbol) {
                    q.info.last_price = Some(trade.price);
                }
                vec![]
            }
            ServerMessage::MarketStatus(status) => {
                self.phase = Some(status.phase);
                self.next_change_at = status.next_change_at;
                vec![]
            }
            ServerMessage::Execution(report) => {
                self.cycle = self.cycle.max(report.cycle);
                vec![]
            }
            _ => vec![],
        }
    }

    fn upsert(&mut self, info: &InstrumentInfo) -> Option<Symbol> {
        match self.quotes.get_mut(&info.symbol) {
            Some(q) => {
                q.info = info.clone();
                None
            }
            None => {
                self.quotes.insert(
                    info.symbol,
                    Quote {
                        info: info.clone(),
                        bid: None,
                        ask: None,
                    },
                );
                Some(info.symbol)
            }
        }
    }

    pub fn set_best(&mut self, symbol: Symbol, bid: Price, ask: Price) {
        if let Some(q) = self.quotes.get_mut(&symbol) {
            q.bid = Some(bid);
            q.ask = Some(ask);
        }
    }

    pub fn knows(&self, symbol: Symbol) -> bool {
        self.quotes.contains_key(&symbol)
    }

    pub fn symbols(&self) -> impl Iterator<Item = Symbol> + '_ {
        self.quotes.keys().copied()
    }

    pub fn is_open(&self) -> bool {
        self.phase.unwrap_or(MarketPhase::Open) == MarketPhase::Open
    }

    pub fn phase(&self) -> MarketPhase {
        self.phase.unwrap_or(MarketPhase::Open)
    }

    pub fn tradable(&self, symbol: Symbol) -> bool {
        self.is_open()
            && self
                .quotes
                .get(&symbol)
                .is_some_and(|q| q.info.status == InstrumentStatus::Trading)
    }

    pub fn best(&self, symbol: Symbol) -> Option<(Price, Price)> {
        let q = self.quotes.get(&symbol)?;
        Some((q.bid?, q.ask?)).filter(|(bid, ask)| bid < ask)
    }

    pub fn band(&self, symbol: Symbol) -> Option<(Price, Price)> {
        let info = &self.quotes.get(&symbol)?.info;
        Some(
            PriceBand { bps: info.band_bps }
                .limits(info.last_price.unwrap_or(info.reference_price)),
        )
    }

    pub fn closing_soon(&self, now: u64) -> bool {
        self.is_open() && self.next_change_at > 0 && now + 3_000 >= self.next_change_at
    }

    pub fn cycle(&self) -> u64 {
        self.cycle
    }
}
