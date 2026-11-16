mod app;
mod desk;
mod dialogs;
mod pages;
mod prefs;
mod session;
mod square;
mod ticket;
mod ui;
mod view;

#[cfg(test)]
mod tests;

use app::RetailApp;
use view::Saved;

const VIEWS: &str = "retail-views";

struct Launch {
    broker: Option<String>,
    user: Option<String>,
    secret: Option<String>,
    interval: Option<u64>,
}

fn launch() -> Launch {
    let mut launch = Launch {
        broker: std::env::var("OME_BROKER").ok(),
        user: std::env::var("OME_USER").ok(),
        secret: std::env::var("OME_SECRET").ok(),
        interval: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next();
        match flag.as_str() {
            "--broker" => launch.broker = value,
            "--user" => launch.user = value,
            "--secret" => launch.secret = value,
            "--interval" => launch.interval = value.and_then(|v| v.parse::<u64>().ok()).map(|s| s * 1_000),
            _ => {
                eprintln!("usage: ome-retail [--broker HOST:PORT] [--user NAME] [--secret SECRET] [--interval SECONDS]");
                std::process::exit(2);
            }
        }
    }
    launch
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

struct Desktop(RetailApp);

impl eframe::App for Desktop {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.draw(ui, now_ms());
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.0.persist();
        eframe::set_value(storage, VIEWS, &self.0.saved);
    }
}

fn main() -> eframe::Result {
    let launch = launch();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OME Trader")
            .with_inner_size([1600.0, 980.0])
            .with_min_inner_size([1100.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "ome-retail",
        options,
        Box::new(move |cc| {
            let saved: Saved = cc.storage.and_then(|s| eframe::get_value(s, VIEWS)).unwrap_or_default();
            let file = prefs::path();
            let mut app = RetailApp::new(prefs::Prefs::load(file.as_deref()), file, session::broker(), saved);
            if let Some(broker) = launch.broker {
                app.core.login.broker = broker;
            }
            if let Some(user) = launch.user {
                app.core.login.user = user;
            }
            if let Some(interval) = launch.interval {
                app.core.interval = interval.max(1_000);
            }
            if let Some(secret) = launch.secret {
                app.core.login.password = secret;
                app.core.log_in();
            }
            Ok(Box::new(Desktop(app)))
        }),
    )
}
