mod app;
mod candle_chart;
mod command;
mod ui;

use app::{App, TICK_MS};
use ome_client::{Client, DEFAULT_ADDR};
use ome_core::symbol::Symbol;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use std::io;
use std::time::{Duration, Instant};

const USAGE: &str =
    "usage: ome-tui [--addr HOST:PORT] [--user NAME] [--secret SECRET] [--symbol SYMBOL]

defaults come from OME_ADDR, OME_USER, OME_SECRET; log on as an admin to get the admin console";

struct Options {
    addr: String,
    user: String,
    secret: String,
    symbol: Option<Symbol>,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        addr: std::env::var("OME_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.into()),
        user: std::env::var("OME_USER").unwrap_or_else(|_| "admin".into()),
        secret: std::env::var("OME_SECRET").unwrap_or_else(|_| "admin".into()),
        symbol: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--addr" => options.addr = value()?,
            "--user" => options.user = value()?,
            "--secret" => options.secret = value()?,
            "--symbol" => options.symbol = Some(Symbol::parse(&value()?).ok_or("invalid symbol")?),
            _ => return Err(USAGE.into()),
        }
    }
    Ok(options)
}

fn main() {
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let (client, session) =
        match Client::connect_and_logon(&options.addr, &options.user, &options.secret) {
            Ok(connected) => connected,
            Err(e) => {
                eprintln!("cannot log on to {} as {}: {e}", options.addr, options.user);
                std::process::exit(1);
            }
        };
    let app = App::new(
        Box::new(client),
        session,
        options.user,
        options.addr,
        options.symbol,
    );
    if let Err(e) = run(app) {
        eprintln!("terminal error: {e}");
        std::process::exit(1);
    }
}

fn run(mut app: App) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    let tick = Duration::from_millis(TICK_MS);
    let mut last_tick = Instant::now();
    app.on_tick();

    while !app.should_quit {
        if app.clear_screen {
            terminal.clear()?;
            app.clear_screen = false;
        }
        terminal.draw(|frame| ui::draw(frame, app))?;

        let timeout = tick.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            loop {
                if let Event::Key(key) = event::read()?
                    && key.kind == KeyEventKind::Press
                {
                    app.on_key(key);
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }

        if last_tick.elapsed() >= tick {
            app.on_tick();
            last_tick = Instant::now();
        }
    }
    Ok(())
}
