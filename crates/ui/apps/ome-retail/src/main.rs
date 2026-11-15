mod app;
mod ticket;

use gpui::{App, Bounds, QuitMode, WindowBounds, WindowOptions, prelude::*, px, size};
use ome_ui_data::Credentials;

const USAGE: &str = "usage: ome-retail [--broker HOST:PORT] [--user NAME] [--secret SECRET]
defaults: OME_BROKER (127.0.0.1:7101), OME_USER (alice), OME_SECRET (alice-pw)";

fn credentials() -> Result<Credentials, String> {
    let env = |name: &str, fallback: &str| std::env::var(name).unwrap_or_else(|_| fallback.into());
    let mut credentials = Credentials {
        addr: env("OME_BROKER", "127.0.0.1:7101"),
        user: env("OME_USER", "alice"),
        secret: env("OME_SECRET", "alice-pw"),
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--broker" => credentials.addr = value()?,
            "--user" => credentials.user = value()?,
            "--secret" => credentials.secret = value()?,
            _ => return Err(USAGE.into()),
        }
    }
    Ok(credentials)
}

fn main() {
    let credentials = match credentials() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    gpui_platform::application()
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx: &mut App| {
            app::bind_keys(cx);
            let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            let title = format!("OME Retail · {}", credentials.user);
            cx.open_window(
                WindowOptions {
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some(title.into()),
                        ..Default::default()
                    }),
                    ..WindowOptions::default().window_bounds(WindowBounds::Windowed(bounds))
                },
                move |window, cx| cx.new(|cx| app::RetailApp::new(credentials, window, cx)),
            )
            .expect("cannot open the window");
            cx.activate(true);
        });
}
