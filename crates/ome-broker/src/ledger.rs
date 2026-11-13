use ome_core::price::format_price;
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, Balance, Qty};
use ome_net::auth::SecretHash;
use ome_protocol::Role;
use std::collections::{BTreeMap, HashMap};
use std::fmt;

pub struct Account {
    pub id: AccountId,
    pub username: String,
    pub role: Role,
    pub secret: SecretHash,
    pub cash: Balance,
    pub positions: BTreeMap<Symbol, Balance>,
}

impl Account {
    pub fn position(&self, symbol: Symbol) -> Balance {
        self.positions.get(&symbol).copied().unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    UnknownAccount(AccountId),
    DuplicateUsername(String),
    InsufficientCash {
        needed: u64,
        available: u64,
    },
    InsufficientShares {
        symbol: Symbol,
        needed: Qty,
        available: Qty,
    },
}

impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LedgerError::UnknownAccount(id) => write!(f, "unknown account {id}"),
            LedgerError::DuplicateUsername(name) => write!(f, "username '{name}' is taken"),
            LedgerError::InsufficientCash { needed, available } => write!(
                f,
                "insufficient cash: need {}, available {}",
                format_price(*needed),
                format_price(*available)
            ),
            LedgerError::InsufficientShares {
                symbol,
                needed,
                available,
            } => write!(
                f,
                "insufficient {symbol} shares: need {needed}, available {available}"
            ),
        }
    }
}

#[derive(Default)]
pub struct Ledger {
    accounts: BTreeMap<AccountId, Account>,
    by_name: HashMap<String, AccountId>,
    next_id: AccountId,
}

impl Ledger {
    pub fn open(
        &mut self,
        username: &str,
        role: Role,
        secret: SecretHash,
        cash: u64,
    ) -> Result<AccountId, LedgerError> {
        if self.by_name.contains_key(username) {
            return Err(LedgerError::DuplicateUsername(username.to_string()));
        }
        self.next_id += 1;
        let id = self.next_id;
        self.by_name.insert(username.to_string(), id);
        self.accounts.insert(
            id,
            Account {
                id,
                username: username.to_string(),
                role,
                secret,
                cash: Balance {
                    available: cash,
                    reserved: 0,
                },
                positions: BTreeMap::new(),
            },
        );
        Ok(id)
    }

    pub fn get(&self, id: AccountId) -> Option<&Account> {
        self.accounts.get(&id)
    }

    pub fn find(&self, username: &str) -> Option<&Account> {
        self.by_name
            .get(username)
            .and_then(|id| self.accounts.get(id))
    }

    pub fn accounts(&self) -> impl Iterator<Item = &Account> {
        self.accounts.values()
    }

    fn account_mut(&mut self, id: AccountId) -> Result<&mut Account, LedgerError> {
        self.accounts
            .get_mut(&id)
            .ok_or(LedgerError::UnknownAccount(id))
    }

    pub fn set_cash(&mut self, id: AccountId, amount: u64) -> Result<(), LedgerError> {
        self.account_mut(id)?.cash.available = amount;
        Ok(())
    }

    pub fn set_shares(
        &mut self,
        id: AccountId,
        symbol: Symbol,
        qty: Qty,
    ) -> Result<(), LedgerError> {
        self.account_mut(id)?
            .positions
            .entry(symbol)
            .or_default()
            .available = qty;
        Ok(())
    }

    pub fn credit_shares(
        &mut self,
        id: AccountId,
        symbol: Symbol,
        qty: Qty,
    ) -> Result<(), LedgerError> {
        self.account_mut(id)?
            .positions
            .entry(symbol)
            .or_default()
            .available += qty;
        Ok(())
    }

    pub fn reserve_cash(&mut self, id: AccountId, amount: u64) -> Result<(), LedgerError> {
        let cash = &mut self.account_mut(id)?.cash;
        if cash.available < amount {
            return Err(LedgerError::InsufficientCash {
                needed: amount,
                available: cash.available,
            });
        }
        cash.available -= amount;
        cash.reserved += amount;
        Ok(())
    }

    pub fn release_cash(&mut self, id: AccountId, amount: u64) {
        if let Ok(account) = self.account_mut(id) {
            let amount = amount.min(account.cash.reserved);
            account.cash.reserved -= amount;
            account.cash.available += amount;
        }
    }

    pub fn reserve_shares(
        &mut self,
        id: AccountId,
        symbol: Symbol,
        qty: Qty,
    ) -> Result<(), LedgerError> {
        let position = self.account_mut(id)?.positions.entry(symbol).or_default();
        if position.available < qty {
            return Err(LedgerError::InsufficientShares {
                symbol,
                needed: qty,
                available: position.available,
            });
        }
        position.available -= qty;
        position.reserved += qty;
        Ok(())
    }

    pub fn release_shares(&mut self, id: AccountId, symbol: Symbol, qty: Qty) {
        if let Ok(account) = self.account_mut(id)
            && let Some(position) = account.positions.get_mut(&symbol)
        {
            let qty = qty.min(position.reserved);
            position.reserved -= qty;
            position.available += qty;
        }
    }

    pub fn settle_buy(
        &mut self,
        id: AccountId,
        symbol: Symbol,
        consumed: u64,
        paid: i64,
        qty: Qty,
    ) {
        if let Ok(account) = self.account_mut(id) {
            account.cash.reserved -= consumed.min(account.cash.reserved);
            let available = account.cash.available as i128 + consumed as i128 - paid as i128;
            account.cash.available = available.max(0) as u64;
            account.positions.entry(symbol).or_default().available += qty;
        }
    }

    pub fn settle_sell(&mut self, id: AccountId, symbol: Symbol, qty: Qty, proceeds: i64) {
        if let Ok(account) = self.account_mut(id) {
            let position = account.positions.entry(symbol).or_default();
            position.reserved -= qty.min(position.reserved);
            let available = account.cash.available as i128 + proceeds as i128;
            account.cash.available = available.max(0) as u64;
        }
    }

    #[cfg(test)]
    pub fn total_cash(&self) -> u64 {
        self.accounts.values().map(|a| a.cash.total()).sum()
    }

    #[cfg(test)]
    pub fn total_shares(&self, symbol: Symbol) -> Qty {
        self.accounts
            .values()
            .map(|a| a.position(symbol).total())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym() -> Symbol {
        Symbol::parse("ACME").unwrap()
    }

    #[test]
    fn reserve_and_release_move_between_buckets() {
        let mut ledger = Ledger::default();
        let id = ledger.open("a", Role::Retail, [0; 32], 1_000).unwrap();

        ledger.reserve_cash(id, 600).unwrap();
        assert_eq!(
            ledger.get(id).unwrap().cash,
            Balance {
                available: 400,
                reserved: 600
            }
        );
        assert!(matches!(
            ledger.reserve_cash(id, 500),
            Err(LedgerError::InsufficientCash { .. })
        ));

        ledger.release_cash(id, 600);
        assert_eq!(ledger.get(id).unwrap().cash.available, 1_000);
    }

    #[test]
    fn settlement_conserves_cash_and_shares() {
        let mut ledger = Ledger::default();
        let buyer = ledger.open("b", Role::Retail, [0; 32], 10_000).unwrap();
        let seller = ledger.open("s", Role::Retail, [0; 32], 0).unwrap();
        ledger.credit_shares(seller, sym(), 10).unwrap();

        ledger.reserve_cash(buyer, 1_100 * 5).unwrap();
        ledger.reserve_shares(seller, sym(), 5).unwrap();
        ledger.settle_buy(buyer, sym(), 1_100 * 5, 1_000 * 5, 5);
        ledger.settle_sell(seller, sym(), 5, 1_000 * 5);

        assert_eq!(ledger.total_cash(), 10_000);
        assert_eq!(ledger.total_shares(sym()), 10);
        assert_eq!(ledger.get(buyer).unwrap().cash.available, 5_000);
        assert_eq!(ledger.get(seller).unwrap().cash.available, 5_000);
        assert_eq!(ledger.get(buyer).unwrap().position(sym()).available, 5);
    }

    #[test]
    fn usernames_are_unique() {
        let mut ledger = Ledger::default();
        ledger.open("a", Role::Retail, [0; 32], 0).unwrap();
        assert!(matches!(
            ledger.open("a", Role::Hft, [0; 32], 0),
            Err(LedgerError::DuplicateUsername(_))
        ));
        assert_eq!(ledger.find("a").unwrap().id, 1);
    }
}
