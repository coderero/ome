use ome_core::price::{Price, format_price};
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, Qty, Side, Timestamp};
use ome_net::throttle::TokenBucket;
use ome_protocol::Role;
use std::collections::{BTreeMap, BTreeSet, HashMap};

const DUPLICATE_WINDOW_MS: Timestamp = 1_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Basis {
    qty: Qty,
    cost: u128,
}

#[derive(Default)]
pub struct AccountRisk {
    suspended: BTreeSet<AccountId>,
    restricted: BTreeSet<Symbol>,
    loss_limits: BTreeMap<AccountId, u64>,
    realized: BTreeMap<AccountId, i64>,
    basis: BTreeMap<(AccountId, Symbol), Basis>,
    day: u64,
}

impl AccountRisk {
    pub fn set_suspended(&mut self, account: AccountId, suspended: bool) {
        if suspended {
            self.suspended.insert(account);
        } else {
            self.suspended.remove(&account);
        }
    }

    pub fn is_suspended(&self, account: AccountId) -> bool {
        self.suspended.contains(&account)
    }

    pub fn set_restricted(&mut self, symbol: Symbol, restricted: bool) {
        if restricted {
            self.restricted.insert(symbol);
        } else {
            self.restricted.remove(&symbol);
        }
    }

    pub fn restricted(&self) -> impl Iterator<Item = &Symbol> {
        self.restricted.iter()
    }

    pub fn set_loss_limit(&mut self, account: AccountId, amount: u64) {
        if amount == 0 {
            self.loss_limits.remove(&account);
        } else {
            self.loss_limits.insert(account, amount);
        }
    }

    pub fn loss_limit(&self, account: AccountId) -> u64 {
        self.loss_limits.get(&account).copied().unwrap_or(0)
    }

    pub fn day(&self) -> u64 {
        self.day
    }

    pub fn start_day(&mut self, day: u64, marks: &[(Symbol, Price)]) {
        self.day = self.day.max(day);
        self.realized.clear();
        let marks: BTreeMap<Symbol, Price> = marks.iter().copied().collect();
        for ((_, symbol), basis) in self.basis.iter_mut() {
            if let Some(mark) = marks.get(symbol) {
                basis.cost = basis.qty as u128 * *mark as u128;
            }
        }
    }

    pub fn on_fill(
        &mut self,
        account: AccountId,
        symbol: Symbol,
        side: Side,
        (price, qty): (Price, Qty),
        charges: i64,
    ) {
        let basis = self.basis.entry((account, symbol)).or_default();
        let realized = self.realized.entry(account).or_default();
        *realized -= charges;
        match side {
            Side::Buy => {
                basis.qty += qty;
                basis.cost += price as u128 * qty as u128;
            }
            Side::Sell => {
                let tracked = qty.min(basis.qty);
                if tracked > 0 {
                    let cost = basis.cost * tracked as u128 / basis.qty as u128;
                    *realized += (price as u128 * tracked as u128) as i64 - cost as i64;
                    basis.cost -= cost;
                    basis.qty -= tracked;
                }
            }
        }
        if basis.qty == 0 {
            self.basis.remove(&(account, symbol));
        }
    }

    pub fn day_pnl(&self, account: AccountId, mark: impl Fn(Symbol) -> Option<Price>) -> i64 {
        let unrealized: i64 = self
            .basis
            .iter()
            .filter(|((holder, _), _)| *holder == account)
            .filter_map(|((_, symbol), basis)| {
                mark(*symbol).map(|m| (basis.qty as u128 * m as u128) as i64 - basis.cost as i64)
            })
            .sum();
        self.realized.get(&account).copied().unwrap_or(0) + unrealized
    }

    pub fn check(
        &self,
        account: AccountId,
        symbol: Symbol,
        side: Side,
        mark: impl Fn(Symbol) -> Option<Price>,
    ) -> Result<(), String> {
        if self.is_suspended(account) {
            return Err("account is suspended by the broker".into());
        }
        if side == Side::Sell {
            return Ok(());
        }
        if self.restricted.contains(&symbol) {
            return Err(format!(
                "{symbol} is on the broker's restricted list: closing sells only"
            ));
        }
        let limit = self.loss_limit(account);
        let pnl = self.day_pnl(account, mark);
        if limit > 0 && pnl <= -(limit as i64) {
            return Err(format!(
                "daily loss limit {} reached (day P&L -{}): closing sells only",
                format_price(limit),
                format_price(pnl.unsigned_abs())
            ));
        }
        Ok(())
    }
}

pub fn rate_limit(role: Role) -> Option<(f64, f64)> {
    match role {
        Role::Retail => Some((10.0, 20.0)),
        Role::Issuer => Some((5.0, 10.0)),
        Role::Prop => Some((200.0, 400.0)),
        Role::Quant => Some((500.0, 1_000.0)),
        Role::Hft | Role::LpDesk => Some((5_000.0, 10_000.0)),
        Role::BrokerAdmin => None,
    }
}

pub fn checks_duplicates(role: Role) -> bool {
    matches!(role, Role::Retail | Role::Prop | Role::Issuer)
}

type OrderKey = (AccountId, Symbol, Side, Price, Qty);

#[derive(Default)]
pub struct EntryGuard {
    buckets: HashMap<AccountId, TokenBucket>,
    recent: HashMap<OrderKey, Timestamp>,
}

impl EntryGuard {
    pub fn admit(&mut self, account: AccountId, role: Role, now: Timestamp) -> Result<(), String> {
        let Some((per_second, burst)) = rate_limit(role) else {
            return Ok(());
        };
        let bucket = self
            .buckets
            .entry(account)
            .or_insert_with(|| TokenBucket::new(per_second, burst, now));
        if bucket.allow(now) {
            Ok(())
        } else {
            Err(format!(
                "throttled: {} accounts may send {per_second} order messages per second",
                role.label()
            ))
        }
    }

    pub fn duplicate(&mut self, key: OrderKey, role: Role, now: Timestamp) -> Result<(), String> {
        if !checks_duplicates(role) {
            return Ok(());
        }
        self.recent
            .retain(|_, at| now.saturating_sub(*at) < DUPLICATE_WINDOW_MS);
        if self.recent.contains_key(&key) {
            return Err(format!(
                "duplicate order: the same order was sent less than {DUPLICATE_WINDOW_MS} ms ago"
            ));
        }
        self.recent.insert(key, now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acme() -> Symbol {
        Symbol::parse("ACME").unwrap()
    }

    #[test]
    fn realized_and_unrealized_pnl_use_average_cost() {
        let mut risk = AccountRisk::default();
        risk.on_fill(1, acme(), Side::Buy, (10_00, 10), 5);
        risk.on_fill(1, acme(), Side::Buy, (12_00, 10), 5);
        assert_eq!(risk.day_pnl(1, |_| Some(11_00)), -10);
        risk.on_fill(1, acme(), Side::Sell, (13_00, 10), 5);
        assert_eq!(risk.day_pnl(1, |_| Some(11_00)), 20_00 - 15);
        risk.on_fill(1, acme(), Side::Sell, (9_00, 25), 0);
        assert_eq!(risk.day_pnl(1, |_| None), 20_00 - 15 - 20_00);
    }

    #[test]
    fn a_new_day_rebases_carried_positions_to_the_close() {
        let mut risk = AccountRisk::default();
        risk.on_fill(1, acme(), Side::Buy, (10_00, 10), 0);
        risk.on_fill(1, acme(), Side::Sell, (11_00, 5), 0);
        assert_eq!(risk.day_pnl(1, |_| Some(12_00)), 5_00 + 10_00);
        risk.start_day(2, &[(acme(), 12_00)]);
        assert_eq!(risk.day_pnl(1, |_| Some(12_00)), 0);
        assert_eq!(risk.day_pnl(1, |_| Some(11_00)), -5_00);
    }

    #[test]
    fn suspension_restriction_and_loss_limit_block_new_risk_but_not_exits() {
        let mut risk = AccountRisk::default();
        let mark = |_| Some(10_00);
        assert!(risk.check(1, acme(), Side::Buy, mark).is_ok());
        risk.set_restricted(acme(), true);
        assert!(
            risk.check(1, acme(), Side::Buy, mark)
                .unwrap_err()
                .contains("restricted")
        );
        assert!(risk.check(1, acme(), Side::Sell, mark).is_ok());
        risk.set_restricted(acme(), false);

        risk.set_loss_limit(1, 50_00);
        risk.on_fill(1, acme(), Side::Buy, (16_00, 10), 0);
        assert!(risk.check(1, acme(), Side::Buy, |_| Some(12_00)).is_ok());
        assert!(
            risk.check(1, acme(), Side::Buy, mark)
                .unwrap_err()
                .contains("loss limit")
        );
        assert!(risk.check(1, acme(), Side::Sell, mark).is_ok());

        risk.set_suspended(1, true);
        assert!(
            risk.check(1, acme(), Side::Sell, mark)
                .unwrap_err()
                .contains("suspended")
        );
    }

    #[test]
    fn entry_guard_throttles_per_account_and_catches_duplicates() {
        let mut guard = EntryGuard::default();
        assert!((0..20).all(|_| guard.admit(1, Role::Retail, 0).is_ok()));
        assert!(guard.admit(1, Role::Retail, 0).is_err());
        assert!(guard.admit(2, Role::Retail, 0).is_ok());
        assert!(guard.admit(3, Role::BrokerAdmin, 0).is_ok());

        let key = (1, acme(), Side::Buy, 10_00, 5);
        assert!(guard.duplicate(key, Role::Retail, 0).is_ok());
        assert!(guard.duplicate(key, Role::Retail, 400).is_err());
        assert!(guard.duplicate(key, Role::Retail, 900).is_err());
        assert!(guard.duplicate(key, Role::Retail, 1_000).is_ok());
        assert!(guard.duplicate(key, Role::Hft, 1_600).is_ok());
    }
}
