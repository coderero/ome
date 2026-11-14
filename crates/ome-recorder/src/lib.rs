pub mod observation;
pub mod postgres_sink;
pub mod redis_sink;
pub mod sink;
pub mod sources;

use observation::Observation;
use ome_net::log;
use sink::{Buffered, Sink};
use sources::{Endpoint, Source};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const FLUSH: Duration = Duration::from_millis(500);
const REPORT: Duration = Duration::from_secs(30);
const BACKLOG: usize = 200_000;

pub struct RecorderConfig {
    pub exchange: Endpoint,
    pub clearing: Option<Endpoint>,
    pub brokers: Vec<Endpoint>,
    pub venues: Vec<Endpoint>,
    pub news: Option<Endpoint>,
    pub postgres: Option<String>,
    pub redis: Option<String>,
    pub poll_ms: u64,
}

pub fn start(config: RecorderConfig) -> Result<JoinHandle<()>, String> {
    let mut sinks: Vec<Box<dyn Sink>> = Vec::new();
    if let Some(url) = &config.postgres {
        sinks.push(Box::new(postgres_sink::PostgresSink::new(url)));
    }
    if let Some(url) = &config.redis {
        sinks.push(Box::new(redis_sink::RedisSink::new(url)));
    }
    if sinks.is_empty() {
        return Err("nothing to record into: pass --postgres and/or --redis".into());
    }
    let (tx, rx) = mpsc::channel();
    let every = Duration::from_millis(config.poll_ms.max(200));
    sources::spawn(Source::Exchange, config.exchange, every, tx.clone());
    if let Some(clearing) = config.clearing {
        sources::spawn(Source::Clearing, clearing, every, tx.clone());
    }
    for broker in config.brokers {
        sources::spawn(Source::Broker, broker, every, tx.clone());
    }
    for venue in config.venues {
        sources::spawn(Source::Venue, venue, every, tx.clone());
    }
    if let Some(news) = config.news {
        sources::spawn_news(news, tx.clone());
    }
    drop(tx);
    let sinks = sinks
        .into_iter()
        .map(|s| Buffered::new(s, BACKLOG))
        .collect();
    Ok(thread::spawn(move || run(rx, sinks)))
}

pub fn run(rx: Receiver<Observation>, mut sinks: Vec<Buffered>) {
    let mut batch = Vec::new();
    let mut next_flush = Instant::now() + FLUSH;
    let mut next_report = Instant::now() + REPORT;
    loop {
        match rx.recv_timeout(next_flush.saturating_duration_since(Instant::now())) {
            Ok(observation) => {
                batch.push(observation);
                batch.extend(rx.try_iter());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                flush(&mut sinks, &mut batch);
                return;
            }
        }
        if Instant::now() >= next_flush {
            flush(&mut sinks, &mut batch);
            next_flush = Instant::now() + FLUSH;
        }
        if Instant::now() >= next_report {
            next_report = Instant::now() + REPORT;
            for sink in &sinks {
                log(&format!(
                    "{}: {} records written, {} pending, {} dropped",
                    sink.name(),
                    sink.written(),
                    sink.pending(),
                    sink.dropped()
                ));
            }
        }
    }
}

fn flush(sinks: &mut [Buffered], batch: &mut Vec<Observation>) {
    let batch = std::mem::take(batch);
    for sink in sinks.iter_mut() {
        sink.push(&batch);
        let was_healthy = sink.is_healthy();
        match sink.flush() {
            Ok(_) if !was_healthy => log(&format!("{} is back; backlog written", sink.name())),
            Err(e) if was_healthy => log(&format!(
                "{} unavailable ({e}); buffering up to {BACKLOG} records",
                sink.name()
            )),
            _ => {}
        }
    }
}
