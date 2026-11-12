use ome_client::{Client, ClientError, DEFAULT_BROKER_ADDR, DEFAULT_EXCHANGE_ADDR};
use ome_core::price::{format_price, parse_amount, parse_price};
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, OrderKind, Side};
use ome_protocol::{
    AccountInfo, Asset, ClientMessage, InstrumentInfo, MemberInfo, MemberKind, MemberMessage,
    OrderStatus, Role, ServerMessage,
};
use std::time::Duration;

const USAGE: &str = "usage:
  ome exchange [--addr HOST:PORT] [--member NAME] [--secret SECRET] <command>
      members                                             list exchange members
      add-member <name> <secret> <broker|lp>              admit a broker or liquidity provider
      ipo <SYMBOL> <price> <shares> <underwriter> <issuer-account> <name..>
                                                          list a company; underwriter is a member name
      halt <SYMBOL> [reason..]  |  resume <SYMBOL>
      instruments  |  watch <SYMBOL>

  ome broker [--addr HOST:PORT] [--user NAME] [--secret SECRET] <command>
      accounts                                            (broker admin)
      create-account <name> <secret> <role> <cash>        role: retail quant hft prop lp issuer admin
      set-cash <account> <amount>  |  set-shares <account> <SYMBOL> <qty>
      balance                                             (trader)
      buy|sell <SYMBOL> <price|mkt> <qty>
      cancel <order_id>  |  cancel-all [SYMBOL]
      instruments  |  watch <SYMBOL>

defaults: OME_EXCHANGE / OME_MEMBER / OME_MEMBER_SECRET and OME_BROKER / OME_USER / OME_SECRET
<account> may be an id or a username";

enum Target {
    Exchange,
    Broker,
}

struct Options {
    target: Target,
    addr: String,
    user: String,
    secret: String,
    command: Vec<String>,
}

fn env(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.into())
}

fn parse_options() -> std::result::Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mut options = match args.next().as_deref() {
        Some("exchange") => Options {
            target: Target::Exchange,
            addr: env("OME_EXCHANGE", DEFAULT_EXCHANGE_ADDR),
            user: env("OME_MEMBER", "ops"),
            secret: env("OME_MEMBER_SECRET", "ops"),
            command: Vec::new(),
        },
        Some("broker") => Options {
            target: Target::Broker,
            addr: env("OME_BROKER", DEFAULT_BROKER_ADDR),
            user: env("OME_USER", "admin"),
            secret: env("OME_SECRET", "admin"),
            command: Vec::new(),
        },
        _ => return Err(USAGE.into()),
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--addr" => options.addr = args.next().ok_or("--addr needs a value")?,
            "--member" | "--user" => options.user = args.next().ok_or("--user needs a value")?,
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
    let args: Vec<&str> = options.command.iter().map(String::as_str).collect();
    let result = match options.target {
        Target::Exchange => Client::connect_member(&options.addr, &options.user, &options.secret)
            .map_err(Into::into)
            .and_then(|(mut client, _)| exchange(&mut client, &args)),
        Target::Broker => Client::connect_and_logon(&options.addr, &options.user, &options.secret)
            .map_err(Into::into)
            .and_then(|(mut client, _)| broker(&mut client, &args)),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn symbol(text: &str) -> std::result::Result<Symbol, String> {
    Symbol::parse(text).ok_or(format!("invalid symbol '{text}'"))
}

fn price(text: &str) -> std::result::Result<u64, String> {
    parse_price(text).ok_or(format!("invalid price '{text}'"))
}

fn amount(text: &str) -> std::result::Result<u64, String> {
    parse_amount(text).ok_or(format!("invalid amount '{text}'"))
}

fn number(text: &str) -> std::result::Result<u64, String> {
    text.parse().map_err(|_| format!("invalid number '{text}'"))
}

fn usage() -> Result<()> {
    eprintln!("{USAGE}");
    std::process::exit(2);
}

fn exchange(client: &mut Client, args: &[&str]) -> Result<()> {
    match args {
        ["members"] => {
            for m in members(client)? {
                println!(
                    "{:>4}  {:<20} {:<20} {}  open orders {}",
                    m.member_id,
                    m.name,
                    m.kind.label(),
                    if m.connected { "online " } else { "offline" },
                    m.open_orders
                );
            }
        }
        ["add-member", name, secret, kind] => {
            let kind = match MemberKind::parse(kind) {
                Some(MemberKind::Admin) | None => {
                    return Err("member kind must be broker or lp".into());
                }
                Some(kind) => kind,
            };
            println!(
                "{}",
                client.request_member(&MemberMessage::CreateMember {
                    kind,
                    name: name.to_string(),
                    secret: secret.to_string()
                })?
            );
        }
        [
            "ipo",
            sym,
            ipo_price,
            shares,
            underwriter,
            issuer,
            name @ ..,
        ] => {
            let underwriter = match underwriter.parse() {
                Ok(id) => id,
                Err(_) => members(client)?
                    .into_iter()
                    .find(|m| m.name == *underwriter)
                    .map(|m| m.member_id)
                    .ok_or(format!("no member named '{underwriter}'"))?,
            };
            println!(
                "{}",
                client.request_member(&MemberMessage::ListInstrument {
                    symbol: symbol(sym)?,
                    ipo_price: price(ipo_price)?,
                    shares: number(shares)?,
                    underwriter,
                    issuer_account: number(issuer)?,
                    name: name.join(" "),
                })?
            );
        }
        ["halt", sym, reason @ ..] => {
            println!(
                "{}",
                client.request_member(&MemberMessage::Halt {
                    symbol: symbol(sym)?,
                    reason: reason.join(" ")
                })?
            );
        }
        ["resume", sym] => println!(
            "{}",
            client.request_member(&MemberMessage::Resume {
                symbol: symbol(sym)?
            })?
        ),
        ["instruments"] => {
            client.send_member(&MemberMessage::QueryInstruments)?;
            print_instruments(client)?;
        }
        ["watch", sym] => {
            client.send_member(&MemberMessage::Subscribe {
                symbol: symbol(sym)?,
            })?;
            watch(client)?;
        }
        _ => usage()?,
    }
    Ok(())
}

fn broker(client: &mut Client, args: &[&str]) -> Result<()> {
    match args {
        ["accounts"] => {
            for a in accounts(client)? {
                println!(
                    "{:>4}  {:<18} {:<7} cash {:>16}  reserved {:>14}  open orders {}",
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
            let role = Role::parse(role)
                .ok_or("role must be retail, quant, hft, prop, lp, issuer or admin")?;
            println!(
                "{}",
                client.request(&ClientMessage::CreateAccount {
                    role,
                    cash: amount(cash)?,
                    username: name.to_string(),
                    secret: secret.to_string()
                })?
            );
        }
        ["set-cash", account, value] => {
            let account_id = resolve(client, account)?;
            println!(
                "{}",
                client.request(&ClientMessage::SetBalance {
                    account_id,
                    asset: Asset::Cash,
                    amount: amount(value)?
                })?
            );
        }
        ["set-shares", account, sym, qty] => {
            let account_id = resolve(client, account)?;
            println!(
                "{}",
                client.request(&ClientMessage::SetBalance {
                    account_id,
                    asset: Asset::Shares(symbol(sym)?),
                    amount: number(qty)?
                })?
            );
        }
        ["balance"] => {
            client.send(&ClientMessage::QueryAccount)?;
            let ServerMessage::Account(snapshot) = client
                .wait_for(Duration::from_secs(5), |m| {
                    matches!(m, ServerMessage::Account(_))
                })?
            else {
                unreachable!()
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
                let r = match client.wait_for(wait, |m| matches!(m, ServerMessage::Execution(r) if r.client_order_id == client_order_id)) {
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
                ServerMessage::Execution(r) => r.order_id == order_id && !r.status.is_open(),
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
        ["instruments"] => {
            client.send(&ClientMessage::QueryInstruments)?;
            print_instruments(client)?;
        }
        ["watch", sym] => {
            client.send(&ClientMessage::Subscribe {
                symbol: symbol(sym)?,
            })?;
            watch(client)?;
        }
        _ => usage()?,
    }
    Ok(())
}

fn print_instruments(client: &mut Client) -> Result<()> {
    let ServerMessage::InstrumentList(list) = client.wait_for(Duration::from_secs(5), |m| {
        matches!(m, ServerMessage::InstrumentList(_))
    })?
    else {
        unreachable!()
    };
    for i in list {
        print_instrument(&i);
    }
    Ok(())
}

fn print_instrument(i: &InstrumentInfo) {
    println!(
        "{:<8} {:<8} last {:>10}  ref {:>10}  limits {}-{}  vol {:>10}  {} {}",
        i.symbol,
        format!("{:?}", i.status).to_uppercase(),
        i.last_price.map_or("-".to_string(), format_price),
        format_price(i.reference_price),
        format_price(i.low_limit),
        format_price(i.high_limit),
        i.volume,
        i.name,
        i.halt_reason
    );
}

fn watch(client: &mut Client) -> Result<()> {
    loop {
        match client.recv_timeout(Duration::from_secs(10))? {
            Some(ServerMessage::Trade(t)) if t.trade_id != 0 => println!(
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

fn members(client: &mut Client) -> std::result::Result<Vec<MemberInfo>, ClientError> {
    client.send_member(&MemberMessage::QueryMembers)?;
    match client.wait_for(Duration::from_secs(5), |m| {
        matches!(
            m,
            ServerMessage::MemberList(_) | ServerMessage::Reject { .. }
        )
    })? {
        ServerMessage::MemberList(list) => Ok(list),
        ServerMessage::Reject { reason, .. } => Err(ClientError::Rejected(reason)),
        _ => unreachable!(),
    }
}

fn accounts(client: &mut Client) -> std::result::Result<Vec<AccountInfo>, ClientError> {
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

fn resolve(client: &mut Client, account: &str) -> Result<AccountId> {
    if let Ok(id) = account.parse() {
        return Ok(id);
    }
    accounts(client)?
        .into_iter()
        .find(|a| a.username == account)
        .map(|a| a.account_id)
        .ok_or_else(|| format!("no account named '{account}'").into())
}
