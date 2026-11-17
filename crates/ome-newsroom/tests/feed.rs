use ome_client::Client;
use ome_core::symbol::Symbol;
use ome_newsroom::{NewsConfig, start as start_newsroom};
use ome_protocol::{MemberKind, MemberMessage, NewsItem, NewsKind, ServerMessage, WireItem};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn newsroom(journal: Option<PathBuf>, llm: Option<ome_llm::LlmConfig>) -> SocketAddr {
    start_newsroom(NewsConfig {
        listen: "127.0.0.1:0".into(),
        journal,
        ops_name: "ops".into(),
        ops_secret: "ops".into(),
        sources: vec![("world".into(), "wire".into())],
        seed: Some(7),
        llm,
        registry: None,
    })
    .unwrap()
    .0
}

fn ops(addr: SocketAddr) -> Client {
    Client::connect_member(&addr.to_string(), "ops", "ops").unwrap().0
}

fn status(client: &mut Client) -> ome_protocol::NewsroomStatus {
    client.send_member(&MemberMessage::QueryNewsroom).unwrap();
    match client
        .wait_for(Duration::from_secs(3), |m| matches!(m, ServerMessage::NewsroomStatus(_)))
        .unwrap()
    {
        ServerMessage::NewsroomStatus(s) => s,
        _ => unreachable!(),
    }
}

fn collect(client: &Client, until: impl Fn(&[NewsItem]) -> bool) -> Vec<NewsItem> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut items = Vec::new();
    while !until(&items) {
        assert!(Instant::now() < deadline, "only got {items:#?}");
        if let Ok(Some(ServerMessage::News(item))) = client.recv_timeout(Duration::from_millis(100)) {
            items.push(item);
        }
    }
    items
}

fn wire(seq: u64, kind: NewsKind, event: &str, facts: &[(&str, &str)]) -> WireItem {
    WireItem {
        seq,
        day: 2,
        kind,
        symbol: if kind == NewsKind::Macro { None } else { Symbol::parse("ACME") },
        sentiment: if event == "lawsuit" { -40 } else { 35 },
        impact_bps: if event == "lawsuit" { -80 } else { 40 },
        event: event.into(),
        sector: "robotics".into(),
        company: "Acme Robotics".into(),
        facts: facts.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        data: "fund=ACME:3125:900:0".into(),
        headline: String::new(),
        body: String::new(),
    }
}

fn wires() -> Vec<WireItem> {
    vec![
        wire(1, NewsKind::Prospectus, "prospectus", &[("revenue", "$875,000"), ("margin", "12.00%"), ("eps", "1.25"), ("growth", "9.00%"), ("shares", "67000"), ("price", "25.00")]),
        wire(2, NewsKind::Company, "upgrade", &[("event", "upgrade")]),
        wire(3, NewsKind::Company, "contract-win", &[("event", "contract-win")]),
        wire(4, NewsKind::Company, "lawsuit", &[("event", "lawsuit")]),
        wire(5, NewsKind::Earnings, "earnings", &[("eps", "0.33"), ("consensus", "0.31"), ("surprise", "+6.45%"), ("revenue", "$220,000"), ("margin", "12.00%"), ("quarter", "1")]),
        wire(6, NewsKind::Macro, "cpi", &[("actual", "2.60%"), ("expected", "2.80%")]),
    ]
}

fn source(addr: SocketAddr) -> (Client, u64) {
    let mut client = Client::connect(&addr.to_string()).unwrap();
    client.logon_member("world", "wire", 0).unwrap();
    match client
        .wait_for(Duration::from_secs(3), |m| matches!(m, ServerMessage::ReplayComplete { .. }))
        .unwrap()
    {
        ServerMessage::ReplayComplete { next_seq } => (client, next_seq),
        _ => unreachable!(),
    }
}

#[test]
fn wire_events_reach_subscribers_admitted_by_application_once_and_after_a_restart() {
    let dir = std::env::temp_dir().join(format!("ome-newsroom-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let journal = dir.join("newsroom.journal");
    let _ = std::fs::remove_file(&journal);
    let news = newsroom(Some(journal.clone()), None);

    let mut applicant = Client::connect(&news.to_string()).unwrap();
    assert!(applicant.logon_member("prime-co", "pw", 0).is_err());
    applicant
        .request_member(&MemberMessage::Apply {
            kind: MemberKind::Broker,
            name: "prime-co".into(),
            secret: "pw".into(),
            note: "prime broker".into(),
        })
        .unwrap();
    let mut desk = ops(news);
    desk.send_member(&MemberMessage::QueryApplications).unwrap();
    let pending = match desk
        .wait_for(Duration::from_secs(3), |m| matches!(m, ServerMessage::Applications(_)))
        .unwrap()
    {
        ServerMessage::Applications(list) => list,
        _ => unreachable!(),
    };
    assert_eq!((pending[0].name.as_str(), pending[0].note.as_str()), ("prime-co", "prime broker"));
    let text = desk
        .request_member(&MemberMessage::Decide { name: "prime-co".into(), approve: true, amount: 0, note: String::new() })
        .unwrap();
    assert!(text.contains("admitted"), "{text}");
    assert!(desk
        .request_member(&MemberMessage::Decide { name: "prime-co".into(), approve: true, amount: 0, note: String::new() })
        .is_err());

    let mut subscriber = Client::connect(&news.to_string()).unwrap();
    subscriber.logon_member("prime-co", "pw", 0).unwrap();
    let (mut world, next) = source(news);
    assert_eq!(next, 1);
    for w in wires().into_iter().take(4) {
        world.send_member(&MemberMessage::Wire(w)).unwrap();
    }
    let items = collect(&subscriber, |items| items.len() >= 4);
    assert_eq!(items.iter().map(|i| i.seq).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
    assert!(items[0].body.contains("25.00") && items[0].value("price") == Some("25.00"), "{:?}", items[0]);
    assert!(items[3].data.contains("fund=ACME:"), "the vendor passes the fundamentals on");
    assert_eq!(items[3].impact_bps, -80);
    drop(world);

    let (mut world, next) = source(news);
    assert_eq!(next, 5, "the source resumes after the last event the vendor took");
    for w in wires() {
        world.send_member(&MemberMessage::Wire(w)).unwrap();
    }
    let more = collect(&subscriber, |items| items.len() >= 2);
    assert_eq!(more.iter().map(|i| i.seq).collect::<Vec<_>>(), vec![5, 6], "resent events are not published twice");
    let s = status(&mut desk);
    assert_eq!((s.published, s.received, s.subscribers, s.sources, s.day), (6, 6, 1, 1, 2));

    let again = newsroom(Some(journal.clone()), None);
    let mut resumed = Client::connect(&again.to_string()).unwrap();
    resumed.logon_member("prime-co", "pw", 4).unwrap();
    let mut seqs = Vec::new();
    while let Ok(Some(message)) = resumed.recv_timeout(Duration::from_millis(300)) {
        if let ServerMessage::News(item) = message {
            seqs.push(item.seq);
        }
    }
    assert_eq!(seqs, vec![5, 6], "replay starts after the subscriber's last story");
    assert_eq!(source(again).1, 7);
    assert!(Client::connect_member(&again.to_string(), "prime-co", "wrong").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

fn fake_llm() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for (n, stream) in listener.incoming().enumerate() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let read = stream.read(&mut buf).unwrap_or(0);
                request.extend_from_slice(&buf[..read]);
                let text = String::from_utf8_lossy(&request);
                if let Some(split) = text.find("\r\n\r\n") {
                    let length: usize = text[..split]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= split + 4 + length {
                        break;
                    }
                }
                if read == 0 {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&request).to_string();
            let body = &text[text.find("\r\n\r\n").unwrap() + 4..];
            let payload: serde_json::Value = serde_json::from_str(body).unwrap();
            let content = payload["messages"][0]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let between = |from: &str, to: &str| {
                content
                    .split_once(from)
                    .and_then(|(_, rest)| rest.split_once(to))
                    .map(|(v, _)| v.trim().to_string())
                    .unwrap_or_default()
            };
            let reply = if content.is_empty() {
                serde_json::json!({"message": {"role": "assistant", "content": ""}, "done": true})
            } else if n % 3 == 2 {
                serde_json::json!({"message": {"role": "assistant", "content": "not json at all"}})
            } else {
                let copy = serde_json::json!({
                    "headline": format!("Wire: {}", between("Draft headline:", "\n")),
                    "body": between("Draft body:", "\nRules:"),
                });
                serde_json::json!({"message": {"role": "assistant", "content": copy.to_string()}, "done": true})
            };
            let reply = reply.to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    addr
}

#[test]
fn an_llm_rewrites_stories_in_order_and_bad_replies_fall_back_to_the_template() {
    let dir = std::env::temp_dir().join(format!("ome-newsroom-llm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let journal = dir.join("newsroom.journal");
    let _ = std::fs::remove_file(&journal);
    let llm = fake_llm();
    let config = ome_llm::LlmConfig { url: format!("http://{llm}"), model: "fake".into(), timeout: Duration::from_secs(2) };
    let news = newsroom(Some(journal.clone()), Some(config.clone()));
    let mut desk = ops(news);
    desk.request_member(&MemberMessage::CreateMember {
        kind: MemberKind::Broker,
        name: "prime-co".into(),
        secret: "pw".into(),
    })
    .unwrap();
    let mut subscriber = Client::connect(&news.to_string()).unwrap();
    subscriber.logon_member("prime-co", "pw", 0).unwrap();
    let (mut world, _) = source(news);
    for w in wires() {
        world.send_member(&MemberMessage::Wire(w)).unwrap();
    }
    let items = collect(&subscriber, |items| items.len() >= 6);
    let seqs: Vec<u64> = items.iter().map(|i| i.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4, 5, 6], "published strictly in order");
    let rewritten = items.iter().filter(|i| i.headline.starts_with("Wire: ")).count();
    assert!((3..=5).contains(&rewritten), "{rewritten} of 6 rewritten: {items:#?}");
    let writer = status(&mut desk).writer;
    assert!(writer.starts_with("llm fake") && writer.contains("fallback"), "{writer}");

    let again = newsroom(Some(journal.clone()), Some(config));
    let mut reader = ops(again);
    reader.send_member(&MemberMessage::QueryNews { limit: 10, symbol: None }).unwrap();
    let mut replayed = Vec::new();
    while let Ok(Some(message)) = reader.recv_timeout(Duration::from_millis(500)) {
        match message {
            ServerMessage::News(item) => replayed.push(item.headline),
            ServerMessage::Ack { .. } => break,
            _ => {}
        }
    }
    let expected: Vec<String> = items.iter().map(|i| i.headline.clone()).collect();
    assert_eq!(replayed, expected, "rewrites survive a restart");
    let _ = std::fs::remove_dir_all(&dir);
}
