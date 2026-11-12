use crate::exchange::Command;
use ome_core::types::{OrderKind, Timestamp};
use ome_protocol::framing::{frame, read_frame};
use ome_protocol::{Asset, DecodeError, Decoder, Encoder, Role, side_from_u8, side_to_u8};
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

mod template {
    pub const CREATE_ACCOUNT: u16 = 200;
    pub const SET_BALANCE: u16 = 201;
    pub const LIST_INSTRUMENT: u16 = 202;
    pub const HALT: u16 = 203;
    pub const RESUME: u16 = 204;
    pub const NEW_ORDER: u16 = 205;
    pub const CANCEL: u16 = 206;
    pub const CANCEL_ALL: u16 = 207;
}

pub struct Journal {
    writer: BufWriter<File>,
}

impl Journal {
    pub fn open(path: &Path) -> io::Result<(Self, Vec<(Timestamp, Command)>)> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)?;

        let mut entries = Vec::new();
        let mut good_len = 0u64;
        {
            let mut reader = BufReader::new(&mut file);
            loop {
                match read_frame(&mut reader) {
                    Ok(Some(payload)) => match decode_entry(&payload) {
                        Ok(entry) => {
                            entries.push(entry);
                            good_len += (payload.len() + ome_protocol::framing::SOFH_LEN) as u64;
                        }
                        Err(e) => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("corrupt journal entry #{}: {e}", entries.len() + 1),
                            ));
                        }
                    },
                    Ok(None) => break,
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                    Err(e) => return Err(e),
                }
            }
        }

        if file.metadata()?.len() > good_len {
            file.set_len(good_len)?;
        }
        file.seek(SeekFrom::End(0))?;
        Ok((
            Self {
                writer: BufWriter::new(file),
            },
            entries,
        ))
    }

    pub fn append(&mut self, timestamp: Timestamp, command: &Command) -> io::Result<()> {
        self.writer
            .write_all(&frame(&encode_entry(timestamp, command)))
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

pub fn encode_entry(ts: Timestamp, command: &Command) -> Vec<u8> {
    let template_id = match command {
        Command::CreateAccount { .. } => template::CREATE_ACCOUNT,
        Command::SetBalance { .. } => template::SET_BALANCE,
        Command::ListInstrument { .. } => template::LIST_INSTRUMENT,
        Command::Halt { .. } => template::HALT,
        Command::Resume { .. } => template::RESUME,
        Command::NewOrder { .. } => template::NEW_ORDER,
        Command::Cancel { .. } => template::CANCEL,
        Command::CancelAll { .. } => template::CANCEL_ALL,
    };
    let mut e = Encoder::new(template_id);
    e.u64(ts);
    match command {
        Command::CreateAccount {
            role,
            username,
            secret,
            cash,
        } => {
            e.u8(*role as u8)
                .u64(*cash)
                .bytes32(secret)
                .end_block()
                .var(username);
        }
        Command::SetBalance {
            account,
            asset,
            amount,
        } => {
            let symbol = match asset {
                Asset::Cash => None,
                Asset::Shares(s) => Some(*s),
            };
            e.u64(*account).symbol(symbol).u64(*amount).end_block();
        }
        Command::ListInstrument {
            symbol,
            name,
            ipo_price,
            shares,
            issuer,
        } => {
            e.symbol(Some(*symbol))
                .u64(*ipo_price)
                .u64(*shares)
                .u64(*issuer)
                .end_block()
                .var(name);
        }
        Command::Halt { symbol, reason } => {
            e.symbol(Some(*symbol)).end_block().var(reason);
        }
        Command::Resume { symbol } => {
            e.symbol(Some(*symbol)).end_block();
        }
        Command::NewOrder {
            account,
            client_order_id,
            symbol,
            side,
            kind,
            qty,
        } => {
            let (ord_type, price) = match kind {
                OrderKind::Limit(p) => (1, *p),
                OrderKind::Market => (2, 0),
            };
            e.u64(*account)
                .u64(*client_order_id)
                .symbol(Some(*symbol))
                .u8(side_to_u8(*side))
                .u8(ord_type)
                .u64(price)
                .u64(*qty)
                .end_block();
        }
        Command::Cancel { account, order_id } => {
            e.u64(*account).u64(*order_id).end_block();
        }
        Command::CancelAll { account, symbol } => {
            e.u64(*account).symbol(*symbol).end_block();
        }
    }
    e.finish()
}

pub fn decode_entry(bytes: &[u8]) -> Result<(Timestamp, Command), DecodeError> {
    let mut d = Decoder::new(bytes)?;
    let ts = d.u64()?;
    let command = match d.template_id {
        template::CREATE_ACCOUNT => {
            let role = Role::from_u8(d.u8()?).ok_or(DecodeError::Invalid("role"))?;
            let cash = d.u64()?;
            let secret = d.bytes32()?;
            d.end_block()?;
            Command::CreateAccount {
                role,
                username: d.var()?,
                secret,
                cash,
            }
        }
        template::SET_BALANCE => {
            let account = d.u64()?;
            let asset = d.optional_symbol()?.map_or(Asset::Cash, Asset::Shares);
            let amount = d.u64()?;
            d.end_block()?;
            Command::SetBalance {
                account,
                asset,
                amount,
            }
        }
        template::LIST_INSTRUMENT => {
            let symbol = d.symbol()?;
            let ipo_price = d.u64()?;
            let shares = d.u64()?;
            let issuer = d.u64()?;
            d.end_block()?;
            Command::ListInstrument {
                symbol,
                name: d.var()?,
                ipo_price,
                shares,
                issuer,
            }
        }
        template::HALT => {
            let symbol = d.symbol()?;
            d.end_block()?;
            Command::Halt {
                symbol,
                reason: d.var()?,
            }
        }
        template::RESUME => {
            let symbol = d.symbol()?;
            d.end_block()?;
            Command::Resume { symbol }
        }
        template::NEW_ORDER => {
            let account = d.u64()?;
            let client_order_id = d.u64()?;
            let symbol = d.symbol()?;
            let side = side_from_u8(d.u8()?)?;
            let ord_type = d.u8()?;
            let price = d.u64()?;
            let qty = d.u64()?;
            d.end_block()?;
            let kind = match ord_type {
                1 => OrderKind::Limit(price),
                2 => OrderKind::Market,
                _ => return Err(DecodeError::Invalid("order type")),
            };
            Command::NewOrder {
                account,
                client_order_id,
                symbol,
                side,
                kind,
                qty,
            }
        }
        template::CANCEL => {
            let account = d.u64()?;
            let order_id = d.u64()?;
            d.end_block()?;
            Command::Cancel { account, order_id }
        }
        template::CANCEL_ALL => {
            let account = d.u64()?;
            let symbol = d.optional_symbol()?;
            d.end_block()?;
            Command::CancelAll { account, symbol }
        }
        other => return Err(DecodeError::UnknownTemplate(other)),
    };
    Ok((ts, command))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::hash_secret;
    use crate::exchange::{Exchange, ExchangeConfig};
    use ome_core::symbol::Symbol;
    use ome_core::types::Side;

    fn sym(s: &str) -> Symbol {
        Symbol::parse(s).unwrap()
    }

    fn script() -> Vec<Command> {
        let mut commands = vec![
            Command::CreateAccount {
                role: Role::Institutional,
                username: "issuer".into(),
                secret: hash_secret("issuer", "pw"),
                cash: 0,
            },
            Command::CreateAccount {
                role: Role::Retail,
                username: "alice".into(),
                secret: hash_secret("alice", "pw"),
                cash: 50_000_00,
            },
            Command::CreateAccount {
                role: Role::Bot,
                username: "bot".into(),
                secret: hash_secret("bot", "pw"),
                cash: 500_000_00,
            },
            Command::ListInstrument {
                symbol: sym("ACME"),
                name: "Acme".into(),
                ipo_price: 25_00,
                shares: 500,
                issuer: 1,
            },
            Command::SetBalance {
                account: 3,
                asset: Asset::Shares(sym("ACME")),
                amount: 300,
            },
        ];
        for i in 0..40u64 {
            let side = if i % 3 == 0 { Side::Sell } else { Side::Buy };
            let account = if i % 2 == 0 { 2 } else { 3 };
            let kind = if i % 5 == 0 {
                OrderKind::Market
            } else {
                OrderKind::Limit(24_00 + (i * 7) % 150)
            };
            commands.push(Command::NewOrder {
                account,
                client_order_id: i,
                symbol: sym("ACME"),
                side,
                kind,
                qty: 1 + i % 9,
            });
            if i % 11 == 0 {
                commands.push(Command::Cancel {
                    account,
                    order_id: i / 2 + 2,
                });
            }
        }
        commands.push(Command::Halt {
            symbol: sym("ACME"),
            reason: "maintenance".into(),
        });
        commands.push(Command::CancelAll {
            account: 2,
            symbol: None,
        });
        commands.push(Command::Resume {
            symbol: sym("ACME"),
        });
        commands
    }

    #[test]
    fn every_command_round_trips() {
        for (i, command) in script().into_iter().enumerate() {
            let bytes = encode_entry(i as u64, &command);
            assert_eq!(decode_entry(&bytes), Ok((i as u64, command)));
        }
    }

    #[test]
    fn replay_rebuilds_identical_state() {
        let dir = std::env::temp_dir().join(format!("ome-journal-{}", std::process::id()));
        let path = dir.join("test.journal");
        let _ = std::fs::remove_file(&path);

        let mut live = Exchange::new(ExchangeConfig::default());
        {
            let (mut journal, entries) = Journal::open(&path).unwrap();
            assert!(entries.is_empty());
            for (ts, command) in script().iter().enumerate() {
                journal.append(ts as u64, command).unwrap();
                let _ = live.execute(command, ts as u64);
            }
            journal.flush().unwrap();
        }

        let (_, entries) = Journal::open(&path).unwrap();
        let mut replayed = Exchange::new(ExchangeConfig::default());
        for (ts, command) in &entries {
            let _ = replayed.execute(command, *ts);
        }

        assert_eq!(entries.len(), script().len());
        assert_eq!(live.account_list(), replayed.account_list());
        assert_eq!(live.instrument_list(), replayed.instrument_list());
        for id in 1..=3 {
            assert_eq!(live.account_snapshot(id), replayed.account_snapshot(id));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn torn_tail_is_truncated_on_open() {
        let dir = std::env::temp_dir().join(format!("ome-torn-{}", std::process::id()));
        let path = dir.join("torn.journal");
        let _ = std::fs::remove_file(&path);
        {
            let (mut journal, _) = Journal::open(&path).unwrap();
            journal
                .append(
                    1,
                    &Command::Resume {
                        symbol: sym("ACME"),
                    },
                )
                .unwrap();
            journal.flush().unwrap();
        }
        let good_len = std::fs::metadata(&path).unwrap().len();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&[0, 0, 0, 40, 0x5B])
            .unwrap();

        let (mut journal, entries) = Journal::open(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), good_len);
        journal
            .append(
                2,
                &Command::Resume {
                    symbol: sym("ACME"),
                },
            )
            .unwrap();
        journal.flush().unwrap();
        assert_eq!(Journal::open(&path).unwrap().1.len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
