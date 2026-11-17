use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmConfig {
    pub url: String,
    pub model: String,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sampling {
    pub temperature: f64,
    pub max_tokens: u32,
    pub seed: Option<u64>,
}

pub trait Model: Send {
    fn name(&self) -> &str;
    fn warm_up(&self) -> Result<(), String>;
    fn chat(&self, prompt: &str, sampling: Sampling) -> Result<String, String>;
}

pub struct Ollama {
    host: String,
    model: String,
    timeout: Duration,
}

pub const UPBEAT: &[&str] = &[
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

pub const GLOOMY: &[&str] = &[
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

pub fn tone_conflict(text: &str, sentiment: i64) -> Option<String> {
    let lower = text.to_lowercase();
    let says = |words: &[&'static str]| words.iter().find(|w| lower.contains(*w)).copied();
    if sentiment <= -20
        && let Some(word) = says(UPBEAT)
    {
        return Some(format!("upbeat wording ('{word}') for bad news"));
    }
    if sentiment >= 20
        && let Some(word) = says(GLOOMY)
    {
        return Some(format!("gloomy wording ('{word}') for good news"));
    }
    None
}

pub fn host_of(url: &str) -> Result<String, String> {
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

pub fn content_of(body: &[u8]) -> Result<String, String> {
    let reply: Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    reply["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "no message content".to_string())
}

pub fn json_reply(content: &str) -> Result<Value, String> {
    serde_json::from_str(content.trim()).map_err(|e| format!("not JSON: {e}"))
}

pub fn offline(error: &str) -> bool {
    error.contains("refused") || error.contains("resolve") || error.contains("no answer")
}

fn get(host: &str, path: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    request(host, &format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"), timeout)
}

fn request(host: &str, text: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let addr = host
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("cannot resolve the LLM host")?;
    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(1)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|e| format!("no answer: {e}"))?;
    parse_http(&response)
}

pub fn probe(url: &str, model: &str) -> Result<bool, String> {
    let host = host_of(url)?;
    let body = get(&host, "/api/tags", Duration::from_secs(2))?;
    let tags: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    Ok(tags["models"]
        .as_array()
        .is_some_and(|models| models.iter().any(|m| m["name"].as_str() == Some(model))))
}

impl Ollama {
    pub fn new(config: &LlmConfig) -> Result<Self, String> {
        Ok(Self {
            host: host_of(&config.url)?,
            model: config.model.clone(),
            timeout: config.timeout,
        })
    }

    fn post(&self, path: &str, payload: &Value) -> Result<Vec<u8>, String> {
        let body = payload.to_string();
        let text = format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.host,
            body.len()
        );
        request(&self.host, &text, self.timeout)
    }
}

impl Model for Ollama {
    fn name(&self) -> &str {
        &self.model
    }

    fn warm_up(&self) -> Result<(), String> {
        self.post(
            "/api/chat",
            &json!({"model": self.model, "messages": [], "stream": false, "keep_alive": "30m"}),
        )
        .map(|_| ())
    }

    fn chat(&self, prompt: &str, sampling: Sampling) -> Result<String, String> {
        let mut options = json!({"temperature": sampling.temperature, "num_predict": sampling.max_tokens});
        if let Some(seed) = sampling.seed {
            options["seed"] = json!(seed);
        }
        let payload = json!({
            "model": self.model,
            "stream": false,
            "format": "json",
            "keep_alive": "30m",
            "options": options,
            "messages": [{"role": "user", "content": prompt}],
        });
        content_of(&self.post("/api/chat", &payload)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let text = content_of(&parse_http(plain.as_bytes()).unwrap()).unwrap();
        assert_eq!(json_reply(&text).unwrap()["headline"], "Zen Biotech misses");
        let chunked = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
            body.len()
        );
        assert_eq!(parse_http(chunked.as_bytes()).unwrap(), body.as_bytes());
        assert!(parse_http(b"HTTP/1.1 404 Not Found\r\n\r\nmodel not found").is_err());
        assert!(json_reply("not json").is_err());
        assert!(host_of("https://x").is_err());
        assert_eq!(host_of("http://127.0.0.1:11434/").unwrap(), "127.0.0.1:11434");
        assert!(tone_conflict("Acme soars", -40).is_some());
        assert!(tone_conflict("Acme misses", 40).is_some());
        assert!(tone_conflict("Acme misses", -40).is_none());
        assert!(offline("connection refused") && !offline("not JSON"));
    }
}
