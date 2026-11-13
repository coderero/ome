use ome_client::{Client, ClientError, DEFAULT_BROKER_ADDR, DEFAULT_EXCHANGE_ADDR};
use ome_core::price::{format_price, parse_amount, parse_price};
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, OrderKind, OrderOptions, Side, StpMode, TimeInForce};
use ome_protocol::{
    AccountInfo, Asset, ClientMessage, InstrumentInfo, MarketPhase, MemberInfo, MemberKind,
    MemberMessage, OrderStatus, Role, ServerMessage,
};
use std::time::Duration;

const USAGE: &str = "usage:
  ome exchange [--addr HOST:PORT] [--member NAME] [--secret SECRET] <command>
      members                                             list exchange members
      add-member <name> <secret> <broker|lp>              admit a broker or liquidity provider
      ipo <SYMBOL> <price> <shares> <underwriter> <issuer-account> <name..>
                                                          list a company; underwriter is a member name
      halt <SYMBOL> [reason..]  |  resume <SYMBOL>
      status                                              market phase and trading day
      session <preopen|open|auction|close>                change the market phase now
      schedule <preopen> <open> <closing> <closed>        repeating trading day in seconds; `schedule off`
      instruments  |  watch <SYMBOL>

  ome broker [--addr HOST:PORT] [--user NAME] [--secret SECRET] <command>
      accounts  |  fees                                   (broker admin)
      create-account <name> <secret> <role> <cash>        role: retail quant hft prop lp issuer admin
      set-cash <account> <amount>  |  set-shares <account> <SYMBOL> <qty>
      balance                                             (trader)
      buy|sell <SYMBOL> <price|mkt> <qty> [options..]
                                                          options: gtc day ioc fok auction gtd:<seconds>
                                                          post show:<qty> stp:keep|new|both
      market                                              market phase and trading day
      cancel <order_id>  |  cancel-all [SYMBOL]
      instruments  |  watch <SYMBOL>

  ome clearing [--addr HOST:PORT] [--user NAME] [--secret SECRET] <command>
      status                                              cycles, collateral, margin, credit, fails
      register <member-name> <secret>                     make an exchange member a clearing member
      deposit <member> <amount>  |  withdraw <member> <amount>

defaults: OME_EXCHANGE / OME_MEMBER / OME_MEMBER_SECRET and OME_BROKER / OME_USER / OME_SECRET
<account> may be an id or a username";

enum Target {
    Exchange,
    Broker,
    Clearing,
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
        Some("clearing") => Options {
            target: Target::Clearing,
            addr: env("OME_CLEARING", "127.0.0.1:7200"),
            user: env("OME_CLEARING_OPS", "ops"),
            secret: env("OME_CLEARING_SECRET", "ops"),
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
        Target::Clearing => Client::connect_member(&options.addr, &options.user, &options.secret)
            .map_err(Into::into)
            .and_then(|(mut client, _)| clearing(&mut client, &args)),
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
                    "{:>4}  {:<20} {:<20} {}  open orders {:<6} net fees {:<10} exposure {:<14} credit {}",
                    m.member_id,
                    m.name,
                    m.kind.label(),
                    if m.connected { "online " } else { "offline" },
                    m.open_orders,
                    format_signed(m.net_fees),
                    format_price(m.exposure),
                    if m.credit_limit == u64::MAX {
                        "unlimited".to_string()
                    } else {
                        format_price(m.credit_limit)
                    }
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
        ["status"] => print_status(client)?,
        ["session", phase] => {
            let phase =
                MarketPhase::parse(phase).ok_or("phase must be preopen, open, auction or close")?;
            println!(
                "{}",
                client.request_member(&MemberMessage::SetSession { phase })?
            );
        }
        ["schedule", "off"] => println!(
            "{}",
            client.request_member(&MemberMessage::SetSchedule {
                pre_open_ms: 0,
                continuous_ms: 0,
                closing_ms: 0,
                closed_ms: 0,
            })?
        ),
        ["schedule", pre_open, open, closing, closed] => {
            let ms = |text: &str| number(text).map(|s| s * 1_000);
            println!(
                "{}",
                client.request_member(&MemberMessage::SetSchedule {
                    pre_open_ms: ms(pre_open)?,
                    continuous_ms: ms(open)?,
                    closing_ms: ms(closing)?,
                    closed_ms: ms(closed)?,
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
        ["fees"] => {
            client.send(&ClientMessage::QueryFees)?;
            let ServerMessage::FeeSummary(summary) =
                client.wait_for(Duration::from_secs(5), |m| {
                    matches!(
                        m,
                        ServerMessage::FeeSummary(_) | ServerMessage::Reject { .. }
                    )
                })?
            else {
                return Err("fees are only visible to the broker admin".into());
            };
            let c = summary.charges;
            println!("fills            {}", summary.fills);
            println!("brokerage        {}", format_price(c.commission));
            println!("exchange fees    {}", format_signed(c.exchange_fee));
            println!("transaction tax  {}", format_price(c.transaction_tax));
            println!("stamp duty       {}", format_price(c.stamp_duty));
            println!("regulatory fee   {}", format_price(c.regulatory_fee));
            println!("GST              {}", format_price(c.gst));
            println!("total charged    {}", format_signed(c.total()));
        }
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
            if snapshot.unsettled_cash != 0 {
                println!("unsettled cash {}", format_signed(snapshot.unsettled_cash));
            }
            for p in snapshot.positions {
                println!(
                    "  {:<8} {:>10} (reserved {}, unsettled {})",
                    p.symbol, p.balance.available, p.balance.reserved, p.unsettled
                );
            }
        }
        ["market"] => print_status(client)?,
        [side @ ("buy" | "sell"), sym, px, qty, extras @ ..] => {
            let options = order_options(extras)?;
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
            let client_order_id = match options {
                Some(options) => {
                    client.send_order_with(symbol(sym)?, side, kind, options, number(qty)?)?
                }
                None => client.send_order(symbol(sym)?, side, kind, number(qty)?)?,
            };
            let first_wait = Duration::from_secs(5);
            let mut wait = first_wait;
            loop {
                let r = match client.wait_for(wait, |m| {
                    matches!(m, ServerMessage::ContractNote(_))
                        || matches!(m, ServerMessage::Execution(r) if r.client_order_id == client_order_id)
                }) {
                    Ok(ServerMessage::ContractNote(note)) => {
                        print_note(&note);
                        continue;
                    }
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

fn format_signed(amount: i64) -> String {
    let text = format_price(amount.unsigned_abs());
    if amount < 0 { format!("-{text}") } else { text }
}

fn print_note(note: &ome_protocol::ContractNote) {
    let c = &note.charges;
    println!(
        "contract note: {} {} {} @ {} | brokerage {} exchange {} tax {} stamp {} reg {} GST {} | total {} | net {}",
        note.side.label(),
        note.qty,
        note.symbol,
        format_price(note.price),
        format_price(c.commission),
        format_signed(c.exchange_fee),
        format_price(c.transaction_tax),
        format_price(c.stamp_duty),
        format_price(c.regulatory_fee),
        format_price(c.gst),
        format_signed(c.total()),
        format_signed(note.net_amount)
    );
}

fn clearing(client: &mut Client, args: &[&str]) -> Result<()> {
    match args {
        ["status"] => {
            let s = clearing_status(client)?;
            println!(
                "cycle {} · settled through {} · exchange fees collected {}",
                s.cycle,
                s.settled_cycle,
                format_signed(s.fee_pool)
            );
            for m in s.members {
                println!(
                    "{:>4}  {:<16} collateral {:>16}  margin {:>14}  unsettled {:>16}  credit {:>18}  fails {}",
                    m.member_id,
                    m.name,
                    format_signed(m.collateral),
                    format_price(m.margin_required),
                    format_price(m.unsettled_gross),
                    format_price(m.credit_limit),
                    m.fails
                );
            }
        }
        ["register", name, secret] => {
            println!(
                "{}",
                client.request_member(&MemberMessage::CreateMember {
                    kind: MemberKind::Broker,
                    name: name.to_string(),
                    secret: secret.to_string(),
                })?
            );
        }
        [verb @ ("deposit" | "withdraw"), member, value] => {
            let member = match member.parse() {
                Ok(id) => id,
                Err(_) => clearing_status(client)?
                    .members
                    .into_iter()
                    .find(|m| m.name == *member)
                    .map(|m| m.member_id)
                    .ok_or(format!("no clearing member named '{member}'"))?,
            };
            let amount = amount(value)? as i64;
            let amount = if *verb == "deposit" { amount } else { -amount };
            println!(
                "{}",
                client.request_member(&MemberMessage::Deposit { member, amount })?
            );
        }
        _ => usage()?,
    }
    Ok(())
}

fn clearing_status(
    client: &mut Client,
) -> std::result::Result<ome_protocol::ClearingStatus, ClientError> {
    client.send_member(&MemberMessage::QueryClearing)?;
    match client.wait_for(Duration::from_secs(5), |m| {
        matches!(
            m,
            ServerMessage::ClearingStatus(_) | ServerMessage::Reject { .. }
        )
    })? {
        ServerMessage::ClearingStatus(s) => Ok(s),
        ServerMessage::Reject { reason, .. } => Err(ClientError::Rejected(reason)),
        _ => unreachable!(),
    }
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

fn order_options(extras: &[&str]) -> std::result::Result<Option<OrderOptions>, String> {
    if extras.is_empty() {
        return Ok(None);
    }
    let mut options = OrderOptions::default();
    for extra in extras {
        let word = extra.to_ascii_lowercase();
        match word.as_str() {
            "gtc" => options.tif = TimeInForce::Gtc,
            "day" => options.tif = TimeInForce::Day,
            "ioc" => options.tif = TimeInForce::Ioc,
            "fok" => options.tif = TimeInForce::Fok,
            "auction" => options.tif = TimeInForce::Auction,
            "post" => options.post_only = true,
            "stp:keep" => options.stp = StpMode::CancelResting,
            "stp:new" => options.stp = StpMode::CancelIncoming,
            "stp:both" => options.stp = StpMode::CancelBoth,
            other => {
                if let Some(seconds) = other.strip_prefix("gtd:") {
                    let seconds = number(seconds)?;
                    options.tif = TimeInForce::Gtd(ome_net_now() + seconds * 1_000);
                } else if let Some(display) = other.strip_prefix("show:") {
                    options.display_qty = Some(number(display)?);
                } else {
                    return Err(format!(
                        "unknown order option '{other}' (gtc day ioc fok auction gtd:<seconds> post show:<qty> stp:keep|new|both)"
                    ));
                }
            }
        }
    }
    Ok(Some(options))
}

fn print_status(client: &mut Client) -> Result<()> {
    let ServerMessage::MarketStatus(m) = client.wait_for(Duration::from_secs(5), |m| {
        matches!(m, ServerMessage::MarketStatus(_))
    })?
    else {
        unreachable!()
    };
    let next = if m.next_change_at == 0 {
        "manual".to_string()
    } else {
        format!(
            "next change in {}s",
            m.next_change_at.saturating_sub(ome_net_now()) / 1_000
        )
    };
    println!(
        "market {} · trading day {} · {next}",
        m.phase.label(),
        m.day
    );
    Ok(())
}

fn ome_net_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
            Some(ServerMessage::MarketStatus(m)) => {
                println!("market {} (day {})", m.phase.label(), m.day)
            }
            Some(ServerMessage::AuctionInfo(a)) if a.volume > 0 => println!(
                "{} auction indicates {} x {} (imbalance {})",
                a.symbol,
                format_price(a.price),
                a.volume,
                a.imbalance
            ),
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
