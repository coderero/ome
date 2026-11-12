use ome_core::price::{parse_amount, parse_price};
use ome_core::symbol::Symbol;
use ome_core::types::{OrderId, OrderKind, Qty, Side};
use ome_protocol::Role;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartMode {
    Candles,
    Line,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Order {
        side: Side,
        kind: OrderKind,
        qty: Qty,
    },
    Cancel(OrderId),
    CancelAll,
    Select(Symbol),
    Chart(ChartMode),
    Clear,
    Help,
    Quit,
    CreateAccount {
        username: String,
        secret: String,
        role: Role,
        cash: u64,
    },
    SetCash {
        account: String,
        amount: u64,
    },
    SetShares {
        account: String,
        symbol: Symbol,
        qty: Qty,
    },
    Ipo {
        symbol: Symbol,
        price: u64,
        shares: Qty,
        issuer: String,
        name: String,
    },
    Halt {
        symbol: Option<Symbol>,
        reason: String,
    },
    Resume(Option<Symbol>),
    Accounts,
}

pub fn parse_command(line: &str) -> Result<Command, String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let Some((head, args)) = tokens.split_first() else {
        return Err("empty command".into());
    };
    let head = head.to_ascii_lowercase();
    let lower: Vec<String> = args.iter().map(|a| a.to_ascii_lowercase()).collect();
    let lower: Vec<&str> = lower.iter().map(String::as_str).collect();

    match (head.as_str(), lower.as_slice()) {
        ("buy" | "b", _) => parse_order(Side::Buy, &lower),
        ("sell" | "s", _) => parse_order(Side::Sell, &lower),
        ("cancel" | "c", ["all"]) => Ok(Command::CancelAll),
        ("cancel" | "c", [id]) => id
            .trim_start_matches('#')
            .parse()
            .map(Command::Cancel)
            .map_err(|_| format!("invalid order id '{id}'")),
        ("sym" | "symbol" | "use", [s]) => symbol(s).map(Command::Select),
        ("chart", ["candles" | "candle" | "c"]) => Ok(Command::Chart(ChartMode::Candles)),
        ("chart", ["line" | "l"]) => Ok(Command::Chart(ChartMode::Line)),
        ("clear" | "cls", []) => Ok(Command::Clear),
        ("help" | "h" | "?", []) => Ok(Command::Help),
        ("quit" | "q" | "exit", []) => Ok(Command::Quit),
        ("create", [_, _, role, cash]) => Ok(Command::CreateAccount {
            username: args[0].to_string(),
            secret: args[1].to_string(),
            role: Role::parse(role).ok_or("role must be retail, institutional, bot or admin")?,
            cash: amount(cash)?,
        }),
        ("cash", [_, value]) => Ok(Command::SetCash {
            account: args[0].to_string(),
            amount: amount(value)?,
        }),
        ("shares", [_, sym, qty]) => Ok(Command::SetShares {
            account: args[0].to_string(),
            symbol: symbol(sym)?,
            qty: number(qty)?,
        }),
        ("ipo", [sym, px, shares, _, ..]) => Ok(Command::Ipo {
            symbol: symbol(sym)?,
            price: price(px)?,
            shares: number(shares)?,
            issuer: args[3].to_string(),
            name: args[4..].join(" "),
        }),
        ("halt", []) => Ok(Command::Halt {
            symbol: None,
            reason: String::new(),
        }),
        ("halt", [sym, ..]) => Ok(Command::Halt {
            symbol: Some(symbol(sym)?),
            reason: args[1..].join(" "),
        }),
        ("resume", []) => Ok(Command::Resume(None)),
        ("resume", [sym]) => Ok(Command::Resume(Some(symbol(sym)?))),
        ("accounts", []) => Ok(Command::Accounts),
        ("cancel" | "c", _) => Err("usage: cancel <id> | cancel all".into()),
        ("sym" | "symbol" | "use", _) => Err("usage: sym <SYMBOL>".into()),
        ("chart", _) => Err("usage: chart candles | chart line".into()),
        ("create", _) => {
            Err("usage: create <username> <secret> <retail|institutional|bot|admin> <cash>".into())
        }
        ("cash", _) => Err("usage: cash <account> <amount>".into()),
        ("shares", _) => Err("usage: shares <account> <SYMBOL> <qty>".into()),
        ("ipo", _) => Err("usage: ipo <SYMBOL> <price> <shares> <issuer> <name...>".into()),
        ("resume", _) => Err("usage: resume [SYMBOL]".into()),
        (other, _) => Err(format!("unknown command '{other}', type 'help'")),
    }
}

fn parse_order(side: Side, args: &[&str]) -> Result<Command, String> {
    let (kind, qty) = match args {
        ["mkt" | "market" | "m", qty] => (OrderKind::Market, *qty),
        [px, qty] => (OrderKind::Limit(price(px)?), *qty),
        _ => {
            return Err(format!(
                "usage: {0} <price> <qty> | {0} mkt <qty>",
                side.label()
            ));
        }
    };
    let qty = number(qty)?;
    if qty == 0 {
        return Err("quantity must be positive".into());
    }
    Ok(Command::Order { side, kind, qty })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(s: &str) -> Symbol {
        Symbol::parse(s).unwrap()
    }

    #[test]
    fn parses_trading_commands() {
        assert_eq!(
            parse_command("BUY 101.50 7"),
            Ok(Command::Order {
                side: Side::Buy,
                kind: OrderKind::Limit(10150),
                qty: 7
            })
        );
        assert_eq!(
            parse_command("s mkt 3"),
            Ok(Command::Order {
                side: Side::Sell,
                kind: OrderKind::Market,
                qty: 3
            })
        );
        assert_eq!(parse_command("cancel #12"), Ok(Command::Cancel(12)));
        assert_eq!(parse_command("sym nova"), Ok(Command::Select(sym("NOVA"))));
    }

    #[test]
    fn admin_commands_keep_case_of_free_text() {
        assert_eq!(
            parse_command("create Alice S3cret retail 25000"),
            Ok(Command::CreateAccount {
                username: "Alice".into(),
                secret: "S3cret".into(),
                role: Role::Retail,
                cash: 2_500_000
            })
        );
        assert_eq!(
            parse_command("ipo zen 12.50 50000 issuer Zen Robotics Inc"),
            Ok(Command::Ipo {
                symbol: sym("ZEN"),
                price: 1250,
                shares: 50_000,
                issuer: "issuer".into(),
                name: "Zen Robotics Inc".into()
            })
        );
        assert_eq!(
            parse_command("halt ACME Pending News"),
            Ok(Command::Halt {
                symbol: Some(sym("ACME")),
                reason: "Pending News".into()
            })
        );
        assert_eq!(
            parse_command("halt"),
            Ok(Command::Halt {
                symbol: None,
                reason: String::new()
            })
        );
        assert_eq!(
            parse_command("shares mm1 ACME 500"),
            Ok(Command::SetShares {
                account: "mm1".into(),
                symbol: sym("ACME"),
                qty: 500
            })
        );
    }

    #[test]
    fn rejects_malformed_input() {
        for bad in [
            "",
            "buy 100",
            "buy abc 5",
            "sell 100 0",
            "cancel",
            "cancel x",
            "chart pie",
            "create a b c",
            "create a b wizard 10",
            "ipo ACME 1",
            "hold 1 2",
            "sym TOOLONGSYMBOL",
        ] {
            assert!(parse_command(bad).is_err(), "'{bad}' should fail");
        }
    }
}
