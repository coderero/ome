use crate::codec::{DecodeError, Decoder, Encoder};
use crate::template::*;
use crate::types::*;
use ome_core::candles::Candle;
use ome_core::order_book::Level;
use ome_core::types::{AccountId, Timestamp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMessage {
    LogonAck {
        account_id: AccountId,
        role: Role,
        server_time: Timestamp,
    },
    MemberLogonAck {
        member_id: MemberId,
        kind: MemberKind,
        server_time: Timestamp,
    },
    ReplayComplete {
        next_seq: u64,
    },
    Heartbeat {
        server_time: Timestamp,
    },
    Reject {
        ref_template: u16,
        reason: String,
    },
    Ack {
        ref_template: u16,
        text: String,
    },
    Execution(ExecutionReport),
    Account(AccountSnapshot),
    Allocation(Allocation),
    ContractNote(ContractNote),
    FeeSummary(FeeSummary),
    ClearedTrade(ClearedTrade),
    ConditionalUpdate(ConditionalReport),
    AlgoUpdate(AlgoReport),
    SettlementReport(SettlementReport),
    ClearingStatus(ClearingStatus),
    MarketStatus(MarketStatus),
    AuctionInfo(AuctionInfo),
    InstrumentList(Vec<InstrumentInfo>),
    Instrument(InstrumentInfo),
    Book(BookSnapshot),
    Trade(TradeEvent),
    Candles(CandleSnapshot),
    AccountList(Vec<AccountInfo>),
    MemberList(Vec<MemberInfo>),
}

impl ServerMessage {
    pub fn template_id(&self) -> u16 {
        match self {
            ServerMessage::LogonAck { .. } => LOGON_ACK,
            ServerMessage::MemberLogonAck { .. } => MEMBER_LOGON_ACK,
            ServerMessage::ReplayComplete { .. } => REPLAY_COMPLETE,
            ServerMessage::Heartbeat { .. } => SERVER_HEARTBEAT,
            ServerMessage::Reject { .. } => REJECT,
            ServerMessage::Ack { .. } => ACK,
            ServerMessage::Execution(_) => EXECUTION_REPORT,
            ServerMessage::Account(_) => ACCOUNT_SNAPSHOT,
            ServerMessage::Allocation(_) => ALLOCATION,
            ServerMessage::ContractNote(_) => CONTRACT_NOTE,
            ServerMessage::FeeSummary(_) => FEE_SUMMARY,
            ServerMessage::ClearedTrade(_) => CLEARED_TRADE,
            ServerMessage::ConditionalUpdate(_) => CONDITIONAL_UPDATE,
            ServerMessage::AlgoUpdate(_) => ALGO_UPDATE,
            ServerMessage::SettlementReport(_) => SETTLEMENT_REPORT,
            ServerMessage::ClearingStatus(_) => CLEARING_STATUS,
            ServerMessage::MarketStatus(_) => MARKET_STATUS,
            ServerMessage::AuctionInfo(_) => AUCTION_INFO,
            ServerMessage::InstrumentList(_) => INSTRUMENT_LIST,
            ServerMessage::Instrument(_) => INSTRUMENT,
            ServerMessage::Book(_) => BOOK_SNAPSHOT,
            ServerMessage::Trade(_) => TRADE,
            ServerMessage::Candles(_) => CANDLE_SNAPSHOT,
            ServerMessage::AccountList(_) => ACCOUNT_LIST,
            ServerMessage::MemberList(_) => MEMBER_LIST,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new(self.template_id());
        match self {
            ServerMessage::LogonAck {
                account_id,
                role,
                server_time,
            } => {
                e.u64(*account_id)
                    .u8(*role as u8)
                    .u64(*server_time)
                    .end_block();
            }
            ServerMessage::MemberLogonAck {
                member_id,
                kind,
                server_time,
            } => {
                e.u64(*member_id)
                    .u8(*kind as u8)
                    .u64(*server_time)
                    .end_block();
            }
            ServerMessage::ReplayComplete { next_seq } => {
                e.u64(*next_seq).end_block();
            }
            ServerMessage::Heartbeat { server_time } => {
                e.u64(*server_time).end_block();
            }
            ServerMessage::Reject {
                ref_template,
                reason,
            } => {
                e.u16(*ref_template).end_block().var(reason);
            }
            ServerMessage::Ack { ref_template, text } => {
                e.u16(*ref_template).end_block().var(text);
            }
            ServerMessage::Execution(r) => {
                e.u64(r.seq)
                    .u64(r.order_id)
                    .u64(r.client_order_id)
                    .u64(r.account)
                    .symbol(Some(r.symbol))
                    .u8(side_to_u8(r.side))
                    .u8(r.status as u8)
                    .u64(r.price)
                    .u64(r.qty)
                    .u64(r.filled)
                    .u64(r.leaves)
                    .u64(r.last_price)
                    .u64(r.last_qty)
                    .i64(r.fees)
                    .u64(r.cycle)
                    .u64(r.timestamp)
                    .end_block()
                    .var(&r.reason);
            }
            ServerMessage::Account(a) => {
                e.u64(a.account_id).u8(a.role as u8);
                put_balance(&mut e, &a.cash);
                e.i64(a.unsettled_cash).i64(a.day_pnl).end_block();
                let mut g = e.begin_group(a.positions.len());
                for p in &a.positions {
                    e.begin_entry(&mut g);
                    e.symbol(Some(p.symbol));
                    put_balance(&mut e, &p.balance);
                    e.i64(p.unsettled);
                    e.end_entry_block(&mut g);
                }
            }
            ServerMessage::Allocation(a) => {
                e.u64(a.seq)
                    .u64(a.allocation_id)
                    .u64(a.member)
                    .symbol(Some(a.symbol))
                    .u64(a.account)
                    .u64(a.shares)
                    .end_block();
            }
            ServerMessage::ContractNote(n) => {
                e.u64(n.account)
                    .u64(n.order_id)
                    .symbol(Some(n.symbol))
                    .u8(side_to_u8(n.side))
                    .u64(n.qty)
                    .u64(n.price);
                put_charges(&mut e, &n.charges);
                e.i64(n.net_amount).u64(n.timestamp).end_block();
            }
            ServerMessage::AlgoUpdate(a) => {
                e.u64(a.algo_id)
                    .u64(a.client_order_id)
                    .u64(a.account)
                    .symbol(Some(a.symbol))
                    .u8(side_to_u8(a.side))
                    .u64(a.qty)
                    .u64(a.filled)
                    .u64(a.avg_price)
                    .u64(a.children)
                    .u8(a.status as u8);
                put_algo_strategy(&mut e, &a.strategy);
                e.end_block().var(&a.reason);
            }
            ServerMessage::ConditionalUpdate(c) => {
                e.u64(c.cond_id)
                    .u64(c.client_order_id)
                    .u64(c.account)
                    .symbol(Some(c.symbol))
                    .u8(side_to_u8(c.side))
                    .u64(c.qty)
                    .u8(c.status as u8);
                put_trigger(&mut e, &c.trigger);
                e.u64(c.level).end_block().var(&c.reason);
            }
            ServerMessage::ClearedTrade(t) => {
                e.u64(t.seq)
                    .u64(t.trade_id)
                    .u64(t.cycle)
                    .symbol(Some(t.symbol))
                    .u64(t.price)
                    .u64(t.qty)
                    .u64(t.buy_member)
                    .u64(t.buy_account)
                    .i64(t.buy_fee)
                    .u64(t.sell_member)
                    .u64(t.sell_account)
                    .i64(t.sell_fee)
                    .u64(t.timestamp)
                    .end_block();
            }
            ServerMessage::SettlementReport(r) => {
                e.u64(r.seq)
                    .u64(r.cycle)
                    .u64(r.member)
                    .i64(r.cash)
                    .i64(r.collateral)
                    .u64(r.margin_required)
                    .end_block();
                let mut g = e.begin_group(r.positions.len());
                for p in &r.positions {
                    e.begin_entry(&mut g);
                    e.symbol(Some(p.symbol))
                        .i64(p.delivered)
                        .i64(p.holding)
                        .u8(p.failed as u8);
                    e.end_entry_block(&mut g);
                }
            }
            ServerMessage::ClearingStatus(c) => {
                e.u64(c.cycle)
                    .u64(c.settled_cycle)
                    .i64(c.fee_pool)
                    .end_block();
                let mut g = e.begin_group(c.members.len());
                for m in &c.members {
                    e.begin_entry(&mut g);
                    e.u64(m.member_id)
                        .i64(m.collateral)
                        .u64(m.margin_required)
                        .u64(m.unsettled_gross)
                        .u64(m.credit_limit)
                        .u32(m.fails);
                    e.end_entry_block(&mut g);
                    e.var(&m.name);
                }
            }
            ServerMessage::MarketStatus(m) => {
                e.u8(m.phase as u8)
                    .u64(m.day)
                    .u64(m.since)
                    .u64(m.next_change_at)
                    .end_block();
            }
            ServerMessage::AuctionInfo(a) => {
                e.symbol(Some(a.symbol))
                    .u64(a.price)
                    .u64(a.volume)
                    .i64(a.imbalance)
                    .end_block();
            }
            ServerMessage::FeeSummary(f) => {
                put_charges(&mut e, &f.charges);
                e.u64(f.fills).end_block();
            }
            ServerMessage::InstrumentList(list) => {
                e.end_block();
                let mut g = e.begin_group(list.len());
                for info in list {
                    e.begin_entry(&mut g);
                    put_instrument_block(&mut e, info);
                    e.end_entry_block(&mut g);
                    e.var(&info.name).var(&info.halt_reason);
                }
            }
            ServerMessage::Instrument(info) => {
                put_instrument_block(&mut e, info);
                e.end_block().var(&info.name).var(&info.halt_reason);
            }
            ServerMessage::Book(b) => {
                e.symbol(Some(b.symbol)).u64(b.timestamp).end_block();
                for levels in [&b.bids, &b.asks] {
                    let mut g = e.begin_group(levels.len());
                    for l in levels {
                        e.begin_entry(&mut g);
                        e.u64(l.price).u64(l.qty).u32(l.orders);
                        e.end_entry_block(&mut g);
                    }
                }
            }
            ServerMessage::Trade(t) => {
                e.symbol(Some(t.symbol))
                    .u64(t.trade_id)
                    .u64(t.price)
                    .u64(t.qty)
                    .u8(side_to_u8(t.aggressor))
                    .u64(t.timestamp)
                    .end_block();
            }
            ServerMessage::Candles(c) => {
                e.symbol(Some(c.symbol)).u64(c.interval).end_block();
                let mut g = e.begin_group(c.candles.len());
                for k in &c.candles {
                    e.begin_entry(&mut g);
                    e.u64(k.start)
                        .u64(k.open)
                        .u64(k.high)
                        .u64(k.low)
                        .u64(k.close)
                        .u64(k.volume)
                        .u64(k.trades);
                    e.end_entry_block(&mut g);
                }
            }
            ServerMessage::AccountList(list) => {
                e.end_block();
                let mut g = e.begin_group(list.len());
                for a in list {
                    e.begin_entry(&mut g);
                    e.u64(a.account_id).u8(a.role as u8);
                    put_balance(&mut e, &a.cash);
                    e.u32(a.open_orders)
                        .u8(a.suspended as u8)
                        .u64(a.loss_limit)
                        .i64(a.day_pnl);
                    e.end_entry_block(&mut g);
                    e.var(&a.username);
                }
            }
            ServerMessage::MemberList(list) => {
                e.end_block();
                let mut g = e.begin_group(list.len());
                for m in list {
                    e.begin_entry(&mut g);
                    e.u64(m.member_id)
                        .u8(m.kind as u8)
                        .u8(m.connected as u8)
                        .u32(m.open_orders)
                        .i64(m.net_fees)
                        .u64(m.credit_limit)
                        .u64(m.exposure)
                        .u8(m.suspended as u8)
                        .u8(m.cancel_on_disconnect as u8);
                    e.end_entry_block(&mut g);
                    e.var(&m.name);
                }
            }
        }
        e.finish()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(bytes)?;
        let message = match d.template_id {
            LOGON_ACK => {
                let account_id = d.u64()?;
                let role = role_from(d.u8()?)?;
                let server_time = d.u64()?;
                d.end_block()?;
                ServerMessage::LogonAck {
                    account_id,
                    role,
                    server_time,
                }
            }
            MEMBER_LOGON_ACK => {
                let member_id = d.u64()?;
                let kind = member_kind_from(d.u8()?)?;
                let server_time = d.u64()?;
                d.end_block()?;
                ServerMessage::MemberLogonAck {
                    member_id,
                    kind,
                    server_time,
                }
            }
            REPLAY_COMPLETE => {
                let next_seq = d.u64()?;
                d.end_block()?;
                ServerMessage::ReplayComplete { next_seq }
            }
            SERVER_HEARTBEAT => {
                let server_time = d.u64()?;
                d.end_block()?;
                ServerMessage::Heartbeat { server_time }
            }
            REJECT => {
                let ref_template = d.u16()?;
                d.end_block()?;
                ServerMessage::Reject {
                    ref_template,
                    reason: d.var()?,
                }
            }
            ACK => {
                let ref_template = d.u16()?;
                d.end_block()?;
                ServerMessage::Ack {
                    ref_template,
                    text: d.var()?,
                }
            }
            EXECUTION_REPORT => {
                let seq = d.u64()?;
                let order_id = d.u64()?;
                let client_order_id = d.u64()?;
                let account = d.u64()?;
                let symbol = d.symbol()?;
                let side = side_from_u8(d.u8()?)?;
                let status = OrderStatus::from_u8(d.u8()?).ok_or(DecodeError::Invalid("status"))?;
                let price = d.u64()?;
                let qty = d.u64()?;
                let filled = d.u64()?;
                let leaves = d.u64()?;
                let last_price = d.u64()?;
                let last_qty = d.u64()?;
                let fees = d.i64()?;
                let cycle = d.u64()?;
                let timestamp = d.u64()?;
                d.end_block()?;
                ServerMessage::Execution(ExecutionReport {
                    seq,
                    order_id,
                    client_order_id,
                    account,
                    symbol,
                    side,
                    status,
                    price,
                    qty,
                    filled,
                    leaves,
                    last_price,
                    last_qty,
                    fees,
                    cycle,
                    timestamp,
                    reason: d.var()?,
                })
            }
            ACCOUNT_SNAPSHOT => {
                let account_id = d.u64()?;
                let role = role_from(d.u8()?)?;
                let cash = get_balance(&mut d)?;
                let unsettled_cash = d.i64()?;
                let day_pnl = d.i64()?;
                d.end_block()?;
                let g = d.group()?;
                let mut positions = Vec::with_capacity(g.count);
                for _ in 0..g.count {
                    let start = d.entry_start();
                    let symbol = d.symbol()?;
                    let balance = get_balance(&mut d)?;
                    let unsettled = d.i64()?;
                    d.end_entry_block(start, &g)?;
                    positions.push(Position {
                        symbol,
                        balance,
                        unsettled,
                    });
                }
                ServerMessage::Account(AccountSnapshot {
                    account_id,
                    role,
                    cash,
                    unsettled_cash,
                    day_pnl,
                    positions,
                })
            }
            ALLOCATION => {
                let seq = d.u64()?;
                let allocation_id = d.u64()?;
                let member = d.u64()?;
                let symbol = d.symbol()?;
                let account = d.u64()?;
                let shares = d.u64()?;
                d.end_block()?;
                ServerMessage::Allocation(Allocation {
                    seq,
                    allocation_id,
                    member,
                    symbol,
                    account,
                    shares,
                })
            }
            ALGO_UPDATE => {
                let algo_id = d.u64()?;
                let client_order_id = d.u64()?;
                let account = d.u64()?;
                let symbol = d.symbol()?;
                let side = side_from_u8(d.u8()?)?;
                let qty = d.u64()?;
                let filled = d.u64()?;
                let avg_price = d.u64()?;
                let children = d.u64()?;
                let status =
                    AlgoStatus::from_u8(d.u8()?).ok_or(DecodeError::Invalid("algo status"))?;
                let strategy = get_algo_strategy(&mut d)?;
                d.end_block()?;
                ServerMessage::AlgoUpdate(AlgoReport {
                    algo_id,
                    client_order_id,
                    account,
                    symbol,
                    side,
                    qty,
                    filled,
                    avg_price,
                    children,
                    status,
                    strategy,
                    reason: d.var()?,
                })
            }
            CONDITIONAL_UPDATE => {
                let cond_id = d.u64()?;
                let client_order_id = d.u64()?;
                let account = d.u64()?;
                let symbol = d.symbol()?;
                let side = side_from_u8(d.u8()?)?;
                let qty = d.u64()?;
                let status = ConditionalStatus::from_u8(d.u8()?)
                    .ok_or(DecodeError::Invalid("conditional status"))?;
                let trigger = get_trigger(&mut d)?;
                let level = d.u64()?;
                d.end_block()?;
                ServerMessage::ConditionalUpdate(ConditionalReport {
                    cond_id,
                    client_order_id,
                    account,
                    symbol,
                    side,
                    qty,
                    status,
                    trigger,
                    level,
                    reason: d.var()?,
                })
            }
            CLEARED_TRADE => {
                let trade = ClearedTrade {
                    seq: d.u64()?,
                    trade_id: d.u64()?,
                    cycle: d.u64()?,
                    symbol: d.symbol()?,
                    price: d.u64()?,
                    qty: d.u64()?,
                    buy_member: d.u64()?,
                    buy_account: d.u64()?,
                    buy_fee: d.i64()?,
                    sell_member: d.u64()?,
                    sell_account: d.u64()?,
                    sell_fee: d.i64()?,
                    timestamp: d.u64()?,
                };
                d.end_block()?;
                ServerMessage::ClearedTrade(trade)
            }
            SETTLEMENT_REPORT => {
                let seq = d.u64()?;
                let cycle = d.u64()?;
                let member = d.u64()?;
                let cash = d.i64()?;
                let collateral = d.i64()?;
                let margin_required = d.u64()?;
                d.end_block()?;
                let g = d.group()?;
                let mut positions = Vec::with_capacity(g.count);
                for _ in 0..g.count {
                    let start = d.entry_start();
                    let position = SettledPosition {
                        symbol: d.symbol()?,
                        delivered: d.i64()?,
                        holding: d.i64()?,
                        failed: d.u8()? != 0,
                    };
                    d.end_entry_block(start, &g)?;
                    positions.push(position);
                }
                ServerMessage::SettlementReport(SettlementReport {
                    seq,
                    cycle,
                    member,
                    cash,
                    collateral,
                    margin_required,
                    positions,
                })
            }
            CLEARING_STATUS => {
                let cycle = d.u64()?;
                let settled_cycle = d.u64()?;
                let fee_pool = d.i64()?;
                d.end_block()?;
                let g = d.group()?;
                let mut members = Vec::with_capacity(g.count);
                for _ in 0..g.count {
                    let start = d.entry_start();
                    let member_id = d.u64()?;
                    let collateral = d.i64()?;
                    let margin_required = d.u64()?;
                    let unsettled_gross = d.u64()?;
                    let credit_limit = d.u64()?;
                    let fails = d.u32()?;
                    d.end_entry_block(start, &g)?;
                    members.push(ClearingMemberInfo {
                        member_id,
                        collateral,
                        margin_required,
                        unsettled_gross,
                        credit_limit,
                        fails,
                        name: d.var()?,
                    });
                }
                ServerMessage::ClearingStatus(ClearingStatus {
                    cycle,
                    settled_cycle,
                    fee_pool,
                    members,
                })
            }
            CONTRACT_NOTE => {
                let account = d.u64()?;
                let order_id = d.u64()?;
                let symbol = d.symbol()?;
                let side = side_from_u8(d.u8()?)?;
                let qty = d.u64()?;
                let price = d.u64()?;
                let charges = get_charges(&mut d)?;
                let net_amount = d.i64()?;
                let timestamp = d.u64()?;
                d.end_block()?;
                ServerMessage::ContractNote(ContractNote {
                    account,
                    order_id,
                    symbol,
                    side,
                    qty,
                    price,
                    charges,
                    net_amount,
                    timestamp,
                })
            }
            MARKET_STATUS => {
                let status = MarketStatus {
                    phase: phase_from(d.u8()?)?,
                    day: d.u64()?,
                    since: d.u64()?,
                    next_change_at: d.u64()?,
                };
                d.end_block()?;
                ServerMessage::MarketStatus(status)
            }
            AUCTION_INFO => {
                let info = AuctionInfo {
                    symbol: d.symbol()?,
                    price: d.u64()?,
                    volume: d.u64()?,
                    imbalance: d.i64()?,
                };
                d.end_block()?;
                ServerMessage::AuctionInfo(info)
            }
            FEE_SUMMARY => {
                let charges = get_charges(&mut d)?;
                let fills = d.u64()?;
                d.end_block()?;
                ServerMessage::FeeSummary(FeeSummary { charges, fills })
            }
            INSTRUMENT_LIST => {
                d.end_block()?;
                let g = d.group()?;
                let mut list = Vec::with_capacity(g.count);
                for _ in 0..g.count {
                    let start = d.entry_start();
                    let mut info = get_instrument_block(&mut d)?;
                    d.end_entry_block(start, &g)?;
                    info.name = d.var()?;
                    info.halt_reason = d.var()?;
                    list.push(info);
                }
                ServerMessage::InstrumentList(list)
            }
            INSTRUMENT => {
                let mut info = get_instrument_block(&mut d)?;
                d.end_block()?;
                info.name = d.var()?;
                info.halt_reason = d.var()?;
                ServerMessage::Instrument(info)
            }
            BOOK_SNAPSHOT => {
                let symbol = d.symbol()?;
                let timestamp = d.u64()?;
                d.end_block()?;
                let mut sides = [Vec::new(), Vec::new()];
                for levels in &mut sides {
                    let g = d.group()?;
                    for _ in 0..g.count {
                        let start = d.entry_start();
                        let level = Level {
                            price: d.u64()?,
                            qty: d.u64()?,
                            orders: d.u32()?,
                        };
                        d.end_entry_block(start, &g)?;
                        levels.push(level);
                    }
                }
                let [bids, asks] = sides;
                ServerMessage::Book(BookSnapshot {
                    symbol,
                    timestamp,
                    bids,
                    asks,
                })
            }
            TRADE => {
                let symbol = d.symbol()?;
                let trade_id = d.u64()?;
                let price = d.u64()?;
                let qty = d.u64()?;
                let aggressor = side_from_u8(d.u8()?)?;
                let timestamp = d.u64()?;
                d.end_block()?;
                ServerMessage::Trade(TradeEvent {
                    symbol,
                    trade_id,
                    price,
                    qty,
                    aggressor,
                    timestamp,
                })
            }
            CANDLE_SNAPSHOT => {
                let symbol = d.symbol()?;
                let interval = d.u64()?;
                d.end_block()?;
                let g = d.group()?;
                let mut candles = Vec::with_capacity(g.count);
                for _ in 0..g.count {
                    let start = d.entry_start();
                    let candle = Candle {
                        start: d.u64()?,
                        open: d.u64()?,
                        high: d.u64()?,
                        low: d.u64()?,
                        close: d.u64()?,
                        volume: d.u64()?,
                        trades: d.u64()?,
                    };
                    d.end_entry_block(start, &g)?;
                    candles.push(candle);
                }
                ServerMessage::Candles(CandleSnapshot {
                    symbol,
                    interval,
                    candles,
                })
            }
            ACCOUNT_LIST => {
                d.end_block()?;
                let g = d.group()?;
                let mut list = Vec::with_capacity(g.count);
                for _ in 0..g.count {
                    let start = d.entry_start();
                    let account_id = d.u64()?;
                    let role = role_from(d.u8()?)?;
                    let cash = get_balance(&mut d)?;
                    let open_orders = d.u32()?;
                    let suspended = d.u8()? != 0;
                    let loss_limit = d.u64()?;
                    let day_pnl = d.i64()?;
                    d.end_entry_block(start, &g)?;
                    list.push(AccountInfo {
                        account_id,
                        role,
                        cash,
                        open_orders,
                        suspended,
                        loss_limit,
                        day_pnl,
                        username: d.var()?,
                    });
                }
                ServerMessage::AccountList(list)
            }
            MEMBER_LIST => {
                d.end_block()?;
                let g = d.group()?;
                let mut list = Vec::with_capacity(g.count);
                for _ in 0..g.count {
                    let start = d.entry_start();
                    let member_id = d.u64()?;
                    let kind = member_kind_from(d.u8()?)?;
                    let connected = d.u8()? != 0;
                    let open_orders = d.u32()?;
                    let net_fees = d.i64()?;
                    let credit_limit = d.u64()?;
                    let exposure = d.u64()?;
                    let suspended = d.u8()? != 0;
                    let cancel_on_disconnect = d.u8()? != 0;
                    d.end_entry_block(start, &g)?;
                    list.push(MemberInfo {
                        member_id,
                        kind,
                        connected,
                        open_orders,
                        net_fees,
                        credit_limit,
                        exposure,
                        suspended,
                        cancel_on_disconnect,
                        name: d.var()?,
                    });
                }
                ServerMessage::MemberList(list)
            }
            other => return Err(DecodeError::UnknownTemplate(other)),
        };
        Ok(message)
    }
}

fn put_instrument_block(e: &mut Encoder, info: &InstrumentInfo) {
    e.symbol(Some(info.symbol))
        .u8(info.status as u8)
        .u64(info.reference_price)
        .u64(info.last_price.unwrap_or(0))
        .u64(info.low_limit)
        .u64(info.high_limit)
        .u64(info.band_bps)
        .u64(info.volume);
}

fn get_instrument_block(d: &mut Decoder) -> Result<InstrumentInfo, DecodeError> {
    let symbol = d.symbol()?;
    let status =
        InstrumentStatus::from_u8(d.u8()?).ok_or(DecodeError::Invalid("instrument status"))?;
    let reference_price = d.u64()?;
    let last_price = Some(d.u64()?).filter(|&p| p > 0);
    Ok(InstrumentInfo {
        symbol,
        status,
        reference_price,
        last_price,
        low_limit: d.u64()?,
        high_limit: d.u64()?,
        band_bps: d.u64()?,
        volume: d.u64()?,
        name: String::new(),
        halt_reason: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::tests::sym;
    use ome_core::types::{Balance, Side};

    fn samples() -> Vec<ServerMessage> {
        let info = InstrumentInfo {
            symbol: sym("ACME"),
            status: InstrumentStatus::Halted,
            reference_price: 2_500,
            last_price: Some(3_300),
            low_limit: 1_750,
            high_limit: 3_250,
            band_bps: 1_000,
            volume: 1_234,
            name: "Acme Corp".into(),
            halt_reason: "circuit breaker".into(),
        };
        vec![
            ServerMessage::LogonAck {
                account_id: 1,
                role: Role::Hft,
                server_time: 99,
            },
            ServerMessage::MemberLogonAck {
                member_id: 2,
                kind: MemberKind::Broker,
                server_time: 100,
            },
            ServerMessage::ReplayComplete { next_seq: 51 },
            ServerMessage::Heartbeat { server_time: 5 },
            ServerMessage::AlgoUpdate(AlgoReport {
                algo_id: 4,
                client_order_id: 30,
                account: 2,
                symbol: sym("ACME"),
                side: Side::Buy,
                qty: 10_000,
                filled: 2_500,
                avg_price: 2_512,
                children: 9,
                status: AlgoStatus::Working,
                strategy: AlgoStrategy::Vwap {
                    duration_ms: 120_000,
                },
                reason: String::new(),
            }),
            ServerMessage::ConditionalUpdate(ConditionalReport {
                cond_id: 3,
                client_order_id: 9,
                account: 4,
                symbol: sym("ACME"),
                side: Side::Sell,
                qty: 10,
                status: ConditionalStatus::Triggered,
                trigger: Trigger::Below(2_400),
                level: 2_400,
                reason: "stop hit".into(),
            }),
            ServerMessage::MarketStatus(MarketStatus {
                phase: MarketPhase::PreOpen,
                day: 3,
                since: 100,
                next_change_at: 5_100,
            }),
            ServerMessage::AuctionInfo(AuctionInfo {
                symbol: sym("ACME"),
                price: 2_510,
                volume: 400,
                imbalance: -120,
            }),
            ServerMessage::Reject {
                ref_template: HALT,
                reason: "no".into(),
            },
            ServerMessage::Ack {
                ref_template: CREATE_ACCOUNT,
                text: "ok".into(),
            },
            ServerMessage::Execution(ExecutionReport {
                seq: 50,
                order_id: 1,
                client_order_id: 2,
                account: 12,
                symbol: sym("ACME"),
                side: Side::Sell,
                status: OrderStatus::PartiallyFilled,
                price: 2_500,
                qty: 10,
                filled: 4,
                leaves: 6,
                last_price: 2_510,
                last_qty: 4,
                fees: 33,
                cycle: 7,
                timestamp: 1_700_000_000_000,
                reason: String::new(),
            }),
            ServerMessage::Account(AccountSnapshot {
                account_id: 3,
                role: Role::Retail,
                cash: Balance {
                    available: 10,
                    reserved: 5,
                },
                unsettled_cash: -300,
                day_pnl: -42,
                positions: vec![Position {
                    symbol: sym("ACME"),
                    balance: Balance {
                        available: 1,
                        reserved: 2,
                    },
                    unsettled: 5,
                }],
            }),
            ServerMessage::ClearedTrade(ClearedTrade {
                seq: 9,
                trade_id: 44,
                cycle: 3,
                symbol: sym("ACME"),
                price: 2_500,
                qty: 10,
                buy_member: 2,
                buy_account: 7,
                buy_fee: 3,
                sell_member: 3,
                sell_account: 1,
                sell_fee: -2,
                timestamp: 11,
            }),
            ServerMessage::SettlementReport(SettlementReport {
                seq: 4,
                cycle: 3,
                member: 2,
                cash: -25_003,
                collateral: 1_000_000,
                margin_required: 2_500,
                positions: vec![SettledPosition {
                    symbol: sym("ACME"),
                    delivered: 10,
                    holding: 10,
                    failed: false,
                }],
            }),
            ServerMessage::ClearingStatus(ClearingStatus {
                cycle: 4,
                settled_cycle: 3,
                fee_pool: 12,
                members: vec![ClearingMemberInfo {
                    member_id: 2,
                    collateral: 1_000_000,
                    margin_required: 2_500,
                    unsettled_gross: 25_000,
                    credit_limit: 10_000_000,
                    fails: 0,
                    name: "retail-co".into(),
                }],
            }),
            ServerMessage::ContractNote(ContractNote {
                account: 4,
                order_id: 9,
                symbol: sym("ACME"),
                side: Side::Buy,
                qty: 10,
                price: 2_500,
                charges: Charges {
                    commission: 8,
                    exchange_fee: -2,
                    transaction_tax: 0,
                    stamp_duty: 4,
                    regulatory_fee: 0,
                    gst: 1,
                },
                net_amount: -25_011,
                timestamp: 5,
            }),
            ServerMessage::FeeSummary(FeeSummary {
                charges: Charges {
                    commission: 100,
                    ..Charges::default()
                },
                fills: 3,
            }),
            ServerMessage::Allocation(Allocation {
                seq: 3,
                allocation_id: 1,
                member: 2,
                symbol: sym("ZEN"),
                account: 9,
                shares: 50_000,
            }),
            ServerMessage::InstrumentList(vec![
                info.clone(),
                InstrumentInfo {
                    last_price: None,
                    halt_reason: String::new(),
                    ..info.clone()
                },
            ]),
            ServerMessage::Instrument(info),
            ServerMessage::Book(BookSnapshot {
                symbol: sym("ACME"),
                timestamp: 7,
                bids: vec![Level {
                    price: 2_490,
                    qty: 10,
                    orders: 2,
                }],
                asks: vec![],
            }),
            ServerMessage::Trade(TradeEvent {
                symbol: sym("ACME"),
                trade_id: 11,
                price: 2_505,
                qty: 2,
                aggressor: Side::Buy,
                timestamp: 8,
            }),
            ServerMessage::Candles(CandleSnapshot {
                symbol: sym("ACME"),
                interval: 1_000,
                candles: vec![Candle {
                    start: 0,
                    open: 1,
                    high: 3,
                    low: 1,
                    close: 2,
                    volume: 9,
                    trades: 4,
                }],
            }),
            ServerMessage::AccountList(vec![AccountInfo {
                account_id: 2,
                role: Role::Quant,
                cash: Balance::default(),
                open_orders: 3,
                suspended: true,
                loss_limit: 1_000_00,
                day_pnl: 77,
                username: "two-sigma".into(),
            }]),
            ServerMessage::MemberList(vec![MemberInfo {
                member_id: 2,
                kind: MemberKind::LiquidityProvider,
                connected: true,
                open_orders: 40,
                net_fees: -1_250,
                credit_limit: u64::MAX,
                exposure: 5_000,
                suspended: true,
                cancel_on_disconnect: true,
                name: "lp-desk".into(),
            }]),
        ]
    }

    #[test]
    fn every_server_message_round_trips() {
        for message in samples() {
            assert_eq!(
                ServerMessage::decode(&message.encode()).as_ref(),
                Ok(&message)
            );
        }
    }

    #[test]
    fn corrupted_messages_never_panic() {
        let all: Vec<Vec<u8>> = samples()
            .iter()
            .map(ServerMessage::encode)
            .chain(crate::client::tests::samples().iter().map(|m| m.encode()))
            .chain(crate::member::tests::samples().iter().map(|m| m.encode()))
            .collect();
        for bytes in all {
            for cut in 0..bytes.len() {
                let _ = ServerMessage::decode(&bytes[..cut]);
                let _ = crate::ClientMessage::decode(&bytes[..cut]);
                let _ = crate::MemberMessage::decode(&bytes[..cut]);
            }
            for i in 0..bytes.len() {
                let mut flipped = bytes.clone();
                flipped[i] ^= 0xFF;
                let _ = ServerMessage::decode(&flipped);
                let _ = crate::ClientMessage::decode(&flipped);
                let _ = crate::MemberMessage::decode(&flipped);
            }
        }
    }
}
