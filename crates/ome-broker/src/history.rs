use ome_core::candles::Candle;
use ome_core::symbol::Symbol;
use ome_net::{SessionId, log};
use ome_protocol::CandleSnapshot;
use postgres::{Client, NoTls};
use std::sync::mpsc::{self, Sender};
use std::thread;

pub struct HistoryJob {
    pub session: SessionId,
    pub symbol: Symbol,
    pub interval: u64,
    pub limit: u32,
}

pub struct HistoryResult {
    pub session: SessionId,
    pub symbol: Symbol,
    pub interval: u64,
    pub limit: u32,
    pub snapshot: Option<CandleSnapshot>,
}

const QUERY: &str = "
SELECT (floor(extract(epoch FROM traded_at)::float8 * 1000 / $2::float8) * $2::float8)::bigint AS bucket,
       (array_agg(price ORDER BY trade_id))[1],
       max(price),
       min(price),
       (array_agg(price ORDER BY trade_id DESC))[1],
       sum(qty)::bigint,
       count(*)
FROM trades
WHERE symbol = $1::text AND traded_at >= now() - make_interval(secs => $2::float8 * $3::float8 / 1000)
GROUP BY bucket
ORDER BY bucket";

fn load(client: &mut Client, job: &HistoryJob) -> Result<Vec<Candle>, postgres::Error> {
    let rows = client.query(
        QUERY,
        &[
            &job.symbol.to_string(),
            &(job.interval as f64),
            &(job.limit as f64),
        ],
    )?;
    Ok(rows
        .iter()
        .map(|row| Candle {
            start: row.get::<_, i64>(0) as u64,
            open: row.get::<_, i64>(1) as u64,
            high: row.get::<_, i64>(2) as u64,
            low: row.get::<_, i64>(3) as u64,
            close: row.get::<_, i64>(4) as u64,
            volume: row.get::<_, i64>(5) as u64,
            trades: row.get::<_, i64>(6) as u64,
        })
        .collect())
}

pub fn spawn<T: Send + 'static>(
    url: String,
    events: Sender<T>,
    wrap: fn(HistoryResult) -> T,
) -> Sender<HistoryJob> {
    let (tx, jobs) = mpsc::channel::<HistoryJob>();
    thread::spawn(move || {
        let mut client: Option<Client> = None;
        let mut warned = false;
        for job in jobs {
            if client.is_none() {
                match Client::connect(&url, NoTls) {
                    Ok(c) => {
                        log("chart history served from PostgreSQL");
                        warned = false;
                        client = Some(c);
                    }
                    Err(e) if !warned => {
                        log(&format!(
                            "PostgreSQL history unavailable ({e}); using memory"
                        ));
                        warned = true;
                    }
                    Err(_) => {}
                }
            }
            let candles = match client.as_mut().map(|c| load(c, &job)) {
                Some(Ok(candles)) => Some(candles),
                Some(Err(e)) => {
                    log(&format!("history query failed: {e}"));
                    client = None;
                    None
                }
                None => None,
            };
            let result = HistoryResult {
                session: job.session,
                symbol: job.symbol,
                interval: job.interval,
                limit: job.limit,
                snapshot: candles
                    .filter(|c| !c.is_empty())
                    .map(|candles| CandleSnapshot {
                        symbol: job.symbol,
                        interval: job.interval,
                        candles,
                    }),
            };
            if events.send(wrap(result)).is_err() {
                return;
            }
        }
    });
    tx
}
