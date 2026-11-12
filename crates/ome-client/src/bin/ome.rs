use ome_client::{Client, ClientError, DEFAULT_ADDR};
use ome_core::price::{format_price, parse_amount, parse_price};
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, OrderKind, Side};
use ome_protocol::{AccountInfo, Asset, ClientMessage, OrderStatus, Role, ServerMessage};
use std::time::Duration;

const USAGE: &str = "usage: ome [--addr HOST:PORT] [--user NAME] [--secret SECRET] <command>

connection defaults come from OME_ADDR, OME_USER, OME_SECRET

admin commands:
  accounts                                         list accounts
  create-account <name> <secret> <role> <cash>     role: retail | institutional | bot | admin
  set-cash <account> <amount>                      set available cash, e.g. 25000.00
  set-shares <account> <SYMBOL> <qty>              set available shares
  ipo <SYMBOL> <price> <shares> <issuer> <name..>  list a new instrument; issuer offers all shares
  halt <SYMBOL> [reason..]                         stop trading
  resume <SYMBOL>                                  resume trading

trader commands:
  balance                                          cash and positions
  buy|sell <SYMBOL> <price|mkt> <qty>              place an order and wait for the result
  cancel <order_id>
  cancel-all [SYMBOL]

anyone:
  instruments                                      list instruments and status
  watch <SYMBOL>                                   stream trades until Ctrl-C

<account> may be an account id or a username";

struct Options {
    addr: String,
    user: String,
    secret: String,
    command: Vec<String>,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        addr: std::env::var("OME_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.into()),
        user: std::env::var("OME_USER").unwrap_or_else(|_| "admin".into()),
        secret: std::env::var("OME_SECRET").unwrap_or_else(|_| "admin".into()),
        command: Vec::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--addr" => options.addr = args.next().ok_or("--addr needs a value")?,
            "--user" => options.user = args.next().ok_or("--user needs a value")?,
            "--secret" => options.secret = args.next().ok_or("--secret needs a value")?,
            "-h" | "--help" => return Err(USAGE.into()),
            _ => {
                options.command.push(arg);
                options.command.extend(args.by_ref());
            }
        }
    }
    if options.command.is_empty() {
        return Err(USAGE.into());
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
    if let Err(e) = run(options) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn symbol(text: &str) -> Result<Symbol, String> {
    Symbol::parse(text).ok_or(format!("invalid symbol '{text}'"))
}

fn price(text: &str) -> Result<u64, String> {
    parse_price(text).ok_or(format!("invalid price '{text}'"))
}

fn amount(text: &str) -> Result<u64, String> {
    parse_amount(text).ok_or(format!("invalid amount '{text}'"))
}

fn number(text: &str) -> Result<u64, String> {
    text.parse().map_err(|_| format!("invalid number '{text}'"))
}

fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let (mut client, _) = Client::connect_and_logon(&options.addr, &options.user, &options.secret)?;
    let args: Vec<&str> = options.command.iter().map(String::as_str).collect();

    match args.as_slice() {
        ["accounts"] => {
            for a in accounts(&mut client)? {
                println!(
                    "{:>4}  {:<16} {:<13} cash {:>14}  reserved {:>12}  open orders {}",
                    a.account_id,
                    a.username,
                    a.role.label(),
                    format_price(a.cash.available),
                    format_price(a.cash.reserved),
                    a.open_orders
                );
            }
        }
        ["create-account", name, secret, role, cash] => {
            let role =
                Role::parse(role).ok_or("role must be retail, institutional, bot or admin")?;
            let text = client.request(&ClientMessage::CreateAccount {
                role,
                cash: amount(cash)?,
                username: name.to_string(),
                secret: secret.to_string(),
            })?;
            println!("{text}");
        }
        ["set-cash", account, value] => {
            let account_id = resolve(&mut client, account)?;
            let text = client.request(&ClientMessage::SetBalance {
                account_id,
                asset: Asset::Cash,
                amount: amount(value)?,
            })?;
            println!("{text}");
        }
        ["set-shares", account, sym, qty] => {
            let account_id = resolve(&mut client, account)?;
            let text = client.request(&ClientMessage::SetBalance {
                account_id,
                asset: Asset::Shares(symbol(sym)?),
                amount: number(qty)?,
            })?;
            println!("{text}");
        }
        ["ipo", sym, ipo_price, shares, issuer, name @ ..] => {
            let issuer = resolve(&mut client, issuer)?;
            let text = client.request(&ClientMessage::ListInstrument {
                symbol: symbol(sym)?,
                ipo_price: price(ipo_price)?,
                shares: number(shares)?,
                issuer,
                name: name.join(" "),
            })?;
            println!("{text}");
        }
        ["halt", sym, reason @ ..] => {
            let text = client.request(&ClientMessage::Halt {
                symbol: symbol(sym)?,
                reason: reason.join(" "),
            })?;
            println!("{text}");
        }
        ["resume", sym] => {
            println!(
                "{}",
                client.request(&ClientMessage::Resume {
                    symbol: symbol(sym)?
                })?
            );
        }
        ["instruments"] => {
            client.send(&ClientMessage::QueryInstruments)?;
            let list = match client.wait_for(Duration::from_secs(5), |m| {
                matches!(m, ServerMessage::InstrumentList(_))
            })? {
                ServerMessage::InstrumentList(list) => list,
                _ => unreachable!(),
            };
            for i in list {
                let last = i.last_price.map_or("-".to_string(), format_price);
                println!(
                    "{:<8} {:<8} last {:>10}  ref {:>10}  limits {}-{}  vol {:>10}  {} {}",
                    i.symbol,
                    format!("{:?}", i.status).to_uppercase(),
                    last,
                    format_price(i.reference_price),
                    format_price(i.low_limit),
                    format_price(i.high_limit),
                    i.volume,
                    i.name,
                    i.halt_reason
                );
            }
        }
        ["balance"] => {
            client.send(&ClientMessage::QueryAccount)?;
            let snapshot = match client.wait_for(Duration::from_secs(5), |m| {
                matches!(m, ServerMessage::Account(_))
            })? {
                ServerMessage::Account(a) => a,
                _ => unreachable!(),
            };
            println!(
                "account {} ({}) cash {} (reserved {})",
                snapshot.account_id,
                snapshot.role.label(),
                format_price(snapshot.cash.available),
                format_price(snapshot.cash.reserved)
            );
            for p in snapshot.positions {
                println!(
                    "  {:<8} {:>10} (reserved {})",
                    p.symbol, p.balance.available, p.balance.reserved
                );
            }
        }
        [side @ ("buy" | "sell"), sym, px, qty] => {
            let side = if *side == "buy" {
                Side::Buy
            } else {
                Side::Sell
            };
            let kind = if px.eq_ignore_ascii_case("mkt") {
                OrderKind::Market
            } else {
                OrderKind::Limit(price(px)?)
            };
            let client_order_id = client.send_order(symbol(sym)?, side, kind, number(qty)?)?;
            let first_wait = Duration::from_secs(5);
            let mut wait = first_wait;
            loop {
                let found = client.wait_for(wait, |m| {
                    matches!(m, ServerMessage::Execution(r) if r.client_order_id == client_order_id)
                });
                let r = match found {
                    Ok(ServerMessage::Execution(r)) => r,
                    Err(ClientError::Timeout) if wait < first_wait => {
                        println!("order is resting in the book");
                        break;
                    }
                    Err(e) => return Err(e.into()),
                    Ok(_) => unreachable!(),
                };
                println!(
                    "#{} {} {} filled {}/{} last {} x {} {}",
                    r.order_id,
                    r.status.label(),
                    r.side.label(),
                    r.filled,
                    r.qty,
                    format_price(r.last_price),
                    r.last_qty,
                    r.reason
                );
                if !r.status.is_open() {
                    break;
                }
                wait = Duration::from_millis(300);
            }
        }
        ["cancel", id] => {
            let order_id = number(id)?;
            client.send(&ClientMessage::Cancel { order_id })?;
            match client.wait_for(Duration::from_secs(5), |m| match m {
                ServerMessage::Execution(r) => r.order_id == order_id,
                ServerMessage::Reject { .. } => true,
                _ => false,
            })? {
                ServerMessage::Reject { reason, .. } => {
                    return Err(ClientError::Rejected(reason).into());
                }
                _ => println!("cancelled #{order_id}"),
            }
        }
        ["cancel-all", rest @ ..] => {
            let symbol = rest.first().map(|s| symbol(s)).transpose()?;
            client.send(&ClientMessage::CancelAll { symbol })?;
            std::thread::sleep(Duration::from_millis(300));
            let mut count = 0;
            while let Some(m) = client.try_recv()? {
                if matches!(m, ServerMessage::Execution(r) if r.status == OrderStatus::Cancelled) {
                    count += 1;
                }
            }
            println!("cancelled {count} order(s)");
        }
        ["watch", sym] => {
            client.send(&ClientMessage::Subscribe {
                symbol: symbol(sym)?,
            })?;
            loop {
                match client.recv_timeout(Duration::from_secs(10))? {
                    Some(ServerMessage::Trade(t)) => println!(
                        "{} {} {:>10} x {}",
                        t.symbol,
                        if t.aggressor == Side::Buy {
                            "▲"
                        } else {
                            "▼"
                        },
                        format_price(t.price),
                        t.qty
                    ),
                    Some(ServerMessage::Instrument(i)) if !i.halt_reason.is_empty() => {
                        println!("{} HALTED: {}", i.symbol, i.halt_reason)
                    }
                    _ => {}
                }
            }
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
    Ok(())
}

fn accounts(client: &mut Client) -> Result<Vec<AccountInfo>, ClientError> {
    client.send(&ClientMessage::QueryAccounts)?;
    match client.wait_for(Duration::from_secs(5), |m| {
        matches!(
            m,
            ServerMessage::AccountList(_) | ServerMessage::Reject { .. }
        )
    })? {
        ServerMessage::AccountList(list) => Ok(list),
        ServerMessage::Reject { reason, .. } => Err(ClientError::Rejected(reason)),
        _ => unreachable!(),
    }
}

fn resolve(client: &mut Client, account: &str) -> Result<AccountId, Box<dyn std::error::Error>> {
    if let Ok(id) = account.parse() {
        return Ok(id);
    }
    accounts(client)?
        .into_iter()
        .find(|a| a.username == account)
        .map(|a| a.account_id)
        .ok_or_else(|| format!("no account named '{account}'").into())
}
