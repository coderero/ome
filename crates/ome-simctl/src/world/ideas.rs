use ome_llm::{Model, Sampling, json_reply, tone_conflict};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct IdeaContext {
    pub company: String,
    pub symbol: String,
    pub sector: String,
    pub revenue: u64,
    pub margin_bps: i64,
    pub growth_bps: i64,
    pub recent: Vec<String>,
    pub economy: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Idea {
    pub sector_wide: bool,
    pub event: String,
    pub headline: String,
    pub body: String,
    pub sentiment: i64,
    pub growth_bps: i64,
    pub margin_rel_bps: i64,
    pub one_off_bps: i64,
}

pub const GROWTH_POINTS: f64 = 1.0;
pub const MARGIN_PERCENT: f64 = 2.0;
pub const ONE_OFF_LOW: f64 = -2.0;
pub const ONE_OFF_HIGH: f64 = 0.8;
pub const BPS_PER_TONE: i64 = 3;

const ANGLES: &[&str] = &[
    "a customer win or loss",
    "a supplier or supply chain issue",
    "its workforce or a labour dispute",
    "a plant, warehouse or facility",
    "a new product or service",
    "a legal or regulatory matter",
    "a technology or patent",
    "a competitor's move",
    "a management or board change",
    "a partnership or joint venture",
    "a cyber security incident",
    "pricing or a price change",
    "an expansion into a new market",
    "a quality problem or a recall",
    "an environmental or safety matter",
    "financing, debt or a credit rating",
];

pub fn idea_prompt(ctx: &IdeaContext, seed: u64) -> String {
    let lean = ["good", "bad", "mixed", "good", "bad", "minor"][(seed % 6) as usize];
    let angle = ANGLES[((seed / 6) % ANGLES.len() as u64) as usize];
    let recent = if ctx.recent.is_empty() {
        "none yet".to_string()
    } else {
        ctx.recent.iter().map(|h| format!("- {h}")).collect::<Vec<_>>().join("\n")
    };
    format!(
        "You are the business news desk of a wire service. Invent ONE plausible new development about {company} ({symbol}), \
         a {sector} company with annual revenue of about ${revenue}, a net margin of {margin:.1}% and revenue growth of {growth:.1}% a year.\n\
         The economy: {economy}.\n\
         Recent coverage (do not repeat these; pick something different):\n{recent}\n\
         This time make the news {lean} for the company and about {angle}. It may affect only {company} or the whole {sector} sector.\n\
         Write a specific, concrete story with a fresh headline; avoid vague phrases like steady growth or thrives despite challenges.\n\
         Size the effect realistically for a company of this size: most news changes little.\n\
         Rules: no numbers, figures, percentages, dates or quotes in the headline or body; no real people or real companies.\n\
         Reply with JSON only:\n\
         {{\"scope\": \"company\" or \"sector\", \"event\": \"short-kebab-case-label\", \"headline\": \"under 90 characters\", \
         \"body\": \"2 or 3 sentences\", \"sentiment\": integer from -100 to 100, \
         \"growth_change_points\": change in yearly revenue growth, between -{g} and {g}, \
         \"margin_change_percent\": relative change of the net margin, between -{m} and {m}, \
         \"one_off_percent_of_revenue\": one-time profit (positive) or cost (negative) as a percent of annual revenue, between {lo} and {hi}}}",
        company = ctx.company,
        symbol = ctx.symbol,
        sector = ctx.sector,
        revenue = ctx.revenue,
        margin = ctx.margin_bps as f64 / 100.0,
        growth = ctx.growth_bps as f64 / 100.0,
        economy = ctx.economy,
        g = GROWTH_POINTS,
        m = MARGIN_PERCENT,
        lo = ONE_OFF_LOW,
        hi = ONE_OFF_HIGH,
    )
}

fn bounded(value: f64, low: f64, high: f64, what: &str) -> Result<f64, String> {
    if !value.is_finite() || value < low * 2.0 - 0.01 || value > high * 2.0 + 0.01 {
        return Err(format!("{what} {value} is far outside {low}..{high}"));
    }
    Ok(value.clamp(low, high))
}

pub fn check_idea(raw: &Value, ctx: &IdeaContext) -> Result<Idea, String> {
    let text = |key: &str| raw[key].as_str().map(|s| s.trim().to_string()).ok_or(format!("missing {key}"));
    let number = |key: &str| raw[key].as_f64().ok_or(format!("missing {key}"));
    let scope = text("scope")?.to_lowercase();
    let sector_wide = match scope.as_str() {
        "company" => false,
        "sector" => true,
        other => return Err(format!("scope '{other}'")),
    };
    let event: String = text("event")?.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let event = event.trim_matches('-').to_string();
    if event.is_empty() || event.len() > 40 {
        return Err(format!("event label '{event}'"));
    }
    let headline = text("headline")?;
    let body = text("body")?;
    if !(10..=110).contains(&headline.chars().count()) {
        return Err(format!("headline length {}", headline.chars().count()));
    }
    if !(40..=700).contains(&body.chars().count()) {
        return Err(format!("body length {}", body.chars().count()));
    }
    if format!("{headline}{body}").chars().any(|c| c.is_ascii_digit()) {
        return Err("invented figures in the text".into());
    }
    let name = ctx.company.split_whitespace().next().unwrap_or(&ctx.company).to_lowercase();
    let lower = format!("{headline} {body}").to_lowercase();
    if !sector_wide && !lower.contains(&name) {
        return Err(format!("the story does not mention {}", ctx.company));
    }
    let sentiment = number("sentiment")?.round() as i64;
    if !(-100..=100).contains(&sentiment) {
        return Err(format!("sentiment {sentiment}"));
    }
    let growth = bounded(number("growth_change_points")?, -GROWTH_POINTS, GROWTH_POINTS, "growth change")?;
    let margin = bounded(number("margin_change_percent")?, -MARGIN_PERCENT, MARGIN_PERCENT, "margin change")?;
    let one_off = bounded(number("one_off_percent_of_revenue")?, ONE_OFF_LOW, ONE_OFF_HIGH, "one-off")?;
    let idea = Idea {
        sector_wide,
        event,
        headline,
        body,
        sentiment,
        growth_bps: (growth * 100.0).round() as i64,
        margin_rel_bps: (margin * 100.0).round() as i64,
        one_off_bps: (one_off * 100.0).round() as i64,
    };
    let effect = idea.growth_bps + idea.margin_rel_bps + idea.one_off_bps;
    if effect != 0 && sentiment.signum() != effect.signum() {
        return Err(format!("tone {sentiment} contradicts the effect on the business ({effect:+} bps)"));
    }
    let mut idea = idea;
    let allowed = sentiment.abs() * BPS_PER_TONE;
    if effect.abs() > allowed {
        let scale = |v: i64| v * allowed / effect.abs();
        idea.growth_bps = scale(idea.growth_bps);
        idea.margin_rel_bps = scale(idea.margin_rel_bps);
        idea.one_off_bps = scale(idea.one_off_bps);
    }
    if let Some(problem) = tone_conflict(&lower, sentiment) {
        return Err(problem);
    }
    Ok(idea)
}

pub fn parse_idea(content: &str, ctx: &IdeaContext) -> Result<Idea, String> {
    check_idea(&json_reply(content)?, ctx)
}

pub fn invent(model: &dyn Model, ctx: &IdeaContext, seed: u64) -> Result<Idea, String> {
    let sampling = Sampling { temperature: 0.9, max_tokens: 320, seed: Some(seed) };
    parse_idea(&model.chat(&idea_prompt(ctx, seed), sampling)?, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn context() -> IdeaContext {
        IdeaContext {
            company: "Acme Robotics".into(),
            symbol: "ACME".into(),
            sector: "robotics".into(),
            revenue: 875_000,
            margin_bps: 1_200,
            growth_bps: 900,
            recent: vec!["Acme Robotics hit with lawsuit".into()],
            economy: "policy rate 4.50%".into(),
        }
    }

    fn idea(overrides: &[(&str, Value)]) -> Value {
        let mut raw = json!({
            "scope": "company",
            "event": "Warehouse Automation Deal",
            "headline": "Acme Robotics lands a warehouse automation deal",
            "body": "Acme Robotics will automate the warehouses of a large regional grocer. The deal extends its reach into food logistics.",
            "sentiment": 45,
            "growth_change_points": 0.3,
            "margin_change_percent": 0.0,
            "one_off_percent_of_revenue": 0.4
        });
        for (k, v) in overrides {
            raw[*k] = v.clone();
        }
        raw
    }

    #[test]
    fn invented_events_are_checked_before_they_move_any_value() {
        let ctx = context();
        let ok = check_idea(&idea(&[]), &ctx).unwrap();
        assert_eq!((ok.event.as_str(), ok.growth_bps, ok.one_off_bps, ok.sector_wide), ("warehouse-automation-deal", 30, 40, false));
        let prompt = idea_prompt(&ctx, 1);
        assert!(prompt.contains("do not repeat") && prompt.contains("Acme Robotics hit with lawsuit") && prompt.contains("make the news bad"));
        let angles: std::collections::BTreeSet<String> = (0..96).map(|seed| idea_prompt(&ctx, seed).split("and about ").nth(1).unwrap().split('.').next().unwrap().to_string()).collect();
        assert!(angles.len() >= 12, "requests spread over many angles: {}", angles.len());
        let clamped = check_idea(&idea(&[("growth_change_points", json!(1.8)), ("sentiment", json!(100))]), &ctx).unwrap();
        assert_eq!(clamped.growth_bps, 100, "a little too big is trimmed to the realistic bound");
        let mild = check_idea(&idea(&[("sentiment", json!(5))]), &ctx).unwrap();
        assert!((14..=15).contains(&(mild.growth_bps + mild.one_off_bps)), "a mild story cannot carry a big effect: scaled to 3 bps per point of tone");
        let fail = |o: &[(&str, Value)]| check_idea(&idea(o), &ctx).unwrap_err();
        assert!(fail(&[("growth_change_points", json!(9.0))]).contains("far outside"));
        assert!(fail(&[("body", json!("Acme Robotics will automate warehouses worth 40 million dollars for a grocer chain."))]).contains("figures"));
        assert!(fail(&[("sentiment", json!(-40))]).contains("contradicts"));
        assert!(fail(&[("headline", json!("A warehouse automation deal for a robot maker")), ("body", json!("A robot maker will automate the warehouses of a large regional grocer, extending its reach."))]).contains("mention"));
        assert!(fail(&[("scope", json!("planet"))]).contains("scope"));
        let sector = check_idea(&idea(&[("scope", json!("sector")), ("headline", json!("Robot makers win a tax credit for factory upgrades"))]), &ctx).unwrap();
        assert!(sector.sector_wide);
        let neutral = check_idea(&idea(&[("sentiment", json!(-5)), ("growth_change_points", json!(0)), ("one_off_percent_of_revenue", json!(0))]), &ctx);
        assert!(neutral.is_ok(), "news with no effect on the business can lean either way");
        assert_eq!(parse_idea(&idea(&[]).to_string(), &ctx).unwrap(), ok);
    }
}
