use ome_core::risk::{MaxNotional, MaxQuantity, PositiveQuantity, RiskEngine};
use ome_core::types::{OrderKind, OrderOptions, Qty, StpMode, TimeInForce};
use ome_protocol::Role;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    Retail,
    Prime,
    Dma,
    Lp,
}

impl ProfileKind {
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "retail" => Some(ProfileKind::Retail),
            "prime" => Some(ProfileKind::Prime),
            "dma" => Some(ProfileKind::Dma),
            "lp" => Some(ProfileKind::Lp),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ProfileKind::Retail => "retail broker",
            ProfileKind::Prime => "prime broker",
            ProfileKind::Dma => "DMA provider",
            ProfileKind::Lp => "liquidity provider",
        }
    }
}

pub struct Limits {
    pub max_qty: Qty,
    pub max_notional: u64,
}

pub struct Profile {
    pub kind: ProfileKind,
    pub allowed: Vec<Role>,
}

impl Profile {
    pub fn new(kind: ProfileKind) -> Self {
        let allowed = match kind {
            ProfileKind::Retail => vec![Role::Retail],
            ProfileKind::Prime => vec![Role::Quant, Role::Prop, Role::Issuer],
            ProfileKind::Dma => vec![Role::Hft],
            ProfileKind::Lp => vec![Role::LpDesk],
        };
        Self { kind, allowed }
    }

    pub fn allows(&self, role: Role) -> bool {
        role == Role::BrokerAdmin || self.allowed.contains(&role)
    }

    pub fn limits(role: Role) -> Limits {
        let (max_qty, max_notional) = match role {
            Role::Retail => (1_000, 10_000_000),
            Role::Quant => (1_000_000, 10_000_000_000),
            Role::Hft => (50_000, 1_000_000_000),
            Role::Prop | Role::LpDesk => (100_000, 2_000_000_000),
            Role::Issuer => (100_000_000, u64::MAX),
            Role::BrokerAdmin => (0, 0),
        };
        Limits {
            max_qty,
            max_notional,
        }
    }

    pub fn permits(role: Role, kind: OrderKind, options: &OrderOptions) -> Result<(), String> {
        let advanced = options.post_only
            || options.display_qty.is_some()
            || options.stp != StpMode::CancelResting;
        let allowed = match role {
            Role::BrokerAdmin => false,
            Role::Issuer => {
                kind != OrderKind::Market && options.tif == TimeInForce::Gtc && !advanced
            }
            Role::Retail => !advanced,
            Role::Quant | Role::Prop | Role::Hft | Role::LpDesk => true,
        };
        if allowed {
            Ok(())
        } else {
            Err(format!(
                "{} accounts may not use {} orders{}",
                role.label(),
                options.tif.label(),
                if advanced {
                    " with post-only, iceberg or self-trade options"
                } else {
                    ""
                }
            ))
        }
    }

    pub fn risk(role: Role) -> RiskEngine {
        let limits = Self::limits(role);
        RiskEngine::new()
            .with(PositiveQuantity)
            .with(MaxQuantity(limits.max_qty))
            .with(MaxNotional(limits.max_notional))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_profile_serves_its_own_client_types() {
        assert!(Profile::new(ProfileKind::Retail).allows(Role::Retail));
        assert!(!Profile::new(ProfileKind::Retail).allows(Role::Hft));
        assert!(Profile::new(ProfileKind::Dma).allows(Role::Hft));
        assert!(Profile::new(ProfileKind::Prime).allows(Role::Quant));
        assert!(Profile::new(ProfileKind::Lp).allows(Role::LpDesk));
        assert!(Profile::new(ProfileKind::Lp).allows(Role::BrokerAdmin));
    }

    #[test]
    fn advanced_order_features_follow_the_participant_matrix() {
        let iceberg = OrderOptions {
            display_qty: Some(10),
            ..OrderOptions::default()
        };
        let fok = OrderOptions::tif(TimeInForce::Fok);
        assert!(Profile::permits(Role::Retail, OrderKind::Limit(1), &fok).is_ok());
        assert!(Profile::permits(Role::Retail, OrderKind::Limit(1), &iceberg).is_err());
        assert!(Profile::permits(Role::Quant, OrderKind::Limit(1), &iceberg).is_ok());
        assert!(Profile::permits(Role::Issuer, OrderKind::Limit(1), &fok).is_err());
        assert!(
            Profile::permits(Role::Issuer, OrderKind::Limit(1), &OrderOptions::default()).is_ok()
        );
    }
}
