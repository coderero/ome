use crate::writer::Copy;
use ome_protocol::{NewsItem, NewsKind};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub struct LlmConfig {
    pub url: String,
    pub model: String,
    pub timeout: Duration,
}

pub struct Llm {
    host: String,
    model: String,
    timeout: Duration,
}

const UPBEAT: &[&str] = &[
    "beat",
    "exceed",
    "above expect",
    "above analyst",
    "tops",
    "topped",
    "soar",
    "surge",
    "jump",
    "rall",
    "record high",
    "outperform",
];
const GLOOMY: &[&str] = &[
    "miss",
    "short of",
    "below expect",
    "below analyst",
    "disappoint",
    "plunge",
    "slump",
    "tumble",
    "sink",
    "underperform",
];

fn host_of(url: &str) -> Result<String, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or("only http:// URLs are supported")?;
    Ok(rest.trim_end_matches('/').to_string())
}

fn decode_chunked(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("bad chunk")?;
        let size_text = std::str::from_utf8(&body[..line_end]).map_err(|e| e.to_string())?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|e| e.to_string())?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size + 2 {
            return Err("truncated chunk".into());
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

pub fn parse_http(response: &[u8]) -> Result<Vec<u8>, String> {
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("no HTTP header")?;
    let head = String::from_utf8_lossy(&response[..split]).to_ascii_lowercase();
    let status = head.split_whitespace().nth(1).unwrap_or("");
    let body = &response[split + 4..];
    let body = if head.contains("transfer-encoding: chunked") {
        decode_chunked(body)?
    } else {
        body.to_vec()
    };
    if status != "200" {
        return Err(format!(
            "HTTP {status}: {}",
            String::from_utf8_lossy(&body)
                .chars()
                .take(200)
                .collect::<String>()
        ));
    }
    Ok(body)
}

pub fn prompt(item: &NewsItem, company: &str) -> String {
    let tone = match item.sentiment {
        s if s >= 30 => "clearly positive",
        s if s >= 8 => "mildly positive",
        s if s <= -30 => "clearly negative",
        s if s <= -8 => "mildly negative",
        _ => "neutral",
    };
    let length = match item.kind {
        NewsKind::Prospectus | NewsKind::Earnings => "3 or 4 sentences",
        _ => "2 or 3 sentences",
    };
    let subject = match item.kind {
        NewsKind::Macro => "the economy".to_string(),
        NewsKind::Sector => format!("the {} sector", item.sector),
        _ => format!(
            "{company} ({}, {} sector)",
            item.symbol.map(|s| s.to_string()).unwrap_or_default(),
            item.sector
        ),
    };
    let draft = format!("{} {}", item.headline, item.body);
    let facts: Vec<&str> = item
        .data
        .split_whitespace()
        .filter(|pair| {
            pair.split_once('=')
                .is_some_and(|(_, v)| draft.contains(v.trim_start_matches(['+', '-'])))
        })
        .collect();
    format!(
        "You are a financial wire reporter. Rewrite this {kind} story about {subject} as a short news item.\n\
         Facts: {data}\n\
         Draft headline: {headline}\n\
         Draft body: {body}\n\
         Rules: keep every number exactly as written in the facts and draft; do not invent numbers, \
         names, quotes or dates; the tone is {tone}; the headline is under 90 characters; the body is {length}.\n\
         Reply with JSON only: {{\"headline\": \"...\", \"body\": \"...\"}}",
        kind = item.kind.label(),
        data = if facts.is_empty() {
            "none beyond the draft".to_string()
        } else {
            facts.join(" ")
        },
        headline = item.headline,
        body = item.body,
    )
}

fn numbers(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .map(|t| t.trim_matches(|c| c == '.' || c == ','))
        .filter(|t| t.chars().any(|c| c.is_ascii_digit()) && t.contains('.'))
        .map(str::to_string)
        .collect()
}

pub fn validate(item: &NewsItem, copy: &Copy) -> Result<(), String> {
    let headline = copy.headline.trim();
    let body = copy.body.trim();
    if !(10..=120).contains(&headline.chars().count()) {
        return Err(format!("headline length {}", headline.chars().count()));
    }
    if !(30..=900).contains(&body.chars().count()) {
        return Err(format!("body length {}", body.chars().count()));
    }
    let text = format!("{headline} {body}");
    for number in numbers(&format!("{} {}", item.headline, item.body)) {
        if !text.contains(&number) {
            return Err(format!("dropped the number {number}"));
        }
    }
    let lower = text.to_lowercase();
    let says = |words: &[&'static str]| words.iter().find(|w| lower.contains(*w)).copied();
    if item.sentiment <= -20
        && let Some(word) = says(UPBEAT)
    {
        return Err(format!("upbeat wording ('{word}') for bad news"));
    }
    if item.sentiment >= 20
        && let Some(word) = says(GLOOMY)
    {
        return Err(format!("gloomy wording ('{word}') for good news"));
    }
    Ok(())
}

pub fn parse_reply(body: &[u8]) -> Result<Copy, String> {
    let reply: Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let content = reply["message"]["content"]
        .as_str()
        .ok_or("no message content")?;
    let copy: Value = serde_json::from_str(content.trim()).map_err(|e| format!("not JSON: {e}"))?;
    let field = |key: &str| {
        copy[key]
            .as_str()
            .map(|s| s.trim().to_string())
            .ok_or(format!("missing {key}"))
    };
    Ok(Copy {
        headline: field("headline")?,
        body: field("body")?,
    })
}

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
    let says = |words: &[&'static str]| words.iter().find(|w| lower.contains(*w)).copied();
    if sentiment <= -20
        && let Some(word) = says(UPBEAT)
    {
        return Err(format!("upbeat wording ('{word}') for bad news"));
    }
    if sentiment >= 20
        && let Some(word) = says(GLOOMY)
    {
        return Err(format!("gloomy wording ('{word}') for good news"));
    }
    Ok(idea)
}

pub fn parse_idea(body: &[u8], ctx: &IdeaContext) -> Result<Idea, String> {
    let reply: Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let content = reply["message"]["content"].as_str().ok_or("no message content")?;
    let raw: Value = serde_json::from_str(content.trim()).map_err(|e| format!("not JSON: {e}"))?;
    check_idea(&raw, ctx)
}

impl Llm {
    pub fn new(config: &LlmConfig) -> Result<Self, String> {
        Ok(Self {
            host: host_of(&config.url)?,
            model: config.model.clone(),
            timeout: config.timeout,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn post(&self, path: &str, payload: &Value) -> Result<Vec<u8>, String> {
        let addr = self
            .host
            .to_socket_addrs()
            .map_err(|e| e.to_string())?
            .next()
            .ok_or("cannot resolve the LLM host")?;
        let mut stream =
            TcpStream::connect_timeout(&addr, Duration::from_secs(1)).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|e| e.to_string())?;
        let body = payload.to_string();
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.host,
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .map_err(|e| format!("no answer: {e}"))?;
        parse_http(&response)
    }

    pub fn warm_up(&self) -> Result<(), String> {
        self.post(
            "/api/chat",
            &json!({"model": self.model, "messages": [], "stream": false, "keep_alive": "30m"}),
        )
        .map(|_| ())
    }

    pub fn rewrite(&self, item: &NewsItem, company: &str) -> Result<Copy, String> {
        let payload = json!({
            "model": self.model,
            "stream": false,
            "format": "json",
            "keep_alive": "30m",
            "options": {"temperature": 0.4, "num_predict": 260},
            "messages": [{"role": "user", "content": prompt(item, company)}],
        });
        let copy = parse_reply(&self.post("/api/chat", &payload)?)?;
        validate(item, &copy)?;
        Ok(copy)
    }

    pub fn idea(&self, ctx: &IdeaContext, seed: u64) -> Result<Idea, String> {
        let payload = json!({
            "model": self.model,
            "stream": false,
            "format": "json",
            "keep_alive": "30m",
            "options": {"temperature": 0.9, "num_predict": 320, "seed": seed},
            "messages": [{"role": "user", "content": idea_prompt(ctx, seed)}],
        });
        parse_idea(&self.post("/api/chat", &payload)?, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ome_core::symbol::Symbol;

    fn earnings() -> NewsItem {
        NewsItem {
            seq: 8,
            time: 0,
            day: 3,
            kind: NewsKind::Earnings,
            symbol: Symbol::parse("ZEN"),
            sentiment: -40,
            impact_bps: -63,
            sector: "biotech".into(),
            headline: "Weak quarter for Zen Biotech: 0.63 a share vs 0.65 expected".into(),
            body: "Zen Biotech reported quarterly earnings of 0.63 per share against a consensus of 0.65, a shortfall of -3.08%. Revenue came in at $55,555 with a net margin of 18.00%.".into(),
            data: "eps=0.63 consensus=0.65 surprise=-3.08%".into(),
        }
    }

    fn copy(headline: &str, body: &str) -> Copy {
        Copy {
            headline: headline.into(),
            body: body.into(),
        }
    }

    #[test]
    fn rewrites_must_keep_the_numbers_and_the_tone() {
        let item = earnings();
        let good = copy(
            "Zen Biotech misses forecasts with 0.63 EPS",
            "Zen Biotech earned 0.63 a share last quarter, short of the 0.65 analysts expected, a shortfall of 3.08%. Revenue was $55,555 and the net margin held at 18.00%.",
        );
        assert_eq!(validate(&item, &good), Ok(()));
        let invented = copy(
            "Zen Biotech misses forecasts",
            "Zen Biotech earned 0.61 a share, short of the 0.65 analysts expected, on revenue of $55,555 and an 18.00% margin.",
        );
        assert!(validate(&item, &invented).unwrap_err().contains("0.63"));
        let cheerful = copy("Zen Biotech soars after quarter", &good.body);
        assert!(validate(&item, &cheerful).is_err());
        let contradicting = copy(
            "Zen Biotech reports 0.63 EPS",
            "Zen Biotech earned 0.63 a share, exceeding the 0.65 analysts expected by 3.08%, on revenue of $55,555 and an 18.00% margin.",
        );
        assert!(
            validate(&item, &contradicting)
                .unwrap_err()
                .contains("exceed")
        );
        assert!(validate(&item, &copy("Short", &good.body)).is_err());
        let prompt = prompt(&item, "Zen Biotech");
        assert!(prompt.contains("clearly negative") && prompt.contains("eps=0.63"));
        let mut recall = item.clone();
        recall.kind = NewsKind::Company;
        recall.headline = "Recall at Zen Biotech raises cost concerns".into();
        recall.body =
            "Zen Biotech is recalling products; the cost is expected to hit margins.".into();
        recall.data = "event=recall growth_change=-0.09% margin_change=-7.46%".into();
        let recall_prompt = super::prompt(&recall, "Zen Biotech");
        assert!(!recall_prompt.contains("margin_change"), "{recall_prompt}");
        assert!(prompt.contains("3 or 4 sentences"));
    }

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
        let reply = json!({"message": {"content": idea(&[]).to_string()}}).to_string();
        assert_eq!(parse_idea(reply.as_bytes(), &ctx).unwrap(), ok);
    }

    #[test]
    fn http_responses_and_ollama_replies_are_parsed() {
        let content = r#"{\"headline\": \"Zen Biotech misses\", \"body\": \"Text.\"}"#;
        let body = format!(
            r#"{{"model":"m","message":{{"role":"assistant","content":"{content}"}},"done":true}}"#
        );
        let plain = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let parsed = parse_reply(&parse_http(plain.as_bytes()).unwrap()).unwrap();
        assert_eq!(parsed.headline, "Zen Biotech misses");
        let chunked = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
            body.len()
        );
        assert_eq!(parse_http(chunked.as_bytes()).unwrap(), body.as_bytes());
        assert!(parse_http(b"HTTP/1.1 404 Not Found\r\n\r\nmodel not found").is_err());
        assert!(parse_reply(br#"{"message":{"content":"not json"}}"#).is_err());
        assert!(host_of("https://x").is_err());
        assert_eq!(
            host_of("http://127.0.0.1:11434/").unwrap(),
            "127.0.0.1:11434"
        );
    }
}
