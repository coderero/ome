use crate::app::{Core, SQUARE_WAIT, Scope};
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::Side;
use ome_ui::toast::Level;
use ome_ui_data::{AccountView, OrderRequest};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leg {
    pub symbol: Symbol,
    pub qty: u64,
    pub bid: Option<Price>,
    pub orders: usize,
    pub conditionals: usize,
}

impl Leg {
    pub fn value(&self) -> Option<u64> {
        self.bid.map(|b| b * self.qty)
    }
}

impl Core {
    pub fn held(&self, symbol: Symbol) -> u64 {
        self.store
            .account()
            .and_then(|a| a.positions.iter().find(|p| p.symbol == symbol))
            .map(|p| p.balance.available + p.balance.reserved)
            .unwrap_or(0)
    }

    pub fn square_legs(&self, scope: Scope) -> Vec<Leg> {
        let Some(account) = self.store.account() else { return Vec::new() };
        account
            .positions
            .iter()
            .filter(|p| p.balance.available + p.balance.reserved > 0)
            .filter(|p| scope == Scope::All || scope == Scope::One(p.symbol))
            .filter(|p| !self.squaring.iter().chain(&self.squared).any(|(s, _)| *s == p.symbol))
            .map(|p| Leg {
                symbol: p.symbol,
                qty: p.balance.available + p.balance.reserved,
                bid: self.quote(p.symbol).bid,
                orders: self.store.orders.values().filter(|o| o.symbol == p.symbol).count(),
                conditionals: self.store.conditionals.values().filter(|c| c.symbol == p.symbol).count(),
            })
            .collect()
    }

    pub fn ask_square_off(&mut self, scope: Scope) {
        if self.square_legs(scope).is_empty() {
            let what = match scope {
                Scope::One(symbol) => format!("No position in {symbol} to square off"),
                Scope::All => "No positions to square off".to_string(),
            };
            self.toasts.push(Level::Info, what, "");
            return;
        }
        self.square_confirm = Some(scope);
    }

    pub fn square_off(&mut self, scope: Scope) {
        self.square_confirm = None;
        for leg in self.square_legs(scope) {
            if leg.orders > 0 {
                self.submit(OrderRequest::CancelAll(Some(leg.symbol)));
            }
            let armed: Vec<u64> = self
                .store
                .conditionals
                .values()
                .filter(|c| c.symbol == leg.symbol)
                .map(|c| c.cond_id)
                .collect();
            for id in armed {
                self.submit(OrderRequest::CancelConditional(id));
            }
            self.squaring.push((leg.symbol, Instant::now()));
        }
        self.release_squares();
    }

    fn sells_working(&self, symbol: Symbol) -> bool {
        self.store
            .orders
            .values()
            .any(|o| o.symbol == symbol && o.side == Side::Sell && o.leaves > 0)
    }

    pub fn release_squares(&mut self) {
        self.squared.retain(|(_, at)| at.elapsed() < SQUARE_WAIT);
        let waiting = std::mem::take(&mut self.squaring);
        for (symbol, since) in waiting {
            if !self.sells_working(symbol) {
                let qty = self.held(symbol);
                if qty > 0 {
                    self.submit(OrderRequest::Market { symbol, side: Side::Sell, qty });
                    self.squared.push((symbol, Instant::now()));
                    self.toasts.push(Level::Info, format!("Squaring off {symbol}"), format!("Sell {qty} at market"));
                }
            } else if since.elapsed() > SQUARE_WAIT {
                self.toasts.push(
                    Level::Bad,
                    format!("{symbol} was not squared off"),
                    "Its working sell orders did not cancel in time; check Orders and try again.",
                );
            } else {
                self.squaring.push((symbol, since));
            }
        }
    }
}
