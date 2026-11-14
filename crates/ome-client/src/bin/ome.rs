use ome_client::{Client, ClientError, DEFAULT_BROKER_ADDR, DEFAULT_EXCHANGE_ADDR};
use ome_core::price::{format_price, parse_amount, parse_price};
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, OrderKind, OrderOptions, Side, StpMode, TimeInForce};
use ome_protocol::{
    AccountInfo, AlgoStatus, AlgoStrategy, Asset, ClientMessage, InstrumentInfo, MarketPhase,
    MemberId, MemberInfo, MemberKind, MemberMessage, OrderStatus, QuoteSide, Role, ServerMessage,
    Trigger,
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
      suspend <member>  |  reinstate <member>             kill switch: cancel all orders and block trading
      cod <member> on|off                                 cancel-on-disconnect for a member
      session <preopen|open|auction|close>                change the market phase now
      schedule <preopen> <open> <closing> <closed>        repeating trading day in seconds; `schedule off`
      dmm <SYMBOL> <member|none>                          designate a market maker (better maker rebate, quoting obligation)
      obligations                                         designated market makers: quoting now and compliance
      stats <SYMBOL>                                      open, high, low, VWAP and volume by venue
      instruments  |  watch <SYMBOL>

  ome broker [--addr HOST:PORT] [--user NAME] [--secret SECRET] <command>
      accounts  |  fees                                   (broker admin)
      create-account <name> <secret> <role> <cash>        role: retail quant hft prop lp issuer admin
      set-cash <account> <amount>  |  set-shares <account> <SYMBOL> <qty>
      suspend-account <account>  |  reinstate-account <account>
                                                          account kill switch: cancels orders and conditionals
      restrict <SYMBOL> on|off                            restricted list: closing sells only
      loss-limit <account> <amount>                       daily loss limit (0 removes); exits stay allowed
      balance                                             (trader)
      buy|sell <SYMBOL> <price|mkt> <qty> [options..]
                                                          options: gtc day ioc fok auction gtd:<seconds>
                                                          post show:<qty> stp:keep|new|both
      market                                              market phase and trading day
      quote <SYMBOL> <bid> <bid-qty> <ask> <ask-qty>      two-sided quote (HFT and LP desks); 0 qty = no side
      mmp <window-ms> <max-qty>                           market maker protection (resets a trip)
      cancel <order_id>  |  cancel-all [SYMBOL]
      stop <SYMBOL> <buy|sell> <qty> <trigger> [limit]    stop-loss (market) or stop-limit, held at the broker
      trail <SYMBOL> <buy|sell> <qty> <distance>          trailing stop that follows the best price seen
      gtt <SYMBOL> <buy|sell> <qty> <above|below> <trigger> <limit>
                                                          good-till-triggered limit order, survives days
      amo <SYMBOL> <buy|sell> <qty> <price|mkt>           after-market order, released when the market opens
      oco <SYMBOL> <buy|sell> <qty> <target> <stop> [stop-limit]
                                                          one-cancels-other exit: target limit + stop
      bracket <SYMBOL> <buy|sell> <qty> <price|mkt> <target> <stop>
                                                          entry order whose fills arm an OCO exit
      conditionals  |  cancel-cond <id>                   armed broker-held orders
      twap <SYMBOL> <side> <qty> <seconds> <slices> [limit] [follow]
      vwap <SYMBOL> <side> <qty> <seconds> [limit] [follow]
      pov <SYMBOL> <side> <qty> <percent> <seconds> [limit] [follow]
      slice <SYMBOL> <side> <qty> <clip> <limit> [follow]   broker-side iceberg slicer
                                                          algorithmic parent orders (quant and prop accounts)
      algos  |  cancel-algo <id>                          working algos with progress
      dark <SYMBOL> <side> <qty> [limit] [min:<qty>]      midpoint order in the dark pool (quant and prop)
      stats <SYMBOL>                                      market statistics with volume by venue
      instruments  |  watch <SYMBOL>

  ome venue [--addr HOST:PORT] [--user NAME] [--secret SECRET] <command>   (wholesaler 7301, dark pool 7302)
      status                                              fills, routing members, inventory and PFOF
      register <member-name> <secret>                     let an exchange member route to this venue

  ome sim [--addr HOST:PORT] [--secret SECRET] <command>        simulation control plane (artificial participants only)
      status                                              agent groups, fair values, active scenarios
      pause <group|*>  |  resume <group|*>                stop or restart a group's trading (open orders are cancelled)
      count <group> <n>                                   scale a group up or down
      param <group|*> <key>=<value>                       change a strategy parameter live
      fund <group|*> <amount>                             add cash to every account in a group
      fair <SYMBOL> <price> [drift-bps-per-minute]        fundamental value that value investors trade around
      crash <SYMBOL> [seconds]  |  news <SYMBOL> <percent> [seconds]
      drought [seconds]  |  volatility [seconds]          market-wide liquidity or activity scenarios
      speed <preopen> <open> <closing> <closed>           trading day length in seconds (time acceleration)

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
    Venue,
    Sim,
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
        Some("sim") => Options {
            target: Target::Sim,
            addr: env("OME_SIMCTL", "127.0.0.1:7400"),
            user: "operator".into(),
            secret: env("OME_SIM_SECRET", "sim"),
            command: Vec::new(),
        },
        Some("venue") => Options {
            target: Target::Venue,
            addr: env("OME_VENUE", "127.0.0.1:7301"),
            user: env("OME_VENUE_OPS", "ops"),
            secret: env("OME_VENUE_SECRET", "ops"),
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
        Target::Venue => Client::connect_member(&options.addr, &options.user, &options.secret)
            .map_err(Into::into)
            .and_then(|(mut client, _)| venue(&mut client, &args)),
        Target::Sim => Client::connect_sim(
            &options.addr,
            ome_protocol::SimRole::Operator,
            &options.user,
            &options.secret,
        )
        .map_err(Into::into)
        .and_then(|mut client| sim(&mut client, &args)),
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
        ["dmm", sym, member] => {
            let member = if *member == "none" {
                0
            } else {
                member_id(client, member)?
            };
            println!(
                "{}",
                client.request_member(&MemberMessage::SetDmm {
                    symbol: symbol(sym)?,
                    member
                })?
            );
        }
        ["obligations"] => {
            client.send_member(&MemberMessage::QueryObligations)?;
            let ServerMessage::Obligations(list) = client
                .wait_for(Duration::from_secs(5), |m| {
                    matches!(m, ServerMessage::Obligations(_))
                })?
            else {
                unreachable!()
            };
            if list.is_empty() {
                println!("no designated market makers");
            }
            for o in list {
                let pct = if o.samples == 0 {
                    0.0
                } else {
                    o.compliant as f64 * 100.0 / o.samples as f64
                };
                println!(
                    "{:<8} {:<16} quoting now {:<3}  compliant {:>5.1}% of {} samples",
                    o.symbol,
                    o.name,
                    if o.quoting { "yes" } else { "no" },
                    pct,
                    o.samples
                );
            }
        }
        ["stats", sym] => {
            let symbol = symbol(sym)?;
            client.send_member(&MemberMessage::Subscribe { symbol })?;
            print_stats(client, symbol)?;
        }
        ["members"] => {
            for m in members(client)? {
                println!(
                    "{:>4}  {:<20} {:<20} {}  open orders {:<6} net fees {:<10} exposure {:<14} credit {}{}{}",
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
                    },
                    if m.suspended { "  SUSPENDED" } else { "" },
                    if m.cancel_on_disconnect { "  cod" } else { "" }
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
        [verb @ ("suspend" | "reinstate"), member] => {
            let member = member_id(client, member)?;
            let enabled = *verb == "reinstate";
            println!(
                "{}",
                client.request_member(&MemberMessage::SetMemberStatus { member, enabled })?
            );
        }
        ["cod", member, flag @ ("on" | "off")] => {
            let member = member_id(client, member)?;
            let enabled = *flag == "on";
            println!(
                "{}",
                client.request_member(&MemberMessage::SetCancelOnDisconnect { member, enabled })?
            );
        }
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
            println!("order routing (606):");
            println!("  exchange       {} orders", summary.routed_orders);
            println!(
                "  internal       {} crosses, {} shares",
                summary.internal_crosses, summary.internalized_shares
            );
            println!(
                "  wholesaler     {} orders, {} shares, payment for order flow {}",
                summary.wholesaler_orders,
                summary.wholesaler_shares,
                format_signed(summary.pfof_earned)
            );
            println!("  dark pool      {} shares", summary.dark_shares);
        }
        ["accounts"] => {
            for a in accounts(client)? {
                println!(
                    "{:>4}  {:<18} {:<7} cash {:>16}  reserved {:>14}  open orders {:>3}  day P&L {:>12}{}{}",
                    a.account_id,
                    a.username,
                    a.role.label(),
                    format_price(a.cash.available),
                    format_price(a.cash.reserved),
                    a.open_orders,
                    format_signed(a.day_pnl),
                    if a.loss_limit > 0 {
                        format!("  loss limit {}", format_price(a.loss_limit))
                    } else {
                        String::new()
                    },
                    if a.suspended { "  SUSPENDED" } else { "" }
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
        [verb @ ("suspend-account" | "reinstate-account"), account] => {
            let account_id = resolve(client, account)?;
            println!(
                "{}",
                client.request(&ClientMessage::SetAccountStatus {
                    account_id,
                    suspended: *verb == "suspend-account"
                })?
            );
        }
        ["restrict", sym, flag @ ("on" | "off")] => {
            println!(
                "{}",
                client.request(&ClientMessage::SetRestricted {
                    symbol: symbol(sym)?,
                    restricted: *flag == "on"
                })?
            );
        }
        ["loss-limit", account, value] => {
            let account_id = resolve(client, account)?;
            println!(
                "{}",
                client.request(&ClientMessage::SetLossLimit {
                    account_id,
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
            println!("day P&L {}", format_signed(snapshot.day_pnl));
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
        ["quote", sym, bid, bid_qty, ask, ask_qty] => {
            let side = |px: &str, qty: &str| -> std::result::Result<QuoteSide, String> {
                let qty = number(qty)?;
                Ok(QuoteSide {
                    price: if qty == 0 { 0 } else { price(px)? },
                    qty,
                })
            };
            let symbol = symbol(sym)?;
            client.send(&ClientMessage::Quote {
                symbol,
                bid: side(bid, bid_qty)?,
                ask: side(ask, ask_qty)?,
            })?;
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while let Some(message) = client
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))?
            {
                match message {
                    ServerMessage::Reject { reason, .. } => {
                        return Err(ClientError::Rejected(reason).into());
                    }
                    ServerMessage::Execution(r) if r.symbol == symbol => println!(
                        "#{} {} {} {} @ {} {}",
                        r.order_id,
                        r.status.label(),
                        r.side.label(),
                        r.qty,
                        format_price(r.price),
                        r.reason
                    ),
                    _ => {}
                }
                if std::time::Instant::now() >= deadline {
                    break;
                }
            }
        }
        ["mmp", window, max_qty] => {
            println!(
                "{}",
                client.request(&ClientMessage::SetMmp {
                    window_ms: number(window)?,
                    max_qty: number(max_qty)?
                })?
            );
        }
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
                        || matches!(m, ServerMessage::Reject { ref_template, .. } if *ref_template == ome_protocol::template::NEW_ORDER)
                        || matches!(m, ServerMessage::Execution(r) if r.client_order_id == client_order_id)
                }) {
                    Ok(ServerMessage::Reject { reason, .. }) => {
                        return Err(ClientError::Rejected(reason).into());
                    }
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
        ["stop", sym, side, qty, trigger, rest @ ..] if rest.len() <= 1 => {
            let side = side_of(side)?;
            let level = price(trigger)?;
            let trigger = match side {
                Side::Buy => Trigger::Above(level),
                Side::Sell => Trigger::Below(level),
            };
            let kind = match rest.first() {
                Some(px) => OrderKind::Limit(price(px)?),
                None => OrderKind::Market,
            };
            conditional(client, symbol(sym)?, side, number(qty)?, trigger, kind)?;
        }
        ["trail", sym, side, qty, distance] => {
            let side = side_of(side)?;
            let distance = price(distance)?;
            let trigger = match side {
                Side::Buy => Trigger::TrailingAbove(distance),
                Side::Sell => Trigger::TrailingBelow(distance),
            };
            conditional(
                client,
                symbol(sym)?,
                side,
                number(qty)?,
                trigger,
                OrderKind::Market,
            )?;
        }
        ["gtt", sym, side, qty, direction, trigger, limit] => {
            let level = price(trigger)?;
            let trigger = match *direction {
                "above" => Trigger::Above(level),
                "below" => Trigger::Below(level),
                _ => return Err("direction must be above or below".into()),
            };
            conditional(
                client,
                symbol(sym)?,
                side_of(side)?,
                number(qty)?,
                trigger,
                OrderKind::Limit(price(limit)?),
            )?;
        }
        ["amo", sym, side, qty, px] => {
            let kind = order_kind(px)?;
            conditional(
                client,
                symbol(sym)?,
                side_of(side)?,
                number(qty)?,
                Trigger::MarketOpens,
                kind,
            )?;
        }
        ["oco", sym, side, qty, target, stop, rest @ ..] if rest.len() <= 1 => {
            let client_order_id = client.next_order_id();
            client.send(&ClientMessage::Oco {
                client_order_id,
                symbol: symbol(sym)?,
                side: side_of(side)?,
                qty: number(qty)?,
                take_profit: price(target)?,
                stop: price(stop)?,
                stop_limit: rest.first().map(|p| price(p)).transpose()?,
            })?;
            print_conditional_replies(client, client_order_id, 2)?;
        }
        ["bracket", sym, side, qty, entry, target, stop] => {
            let client_order_id = client.next_order_id();
            client.send(&ClientMessage::Bracket {
                client_order_id,
                symbol: symbol(sym)?,
                side: side_of(side)?,
                qty: number(qty)?,
                entry: order_kind(entry)?,
                take_profit: price(target)?,
                stop_loss: price(stop)?,
            })?;
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while let Some(message) = client
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))?
            {
                match message {
                    ServerMessage::Reject { reason, .. } => {
                        return Err(ClientError::Rejected(reason).into());
                    }
                    ServerMessage::Execution(r) if r.client_order_id == client_order_id => {
                        println!(
                            "entry #{} {} {} filled {}/{} {}",
                            r.order_id,
                            r.status.label(),
                            r.side.label(),
                            r.filled,
                            r.qty,
                            r.reason
                        );
                        if r.status == OrderStatus::Rejected {
                            break;
                        }
                    }
                    ServerMessage::ConditionalUpdate(c) if c.client_order_id == client_order_id => {
                        print_conditional(&c)
                    }
                    ServerMessage::ContractNote(note) => print_note(&note),
                    _ => {}
                }
            }
        }
        [
            name @ ("twap" | "vwap" | "pov" | "slice"),
            sym,
            side,
            qty,
            rest @ ..,
        ] => {
            let (rest, follow) = match rest.split_last() {
                Some((&"follow", head)) => (head, true),
                _ => (rest, false),
            };
            let seconds =
                |text: &str| -> std::result::Result<u64, String> { Ok(number(text)? * 1_000) };
            let limit = |text: Option<&&str>| text.map(|p| price(p)).transpose();
            let (strategy, limit) = match (*name, rest) {
                ("twap", [secs, slices, more @ ..]) if more.len() <= 1 => (
                    AlgoStrategy::Twap {
                        duration_ms: seconds(secs)?,
                        slices: number(slices)?,
                    },
                    limit(more.first())?,
                ),
                ("vwap", [secs, more @ ..]) if more.len() <= 1 => (
                    AlgoStrategy::Vwap {
                        duration_ms: seconds(secs)?,
                    },
                    limit(more.first())?,
                ),
                ("pov", [pct, secs, more @ ..]) if more.len() <= 1 => (
                    AlgoStrategy::Pov {
                        rate_bps: price(pct)?,
                        duration_ms: seconds(secs)?,
                    },
                    limit(more.first())?,
                ),
                ("slice", [clip, px]) => (
                    AlgoStrategy::Iceberg {
                        clip: number(clip)?,
                    },
                    Some(price(px)?),
                ),
                _ => return usage(),
            };
            let client_order_id = client.next_order_id();
            client.send(&ClientMessage::AlgoOrder {
                client_order_id,
                symbol: symbol(sym)?,
                side: side_of(side)?,
                qty: number(qty)?,
                limit,
                strategy,
            })?;
            loop {
                let wait = if follow {
                    Duration::from_secs(600)
                } else {
                    Duration::from_secs(5)
                };
                match client.wait_for(wait, |m| match m {
                    ServerMessage::AlgoUpdate(a) => a.client_order_id == client_order_id,
                    ServerMessage::Reject { .. } => true,
                    _ => false,
                })? {
                    ServerMessage::Reject { reason, .. } => {
                        return Err(ClientError::Rejected(reason).into());
                    }
                    ServerMessage::AlgoUpdate(a) => {
                        print_algo(&a);
                        if !follow || a.status != AlgoStatus::Working {
                            break;
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
        ["algos"] => {
            client.send(&ClientMessage::QueryAccount)?;
            let deadline = std::time::Instant::now() + Duration::from_millis(500);
            let mut working = std::collections::BTreeMap::new();
            while let Some(message) = client
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))?
            {
                if let ServerMessage::AlgoUpdate(a) = message {
                    if a.status == AlgoStatus::Working {
                        working.insert(a.algo_id, a);
                    } else {
                        working.remove(&a.algo_id);
                    }
                }
            }
            if working.is_empty() {
                println!("no working algos");
            }
            for a in working.values() {
                print_algo(a);
            }
        }
        ["cancel-algo", id] => {
            let algo_id = number(id)?;
            client.send(&ClientMessage::CancelAlgo { algo_id })?;
            match client.wait_for(Duration::from_secs(5), |m| match m {
                ServerMessage::AlgoUpdate(a) => {
                    a.algo_id == algo_id && a.status != AlgoStatus::Working
                }
                ServerMessage::Reject { .. } => true,
                _ => false,
            })? {
                ServerMessage::Reject { reason, .. } => {
                    return Err(ClientError::Rejected(reason).into());
                }
                ServerMessage::AlgoUpdate(a) => print_algo(&a),
                _ => unreachable!(),
            }
        }
        ["stats", sym] => {
            let symbol = symbol(sym)?;
            client.send(&ClientMessage::Subscribe { symbol })?;
            print_stats(client, symbol)?;
        }
        ["dark", sym, side, qty, rest @ ..] if rest.len() <= 2 => {
            let mut limit = None;
            let mut min_qty = 0;
            for word in rest {
                match word.strip_prefix("min:") {
                    Some(min) => min_qty = number(min)?,
                    None => limit = Some(price(word)?),
                }
            }
            let client_order_id = client.next_order_id();
            client.send(&ClientMessage::DarkOrder {
                client_order_id,
                symbol: symbol(sym)?,
                side: side_of(side)?,
                qty: number(qty)?,
                limit,
                min_qty,
            })?;
            match client.wait_for(Duration::from_secs(5), |m| match m {
                ServerMessage::Execution(r) => r.client_order_id == client_order_id,
                ServerMessage::Reject { .. } => true,
                _ => false,
            })? {
                ServerMessage::Reject { reason, .. } => {
                    return Err(ClientError::Rejected(reason).into());
                }
                ServerMessage::Execution(r) => println!(
                    "#{} {} {} {}/{} {}",
                    r.order_id,
                    r.status.label(),
                    r.side.label(),
                    r.filled,
                    r.qty,
                    r.reason
                ),
                _ => unreachable!(),
            }
        }
        ["conditionals"] => {
            client.send(&ClientMessage::QueryAccount)?;
            let deadline = std::time::Instant::now() + Duration::from_millis(500);
            let mut armed = std::collections::BTreeMap::new();
            while let Some(message) = client
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))?
            {
                if let ServerMessage::ConditionalUpdate(c) = message {
                    if c.status == ome_protocol::ConditionalStatus::Armed {
                        armed.insert(c.cond_id, c);
                    } else {
                        armed.remove(&c.cond_id);
                    }
                }
            }
            if armed.is_empty() {
                println!("no armed conditional orders");
            }
            for c in armed.values() {
                print_conditional(c);
            }
        }
        ["cancel-cond", id] => {
            let cond_id = number(id)?;
            client.send(&ClientMessage::CancelConditional { cond_id })?;
            match client.wait_for(Duration::from_secs(5), |m| match m {
                ServerMessage::ConditionalUpdate(c) => {
                    c.cond_id == cond_id && c.status == ome_protocol::ConditionalStatus::Cancelled
                }
                ServerMessage::Reject { .. } => true,
                _ => false,
            })? {
                ServerMessage::Reject { reason, .. } => {
                    return Err(ClientError::Rejected(reason).into());
                }
                ServerMessage::ConditionalUpdate(c) => print_conditional(&c),
                _ => unreachable!(),
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

fn side_of(text: &str) -> std::result::Result<Side, String> {
    match text {
        "buy" => Ok(Side::Buy),
        "sell" => Ok(Side::Sell),
        _ => Err(format!("side must be buy or sell, not '{text}'")),
    }
}

fn order_kind(text: &str) -> std::result::Result<OrderKind, String> {
    if text.eq_ignore_ascii_case("mkt") {
        Ok(OrderKind::Market)
    } else {
        Ok(OrderKind::Limit(price(text)?))
    }
}

fn conditional(
    client: &mut Client,
    symbol: Symbol,
    side: Side,
    qty: u64,
    trigger: Trigger,
    kind: OrderKind,
) -> Result<()> {
    let client_order_id = client.next_order_id();
    client.send(&ClientMessage::Conditional {
        client_order_id,
        symbol,
        side,
        qty,
        trigger,
        kind,
        options: OrderOptions::default(),
    })?;
    print_conditional_replies(client, client_order_id, 1)
}

fn print_conditional_replies(client: &mut Client, client_order_id: u64, legs: usize) -> Result<()> {
    for _ in 0..legs {
        match client.wait_for(Duration::from_secs(5), |m| match m {
            ServerMessage::ConditionalUpdate(c) => c.client_order_id == client_order_id,
            ServerMessage::Reject { .. } => true,
            _ => false,
        })? {
            ServerMessage::Reject { reason, .. } => {
                return Err(ClientError::Rejected(reason).into());
            }
            ServerMessage::ConditionalUpdate(c) => print_conditional(&c),
            _ => unreachable!(),
        }
    }
    Ok(())
}

fn print_stats(client: &mut Client, symbol: Symbol) -> Result<()> {
    let ServerMessage::MarketStats(s) = client.wait_for(
        Duration::from_secs(5),
        |m| matches!(m, ServerMessage::MarketStats(s) if s.symbol == symbol),
    )?
    else {
        unreachable!()
    };
    println!(
        "{} open {} high {} low {} last {} vwap {}",
        s.symbol,
        format_price(s.open),
        format_price(s.high),
        format_price(s.low),
        format_price(s.last),
        format_price(s.vwap)
    );
    let total = s.volume().max(1);
    println!(
        "volume {} in {} trades, turnover {}",
        s.volume(),
        s.trades,
        format_price(s.turnover)
    );
    for (venue, qty) in [
        ("exchange", s.exchange_volume),
        ("internal", s.internal_volume),
        ("wholesaler", s.wholesaler_volume),
        ("dark pool", s.ats_volume),
    ] {
        println!(
            "  {venue:<11} {qty:>10}  {:>5.1}%",
            qty as f64 * 100.0 / total as f64
        );
    }
    Ok(())
}

fn print_algo(a: &ome_protocol::AlgoReport) {
    println!(
        "algo #{} {} {} {} {}/{} filled avg {} in {} child order(s) [{}]{}",
        a.algo_id,
        a.status.label(),
        a.side.label(),
        a.symbol,
        a.filled,
        a.qty,
        format_price(a.avg_price),
        a.children,
        a.strategy.describe(),
        if a.reason.is_empty() {
            String::new()
        } else {
            format!(" ({})", a.reason)
        }
    );
}

fn print_conditional(c: &ome_protocol::ConditionalReport) {
    let level = match c.trigger {
        Trigger::TrailingAbove(_) | Trigger::TrailingBelow(_) => {
            format!(", now at {}", format_price(c.level))
        }
        _ => String::new(),
    };
    println!(
        "cond #{} {} {} {} {} when {}{level}{}",
        c.cond_id,
        c.status.label(),
        c.side.label(),
        c.qty,
        c.symbol,
        c.trigger.describe(),
        if c.reason.is_empty() {
            String::new()
        } else {
            format!(" ({})", c.reason)
        }
    );
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

fn venue(client: &mut Client, args: &[&str]) -> Result<()> {
    match args {
        ["status"] => {
            client.send_member(&MemberMessage::QueryVenue)?;
            let ServerMessage::VenueStatus(v) = client.wait_for(Duration::from_secs(5), |m| {
                matches!(m, ServerMessage::VenueStatus(_))
            })?
            else {
                unreachable!()
            };
            println!(
                "{} · fills {} · declined {} · shares {} · resting {}",
                v.venue.label(),
                v.fills,
                v.declined,
                v.shares,
                v.resting
            );
            if v.venue == ome_protocol::Venue::Wholesaler {
                println!(
                    "cash {} · payment for order flow paid {}",
                    format_signed(v.cash),
                    format_signed(v.pfof_paid)
                );
                for (symbol, qty) in &v.inventory {
                    println!("  inventory {symbol:<8} {qty:>8}");
                }
            }
            for m in &v.members {
                println!(
                    "{:>4}  {:<16} orders {:>8}  shares {:>10}",
                    m.member_id, m.name, m.orders, m.shares
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
        _ => usage()?,
    }
    Ok(())
}

fn sim(client: &mut Client, args: &[&str]) -> Result<()> {
    use ome_protocol::{Scenario, SimCommand, SimMessage};
    let seconds = |rest: &[&str], default: u64| -> std::result::Result<u64, String> {
        rest.first().map_or(Ok(default), |s| number(s))
    };
    let command = match args {
        ["status"] => {
            client.send_sim(&SimMessage::QueryStatus)?;
            let ServerMessage::SimStatus(s) = client.wait_for(Duration::from_secs(5), |m| {
                matches!(m, ServerMessage::SimStatus(_))
            })?
            else {
                unreachable!()
            };
            println!("{} runner(s) connected", s.runners);
            for g in &s.groups {
                println!(
                    "  {:<12} {:<13} {:<10} {:>3}/{:<3} {:<7} fills {:>7}  equity {:>14}  {}",
                    g.name,
                    g.strategy,
                    g.broker,
                    g.running,
                    g.target,
                    if g.paused { "PAUSED" } else { "running" },
                    g.fills,
                    format_price(g.equity),
                    g.params
                );
            }
            for (symbol, price, drift) in &s.fair {
                println!(
                    "  fair {symbol:<8} {:>10}  drift {drift} bps/min",
                    format_price(*price)
                );
            }
            for (text, until) in &s.scenarios {
                let left = until.saturating_sub(ome_net_now()) / 1_000;
                println!("  scenario: {text} ({left}s left)");
            }
            return Ok(());
        }
        ["speed", a, b, c, d] => {
            println!(
                "{}",
                client.request_sim(&SimMessage::Accelerate {
                    pre_open_s: number(a)?,
                    open_s: number(b)?,
                    closing_s: number(c)?,
                    closed_s: number(d)?,
                })?
            );
            return Ok(());
        }
        ["pause", group] => SimCommand::Pause {
            group: group.to_string(),
        },
        ["resume", group] => SimCommand::Resume {
            group: group.to_string(),
        },
        ["count", group, n] => SimCommand::SetCount {
            group: group.to_string(),
            count: number(n)? as u32,
        },
        ["param", group, pair] => {
            let (key, value) = pair.split_once('=').ok_or("expected key=value")?;
            SimCommand::SetParam {
                group: group.to_string(),
                key: key.into(),
                value: value.into(),
            }
        }
        ["fund", group, value] => SimCommand::Fund {
            group: group.to_string(),
            amount: amount(value)?,
        },
        ["fair", sym, px, rest @ ..] if rest.len() <= 1 => SimCommand::SetFair {
            symbol: symbol(sym)?,
            price: price(px)?,
            drift_bps: rest.first().map_or(Ok(0), |d| {
                d.parse().map_err(|_| format!("invalid drift '{d}'"))
            })?,
        },
        ["crash", sym, rest @ ..] => SimCommand::Scenario(Scenario::FlashCrash {
            symbol: symbol(sym)?,
            seconds: seconds(rest, 20)?,
        }),
        ["news", sym, pct, rest @ ..] => {
            let negative = pct.starts_with('-');
            let magnitude = price(pct.trim_start_matches(['-', '+']))? as i64;
            SimCommand::Scenario(Scenario::NewsShock {
                symbol: symbol(sym)?,
                move_bps: if negative { -magnitude } else { magnitude },
                seconds: seconds(rest, 60)?,
            })
        }
        ["drought", rest @ ..] => SimCommand::Scenario(Scenario::LiquidityDrought {
            seconds: seconds(rest, 30)?,
        }),
        ["volatility", rest @ ..] => SimCommand::Scenario(Scenario::VolatilityBurst {
            seconds: seconds(rest, 30)?,
        }),
        _ => return usage(),
    };
    println!("{}", client.request_sim(&SimMessage::Command(command))?);
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

fn member_id(client: &mut Client, member: &str) -> Result<MemberId> {
    if let Ok(id) = member.parse() {
        return Ok(id);
    }
    members(client)?
        .into_iter()
        .find(|m| m.name == member)
        .map(|m| m.member_id)
        .ok_or_else(|| format!("no member named '{member}'").into())
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
                "{} {} {:>10} x {}{}",
                t.symbol,
                if t.venue != ome_protocol::Venue::Exchange {
                    "◆"
                } else if t.aggressor == Side::Buy {
                    "▲"
                } else {
                    "▼"
                },
                format_price(t.price),
                t.qty,
                if t.venue == ome_protocol::Venue::Exchange {
                    String::new()
                } else {
                    format!("  ({} print)", t.venue.label())
                }
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
