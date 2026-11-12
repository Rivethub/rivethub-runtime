use std::os::unix::fs::PermissionsExt as _;

use capture::writer::PostError;
use chrono::Utc;
use config::Environment;
use tokio::io::AsyncReadExt as _;

use crate::Exit;

const CONFIG: &str = "runtime:\n  workspace: /home/user/work\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    type: openai-compatible\n    base_url: http://127.0.0.1:1/v1\n    model: m\n    api_key: secret\n";
const TOOL: &str = r#"{"session_id":"55555555-5555-5555-5555-555555555555","cwd":"/work/demo","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"toolu_1"}"#;

enum Reply {
    None,
    Ok,
    Status(u16),
    Garbage,
    Hangup,
}

enum Token {
    Valid,
    Missing,
    Symlink,
    Shared,
    Fifo,
}

enum Prep {
    None,
    FullSpool,
    CorruptEntry,
    UnwritableSpool,
}

const CORRUPT: &[u8] = &[0xff, 0xfe, 0x00, 0x7b];

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Delivered,
    Spooled,
    DeadLettered,
    Lost,
    KeptEverything,
    DeliveredAroundCorrupt,
    NotSaved,
}

fn expected(error: &PostError) -> Outcome {
    match error {
        PostError::Unauthorized
        | PostError::Refused { .. }
        | PostError::Server { .. }
        | PostError::Throttled { .. }
        | PostError::Network { .. } => Outcome::Spooled,
        PostError::Client { .. } => Outcome::DeadLettered,
    }
}

struct Row {
    name: &'static str,
    reply: Reply,
    token: Token,
    foreign: bool,
    prep: Prep,
    error: Option<PostError>,
    needle: &'static str,
}

#[expect(clippy::too_many_lines, reason = "one table, one loop")]
#[tokio::test]
async fn a_capture_batch_is_delivered_or_kept_and_a_foreign_listener_sees_nothing() {
    let rows = [
        Row {
            name: "delivered",
            reply: Reply::Ok,
            token: Token::Valid,
            foreign: false,
            prep: Prep::None,
            error: None,
            needle: "",
        },
        Row {
            name: "no token file",
            reply: Reply::Ok,
            token: Token::Missing,
            foreign: false,
            prep: Prep::None,
            error: None,
            needle: "",
        },
        Row {
            name: "port closed",
            reply: Reply::None,
            token: Token::Valid,
            foreign: false,
            prep: Prep::None,
            error: Some(PostError::Refused {
                reason: String::new(),
            }),
            needle: "is not running",
        },
        Row {
            name: "foreign listener",
            reply: Reply::Ok,
            token: Token::Valid,
            foreign: true,
            prep: Prep::None,
            error: Some(PostError::Refused {
                reason: String::new(),
            }),
            needle: "not owned by this user",
        },
        Row {
            name: "symlinked token",
            reply: Reply::Ok,
            token: Token::Symlink,
            foreign: false,
            prep: Prep::None,
            error: Some(PostError::Refused {
                reason: String::new(),
            }),
            needle: "local owner token",
        },
        Row {
            name: "mode 0640 token",
            reply: Reply::Ok,
            token: Token::Shared,
            foreign: false,
            prep: Prep::None,
            error: Some(PostError::Refused {
                reason: String::new(),
            }),
            needle: "readable by other users",
        },
        Row {
            name: "fifo token",
            reply: Reply::Ok,
            token: Token::Fifo,
            foreign: false,
            prep: Prep::None,
            error: Some(PostError::Refused {
                reason: String::new(),
            }),
            needle: "local owner token",
        },
        Row {
            name: "401",
            reply: Reply::Status(401),
            token: Token::Valid,
            foreign: false,
            prep: Prep::None,
            error: Some(PostError::Unauthorized),
            needle: "401",
        },
        Row {
            name: "400",
            reply: Reply::Status(400),
            token: Token::Valid,
            foreign: false,
            prep: Prep::None,
            error: Some(PostError::Client { status: 400 }),
            needle: "HTTP 400",
        },
        Row {
            name: "408",
            reply: Reply::Status(408),
            token: Token::Valid,
            foreign: false,
            prep: Prep::None,
            error: Some(PostError::Throttled {
                status: 408,
                retry_after_seconds: None,
            }),
            needle: "HTTP 408",
        },
        Row {
            name: "429 with Retry-After",
            reply: Reply::Status(429),
            token: Token::Valid,
            foreign: false,
            prep: Prep::None,
            error: Some(PostError::Throttled {
                status: 429,
                retry_after_seconds: None,
            }),
            needle: "HTTP 429",
        },
        Row {
            name: "500",
            reply: Reply::Status(500),
            token: Token::Valid,
            foreign: false,
            prep: Prep::None,
            error: Some(PostError::Server { status: 500 }),
            needle: "HTTP 500",
        },
        Row {
            name: "connection dropped",
            reply: Reply::Hangup,
            token: Token::Valid,
            foreign: false,
            prep: Prep::None,
            error: Some(PostError::Network {
                message: String::new(),
            }),
            needle: "capture request failed",
        },
        Row {
            name: "200 with a garbage body",
            reply: Reply::Garbage,
            token: Token::Valid,
            foreign: false,
            prep: Prep::None,
            error: Some(PostError::Network {
                message: String::new(),
            }),
            needle: "capture request failed",
        },
        Row {
            name: "1,000 queued and the gateway down",
            reply: Reply::None,
            token: Token::Valid,
            foreign: false,
            prep: Prep::FullSpool,
            error: Some(PostError::Refused {
                reason: String::new(),
            }),
            needle: "is not running",
        },
        Row {
            name: "corrupt entry queued, gateway up",
            reply: Reply::Ok,
            token: Token::Valid,
            foreign: false,
            prep: Prep::CorruptEntry,
            error: None,
            needle: "",
        },
        Row {
            name: "corrupt entry queued, gateway down",
            reply: Reply::None,
            token: Token::Valid,
            foreign: false,
            prep: Prep::CorruptEntry,
            error: Some(PostError::Refused {
                reason: String::new(),
            }),
            needle: "is not running",
        },
        Row {
            name: "disk full",
            reply: Reply::None,
            token: Token::Valid,
            foreign: false,
            prep: Prep::UnwritableSpool,
            error: Some(PostError::Refused {
                reason: String::new(),
            }),
            needle: "NOT saved",
        },
    ];
    for Row {
        name,
        reply,
        token: token_file,
        foreign,
        prep,
        error,
        needle,
    } in rows
    {
        let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = match reply {
            Reply::None => {
                drop(listener);
                None
            }
            Reply::Hangup => {
                let seen = std::sync::Arc::clone(&seen);
                Some(tokio::spawn(async move {
                    loop {
                        let Ok((mut stream, _)) = listener.accept().await else {
                            break;
                        };
                        let mut buffer = vec![0; 65536];
                        let read = stream.read(&mut buffer).await.unwrap_or(0);
                        seen.lock().push(
                            String::from_utf8_lossy(buffer.get(..read).unwrap_or_default())
                                .into_owned(),
                        );
                    }
                }))
            }
            Reply::Ok | Reply::Status(_) | Reply::Garbage => {
                let seen = std::sync::Arc::clone(&seen);
                let canned = match reply {
                    Reply::Ok => (
                        axum::http::StatusCode::OK,
                        [("retry-after", "120")],
                        r#"{"ok":true,"conversation_id":"c","inserted":1,"skipped":0}"#,
                    ),
                    Reply::Garbage => (
                        axum::http::StatusCode::OK,
                        [("retry-after", "120")],
                        "not json",
                    ),
                    Reply::Status(code) => (
                        axum::http::StatusCode::from_u16(code).expect("status"),
                        [("retry-after", "120")],
                        "",
                    ),
                    Reply::None | Reply::Hangup => unreachable!(),
                };
                let app = axum::Router::new().fallback(
                    move |headers: axum::http::HeaderMap, body: String| {
                        seen.lock().push(format!("{headers:?} {body}"));
                        async move { canned }
                    },
                );
                Some(tokio::spawn(async move {
                    axum::serve(listener, app).await.expect("serve");
                }))
            }
        };
        let home = tempfile::tempdir().expect("home");
        let env = Environment::from_vars([("HOME".as_ref(), home.path().as_os_str())]);
        let state = gateway::state_dir(&env).expect("state");
        std::fs::create_dir_all(&state).expect("state dir");
        let path = gateway::local_owner::token_path(&state);
        let secret = gateway::local_owner::OwnerToken::generate().expect("token");
        let write = |mode| {
            std::fs::write(&path, secret.expose()).expect("token file");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        };
        match token_file {
            Token::Valid => write(0o600),
            Token::Shared => write(0o640),
            Token::Missing => {}
            Token::Symlink => {
                let elsewhere = home.path().join("elsewhere");
                std::fs::write(&elsewhere, secret.expose()).expect("target");
                std::os::unix::fs::symlink(&elsewhere, &path).expect("symlink");
            }
            Token::Fifo => {
                nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRUSR).expect("fifo");
            }
        }
        std::fs::write(
            home.path().join(".rivethub/config.yaml"),
            format!("{CONFIG}gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
        )
        .expect("config");
        let spool = state.join("capture-spool");
        let queued: Vec<std::path::PathBuf> = match prep {
            Prep::FullSpool => (0..1000_u64)
                .map(|index| spool.join(format!("{}-{index:032x}.json", 1_000_000 + index)))
                .collect(),
            Prep::CorruptEntry => vec![spool.join("1-corrupt.json")],
            Prep::None | Prep::UnwritableSpool => Vec::new(),
        };
        if matches!(prep, Prep::UnwritableSpool) {
            std::fs::write(&spool, "").expect("a file where the spool dir should be");
        } else if !matches!(prep, Prep::None) {
            std::fs::create_dir_all(&spool).expect("spool dir");
        }
        for entry in &queued {
            let bytes: &[u8] = if matches!(prep, Prep::CorruptEntry) {
                CORRUPT
            } else {
                br#"{"session_key":"old"}"#
            };
            std::fs::write(entry, bytes).expect("queued entry");
        }
        let uid = nix::unistd::getuid().as_raw();
        let uid = if foreign { uid.wrapping_add(1) } else { uid };
        let report = super::hook_as(&env, TOOL, Utc::now(), uid).await;
        if let Some(server) = server {
            server.abort();
        }
        let files = |dir: std::path::PathBuf| {
            std::fs::read_dir(dir).map_or(0, |entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        entry.path().is_file()
                            && entry.path().extension().is_some_and(|ext| ext == "json")
                    })
                    .count()
            })
        };
        let seen = seen.lock().clone();
        let counts = (files(spool.clone()), files(spool.join("dead")), seen.len());
        let observed = match (&prep, counts) {
            (Prep::FullSpool, (1001, 0, _)) if queued.iter().all(|path| path.is_file()) => {
                Outcome::KeptEverything
            }
            (Prep::CorruptEntry, (1, 1, _)) | (Prep::None, (1, 0, _)) => Outcome::Spooled,
            (Prep::CorruptEntry, (0, 1, 1))
                if std::fs::read(spool.join("dead/1-corrupt.json"))
                    .ok()
                    .as_deref()
                    == Some(CORRUPT) =>
            {
                Outcome::DeliveredAroundCorrupt
            }
            (Prep::UnwritableSpool, (0, 0, 0)) if report.exit == Exit::Failure => Outcome::NotSaved,
            (Prep::None, (0, 1, _)) => Outcome::DeadLettered,
            (Prep::None, (0, 0, 1)) => Outcome::Delivered,
            _ => Outcome::Lost,
        };
        let want = match prep {
            Prep::FullSpool => Outcome::KeptEverything,
            Prep::UnwritableSpool => Outcome::NotSaved,
            Prep::CorruptEntry if error.is_none() => Outcome::DeliveredAroundCorrupt,
            Prep::CorruptEntry | Prep::None => error.as_ref().map_or(Outcome::Delivered, expected),
        };
        let wanted_exit = if matches!(prep, Prep::UnwritableSpool) {
            Exit::Failure
        } else {
            Exit::Ok
        };
        assert_eq!(report.exit, wanted_exit, "{name}: {}", report.stderr);
        assert_eq!(observed, want, "{name}: {}", report.stderr);
        assert!(report.stderr.contains(needle), "{name}: {}", report.stderr);
        let refused = matches!(error, Some(PostError::Refused { .. }));
        if refused {
            assert!(seen.is_empty(), "{name}: a request left the hook");
        }
        if refused || matches!(token_file, Token::Missing) {
            assert!(
                seen.iter()
                    .all(|request| !request.contains(secret.expose())),
                "{name}: the token was sent"
            );
        }
        if matches!(token_file, Token::Missing) {
            assert!(
                seen.iter()
                    .all(|request| !request.contains("authorization")),
                "{name}: an Authorization header was sent"
            );
        }
        if name == "429 with Retry-After" {
            let held = std::fs::read_dir(&spool)
                .expect("spool")
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let name = entry.file_name().into_string().ok()?;
                    name.strip_suffix(".json")?
                        .rsplit('.')
                        .next()?
                        .parse::<i64>()
                        .ok()
                })
                .max()
                .expect("a held batch");
            assert!(
                held >= (Utc::now() + chrono::TimeDelta::seconds(100)).timestamp_millis(),
                "{name}: the batch is not held for the Retry-After"
            );
        }
        if name == "delivered" {
            let bearer = format!("Bearer {}", secret.expose());
            assert!(
                seen.first()
                    .is_some_and(|request| request.contains(&bearer)),
                "{name}: the owner token is not sent"
            );
        }
        if refused {
            assert!(
                seen.iter().all(|request| !request.contains("toolu_1")),
                "{name}: the payload was sent"
            );
        }
    }
}
