use crate::strategies::{Params, STRATEGIES};
use ome_core::price::parse_amount;
use ome_core::symbol::Symbol;
use ome_net::auth::valid_username;
use ome_protocol::Role;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSpec {
    pub name: String,
    pub group: String,
    pub broker: String,
    pub role: Role,
    pub strategy: String,
    pub symbols: Option<Vec<Symbol>>,
    pub cash: u64,
    pub seed: u64,
    pub params: Params,
}

impl AgentSpec {
    pub fn resolve(&self, listed: &[Symbol]) -> Vec<Symbol> {
        match &self.symbols {
            None => listed.to_vec(),
            Some(wanted) => wanted
                .iter()
                .copied()
                .filter(|s| listed.contains(s))
                .collect(),
        }
    }
}

pub fn parse(text: &str, scale: f64) -> Result<Vec<AgentSpec>, String> {
    let mut agents = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let at = |why: String| format!("roster line {}: {why}", number + 1);
        let mut words = line.split_whitespace();
        if words.next() != Some("group") {
            return Err(at("lines start with 'group <name>'".into()));
        }
        let group = words
            .next()
            .ok_or_else(|| at("missing group name".into()))?;
        let mut count = 1.0;
        let (mut broker, mut role, mut strategy) = (None, None, None);
        let mut symbols = None;
        let (mut cash, mut seed) = (0, 0);
        let mut params = Params::default();
        for word in words {
            let (key, value) = word
                .split_once('=')
                .ok_or_else(|| at(format!("expected key=value, got '{word}'")))?;
            let amount = |v: &str| parse_amount(v).ok_or_else(|| at(format!("bad amount '{v}'")));
            match key {
                "count" => {
                    count = value
                        .parse::<f64>()
                        .map_err(|_| at(format!("bad count '{value}'")))?
                }
                "broker" => broker = Some(value.to_string()),
                "role" => {
                    role =
                        Some(Role::parse(value).ok_or_else(|| at(format!("bad role '{value}'")))?)
                }
                "strategy" => {
                    if !STRATEGIES.contains(&value) {
                        return Err(at(format!(
                            "unknown strategy '{value}' (one of {})",
                            STRATEGIES.join(", ")
                        )));
                    }
                    strategy = Some(value.to_string());
                }
                "symbols" if value == "*" => symbols = None,
                "symbols" => {
                    symbols = Some(
                        value
                            .split(',')
                            .map(|s| {
                                Symbol::parse(s).ok_or_else(|| at(format!("bad symbol '{s}'")))
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    )
                }
                "cash" => cash = amount(value)?,
                "seed" => seed = amount(value)?,
                _ => {
                    params.0.insert(key.to_string(), value.to_string());
                }
            }
        }
        let broker = broker.ok_or_else(|| at("missing broker=".into()))?;
        let role = role.ok_or_else(|| at("missing role=".into()))?;
        let strategy = strategy.ok_or_else(|| at("missing strategy=".into()))?;
        let copies = (count * scale).round().max(0.0) as usize;
        for n in 1..=copies {
            let name = format!("{group}-{n}");
            if !valid_username(&name) {
                return Err(at(format!("'{name}' is not a valid account name")));
            }
            agents.push(AgentSpec {
                name,
                group: group.to_string(),
                broker: broker.clone(),
                role,
                strategy: strategy.clone(),
                symbols: symbols.clone(),
                cash,
                seed,
                params: params.clone(),
            });
        }
    }
    Ok(agents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_expand_into_named_agents_with_parameters() {
        let text = "
            # tier 1
            group hft-a count=2 broker=dma-co role=hft strategy=market-maker cash=400000 seed=30000 gamma=0.2
            group pairs broker=prime-co role=quant strategy=pairs symbols=ACME,NOVA exec=dark
        ";
        let agents = parse(text, 1.0).unwrap();
        assert_eq!(agents.len(), 3);
        assert_eq!(agents[1].name, "hft-a-2");
        assert_eq!((agents[0].cash, agents[0].seed), (400_000_00, 30_000_00));
        assert_eq!(agents[0].params.f64("gamma", 0.0), 0.2);
        assert_eq!(agents[0].symbols, None);
        let listed = [
            Symbol::parse("ACME").unwrap(),
            Symbol::parse("ZEN").unwrap(),
        ];
        assert_eq!(agents[2].resolve(&listed), vec![listed[0]]);
        assert_eq!(parse(text, 2.0).unwrap().len(), 6);
        assert!(parse("group x broker=a role=retail strategy=nope", 1.0).is_err());
        assert!(parse("group x role=retail strategy=noise", 1.0).is_err());
        assert!(parse("agent x", 1.0).is_err());
    }
}
