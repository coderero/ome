use crate::observation::Observation;
use crate::sink::Sink;
use ome_core::order_book::Level;
use ome_protocol::BookSnapshot;
use redis::{Client, Connection, Pipeline};

pub const TRADE_STREAM_LENGTH: usize = 10_000;

pub fn book_json(book: &BookSnapshot) -> String {
    let side = |levels: &[Level]| {
        levels
            .iter()
            .map(|l| format!("[{},{},{}]", l.price, l.qty, l.orders))
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "{{\"symbol\":\"{}\",\"ts\":{},\"bids\":[{}],\"asks\":[{}]}}",
        book.symbol,
        book.timestamp,
        side(&book.bids),
        side(&book.asks)
    )
}

pub struct RedisSink {
    url: String,
    connection: Option<Connection>,
}

impl RedisSink {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            connection: None,
        }
    }
}

pub fn commands(batch: &[Observation]) -> Pipeline {
    let mut pipe = redis::pipe();
    for observation in batch {
        match observation {
            Observation::Instrument(i) => {
                pipe.cmd("HSET")
                    .arg(format!("ome:instrument:{}", i.symbol))
                    .arg("name")
                    .arg(&i.name)
                    .arg("reference")
                    .arg(i.reference_price)
                    .arg("low_limit")
                    .arg(i.low_limit)
                    .arg("high_limit")
                    .arg(i.high_limit)
                    .arg("halted")
                    .arg(!i.halt_reason.is_empty())
                    .ignore();
                pipe.cmd("SADD")
                    .arg("ome:symbols")
                    .arg(i.symbol.to_string())
                    .ignore();
            }
            Observation::Trade(t) => {
                let symbol = t.symbol.to_string();
                pipe.cmd("SET")
                    .arg(format!("ome:last:{symbol}"))
                    .arg(t.price)
                    .ignore();
                pipe.cmd("XADD")
                    .arg(format!("ome:trades:{symbol}"))
                    .arg("MAXLEN")
                    .arg("~")
                    .arg(TRADE_STREAM_LENGTH)
                    .arg("*")
                    .arg("id")
                    .arg(t.trade_id)
                    .arg("price")
                    .arg(t.price)
                    .arg("qty")
                    .arg(t.qty)
                    .arg("side")
                    .arg(t.aggressor.label())
                    .arg("venue")
                    .arg(t.venue.label())
                    .arg("ts")
                    .arg(t.timestamp)
                    .ignore();
                pipe.cmd("PUBLISH")
                    .arg("ome:trades")
                    .arg(format!(
                        "{symbol} {} {} {} {}",
                        t.price,
                        t.qty,
                        t.aggressor.label(),
                        t.venue.label()
                    ))
                    .ignore();
            }
            Observation::Book(b) => {
                pipe.cmd("SET")
                    .arg(format!("ome:book:{}", b.symbol))
                    .arg(book_json(b))
                    .ignore();
            }
            Observation::Stats(s) => {
                pipe.cmd("HSET")
                    .arg(format!("ome:stats:{}", s.symbol))
                    .arg("open")
                    .arg(s.open)
                    .arg("high")
                    .arg(s.high)
                    .arg("low")
                    .arg(s.low)
                    .arg("last")
                    .arg(s.last)
                    .arg("vwap")
                    .arg(s.vwap)
                    .arg("trades")
                    .arg(s.trades)
                    .arg("volume")
                    .arg(s.volume())
                    .arg("exchange_volume")
                    .arg(s.exchange_volume)
                    .arg("internal_volume")
                    .arg(s.internal_volume)
                    .arg("wholesaler_volume")
                    .arg(s.wholesaler_volume)
                    .arg("ats_volume")
                    .arg(s.ats_volume)
                    .ignore();
            }
            Observation::Market(m) => {
                pipe.cmd("HSET")
                    .arg("ome:market")
                    .arg("phase")
                    .arg(m.phase.label())
                    .arg("day")
                    .arg(m.day)
                    .arg("next_change_at")
                    .arg(m.next_change_at)
                    .ignore();
            }
            _ => {}
        }
    }
    pipe
}

impl Sink for RedisSink {
    fn name(&self) -> &str {
        "redis"
    }

    fn write(&mut self, batch: &[Observation]) -> Result<(), String> {
        if self.connection.is_none() {
            let client = Client::open(self.url.as_str()).map_err(|e| e.to_string())?;
            self.connection = Some(client.get_connection().map_err(|e| e.to_string())?);
        }
        let connection = self.connection.as_mut().expect("connected above");
        let result = commands(batch)
            .query::<()>(connection)
            .map_err(|e| e.to_string());
        if result.is_err() {
            self.connection = None;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ome_core::symbol::Symbol;

    #[test]
    fn books_serialise_to_compact_json() {
        let book = BookSnapshot {
            symbol: Symbol::parse("ACME").unwrap(),
            timestamp: 7,
            bids: vec![Level {
                price: 24_90,
                qty: 100,
                orders: 2,
            }],
            asks: vec![],
        };
        assert_eq!(
            book_json(&book),
            r#"{"symbol":"ACME","ts":7,"bids":[[2490,100,2]],"asks":[]}"#
        );
    }
}
