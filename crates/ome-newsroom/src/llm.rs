use crate::writer::Copy;
use ome_llm::{Model, Sampling, json_reply, tone_conflict};
use ome_protocol::{NewsItem, NewsKind};

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
    if let Some(problem) = tone_conflict(&text, item.sentiment) {
        return Err(problem);
    }
    Ok(())
}

pub fn parse_reply(content: &str) -> Result<Copy, String> {
    let copy = json_reply(content)?;
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

pub fn rewrite(model: &dyn Model, item: &NewsItem, company: &str) -> Result<Copy, String> {
    let sampling = Sampling { temperature: 0.4, max_tokens: 260, seed: None };
    let copy = parse_reply(&model.chat(&prompt(item, company), sampling)?)?;
    validate(item, &copy)?;
    Ok(copy)
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
    fn replies_parse_into_copy() {
        let copy = parse_reply(r#"{"headline": "Zen Biotech misses", "body": "Text."}"#).unwrap();
        assert_eq!(copy.headline, "Zen Biotech misses");
        assert!(parse_reply("not json").is_err());
    }
}
