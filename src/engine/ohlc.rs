use chrono::{DateTime, TimeZone, Utc};

#[derive(Default, Debug)]
pub struct OhlcTracker {
    open: Option<f64>,
    high: f64,
    low: f64,
    close: Option<f64>,
    last_timestamp: Option<i64>,
}

impl OhlcTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, price: f64, timestamp: i64) {
        if self.open.is_none() {
            self.open = Some(price);
            self.low = price;
            self.high = price;
        } else {
            if price > self.high {
                self.high = price;
            }
            if price < self.low {
                self.low = price;
            }
        }
        self.close = Some(price);
        self.last_timestamp = Some(timestamp);
    }

    pub fn print(&self) {
        if let Some(open) = self.open {
            let ts = self.last_timestamp.unwrap_or(0);
            let dt: DateTime<Utc> = Utc.timestamp_millis_opt(ts).unwrap();
            println!("OHLC [{}] => O: {:.2}, H: {:.2}, L: {:.2}, C: {:.2}", dt.to_rfc3339(), open, self.high, self.low, self.close.unwrap());
        }
    }
}