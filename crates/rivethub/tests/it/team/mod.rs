mod addresses;
mod messages;
mod no_model;
mod sessions;

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::sync::Arc;
use std::time::Duration;

use config::{Config, Environment};
use rivethub::composition::Composition;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use sqlx::ConnectOptions as _;
use tokio_util::sync::CancellationToken;
use wire::capture::{CaptureBatchRequest, CaptureMessage};
use wire::gateway::tenancy::{SpaceConfig, SpaceKind, UserSettingsUpdate};
use wire::gateway::users::{UserRecord, UserToken, UsersRegistry};
use wire::message::Role;

pub(crate) type TestResult = Result<(), Box<dyn std::error::Error>>;

pub(crate) const PHIL: &str = "phil";
pub(crate) const ALEX: &str = "alex";
pub(crate) const TOM: &str = "tom";
pub(crate) const SPACE: &str = "team";

pub(crate) struct Hub {
    pub(crate) base: String,
    pub(crate) db: crate::pgdb::TestDb,
    shutdown: CancellationToken,
    server: tokio::task::JoinHandle<Result<(), gateway::server::ServeError>>,
    composition: Option<Composition>,
    _home: tempfile::TempDir,
}

pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: Value,
}

fn token(person: &str) -> String {
    format!("{person}-team-token")
}

fn users() -> UsersRegistry {
    let users = [PHIL, ALEX, TOM]
        .into_iter()
        .map(|person| {
            let digest = hex::encode(Sha256::digest(token(person).as_bytes()));
            (
                person.to_owned(),
                UserRecord {
                    id: None,
                    devices: None,
                    tokens: Some(vec![UserToken::Digest(digest)]),
                    pg_url: None,
                    env_file: None,
                    persona: None,
                    admin: None,
                },
            )
        })
        .collect();
    UsersRegistry {
        owner_user_id: PHIL.to_owned(),
        unmapped_is_owner: false,
        local_stores: None,
        users,
        spaces: Some(BTreeMap::from([(
            SPACE.to_owned(),
            SpaceConfig {
                kind: SpaceKind::Team,
                name: Some("Team".to_owned()),
                members: vec![PHIL.to_owned(), ALEX.to_owned(), TOM.to_owned()],
            },
        )])),
    }
}

pub(crate) const SERVICE: &str = "rh_team_hub";

const SUPERUSER_PREP: [&str; 8] = [
    "CREATE EXTENSION IF NOT EXISTS vector",
    "CREATE EXTENSION IF NOT EXISTS pg_trgm",
    "ALTER FUNCTION ts_match_vq(tsvector, tsquery) LEAKPROOF",
    "ALTER FUNCTION textlike(text, text) LEAKPROOF",
    "ALTER FUNCTION texticlike(text, text) LEAKPROOF",
    "ALTER FUNCTION textregexeq(text, text) LEAKPROOF",
    "ALTER FUNCTION texticregexeq(text, text) LEAKPROOF",
    "ALTER FUNCTION jsonb_object_field_text(jsonb, text) LEAKPROOF",
];

async fn served_by_an_ordinary_role(
    db: &crate::pgdb::TestDb,
) -> Result<String, Box<dyn std::error::Error>> {
    let pool = db.pool();
    let database = pool
        .connect_options()
        .get_database()
        .ok_or("the test database has a name")?
        .to_owned();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DO $$ BEGIN CREATE ROLE {SERVICE} LOGIN NOSUPERUSER NOBYPASSRLS PASSWORD '{SERVICE}'; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
    )))
    .execute(&pool)
    .await?;
    for statement in SUPERUSER_PREP.into_iter().map(str::to_owned).chain([
        format!("ALTER DATABASE \"{database}\" OWNER TO {SERVICE}"),
        format!("ALTER SCHEMA public OWNER TO {SERVICE}"),
    ]) {
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(&pool)
            .await?;
    }
    let options = pool
        .connect_options()
        .as_ref()
        .clone()
        .username(SERVICE)
        .password(SERVICE);
    let url = options.to_url_lossy().to_string();
    migrate::postgres(&url).await?;
    Ok(url)
}

impl Hub {
    pub(crate) async fn start() -> Result<Self, Box<dyn std::error::Error>> {
        Self::start_with(memory_postgres::MissingModels::default()).await
    }

    pub(crate) async fn start_with(
        missing: memory_postgres::MissingModels,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let db = crate::pgdb::TestDb::bare().await?;
        let url = served_by_an_ordinary_role(&db).await?;
        let home = tempfile::tempdir()?;
        let users_file = home.path().join("users.json");
        tokio::fs::write(&users_file, serde_json::to_vec(&users())?).await?;
        let env = Environment::from_vars([
            (OsStr::new(config::POSTGRES_URL_VARIABLE), OsStr::new(&url)),
            (OsStr::new("HOME"), home.path().as_os_str()),
            (OsStr::new("RIVETHUB_SHARED_DIR"), home.path().as_os_str()),
        ]);
        let config: Config = serde_yaml_ng::from_str(&format!(
            "gateway:\n  host: 127.0.0.1\n  port: 0\n  users_file: {}\n",
            users_file.display()
        ))?;
        let memory = Arc::new(
            memory_postgres::PostgresMemory::connect(&url)
                .await?
                .with_missing_models(missing),
        );
        let shutdown = CancellationToken::new();
        let composition = Composition::on_postgres(
            &config,
            &env,
            &memory,
            &shutdown,
            &kernel::shutdown::Activities::new(),
        )
        .await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let server = tokio::spawn(gateway::server::serve(
            listener,
            Arc::clone(composition.gateway()),
            None,
            shutdown.clone(),
        ));
        let hub = Self {
            base: format!("http://127.0.0.1:{port}"),
            db,
            shutdown,
            server,
            composition: Some(composition),
            _home: home,
        };
        hub.wait_for_spaces().await?;
        Ok(hub)
    }

    async fn wait_for_spaces(&self) -> TestResult {
        for _ in 0..100 {
            let members: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_space_members")
                .fetch_one(&self.db.pool())
                .await?;
            if members == 3 {
                return Ok(());
            }
            #[cfg_attr(
                dylint_lib = "rr_lints",
                expect(
                    sleep,
                    reason = "the space sync is a background retry loop that signals nothing"
                )
            )]
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Err("the hub never saved its space".into())
    }

    pub(crate) async fn call(
        &self,
        person: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        let request = reqwest::Client::new()
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token(person));
        let request = match body {
            Some(body) => request.json(&body),
            None => request,
        };
        let response = request.send().await?;
        let status = response.status().as_u16();
        let text = response.text().await?;
        let body = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        Ok(Reply { status, body })
    }

    pub(crate) async fn get(
        &self,
        person: &str,
        path: &str,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.call(person, reqwest::Method::GET, path, None).await
    }

    pub(crate) async fn share_by_default(&self, person: &str) -> TestResult {
        let update = UserSettingsUpdate {
            default_visibility: SPACE.to_owned(),
        };
        let reply = self
            .call(
                person,
                reqwest::Method::PUT,
                "/api/me/settings",
                Some(body(&update)?),
            )
            .await?;
        assert_eq!(
            reply.status, 200,
            "{person} shares by default: {}",
            reply.body
        );
        Ok(())
    }

    pub(crate) async fn capture(&self, person: &str, session: &str, texts: &[&str]) -> TestResult {
        let messages = texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let role = if index % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                };
                message(session, index, role, text)
            })
            .collect::<Result<_, _>>()?;
        self.capture_messages(person, session, messages).await
    }

    pub(crate) async fn capture_messages(
        &self,
        person: &str,
        session: &str,
        messages: Vec<CaptureMessage>,
    ) -> TestResult {
        let batch = CaptureBatchRequest {
            session_key: session.to_owned(),
            agent: "rivet-claude".to_owned(),
            channel: Some("claude-code".to_owned()),
            title: None,
            settings: None,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages,
        };
        let reply = self
            .call(
                person,
                reqwest::Method::POST,
                "/api/capture",
                Some(body(&batch)?),
            )
            .await?;
        assert_eq!(
            reply.status, 200,
            "{person} captures {session}: {}",
            reply.body
        );
        Ok(())
    }

    pub(crate) async fn stop(mut self) -> TestResult {
        self.shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(10), &mut self.server).await???;
        if let Some(composition) = self.composition.take() {
            composition.close().await?;
        }
        Ok(())
    }
}

pub(crate) fn message(
    session: &str,
    index: usize,
    role: Role,
    text: &str,
) -> Result<CaptureMessage, std::num::TryFromIntError> {
    Ok(CaptureMessage {
        event_id: format!("{session}-{index}"),
        role,
        content: text.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: Some(
            chrono::Utc::now() + chrono::TimeDelta::milliseconds(i64::try_from(index)?),
        ),
    })
}

pub(crate) fn body<T: serde::Serialize>(value: &T) -> Result<Value, serde_json::Error> {
    serde_json::to_value(value)
}

pub(crate) fn segment(session: &str) -> Result<String, wire::session_id::SessionIdError> {
    Ok(wire::session_id::encode_segment(&session.parse()?))
}

pub(crate) fn listed(reply: &Reply) -> Vec<String> {
    reply
        .body
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect()
}
