use super::*;

#[test]
fn jev_last_option_is_new_and_a_missing_probability_holds() {
    let chosen = read_jev(
        &JevAnswer {
            probabilities: Some(vec![0.2, 0.8]),
            choice_index: Some(1),
            options: Vec::new(),
        },
        2,
    );
    assert_eq!(chosen.choice, "new");
    assert!((chosen.confidence - 0.8).abs() < 0.001);
    let missing = read_jev(
        &JevAnswer {
            probabilities: None,
            choice_index: Some(0),
            options: Vec::new(),
        },
        2,
    );
    let yes = read_noul(&JevAnswer {
        probabilities: Some(vec![0.19, 0.81]),
        choice_index: Some(1),
        options: vec!["false".to_owned(), "true".to_owned()],
    });
    assert!((yes - 0.81).abs() < 0.001);
    let no = read_noul(&JevAnswer {
        probabilities: Some(vec![0.7, 0.3]),
        choice_index: Some(0),
        options: vec!["false".to_owned(), "true".to_owned()],
    });
    assert!((no - 0.3).abs() < 0.001);
    assert_eq!(
        read_noul(&JevAnswer {
            probabilities: Some(vec![0.2, 0.8]),
            choice_index: Some(1),
            options: Vec::new(),
        }),
        0.0
    );
    assert_eq!(missing.confidence, 0.0);
    assert_eq!(
        MatcherSettings::selected("thinkingcap", "")
            .unwrap()
            .backend,
        MatchBackend::ThinkingCap
    );
    assert!(MatcherSettings::selected("other", "").is_err());
}

#[test]
fn matcher_settings_follow_the_entity_config() {
    let configured = config::Config {
        entities: Some(config::EntitiesSection {
            matcher: Some("jev".to_owned()),
            matcher_url: Some("http://host:8015".to_owned()),
        }),
        memory: Some(config::model::MemorySection {
            postgres: Some(config::model::PostgresSection {
                embed_endpoint: Some("http://embed:9402".to_owned()),
                embed_model: Some("custom-embed".to_owned()),
                ..config::model::PostgresSection::default()
            }),
            ..config::model::MemorySection::default()
        }),
        ..config::Config::default()
    };
    let settings = MatcherSettings::from_config(&configured).expect("configured matcher");
    assert_eq!(settings.backend, MatchBackend::Jev);
    assert_eq!(settings.matcher_url, "http://host:8015");
    assert_eq!(settings.embed_url, "http://embed:9402");
    assert_eq!(settings.embed_model, "custom-embed");
    assert_eq!(
        settings.threshold,
        kernel::wiki::entity_match::MATCH_THRESHOLD
    );

    let sqlite = config::Config {
        memory: Some(config::model::MemorySection {
            backend: Some(config::MemoryBackend::Sqlite),
            sqlite: Some(config::model::SqliteSection {
                embed_endpoint: Some(" http://sqlite-embed:9 ".to_owned()),
                embed_model: Some(" sqlite-model ".to_owned()),
                ..config::model::SqliteSection::default()
            }),
            postgres: Some(config::model::PostgresSection {
                embed_endpoint: Some("http://ignored:1".to_owned()),
                embed_model: Some("ignored-model".to_owned()),
                ..config::model::PostgresSection::default()
            }),
            ..config::model::MemorySection::default()
        }),
        ..config::Config::default()
    };
    let settings = MatcherSettings::from_config(&sqlite).expect("sqlite embedder");
    assert_eq!(settings.backend, MatchBackend::ThinkingCap);
    assert_eq!(settings.matcher_url, "");
    assert_eq!(settings.embed_url, "http://sqlite-embed:9");
    assert_eq!(settings.embed_model, "sqlite-model");

    let blank = config::Config {
        memory: Some(config::model::MemorySection {
            postgres: Some(config::model::PostgresSection {
                embed_endpoint: Some("  ".to_owned()),
                embed_model: Some(String::new()),
                ..config::model::PostgresSection::default()
            }),
            ..config::model::MemorySection::default()
        }),
        ..config::Config::default()
    };
    assert_eq!(
        MatcherSettings::from_config(&blank).expect("blank embedder"),
        MatcherSettings::default()
    );
    assert_eq!(
        MatcherSettings::from_config(&config::Config::default()).expect("defaults"),
        MatcherSettings::default()
    );
    assert_eq!(
        MatcherSettings::from_config(&config::Config {
            entities: Some(config::EntitiesSection {
                matcher: Some("jev".to_owned()),
                matcher_url: None,
            }),
            ..config::Config::default()
        })
        .expect_err("jev needs a url"),
        "entities.matcher jev needs matcher_url"
    );
}

#[tokio::test]
async fn an_empty_jev_url_does_not_decide() {
    let decider = JevDecider {
        base_url: String::new(),
    };
    let error = decider
        .decide(&DecideRequest {
            observation: "name: gpubox".to_owned(),
            options: vec!["gpubox (Hosts)".to_owned()],
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("empty"));
}

#[tokio::test]
async fn jev_decide_reads_choice_index_and_probability() {
    let body = r#"{"probabilities":[0.91,0.09],"choice":"gpubox","choice_index":0}"#;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { serve_json(listener, body).await });
    let decider = JevDecider {
        base_url: format!("http://{address}"),
    };
    let decision = tokio::time::timeout(
        Duration::from_secs(5),
        decider.decide(&DecideRequest {
            observation: "name: gpubox\ntype: Hosts".to_owned(),
            options: vec!["gpubox (Hosts) aliases: ".to_owned()],
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(decision.choice, "0");
    assert!((decision.confidence - 0.91).abs() < 0.001);
    server.await.unwrap();
}

async fn serve_json(listener: tokio::net::TcpListener, body: &str) {
    use tokio::io::AsyncWriteExt as _;
    let (mut socket, _) = listener.accept().await.unwrap();
    read_http(&mut socket).await;
    let header = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(header.as_bytes()).await.unwrap();
    socket.write_all(body.as_bytes()).await.unwrap();
}

async fn read_http(socket: &mut tokio::net::TcpStream) {
    use tokio::io::AsyncReadExt as _;
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 2048];
    for _ in 0..8 {
        let n = socket.read(&mut tmp).await.unwrap();
        if n == 0 {
            return;
        }
        let Some(chunk) = tmp.get(..n) else {
            return;
        };
        buf.extend_from_slice(chunk);
        let Some(end) = buf.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        if buf.len() >= end.saturating_add(4).saturating_add(body_len(&buf, end)) {
            return;
        }
    }
}

fn body_len(buf: &[u8], end: usize) -> usize {
    let Some(headers) = buf.get(..end) else {
        return 0;
    };
    String::from_utf8_lossy(headers)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0)
}
