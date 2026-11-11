#![cfg(test)]

#[path = "memory_review/endpoint.rs"]
mod endpoint;
#[path = "memory_review/marks.rs"]
mod marks;
#[path = "memory_review/places.rs"]
mod places;
#[path = "memory_review/spans.rs"]
mod spans;

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use cli::Exit;
use cli::Report;
use config::Environment;
use tempfile::TempDir;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;
use wire::message::Role;

use crate::support;

#[derive(serde::Deserialize)]
struct Ask {
    state: String,
}

#[derive(serde::Serialize)]
struct Answer {
    probabilities: [f64; 2],
}

#[derive(serde::Serialize)]
struct Info {
    model: &'static str,
}

#[derive(Clone, Default)]
struct Model {
    scores: Arc<Vec<(&'static str, f64)>>,
    exact: bool,
    asked: Arc<parking_lot::Mutex<Vec<String>>>,
}

impl Model {
    fn scoring(scores: &[(&'static str, f64)]) -> Self {
        Self {
            scores: Arc::new(scores.to_vec()),
            exact: false,
            asked: Arc::default(),
        }
    }

    fn exactly(scores: &[(&'static str, f64)]) -> Self {
        Self {
            exact: true,
            ..Self::scoring(scores)
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().clone()
    }
}

async fn decide(
    axum::extract::State(model): axum::extract::State<Model>,
    axum::Json(ask): axum::Json<Ask>,
) -> axum::Json<Answer> {
    let yes = model
        .scores
        .iter()
        .filter(|(needle, _)| {
            if model.exact {
                ask.state == *needle
            } else {
                ask.state.contains(needle)
            }
        })
        .map(|(_, score)| *score)
        .fold(0.05, f64::max);
    model.asked.lock().push(ask.state);
    axum::Json(Answer {
        probabilities: [1.0 - yes, yes],
    })
}

async fn info() -> axum::Json<Info> {
    axum::Json(Info {
        model: "stub-model",
    })
}

fn routes(model: Model) -> axum::Router {
    axum::Router::new()
        .route("/v1/decide", axum::routing::post(decide))
        .route("/v1/decide/info", axum::routing::get(info))
        .with_state(model)
}

async fn serve(model: &Model) -> testkit::Stub {
    testkit::stub_server(routes(model.clone()))
        .await
        .expect("stub model")
}

struct Node {
    _home: TempDir,
    directory: TempDir,
    environment: Environment,
}

impl Node {
    fn new(url: &str, top: usize) -> Self {
        let home = support::home();
        let directory = tempfile::tempdir().expect("directory");
        let users = directory.path().join("users.json");
        std::fs::write(
            &users,
            r#"{"ownerUserId":"phil","unmappedIsOwner":true,"users":{}}"#,
        )
        .expect("users file");
        support::config(
            &home,
            &format!("memory:\n  sanitize:\n    decide:\n      url: {url}\n      top: {top}\n"),
        );
        let environment = support::environment(&home)
            .with_override("RIVETHUB_USERS_FILE", users.display().to_string())
            .with_override("RIVETHUB_NODE_NAME", "desk");
        std::fs::write(directory.path().join("rules.toml"), "").expect("empty mapping");
        Self {
            _home: home,
            directory,
            environment,
        }
    }

    fn path(&self, name: &str) -> String {
        self.directory.path().join(name).display().to_string()
    }

    fn raw(&self, name: &str, sessions: &[&[&str]]) -> String {
        self.raw_sessions(
            name,
            sessions
                .iter()
                .enumerate()
                .map(|(index, contents)| {
                    session(index, contents.iter().map(|text| message(text)).collect())
                })
                .collect(),
        )
    }

    fn raw_sessions(&self, name: &str, sessions: Vec<wire::capture::BundledSession>) -> String {
        let path = self.path(name);
        std::fs::write(&path, sealed(sessions)).expect("raw bundle");
        path
    }

    async fn run(&self, parts: &[&str]) -> Report {
        support::run(parts, &self.environment).await
    }

    async fn sanitize(&self, raw: &str, mode: &[&str]) -> Sanitized {
        let out = raw.replace(".json", ".clean.json");
        let rules = self.path("rules.toml");
        let mut args = vec![
            "memory",
            "sanitize",
            raw,
            "--out",
            &out,
            "--mapping",
            &rules,
        ];
        args.extend_from_slice(mode);
        let report = self.run(&args).await;
        assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
        Sanitized {
            report: PathBuf::from(out.replace(".clean.json", ".clean.report.md")),
            out: PathBuf::from(out),
            stdout: report.stdout,
        }
    }

    async fn resolve(&self, sanitized: &Sanitized, extra: &[&str]) -> Report {
        let report = sanitized.report.display().to_string();
        let rules = self.path("rules.toml");
        let mut args = vec![
            "memory",
            "sanitize",
            "--resolve",
            &report,
            "--mapping",
            &rules,
        ];
        args.extend_from_slice(extra);
        self.run(&args).await
    }

    async fn ingest_line(&self, sanitized: &Sanitized) -> String {
        let out = sanitized.out.display().to_string();
        let ingest = self
            .run(&["memory", "ingest-bundle", &out, "--dry-run"])
            .await;
        assert_eq!(ingest.exit, Exit::Ok, "{}", ingest.stderr);
        ingest.stdout.lines().last().unwrap_or_default().to_owned()
    }
}

struct Sanitized {
    out: PathBuf,
    report: PathBuf,
    stdout: String,
}

impl Sanitized {
    fn report(&self) -> String {
        std::fs::read_to_string(&self.report).expect("report")
    }

    fn write_report(&self, text: &str) {
        std::fs::write(&self.report, text).expect("report written");
    }

    fn mark(&self, number: usize, mark: &str) {
        self.write_report(&marked(&self.report(), number, mark));
    }

    fn bundle_text(&self) -> String {
        std::fs::read_to_string(&self.out).expect("bundle")
    }

    fn bundle(&self) -> SessionBundle {
        bundle_at(&self.out)
    }

    fn contents(&self) -> Vec<Vec<String>> {
        self.bundle()
            .sessions
            .iter()
            .map(|session| {
                session
                    .batch
                    .messages
                    .iter()
                    .map(|message| message.content.clone())
                    .collect()
            })
            .collect()
    }

    fn unresolved(&self) -> Option<u32> {
        match self.bundle().manifest.sanitization {
            Sanitization::Sanitized(stamp) => Some(stamp.unresolved_flags),
            Sanitization::Raw => None,
        }
    }
}

fn marked(report: &str, number: usize, mark: &str) -> String {
    let heading = format!("### Flag {number} ");
    let mut inside = false;
    let mut done = false;
    report
        .split('\n')
        .map(|line| {
            if line.starts_with("### Flag ") {
                inside = line.starts_with(&heading);
            }
            if inside && !done && line == "Mark: open" {
                done = true;
                return mark.to_owned();
            }
            line.to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn bundle_at(path: &Path) -> SessionBundle {
    capture::bundle::parse(&std::fs::read_to_string(path).expect("bundle file")).expect("bundle")
}

fn message(content: &str) -> wire::capture::CaptureMessage {
    wire::capture::CaptureMessage {
        event_id: String::new(),
        role: Role::Assistant,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
    }
}

fn session(
    index: usize,
    messages: Vec<wire::capture::CaptureMessage>,
) -> wire::capture::BundledSession {
    let key = format!("claude-code:s{index}");
    wire::capture::BundledSession {
        origin: wire::capture::SessionOrigin {
            author: "phil".to_owned(),
            node: "desk".to_owned(),
            conversation_id: format!("c{index}"),
            session_key: key.clone(),
            agent: "rivet".to_owned(),
            created_at: None,
            updated_at: None,
            identity: None,
        },
        batch: wire::capture::CaptureBatchRequest {
            session_key: key,
            agent: "rivet".to_owned(),
            channel: None,
            title: None,
            settings: None,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages: messages
                .into_iter()
                .enumerate()
                .map(|(at, message)| wire::capture::CaptureMessage {
                    event_id: format!("evt-{index}-{at}"),
                    ..message
                })
                .collect(),
        },
    }
}

fn sealed(sessions: Vec<wire::capture::BundledSession>) -> Vec<u8> {
    capture::bundle::seal(
        wire::capture::BundleAuthor {
            user: "phil".to_owned(),
            node: "desk".to_owned(),
        },
        chrono::DateTime::UNIX_EPOCH,
        Sanitization::Raw,
        sessions,
    )
    .expect("seal")
    .text
}
