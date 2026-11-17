use ome_core::symbol::Symbol;
use ome_protocol::NewsKind;
use rand::Rng;
use rand::rngs::StdRng;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Story {
    pub kind: NewsKind,
    pub event: String,
    pub symbol: Option<Symbol>,
    pub company: String,
    pub sector: String,
    pub sentiment: i64,
    pub facts: Vec<(String, String)>,
}

impl Story {
    pub fn fact(&self, key: &str) -> Option<&str> {
        self.facts
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn data(&self) -> String {
        self.facts
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copy {
    pub headline: String,
    pub body: String,
}

pub trait Writer: Send {
    fn name(&self) -> String;
    fn write(&mut self, story: &Story, rng: &mut StdRng) -> Copy;
}

#[derive(Default)]
pub struct TemplateWriter;

fn templates(story: &Story) -> (&'static [&'static str], &'static [&'static str]) {
    let positive = story.sentiment >= 0;
    match (story.event.as_str(), positive) {
        ("prospectus", _) => (
            &["{company} files prospectus ahead of {symbol} listing"],
            &[
                "{company} ({sector}) has filed its prospectus: annual revenue of {revenue}, net margin {margin}, earnings of {eps} per share and revenue growth of {growth}. {shares} shares are offered at {price}.",
            ],
        ),
        ("earnings", true) => (
            &[
                "{company} beats estimates with EPS of {eps}",
                "{company} tops forecasts as quarterly profit rises",
                "Strong quarter for {company}: {eps} a share vs {consensus} expected",
            ],
            &[
                "{company} reported quarterly earnings of {eps} per share against a consensus of {consensus}, a surprise of {surprise}. Revenue came in at {revenue} with a net margin of {margin}.",
                "Quarter {quarter} results: {company} earned {eps} a share, {surprise} above the {consensus} analysts expected, on revenue of {revenue}.",
            ],
        ),
        ("earnings", false) => (
            &[
                "{company} misses estimates with EPS of {eps}",
                "{company} falls short as quarterly profit disappoints",
                "Weak quarter for {company}: {eps} a share vs {consensus} expected",
            ],
            &[
                "{company} reported quarterly earnings of {eps} per share against a consensus of {consensus}, a shortfall of {surprise}. Revenue came in at {revenue} with a net margin of {margin}.",
                "Quarter {quarter} results: {company} earned {eps} a share, {surprise} versus the {consensus} analysts expected, on revenue of {revenue}.",
            ],
        ),
        ("cpi", true) => (
            &[
                "Inflation cools to {actual}",
                "Consumer prices rise less than expected",
            ],
            &[
                "Annual inflation came in at {actual} against {expected} expected, easing pressure on the central bank.",
            ],
        ),
        ("cpi", false) => (
            &[
                "Inflation runs hot at {actual}",
                "Consumer prices rise faster than expected",
            ],
            &[
                "Annual inflation came in at {actual} against {expected} expected, raising the odds of tighter policy.",
            ],
        ),
        ("gdp", true) => (
            &[
                "Economy grows {actual}, beating forecasts",
                "Growth surprises to the upside",
            ],
            &["The economy grew at an annualised {actual} against {expected} expected."],
        ),
        ("gdp", false) => (
            &["Growth slows to {actual}", "Economy loses momentum"],
            &["The economy grew at an annualised {actual} against {expected} expected."],
        ),
        ("rates", _) => match story.fact("move").unwrap_or("0") {
            "0" => (
                &["Central bank holds rates at {rate}"],
                &[
                    "The central bank left its policy rate at {rate}; markets had expected {expected}.",
                ],
            ),
            m if m.starts_with('-') => (
                &["Central bank cuts rates to {rate}"],
                &[
                    "The central bank cut its policy rate by {change} to {rate}; markets had expected {expected}.",
                ],
            ),
            _ => (
                &["Central bank raises rates to {rate}"],
                &[
                    "The central bank raised its policy rate by {change} to {rate}; markets had expected {expected}.",
                ],
            ),
        },
        ("shock", true) => (
            &[
                "Surprise news lifts {company}",
                "{company} jumps on unexpected news",
            ],
            &["An unexpected development is expected to change {company}'s earnings by {move}."],
        ),
        ("shock", false) => (
            &[
                "{company} hit by surprise news",
                "Unexpected setback for {company}",
            ],
            &["An unexpected development is expected to change {company}'s earnings by {move}."],
        ),
        ("contract-win", _) => (
            &[
                "{company} wins major contract",
                "{company} lands a multi-year deal",
            ],
            &["{company} announced a significant new contract expected to lift revenue growth."],
        ),
        ("product-launch", _) => (
            &[
                "{company} unveils new product line",
                "{company} launches its next generation",
            ],
            &["{company} launched a new product that analysts expect to add to sales growth."],
        ),
        ("lawsuit", _) => (
            &[
                "{company} hit with lawsuit",
                "Legal fight weighs on {company}",
            ],
            &["{company} faces a lawsuit that could raise costs and pressure margins."],
        ),
        ("recall", _) => (
            &[
                "{company} announces product recall",
                "Recall at {company} raises cost concerns",
            ],
            &["{company} is recalling products; the cost is expected to hit margins and sales."],
        ),
        ("exec-departure", _) => (
            &["{company} executive steps down unexpectedly"],
            &["A senior executive at {company} has resigned, adding uncertainty to its plans."],
        ),
        ("buyback", _) => (
            &["{company} announces share buyback"],
            &[
                "{company} will repurchase shares, returning cash to investors and lifting earnings per share.",
            ],
        ),
        ("guidance-raise", _) => (
            &[
                "{company} raises its outlook",
                "{company} lifts full-year guidance",
            ],
            &["{company} raised its growth outlook, citing stronger demand."],
        ),
        ("guidance-cut", _) => (
            &[
                "{company} cuts its outlook",
                "{company} lowers full-year guidance",
            ],
            &["{company} lowered its growth outlook, citing softer demand."],
        ),
        ("upgrade", _) => (
            &["Analysts upgrade {company} to buy"],
            &[
                "Analysts upgraded {company}, citing an attractive valuation and improving prospects.",
            ],
        ),
        ("downgrade", _) => (
            &["Analysts downgrade {company} to sell"],
            &["Analysts downgraded {company}, citing valuation and execution risks."],
        ),
        ("merger-rumour", _) => (
            &[
                "Report: {company} explores a sale",
                "{company} draws takeover interest",
            ],
            &[
                "People familiar with the matter say {company} has received takeover interest; the company declined to comment.",
            ],
        ),
        ("regulation", _) => (
            &["New rules weigh on the {sector} sector"],
            &[
                "Regulators announced new rules for {sector} companies that are expected to slow growth.",
            ],
        ),
        ("demand-boom", _) => (
            &["Demand surges across {sector}"],
            &["Orders across the {sector} sector are running well ahead of expectations."],
        ),
        ("supply-shock", _) => (
            &["Supply shortages squeeze {sector} margins"],
            &["Input shortages are raising costs for {sector} companies."],
        ),
        ("subsidy", _) => (
            &["Government backs {sector} with new subsidies"],
            &["New government support is expected to lift margins in the {sector} sector."],
        ),
        (_, true) => (
            &["Positive news for {company}"],
            &["{company} reported positive news."],
        ),
        (_, false) => (
            &["Negative news for {company}"],
            &["{company} reported negative news."],
        ),
    }
}

fn fill(template: &str, story: &Story) -> String {
    let mut out = template
        .replace("{company}", &story.company)
        .replace("{sector}", &story.sector)
        .replace(
            "{symbol}",
            &story.symbol.map(|s| s.to_string()).unwrap_or_default(),
        );
    for (key, value) in &story.facts {
        out = out.replace(&format!("{{{key}}}"), value);
    }
    out
}

impl Writer for TemplateWriter {
    fn name(&self) -> String {
        "templates".into()
    }

    fn write(&mut self, story: &Story, rng: &mut StdRng) -> Copy {
        let (headlines, bodies) = templates(story);
        Copy {
            headline: fill(headlines[rng.random_range(0..headlines.len())], story),
            body: fill(bodies[rng.random_range(0..bodies.len())], story),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    const CATALOGUE: &[&str] = &[
        "contract-win", "product-launch", "lawsuit", "recall", "exec-departure", "buyback", "guidance-raise",
        "guidance-cut", "upgrade", "downgrade", "merger-rumour", "regulation", "demand-boom", "supply-shock", "subsidy",
    ];

    fn story(event: &str, sentiment: i64, facts: &[(&str, &str)]) -> Story {
        Story {
            kind: NewsKind::Company,
            event: event.into(),
            symbol: Symbol::parse("ACME"),
            company: "Acme Robotics".into(),
            sector: "robotics".into(),
            sentiment,
            facts: facts
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn templates_fill_every_placeholder_for_every_story() {
        let mut rng = StdRng::seed_from_u64(1);
        let mut writer = TemplateWriter;
        let numbers = [
            ("eps", "0.32"),
            ("consensus", "0.30"),
            ("surprise", "+6.7%"),
            ("revenue", "135,416"),
            ("margin", "12.0%"),
            ("growth", "9.0%"),
            ("quarter", "2"),
            ("shares", "52000"),
            ("price", "25.00"),
            ("actual", "2.9%"),
            ("expected", "2.7%"),
            ("rate", "4.75%"),
            ("change", "0.25%"),
            ("move", "25"),
        ];
        let mut events: Vec<&str> = CATALOGUE.to_vec();
        events.extend([
            "prospectus",
            "earnings",
            "cpi",
            "gdp",
            "rates",
            "shock",
            "other",
        ]);
        for event in events {
            for sentiment in [-40, 40] {
                for _ in 0..5 {
                    let copy = writer.write(&story(event, sentiment, &numbers), &mut rng);
                    assert!(
                        !copy.headline.contains('{') && !copy.body.contains('{'),
                        "{event}: {copy:?}"
                    );
                    assert!(!copy.headline.is_empty() && !copy.body.is_empty());
                }
            }
        }
        let beat = writer.write(&story("earnings", 30, &numbers), &mut rng);
        assert!(beat.body.contains("0.32") && beat.body.contains("0.30"));
        let cut = story(
            "rates",
            -30,
            &[
                ("move", "-25"),
                ("rate", "4.25%"),
                ("change", "0.25%"),
                ("expected", "a hold"),
            ],
        );
        assert!(writer.write(&cut, &mut rng).headline.contains("cuts"));
        assert_eq!(
            story("x", 0, &numbers[..2]).data(),
            "eps=0.32 consensus=0.30"
        );
    }
}
