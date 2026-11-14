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
