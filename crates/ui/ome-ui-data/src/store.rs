use ome_core::candles::{Candle, CandleSeries};
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, OrderId, Timestamp};
use ome_protocol::{
    AccountSnapshot, BookSnapshot, ConditionalReport, ConditionalStatus, ContractNote,
    ExecutionReport, InstrumentInfo, MarketPhase, MarketStats, MarketStatus, NewsItem, Role,
    ServerMessage, TradeEvent,
};
use std::collections::{BTreeMap, VecDeque};

const TAPE: usize = 200;
const NOTES: usize = 100;
const NEWS: usize = 200;
const CANDLES: usize = 600;
const NOTICES: usize = 20;
const CLOSED: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Connection {
    #[default]
    Connecting,
    Online {
        account: AccountId,
        role: Role,
    },
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub at: Timestamp,
    pub text: String,
    pub error: bool,
}

#[derive(Default)]
pub struct MarketStore {
    pub connection: Connection,
    pub now: Timestamp,
    pub status: Option<MarketStatus>,
    pub instruments: BTreeMap<Symbol, InstrumentInfo>,
    pub books: BTreeMap<Symbol, BookSnapshot>,
    pub stats: BTreeMap<Symbol, MarketStats>,
    tapes: BTreeMap<Symbol, VecDeque<TradeEvent>>,
    candles: BTreeMap<(Symbol, Timestamp), CandleSeries>,
    pub estimate: Option<ome_protocol::ChargesEstimate>,
    pub account: Option<AccountSnapshot>,
    pub orders: BTreeMap<OrderId, ExecutionReport>,
    closed: VecDeque<ExecutionReport>,
    pub conditionals: BTreeMap<u64, ConditionalReport>,
    notes: VecDeque<ContractNote>,
    news: VecDeque<NewsItem>,
    notices: VecDeque<Notice>,
    pub fills: u64,
}

pub trait MarketData {
    fn symbols(&self) -> Vec<Symbol>;
    fn instrument(&self, symbol: Symbol) -> Option<&InstrumentInfo>;
    fn book(&self, symbol: Symbol) -> Option<&BookSnapshot>;
    fn tape(&self, symbol: Symbol) -> Vec<&TradeEvent>;
    fn candles(&self, symbol: Symbol, interval: Timestamp) -> Vec<Candle>;
    fn intervals(&self, symbol: Symbol) -> Vec<Timestamp>;
    fn stats(&self, symbol: Symbol) -> Option<&MarketStats>;
    fn phase(&self) -> Option<MarketPhase>;
    fn last(&self, symbol: Symbol) -> Option<u64>;
}

pub trait AccountView {
    fn account(&self) -> Option<&AccountSnapshot>;
    fn open_orders(&self) -> Vec<&ExecutionReport>;
    fn armed(&self) -> Vec<&ConditionalReport>;
    fn contract_notes(&self) -> Vec<&ContractNote>;
    fn closed_orders(&self) -> Vec<&ExecutionReport>;
    fn notices(&self) -> Vec<&Notice>;
}

pub trait NewsFeed {
    fn news(&self) -> Vec<&NewsItem>;
}

impl MarketStore {
    fn notice(&mut self, text: String, error: bool) {
        if self.notices.len() == NOTICES {
            self.notices.pop_front();
        }
        self.notices.push_back(Notice {
            at: self.now,
            text,
            error,
        });
    }

    pub fn apply(&mut self, message: ServerMessage) {
        match message {
            ServerMessage::LogonAck {
                account_id,
                role,
                server_time,
            } => {
                self.connection = Connection::Online {
                    account: account_id,
                    role,
                };
                self.now = self.now.max(server_time);
            }
            ServerMessage::Heartbeat { server_time } => self.now = self.now.max(server_time),
            ServerMessage::MarketStatus(status) => self.status = Some(status),
            ServerMessage::InstrumentList(list) => {
                for info in list {
                    self.instruments.insert(info.symbol, info);
                }
            }
            ServerMessage::Instrument(info) => {
                self.instruments.insert(info.symbol, info);
            }
            ServerMessage::Book(book) => {
                self.now = self.now.max(book.timestamp);
                self.books.insert(book.symbol, book);
            }
            ServerMessage::MarketStats(stats) => {
                self.stats.insert(stats.symbol, stats);
            }
            ServerMessage::Candles(snapshot) => {
                self.candles.insert(
                    (snapshot.symbol, snapshot.interval.max(1)),
                    CandleSeries::from_candles(snapshot.interval.max(1), CANDLES, snapshot.candles),
                );
            }
            ServerMessage::Trade(trade) => {
                self.now = self.now.max(trade.timestamp);
                if let Some(info) = self.instruments.get_mut(&trade.symbol) {
                    info.last_price = Some(trade.price);
                }
                if trade.trade_id == 0 {
                    return;
                }
                for ((symbol, _), series) in self.candles.iter_mut() {
                    if *symbol == trade.symbol {
                        series.record(trade.price, trade.qty, trade.timestamp);
                    }
                }
                let tape = self.tapes.entry(trade.symbol).or_default();
                if tape.len() == TAPE {
                    tape.pop_back();
                }
                tape.push_front(trade);
            }
            ServerMessage::Account(snapshot) => self.account = Some(snapshot),
            ServerMessage::Execution(report) => {
                if report.last_qty > 0 {
                    self.fills += 1;
                }
                if report.status == ome_protocol::OrderStatus::Rejected {
                    let text = format!("order rejected: {}", report.reason);
                    self.notice(text, true);
                }
                if report.status.is_open() && report.order_id != 0 {
                    self.orders.insert(report.order_id, report);
                } else {
                    self.orders.remove(&report.order_id);
                    if self.closed.len() == CLOSED {
                        self.closed.pop_back();
                    }
                    self.closed.push_front(report);
                }
            }
            ServerMessage::ConditionalUpdate(report) => match report.status {
                ConditionalStatus::Armed => {
                    self.conditionals.insert(report.cond_id, report);
                }
                ConditionalStatus::Rejected => {
                    let text = format!("conditional rejected: {}", report.reason);
                    self.notice(text, true);
                    self.conditionals.remove(&report.cond_id);
                }
                _ => {
                    self.conditionals.remove(&report.cond_id);
                }
            },
            ServerMessage::ContractNote(note) => {
                if self.notes.len() == NOTES {
                    self.notes.pop_back();
                }
                self.notes.push_front(note);
            }
            ServerMessage::News(item) => {
                if self.news.iter().any(|n| n.seq == item.seq) {
                    return;
                }
                if self.news.len() == NEWS {
                    self.news.pop_back();
                }
                self.news.push_front(item);
            }
            ServerMessage::ChargesEstimate(estimate) => self.estimate = Some(estimate),
            ServerMessage::Reject { reason, .. } => self.notice(reason, true),
            ServerMessage::Ack { text, .. } => self.notice(text, false),
            _ => {}
        }
    }

    pub fn disconnected(&mut self) {
        self.connection = Connection::Offline;
        self.orders.clear();
        self.conditionals.clear();
        self.notice("connection to the broker lost; reconnecting".into(), true);
    }

    pub fn roll(&mut self, now: Timestamp) {
        self.now = self.now.max(now);
        for series in self.candles.values_mut() {
            series.roll_to(self.now);
        }
    }
}

impl MarketData for MarketStore {
    fn symbols(&self) -> Vec<Symbol> {
        self.instruments.keys().copied().collect()
    }

    fn instrument(&self, symbol: Symbol) -> Option<&InstrumentInfo> {
        self.instruments.get(&symbol)
    }

    fn book(&self, symbol: Symbol) -> Option<&BookSnapshot> {
        self.books.get(&symbol)
    }

    fn tape(&self, symbol: Symbol) -> Vec<&TradeEvent> {
        self.tapes
            .get(&symbol)
            .map(|t| t.iter().collect())
            .unwrap_or_default()
    }

    fn candles(&self, symbol: Symbol, interval: Timestamp) -> Vec<Candle> {
        if let Some(series) = self.candles.get(&(symbol, interval)) {
            return series.candles().iter().copied().collect();
        }
        let finest = self
            .candles
            .iter()
            .filter(|((s, i), _)| *s == symbol && *i <= interval)
            .min_by_key(|((_, i), _)| *i);
        match finest {
            Some((_, series)) => {
                let candles: Vec<Candle> = series.candles().iter().copied().collect();
                ome_core::candles::aggregate(&candles, interval)
            }
            None => Vec::new(),
        }
    }

    fn intervals(&self, symbol: Symbol) -> Vec<Timestamp> {
        self.candles
            .keys()
            .filter(|(s, _)| *s == symbol)
            .map(|(_, i)| *i)
            .collect()
    }

    fn stats(&self, symbol: Symbol) -> Option<&MarketStats> {
        self.stats.get(&symbol)
    }

    fn phase(&self) -> Option<MarketPhase> {
        self.status.map(|s| s.phase)
    }

    fn last(&self, symbol: Symbol) -> Option<u64> {
        let info = self.instruments.get(&symbol)?;
        info.last_price.or(Some(info.reference_price))
    }
}

impl AccountView for MarketStore {
    fn account(&self) -> Option<&AccountSnapshot> {
        self.account.as_ref()
    }

    fn open_orders(&self) -> Vec<&ExecutionReport> {
        self.orders.values().collect()
    }

    fn armed(&self) -> Vec<&ConditionalReport> {
        self.conditionals.values().collect()
    }

    fn contract_notes(&self) -> Vec<&ContractNote> {
        self.notes.iter().collect()
    }

    fn closed_orders(&self) -> Vec<&ExecutionReport> {
        self.closed.iter().collect()
    }

    fn notices(&self) -> Vec<&Notice> {
        self.notices.iter().rev().collect()
    }
}

impl NewsFeed for MarketStore {
    fn news(&self) -> Vec<&NewsItem> {
        self.news.iter().collect()
    }
}
