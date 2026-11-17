use crate::writer::Copy;
use ome_llm::{Model, offline};
use ome_net::log;
use ome_protocol::NewsItem;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

pub struct Job {
    pub item: NewsItem,
    pub company: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub seq: u64,
    pub result: Result<Copy, String>,
    pub elapsed_ms: u64,
}

pub fn spawn_writer<T: Send + 'static>(
    llm: Box<dyn Model>,
    jobs: Receiver<Job>,
    events: Sender<T>,
    wrap: fn(Done) -> T,
) {
    thread::spawn(move || {
        let started = Instant::now();
        match llm.warm_up() {
            Ok(()) => log(&format!(
                "LLM writer ready: {} loaded in {:?}",
                llm.name(),
                started.elapsed()
            )),
            Err(e) => log(&format!(
                "LLM writer offline ({e}); stories keep their template text"
            )),
        }
        let mut offline_until: Option<Instant> = None;
        for job in jobs {
            let begun = Instant::now();
            let result = if offline_until.is_some_and(|t| t > begun) {
                Err("LLM offline".to_string())
            } else {
                let result = crate::llm::rewrite(llm.as_ref(), &job.item, &job.company);
                if let Err(e) = &result
                    && offline(e)
                {
                    offline_until = Some(Instant::now() + Duration::from_secs(10));
                }
                result
            };
            let done = Done {
                seq: job.item.seq,
                result,
                elapsed_ms: begun.elapsed().as_millis() as u64,
            };
            if events.send(wrap(done)).is_err() {
                return;
            }
        }
    });
}
