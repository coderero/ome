use crate::action::{Action, execute};
use crate::control::{AgentStats, GroupControl, Shared};
use crate::roster::AgentSpec;
use crate::state::AgentState;
use crate::strategies::{self, Strategy, affordable, cancel_stale, scale};
use ome_client::{Client, ClientError};
use ome_core::price::format_price;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Side, Timestamp};
use ome_protocol::{ClientMessage, ServerMessage};
use rand::SeedableRng;
use rand::rngs::StdRng;
use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const STATUS_EVERY: Duration = Duration::from_secs(120);
const BOOTSTRAP_MS: Timestamp = 90_000;

pub struct Bootstrap {
    seed: u64,
    deadline: Option<Timestamp>,
}

impl Bootstrap {
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            deadline: None,
        }
    }

    pub fn on_tick(&mut self, state: &AgentState, symbols: &[Symbol]) -> Option<Vec<Action>> {
        if self.seed == 0 {
            return None;
        }
        let deadline = *self.deadline.get_or_insert(state.now + BOOTSTRAP_MS);
        if state.now > deadline {
            return None;
        }
        let mut actions = cancel_stale(state, None, 3_000);
        let mut remaining = false;
        for symbol in symbols {
            let Some(market) = state.market(*symbol) else {
                continue;
            };
            let Some(price) = market.fair() else {
                continue;
            };
            let have = state.holding(*symbol).total() * price;
            if have * 10 >= self.seed * 8 {
                continue;
            }
            remaining = true;
            if state.busy() || !state.trading(*symbol) || state.open_in(*symbol) > 0 {
                continue;
            }
            let limit = market
                .ask
                .unwrap_or(price)
                .min(scale(market.reference.max(price), 200.0));
            let qty = affordable(state.cash, limit, (self.seed - have) / limit.max(1));
            if qty > 0 {
                actions.push(Action::Place {
                    symbol: *symbol,
                    side: Side::Buy,
                    kind: OrderKind::Limit(limit),
                    qty,
                });
                break;
            }
        }
        remaining.then_some(actions)
    }
}

fn seed_for(name: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    std::time::SystemTime::now().hash(&mut hasher);
    hasher.finish()
}

#[derive(Clone)]
pub struct AgentContext {
    pub group: Arc<GroupControl>,
    pub shared: Arc<Shared>,
    pub stats: Arc<AgentStats>,
    pub stop: Arc<AtomicBool>,
}

impl AgentContext {
    pub fn standalone(spec: &AgentSpec, stop: Arc<AtomicBool>) -> Self {
        let group = Arc::new(GroupControl::new(
            &spec.group,
            &spec.strategy,
            &spec.broker,
            1,
            spec.params.clone(),
        ));
        Self {
            shared: Arc::new(Shared::new(BTreeMap::from([(
                spec.group.clone(),
                group.clone(),
            )]))),
            group,
            stats: Arc::new(AgentStats::default()),
            stop,
        }
    }
}

pub fn supervise(spec: AgentSpec, addr: String, secret: String, ctx: AgentContext) {
    let mut rng = StdRng::seed_from_u64(seed_for(&spec.name));
    while !ctx.stop.load(Ordering::Relaxed) {
        match run(&spec, &addr, &secret, &mut rng, &ctx) {
            Err(ClientError::Rejected(reason)) => {
                eprintln!("[{}] logon rejected: {reason}; giving up", spec.name);
                return;
            }
            Err(e) => eprintln!("[{}] {e}; reconnecting in 2s", spec.name),
            Ok(()) => return,
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn listed(client: &mut Client, state: &mut AgentState) -> Result<Vec<Symbol>, ClientError> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Some(message) = client.recv_timeout(Duration::from_millis(100))? {
            state.apply(&message);
            if let ServerMessage::InstrumentList(list) = &message {
                return Ok(list.iter().map(|i| i.symbol).collect());
            }
        }
    }
    Ok(state.markets.keys().copied().collect())
}

fn run(
    spec: &AgentSpec,
    addr: &str,
    secret: &str,
    rng: &mut StdRng,
    ctx: &AgentContext,
) -> Result<(), ClientError> {
    let (mut client, session) = Client::connect_and_logon(addr, &spec.name, secret)?;
    let mut state = AgentState::default();
    let symbols = spec.resolve(&listed(&mut client, &mut state)?);
    for symbol in &symbols {
        client.send(&ClientMessage::Subscribe { symbol: *symbol })?;
    }
    let mut generation = ctx.group.generation();
    let Some(mut strategy) = strategies::create(&spec.strategy, &symbols, &ctx.group.params())
    else {
        eprintln!(
            "[{}] {} needs other symbols than {:?}; idle",
            spec.name, spec.strategy, symbols
        );
        return Ok(());
    };
    eprintln!(
        "[{}] {} on {} symbol(s) at account {}",
        spec.name,
        strategy.name(),
        symbols.len(),
        session.account_id
    );
    let mut bootstrap = Bootstrap::new(spec.seed);
    let mut bootstrapping = spec.seed > 0;
    let mut next_tick = Instant::now() + strategy.interval();
    let mut next_status = Instant::now() + STATUS_EVERY;
    let mut was_paused = false;
    while !ctx.stop.load(Ordering::Relaxed) {
        let wait = next_tick.saturating_duration_since(Instant::now());
        if let Some(message) = client.recv_timeout(wait)? {
            state.apply(&message);
            while let Some(message) = client.try_recv()? {
                state.apply(&message);
            }
        }
        if Instant::now() < next_tick {
            continue;
        }
        next_tick = Instant::now() + strategy.interval();
        state.world = ctx
            .shared
            .world
            .lock()
            .expect("world lock")
            .view(Instant::now());
        ctx.stats.fills.store(state.fills, Ordering::Relaxed);
        ctx.stats.equity.store(state.equity(), Ordering::Relaxed);
        if ctx.group.is_paused() {
            if !was_paused {
                was_paused = true;
                client.send(&ClientMessage::CancelAll { symbol: None })?;
            }
            continue;
        }
        was_paused = false;
        if ctx.group.generation() != generation {
            generation = ctx.group.generation();
            let params = ctx.group.params();
            if strategy.retune(&params) {
                eprintln!("[{}] retuned: {:?}", spec.name, params.0);
            } else if let Some(rebuilt) = strategies::create(&spec.strategy, &symbols, &params) {
                strategy = rebuilt;
                eprintln!(
                    "[{}] parameters changed: {:?}",
                    spec.name,
                    ctx.group.params().0
                );
            }
        }
        let actions = if bootstrapping {
            match bootstrap.on_tick(&state, &symbols) {
                Some(actions) => actions,
                None => {
                    bootstrapping = false;
                    eprintln!(
                        "[{}] inventory ready, equity {}",
                        spec.name,
                        format_price(state.equity())
                    );
                    continue;
                }
            }
        } else {
            strategy.on_tick(&state, rng)
        };
        for action in &actions {
            execute(&mut client, &mut state, action)?;
        }
        if Instant::now() >= next_status {
            next_status += STATUS_EVERY;
            report(spec, strategy.as_ref(), &state);
        }
    }
    client.send(&ClientMessage::CancelAll { symbol: None })?;
    Ok(())
}

fn report(spec: &AgentSpec, strategy: &dyn Strategy, state: &AgentState) {
    eprintln!(
        "[{}] {} equity {} cash {} day P&L {}{} fills {} open {} armed {} algos {} {}",
        spec.name,
        strategy.name(),
        format_price(state.equity()),
        format_price(state.cash),
        if state.day_pnl < 0 { "-" } else { "" },
        format_price(state.day_pnl.unsigned_abs()),
        state.fills,
        state.open_orders.len(),
        state.armed.len(),
        state.algos.len(),
        strategy.detail()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{acme, hold, state_with};

    #[test]
    fn bootstrap_buys_starting_inventory_with_real_orders_then_steps_aside() {
        let mut state = state_with(&[(acme(), 10_000)], 10_000_000);
        let mut boot = Bootstrap::new(2_000_000);
        let first = boot.on_tick(&state, &[acme()]).unwrap();
        assert_eq!(
            first,
            vec![Action::Place {
                symbol: acme(),
                side: Side::Buy,
                kind: OrderKind::Limit(10_005),
                qty: 199
            }]
        );
        hold(&mut state, acme(), 190);
        assert_eq!(boot.on_tick(&state, &[acme()]), None);
        assert_eq!(Bootstrap::new(0).on_tick(&state, &[acme()]), None);
        let mut late = Bootstrap::new(5_000_000);
        let empty = state_with(&[(acme(), 10_000)], 10_000_000);
        late.on_tick(&empty, &[acme()]);
        let mut later = empty.clone();
        later.now += BOOTSTRAP_MS + 1;
        assert_eq!(
            late.on_tick(&later, &[acme()]),
            None,
            "gives up after the deadline"
        );
    }
}
