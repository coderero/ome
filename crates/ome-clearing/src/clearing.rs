use ome_core::symbol::Symbol;
use ome_net::auth::SecretHash;
use ome_net::feed::{Feeds, Replay};
use ome_protocol::{
    Allocation, ClearedTrade, ClearingMemberInfo, ClearingStatus, MemberId, MemberInfo,
    MemberMessage, ServerMessage, SettledPosition, SettlementReport,
};
use std::collections::BTreeMap;

const BPS: u128 = 10_000;

#[derive(Debug, Clone)]
pub struct ClearingConfig {
    pub cycle_ms: u64,
    pub settlement_lag: u64,
    pub margin_bps: u64,
    pub feed_capacity: usize,
}

impl Default for ClearingConfig {
    fn default() -> Self {
        Self {
            cycle_ms: 60_000,
            settlement_lag: 1,
            margin_bps: 1_000,
            feed_capacity: 100_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact {
    MemberRegistered {
        member: MemberId,
        name: String,
        secret: SecretHash,
    },
    Deposited {
        member: MemberId,
        amount: i64,
    },
    TradeCleared(ClearedTrade),
    AllocationCleared(Allocation),
    CycleOpened {
        cycle: u64,
    },
    CycleSettled {
        cycle: u64,
    },
    TradingDayStarted {
        day: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    ToExchange(MemberMessage),
    ToMember {
        member: MemberId,
        message: ServerMessage,
    },
}

struct Account {
    name: String,
    secret: Option<SecretHash>,
    cash: i64,
    securities: BTreeMap<Symbol, i64>,
    fails: u32,
}

#[derive(Default)]
struct Obligation {
    cash: i64,
    fees: i64,
    shares: BTreeMap<Symbol, i64>,
    gross: u64,
}

pub struct Clearing {
    config: ClearingConfig,
    accounts: BTreeMap<MemberId, Account>,
    obligations: BTreeMap<u64, BTreeMap<MemberId, Obligation>>,
    cycle: u64,
    next_to_settle: u64,
    fee_pool: i64,
    last_exchange_seq: u64,
    trading_day: u64,
    credit_limits: BTreeMap<MemberId, u64>,
    feeds: Feeds,
}

impl Clearing {
    pub fn new(config: ClearingConfig) -> Self {
        let feeds = Feeds::new(config.feed_capacity);
        Self {
            config,
            accounts: BTreeMap::new(),
            obligations: BTreeMap::new(),
            cycle: 0,
            next_to_settle: 0,
            fee_pool: 0,
            last_exchange_seq: 0,
            trading_day: 0,
            credit_limits: BTreeMap::new(),
            feeds,
        }
    }

    pub fn config(&self) -> &ClearingConfig {
        &self.config
    }

    pub fn cycle(&self) -> u64 {
        self.cycle
    }

    pub fn last_exchange_seq(&self) -> u64 {
        self.last_exchange_seq
    }

    pub fn is_new(&self, seq: u64) -> bool {
        seq > self.last_exchange_seq
    }

    pub fn replay(&self, member: MemberId, after_seq: u64) -> Replay {
        self.feeds.replay(member, after_seq)
    }

    pub fn find(&self, name: &str) -> Option<(MemberId, Option<SecretHash>)> {
        self.accounts
            .iter()
            .find(|(_, a)| a.name == name)
            .map(|(id, a)| (*id, a.secret))
    }

    pub fn margin_required(&self, member: MemberId) -> u64 {
        let gross: u128 = self.unsettled_gross(member) as u128;
        (gross * self.config.margin_bps as u128 / BPS) as u64
    }

    fn unsettled_gross(&self, member: MemberId) -> u64 {
        self.obligations
            .range(self.next_to_settle..)
            .filter_map(|(_, by_member)| by_member.get(&member))
            .map(|o| o.gross)
            .sum()
    }

    pub fn credit_limit(&self, member: MemberId) -> u64 {
        let cash = self.accounts.get(&member).map_or(0, |a| a.cash);
        if cash <= 0 || self.config.margin_bps == 0 {
            return 0;
        }
        (cash as u128 * BPS / self.config.margin_bps as u128).min(u64::MAX as u128) as u64
    }

    pub fn follows_trading_days(&self) -> bool {
        self.config.cycle_ms == 0
    }

    pub fn due_on_trading_day(&self, day: u64) -> Vec<Fact> {
        if !self.follows_trading_days() || day <= self.trading_day {
            return Vec::new();
        }
        let mut facts = vec![Fact::TradingDayStarted { day }];
        facts.extend(self.due_on_open());
        facts
    }

    pub fn due_on_open(&self) -> Vec<Fact> {
        let next = self.cycle + 1;
        let mut facts = vec![Fact::CycleOpened { cycle: next }];
        if next > self.config.settlement_lag {
            let last_due = next - self.config.settlement_lag - 1;
            facts
                .extend((self.next_to_settle..=last_due).map(|cycle| Fact::CycleSettled { cycle }));
        }
        facts
    }

    pub fn decide_register(
        &self,
        name: &str,
        secret: SecretHash,
        exchange_members: &[MemberInfo],
    ) -> Result<Fact, String> {
        let member = exchange_members
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| format!("'{name}' is not an exchange member"))?;
        if self
            .accounts
            .get(&member.member_id)
            .is_some_and(|a| a.secret.is_some())
        {
            return Err(format!("'{name}' is already a clearing member"));
        }
        Ok(Fact::MemberRegistered {
            member: member.member_id,
            name: name.to_string(),
            secret,
        })
    }

    pub fn decide_deposit(&self, member: MemberId, amount: i64) -> Result<Fact, String> {
        let account = self
            .accounts
            .get(&member)
            .ok_or_else(|| format!("unknown clearing member {member}"))?;
        if amount < 0 && account.cash + amount < self.margin_required(member) as i64 {
            return Err("withdrawal would leave collateral below the margin requirement".into());
        }
        Ok(Fact::Deposited { member, amount })
    }

    pub fn resync(&self) -> Vec<MemberMessage> {
        let mut messages = vec![MemberMessage::OpenCycle { cycle: self.cycle }];
        if self.next_to_settle > 0 {
            messages.push(MemberMessage::Settled {
                cycle: self.next_to_settle - 1,
            });
        }
        messages.extend(self.credit_limits.iter().map(|(member, limit)| {
            MemberMessage::SetCreditLimit {
                member: *member,
                limit: *limit,
            }
        }));
        messages
    }

    fn account(&mut self, member: MemberId) -> &mut Account {
        self.accounts.entry(member).or_insert_with(|| Account {
            name: format!("member-{member}"),
            secret: None,
            cash: 0,
            securities: BTreeMap::new(),
            fails: 0,
        })
    }

    fn refresh_credit(&mut self, member: MemberId) -> Option<Effect> {
        let limit = self.credit_limit(member);
        if self.credit_limits.get(&member) == Some(&limit) {
            return None;
        }
        self.credit_limits.insert(member, limit);
        Some(Effect::ToExchange(MemberMessage::SetCreditLimit {
            member,
            limit,
        }))
    }

    pub fn apply(&mut self, fact: &Fact) -> Vec<Effect> {
        match fact {
            Fact::MemberRegistered {
                member,
                name,
                secret,
            } => {
                let account = self.account(*member);
                account.name = name.clone();
                account.secret = Some(*secret);
                self.refresh_credit(*member).into_iter().collect()
            }
            Fact::Deposited { member, amount } => {
                self.account(*member).cash += amount;
                self.refresh_credit(*member).into_iter().collect()
            }
            Fact::TradeCleared(trade) => {
                self.last_exchange_seq = self.last_exchange_seq.max(trade.seq);
                let notional =
                    (trade.price as u128 * trade.qty as u128).min(i64::MAX as u128) as i64;
                self.account(trade.buy_member);
                self.account(trade.sell_member);
                let by_member = self.obligations.entry(trade.cycle).or_default();
                let buyer = by_member.entry(trade.buy_member).or_default();
                buyer.cash -= notional + trade.buy_fee;
                buyer.fees += trade.buy_fee;
                *buyer.shares.entry(trade.symbol).or_default() += trade.qty as i64;
                buyer.gross += notional as u64;
                let seller = by_member.entry(trade.sell_member).or_default();
                seller.cash += notional - trade.sell_fee;
                seller.fees += trade.sell_fee;
                *seller.shares.entry(trade.symbol).or_default() -= trade.qty as i64;
                seller.gross += notional as u64;
                vec![]
            }
            Fact::AllocationCleared(allocation) => {
                self.last_exchange_seq = self.last_exchange_seq.max(allocation.seq);
                *self
                    .account(allocation.member)
                    .securities
                    .entry(allocation.symbol)
                    .or_default() += allocation.shares as i64;
                vec![]
            }
            Fact::CycleOpened { cycle } => {
                self.cycle = self.cycle.max(*cycle);
                vec![Effect::ToExchange(MemberMessage::OpenCycle {
                    cycle: *cycle,
                })]
            }
            Fact::CycleSettled { cycle } => self.settle(*cycle),
            Fact::TradingDayStarted { day } => {
                self.trading_day = self.trading_day.max(*day);
                vec![]
            }
        }
    }

    fn settle(&mut self, cycle: u64) -> Vec<Effect> {
        self.next_to_settle = self.next_to_settle.max(cycle + 1);
        let obligations = self.obligations.remove(&cycle).unwrap_or_default();
        let mut effects = Vec::new();
        for (member, obligation) in obligations {
            self.fee_pool += obligation.fees;
            let account = self.account(member);
            account.cash += obligation.cash;
            let mut positions = Vec::new();
            for (symbol, delivered) in obligation.shares {
                let holding = account.securities.entry(symbol).or_default();
                *holding += delivered;
                let failed = *holding < 0;
                positions.push(SettledPosition {
                    symbol,
                    delivered,
                    holding: *holding,
                    failed,
                });
            }
            let fails = positions.iter().filter(|p| p.failed).count() as u32;
            account.fails += fails;
            let collateral = account.cash;
            let report = SettlementReport {
                seq: 0,
                cycle,
                member,
                cash: obligation.cash,
                collateral,
                margin_required: self.margin_required(member),
                positions,
            };
            let message = self
                .feeds
                .publish(member, ServerMessage::SettlementReport(report));
            effects.push(Effect::ToMember { member, message });
            effects.extend(self.refresh_credit(member));
        }
        effects.push(Effect::ToExchange(MemberMessage::Settled { cycle }));
        effects
    }

    pub fn status(&self, only: Option<MemberId>) -> ClearingStatus {
        ClearingStatus {
            cycle: self.cycle,
            settled_cycle: self.next_to_settle.saturating_sub(1),
            fee_pool: self.fee_pool,
            members: self
                .accounts
                .iter()
                .filter(|(id, _)| only.is_none_or(|o| o == **id))
                .map(|(id, a)| ClearingMemberInfo {
                    member_id: *id,
                    collateral: a.cash,
                    margin_required: self.margin_required(*id),
                    unsettled_gross: self.unsettled_gross(*id),
                    credit_limit: self.credit_limit(*id),
                    fails: a.fails,
                    name: a.name.clone(),
                })
                .collect(),
        }
    }

    pub fn total_cash(&self) -> i64 {
        self.accounts.values().map(|a| a.cash).sum::<i64>() + self.fee_pool
    }

    pub fn holding(&self, member: MemberId, symbol: Symbol) -> i64 {
        self.accounts
            .get(&member)
            .and_then(|a| a.securities.get(&symbol))
            .copied()
            .unwrap_or(0)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use ome_protocol::MemberKind;

    pub fn sym() -> Symbol {
        Symbol::parse("ACME").unwrap()
    }

    pub fn trade(
        seq: u64,
        cycle: u64,
        buyer: MemberId,
        seller: MemberId,
        price: u64,
        qty: u64,
    ) -> ClearedTrade {
        ClearedTrade {
            seq,
            trade_id: seq,
            cycle,
            symbol: sym(),
            price,
            qty,
            buy_member: buyer,
            buy_account: 1,
            buy_fee: 3,
            sell_member: seller,
            sell_account: 2,
            sell_fee: -2,
            timestamp: 0,
        }
    }

    fn members() -> Vec<MemberInfo> {
        ["retail", "prime"]
            .iter()
            .enumerate()
            .map(|(i, name)| MemberInfo {
                member_id: i as u64 + 2,
                kind: MemberKind::Broker,
                connected: true,
                open_orders: 0,
                net_fees: 0,
                credit_limit: u64::MAX,
                exposure: 0,
                suspended: false,
                cancel_on_disconnect: false,
                name: name.to_string(),
            })
            .collect()
    }

    fn funded() -> Clearing {
        let mut c = Clearing::new(ClearingConfig::default());
        for name in ["retail", "prime"] {
            let fact = c.decide_register(name, [0; 32], &members()).unwrap();
            c.apply(&fact);
        }
        c.apply(&Fact::Deposited {
            member: 2,
            amount: 1_000_000_00,
        });
        c.apply(&Fact::Deposited {
            member: 3,
            amount: 1_000_000_00,
        });
        c.apply(&Fact::AllocationCleared(Allocation {
            seq: 1,
            allocation_id: 1,
            member: 3,
            symbol: sym(),
            account: 9,
            shares: 1_000,
        }));
        c
    }

    fn open_next(c: &mut Clearing) -> Vec<Effect> {
        c.due_on_open().iter().flat_map(|f| c.apply(f)).collect()
    }

    #[test]
    fn registration_requires_exchange_membership_and_pushes_credit() {
        let mut c = Clearing::new(ClearingConfig::default());
        assert!(c.decide_register("ghost", [0; 32], &members()).is_err());
        let fact = c.decide_register("retail", [0; 32], &members()).unwrap();
        c.apply(&fact);
        let effects = c.apply(&Fact::Deposited {
            member: 2,
            amount: 100_00,
        });
        assert_eq!(
            effects,
            vec![Effect::ToExchange(MemberMessage::SetCreditLimit {
                member: 2,
                limit: 1_000_00
            })]
        );
        assert!(c.decide_register("retail", [0; 32], &members()).is_err());
    }

    #[test]
    fn cycles_settle_t_plus_one_with_dvp_netting() {
        let mut c = funded();
        assert_eq!(c.due_on_open(), vec![Fact::CycleOpened { cycle: 1 }]);
        open_next(&mut c);
        c.apply(&Fact::TradeCleared(trade(2, 1, 2, 3, 25_00, 100)));
        c.apply(&Fact::TradeCleared(trade(3, 1, 3, 2, 26_00, 40)));
        assert_eq!(c.margin_required(2), (2_500_00 + 1_040_00) / 10);

        let effects = open_next(&mut c);
        assert_eq!(c.holding(2, sym()), 0, "nothing settles on T+0");
        assert!(
            effects
                .iter()
                .all(|e| !matches!(e, Effect::ToMember { .. }))
        );

        let effects = open_next(&mut c);
        assert_eq!(c.cycle(), 3);
        assert_eq!(c.holding(2, sym()), 60);
        assert_eq!(c.holding(3, sym()), 1_000 - 60);
        let report = effects
            .iter()
            .find_map(|e| match e {
                Effect::ToMember {
                    member: 2,
                    message: ServerMessage::SettlementReport(r),
                } => Some(r.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(report.cycle, 1);
        assert_eq!(report.cash, -2_500_00 + 1_040_00 - 3 + 2);
        assert_eq!(report.positions[0].delivered, 60);
        assert!(effects.contains(&Effect::ToExchange(MemberMessage::Settled { cycle: 1 })));
        assert_eq!(c.margin_required(2), 0);
        assert_eq!(c.total_cash(), 2_000_000_00);
    }

    #[test]
    fn delivering_shares_you_do_not_hold_is_a_reported_fail() {
        let mut c = funded();
        open_next(&mut c);
        c.apply(&Fact::TradeCleared(trade(2, 1, 3, 2, 25_00, 10)));
        open_next(&mut c);
        let effects = open_next(&mut c);
        let failed = effects.iter().any(|e| matches!(e, Effect::ToMember { member: 2, message: ServerMessage::SettlementReport(r) } if r.positions[0].failed));
        assert!(failed);
        assert_eq!(c.status(Some(2)).members[0].fails, 1);
    }

    #[test]
    fn withdrawals_cannot_breach_margin_and_credit_follows_collateral() {
        let mut c = funded();
        open_next(&mut c);
        c.apply(&Fact::TradeCleared(trade(2, 1, 2, 3, 25_00, 1_000)));
        assert!(c.decide_deposit(2, -999_000_00).is_err());
        assert!(c.decide_deposit(2, -100_000_00).is_ok());
        assert_eq!(c.credit_limit(2), 10_000_000_00);
        assert!(c.decide_deposit(99, 1).is_err());
    }

    #[test]
    fn resync_restates_cycle_settlement_and_limits() {
        let mut c = funded();
        open_next(&mut c);
        open_next(&mut c);
        open_next(&mut c);
        let messages = c.resync();
        assert!(messages.contains(&MemberMessage::OpenCycle { cycle: 3 }));
        assert!(messages.contains(&MemberMessage::Settled { cycle: 1 }));
        assert!(messages.contains(&MemberMessage::SetCreditLimit {
            member: 2,
            limit: 10_000_000_00
        }));
    }

    #[test]
    fn session_mode_opens_one_cycle_per_trading_day() {
        let mut c = Clearing::new(ClearingConfig {
            cycle_ms: 0,
            ..ClearingConfig::default()
        });
        assert!(c.follows_trading_days());
        let facts = c.due_on_trading_day(1);
        assert_eq!(
            facts[..2],
            [
                Fact::TradingDayStarted { day: 1 },
                Fact::CycleOpened { cycle: 1 }
            ]
        );
        for fact in &facts {
            c.apply(fact);
        }
        assert!(
            c.due_on_trading_day(1).is_empty(),
            "the same day never opens twice"
        );
        assert_eq!(
            c.due_on_trading_day(2)[..3],
            [
                Fact::TradingDayStarted { day: 2 },
                Fact::CycleOpened { cycle: 2 },
                Fact::CycleSettled { cycle: 0 }
            ]
        );
        let timed = Clearing::new(ClearingConfig::default());
        assert!(timed.due_on_trading_day(5).is_empty());
    }
}
