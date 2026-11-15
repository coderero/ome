mod app;
mod charts;
mod login;
mod monitor;
mod news;
mod prefs;
mod shell;
mod ticket;
mod trade;

use gpui::{
    App, Bounds, QuitMode, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size,
};

pub struct Launch {
    pub broker: Option<String>,
    pub user: Option<String>,
    pub secret: Option<String>,
    pub tab: Option<String>,
    pub interval: Option<u64>,
}

fn launch() -> Launch {
    let mut launch = Launch {
        broker: std::env::var("OME_BROKER").ok(),
        user: std::env::var("OME_USER").ok(),
        secret: std::env::var("OME_SECRET").ok(),
        tab: None,
        interval: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next();
        match flag.as_str() {
            "--broker" => launch.broker = value,
            "--user" => launch.user = value,
            "--secret" => launch.secret = value,
            "--tab" => launch.tab = value,
            "--interval" => {
                launch.interval = value.and_then(|v| v.parse::<u64>().ok()).map(|s| s * 1_000)
            }
            _ => {
                eprintln!(
                    "usage: ome-retail [--broker HOST:PORT] [--user NAME] [--secret SECRET] [--tab monitor|trade|charts|news] [--interval SECONDS]"
                );
                std::process::exit(2);
            }
        }
    }
    launch
}

fn main() {
    let launch = launch();
    gpui_platform::application()
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx: &mut App| {
            ome_ui_kit::fonts::register(cx);
            app::bind_keys(cx);
            let bounds = Bounds::centered(None, size(px(1600.), px(980.)), cx);
            cx.open_window(
                WindowOptions {
                    titlebar: Some(TitlebarOptions {
                        title: Some("OME Trader".into()),
                        ..Default::default()
                    }),
                    ..WindowOptions::default().window_bounds(WindowBounds::Windowed(bounds))
                },
                move |window, cx| {
                    cx.new(|cx| {
                        let mut app = app::RetailApp::new(window, cx);
                        app.prefill(launch, window, cx);
                        app
                    })
                },
            )
            .expect("cannot open the window");
            cx.activate(true);
        });
}
