use crate::codec::{DecodeError, Decoder, Encoder};
use crate::template::*;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimRole {
    Operator = 1,
    Runner = 2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInfo {
    pub name: String,
    pub strategy: String,
    pub broker: String,
    pub target: u32,
    pub running: u32,
    pub paused: bool,
    pub fills: u64,
    pub equity: u64,
    pub params: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    FlashCrash {
        symbol: Symbol,
        seconds: u64,
    },
    NewsShock {
        symbol: Symbol,
        move_bps: i64,
        seconds: u64,
    },
    LiquidityDrought {
        seconds: u64,
    },
    VolatilityBurst {
        seconds: u64,
    },
}

impl Scenario {
    pub fn seconds(&self) -> u64 {
        match *self {
            Scenario::FlashCrash { seconds, .. }
            | Scenario::NewsShock { seconds, .. }
            | Scenario::LiquidityDrought { seconds }
            | Scenario::VolatilityBurst { seconds } => seconds,
        }
    }

    pub fn with_seconds(self, seconds: u64) -> Self {
        match self {
            Scenario::FlashCrash { symbol, .. } => Scenario::FlashCrash { symbol, seconds },
            Scenario::NewsShock {
                symbol, move_bps, ..
            } => Scenario::NewsShock {
                symbol,
                move_bps,
                seconds,
            },
            Scenario::LiquidityDrought { .. } => Scenario::LiquidityDrought { seconds },
            Scenario::VolatilityBurst { .. } => Scenario::VolatilityBurst { seconds },
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Scenario::FlashCrash { symbol, seconds } => {
                format!("flash crash in {symbol} for {seconds}s")
            }
            Scenario::NewsShock {
                symbol,
                move_bps,
                seconds,
            } => format!(
                "news shock in {symbol}: fair value {}{}.{:02}% (sentiment for {seconds}s)",
                if *move_bps < 0 { "-" } else { "+" },
                move_bps.unsigned_abs() / 100,
                move_bps.unsigned_abs() % 100
            ),
            Scenario::LiquidityDrought { seconds } => format!("liquidity drought for {seconds}s"),
            Scenario::VolatilityBurst { seconds } => format!("volatility burst for {seconds}s"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimCommand {
    Pause {
        group: String,
    },
    Resume {
        group: String,
    },
    SetCount {
        group: String,
        count: u32,
    },
    SetParam {
        group: String,
        key: String,
        value: String,
    },
    Fund {
        group: String,
        amount: u64,
    },
    SetFair {
        symbol: Symbol,
        price: Price,
        drift_bps: i64,
    },
    Scenario(Scenario),
}

impl SimCommand {
    pub fn describe(&self) -> String {
        match self {
            SimCommand::Pause { group } => format!("paused {group}"),
            SimCommand::Resume { group } => format!("resumed {group}"),
            SimCommand::SetCount { group, count } => format!("{group} scaled to {count} agent(s)"),
            SimCommand::SetParam { group, key, value } => format!("{group}: {key} = {value}"),
            SimCommand::Fund { group, amount } => format!(
                "funding every {group} agent with {}",
                ome_core::price::format_price(*amount)
            ),
            SimCommand::SetFair {
                symbol,
                price,
                drift_bps,
            } => format!(
                "{symbol} fair value {} drifting {drift_bps} bps per minute",
                ome_core::price::format_price(*price)
            ),
            SimCommand::Scenario(s) => s.describe(),
        }
    }
}

struct Flat {
    code: u8,
    symbol: Option<Symbol>,
    n: u64,
    i: i64,
    seconds: u64,
    group: String,
    key: String,
    value: String,
}

impl Flat {
    fn of(command: &SimCommand) -> Flat {
        let mut f = Flat {
            code: 0,
            symbol: None,
            n: 0,
            i: 0,
            seconds: 0,
            group: String::new(),
            key: String::new(),
            value: String::new(),
        };
        match command {
            SimCommand::Pause { group } => (f.code, f.group) = (1, group.clone()),
            SimCommand::Resume { group } => (f.code, f.group) = (2, group.clone()),
            SimCommand::SetCount { group, count } => {
                (f.code, f.group, f.n) = (3, group.clone(), *count as u64)
            }
            SimCommand::SetParam { group, key, value } => {
                (f.code, f.group, f.key, f.value) = (4, group.clone(), key.clone(), value.clone())
            }
            SimCommand::Fund { group, amount } => {
                (f.code, f.group, f.n) = (5, group.clone(), *amount)
            }
            SimCommand::SetFair {
                symbol,
                price,
                drift_bps,
            } => (f.code, f.symbol, f.n, f.i) = (6, Some(*symbol), *price, *drift_bps),
            SimCommand::Scenario(Scenario::FlashCrash { symbol, seconds }) => {
                (f.code, f.symbol, f.seconds) = (7, Some(*symbol), *seconds)
            }
            SimCommand::Scenario(Scenario::NewsShock {
                symbol,
                move_bps,
                seconds,
            }) => (f.code, f.symbol, f.i, f.seconds) = (8, Some(*symbol), *move_bps, *seconds),
            SimCommand::Scenario(Scenario::LiquidityDrought { seconds }) => {
                (f.code, f.seconds) = (9, *seconds)
            }
            SimCommand::Scenario(Scenario::VolatilityBurst { seconds }) => {
                (f.code, f.seconds) = (10, *seconds)
            }
        }
        f
    }

    fn command(self) -> Result<SimCommand, DecodeError> {
        let symbol = || self.symbol.ok_or(DecodeError::Invalid("command symbol"));
        Ok(match self.code {
            1 => SimCommand::Pause { group: self.group },
            2 => SimCommand::Resume { group: self.group },
            3 => SimCommand::SetCount {
                group: self.group,
                count: self.n as u32,
            },
            4 => SimCommand::SetParam {
                group: self.group,
                key: self.key,
                value: self.value,
            },
            5 => SimCommand::Fund {
                group: self.group,
                amount: self.n,
            },
            6 => SimCommand::SetFair {
                symbol: symbol()?,
                price: self.n,
                drift_bps: self.i,
            },
            7 => SimCommand::Scenario(Scenario::FlashCrash {
                symbol: symbol()?,
                seconds: self.seconds,
            }),
            8 => SimCommand::Scenario(Scenario::NewsShock {
                symbol: symbol()?,
                move_bps: self.i,
                seconds: self.seconds,
            }),
            9 => SimCommand::Scenario(Scenario::LiquidityDrought {
                seconds: self.seconds,
            }),
            10 => SimCommand::Scenario(Scenario::VolatilityBurst {
                seconds: self.seconds,
            }),
            _ => return Err(DecodeError::Invalid("sim command")),
        })
    }
}

pub fn put_command(e: &mut Encoder, command: &SimCommand) {
    let f = Flat::of(command);
    e.u8(f.code)
        .symbol(f.symbol)
        .u64(f.n)
        .i64(f.i)
        .u64(f.seconds)
        .end_block()
        .var(&f.group)
        .var(&f.key)
        .var(&f.value);
}

pub fn get_command(d: &mut Decoder) -> Result<SimCommand, DecodeError> {
    let code = d.u8()?;
    let symbol = d.optional_symbol()?;
    let n = d.u64()?;
    let i = d.i64()?;
    let seconds = d.u64()?;
    d.end_block()?;
    Flat {
        code,
        symbol,
        n,
        i,
        seconds,
        group: d.var()?,
        key: d.var()?,
        value: d.var()?,
    }
    .command()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimStatus {
    pub runners: u32,
    pub groups: Vec<GroupInfo>,
    pub fair: Vec<(Symbol, Price, i64)>,
    pub scenarios: Vec<(String, Timestamp)>,
}

pub fn put_groups(e: &mut Encoder, groups: &[GroupInfo]) {
    let mut g = e.begin_group(groups.len());
    for info in groups {
        e.begin_entry(&mut g);
        e.u32(info.target)
            .u32(info.running)
            .u8(info.paused as u8)
            .u64(info.fills)
            .u64(info.equity);
        e.end_entry_block(&mut g);
        e.var(&info.name)
            .var(&info.strategy)
            .var(&info.broker)
            .var(&info.params);
    }
}

pub fn get_groups(d: &mut Decoder) -> Result<Vec<GroupInfo>, DecodeError> {
    let g = d.group()?;
    let mut groups = Vec::with_capacity(g.count);
    for _ in 0..g.count {
        let start = d.entry_start();
        let target = d.u32()?;
        let running = d.u32()?;
        let paused = d.u8()? != 0;
        let fills = d.u64()?;
        let equity = d.u64()?;
        d.end_entry_block(start, &g)?;
        groups.push(GroupInfo {
            name: d.var()?,
            strategy: d.var()?,
            broker: d.var()?,
            target,
            running,
            paused,
            fills,
            equity,
            params: d.var()?,
        });
    }
    Ok(groups)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimMessage {
    Logon {
        role: SimRole,
        name: String,
        secret: String,
    },
    Groups(Vec<GroupInfo>),
    Command(SimCommand),
    Accelerate {
        pre_open_s: u64,
        open_s: u64,
        closing_s: u64,
        closed_s: u64,
    },
    QueryStatus,
}

impl SimMessage {
    pub fn template_id(&self) -> u16 {
        match self {
            SimMessage::Logon { .. } => SIM_LOGON,
            SimMessage::Groups(_) => SIM_GROUPS,
            SimMessage::Command(_) => SIM_COMMAND,
            SimMessage::Accelerate { .. } => SIM_ACCELERATE,
            SimMessage::QueryStatus => SIM_QUERY,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new(self.template_id());
        match self {
            SimMessage::Logon { role, name, secret } => {
                e.u8(*role as u8).end_block().var(name).var(secret);
            }
            SimMessage::Groups(groups) => {
                e.end_block();
                put_groups(&mut e, groups);
            }
            SimMessage::Command(command) => put_command(&mut e, command),
            SimMessage::Accelerate {
                pre_open_s,
                open_s,
                closing_s,
                closed_s,
            } => {
                e.u64(*pre_open_s)
                    .u64(*open_s)
                    .u64(*closing_s)
                    .u64(*closed_s)
                    .end_block();
            }
            SimMessage::QueryStatus => {
                e.end_block();
            }
        }
        e.finish()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(bytes)?;
        Ok(match d.template_id {
            SIM_LOGON => {
                let role = match d.u8()? {
                    1 => SimRole::Operator,
                    2 => SimRole::Runner,
                    _ => return Err(DecodeError::Invalid("sim role")),
                };
                d.end_block()?;
                SimMessage::Logon {
                    role,
                    name: d.var()?,
                    secret: d.var()?,
                }
            }
            SIM_GROUPS => {
                d.end_block()?;
                SimMessage::Groups(get_groups(&mut d)?)
            }
            SIM_COMMAND => SimMessage::Command(get_command(&mut d)?),
            SIM_ACCELERATE => {
                let message = SimMessage::Accelerate {
                    pre_open_s: d.u64()?,
                    open_s: d.u64()?,
                    closing_s: d.u64()?,
                    closed_s: d.u64()?,
                };
                d.end_block()?;
                message
            }
            SIM_QUERY => {
                d.end_block()?;
                SimMessage::QueryStatus
            }
            other => return Err(DecodeError::UnknownTemplate(other)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acme() -> Symbol {
        Symbol::parse("ACME").unwrap()
    }

    pub fn commands() -> Vec<SimCommand> {
        vec![
            SimCommand::Pause {
                group: "noise".into(),
            },
            SimCommand::Resume { group: "*".into() },
            SimCommand::SetCount {
                group: "swing".into(),
                count: 25,
            },
            SimCommand::SetParam {
                group: "hft-vega".into(),
                key: "gamma".into(),
                value: "0.9".into(),
            },
            SimCommand::Fund {
                group: "daytrader".into(),
                amount: 5_000_00,
            },
            SimCommand::SetFair {
                symbol: acme(),
                price: 26_00,
                drift_bps: -3,
            },
            SimCommand::Scenario(Scenario::FlashCrash {
                symbol: acme(),
                seconds: 20,
            }),
            SimCommand::Scenario(Scenario::NewsShock {
                symbol: acme(),
                move_bps: -800,
                seconds: 60,
            }),
            SimCommand::Scenario(Scenario::LiquidityDrought { seconds: 30 }),
            SimCommand::Scenario(Scenario::VolatilityBurst { seconds: 45 }),
        ]
    }

    #[test]
    fn every_sim_message_round_trips() {
        let mut messages = vec![
            SimMessage::Logon {
                role: SimRole::Runner,
                name: "runner".into(),
                secret: "pw".into(),
            },
            SimMessage::Groups(vec![GroupInfo {
                name: "noise".into(),
                strategy: "noise".into(),
                broker: "retail-co".into(),
                target: 15,
                running: 14,
                paused: true,
                fills: 900,
                equity: 30_000_000,
                params: "activity=0.5".into(),
            }]),
            SimMessage::Accelerate {
                pre_open_s: 3,
                open_s: 30,
                closing_s: 3,
                closed_s: 3,
            },
            SimMessage::QueryStatus,
        ];
        messages.extend(commands().into_iter().map(SimMessage::Command));
        for message in messages {
            assert_eq!(SimMessage::decode(&message.encode()), Ok(message));
        }
        assert!(SimMessage::decode(&[1, 2, 3]).is_err());
    }

    #[test]
    fn scenarios_describe_themselves_and_keep_their_kind_when_shortened() {
        let shock = Scenario::NewsShock {
            symbol: acme(),
            move_bps: -850,
            seconds: 60,
        };
        assert!(shock.describe().contains("-8.50%"));
        assert_eq!(shock.with_seconds(5).seconds(), 5);
        assert!(matches!(
            shock.with_seconds(5),
            Scenario::NewsShock { move_bps: -850, .. }
        ));
    }
}
