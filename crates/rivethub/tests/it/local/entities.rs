use std::path::Path;
use std::time::Duration;

use kernel::wiki::entity_pass::{
    EntityIdentification, EntityIdentifications, EntityRewrite, RewriteFact,
};
use rusqlite::{Connection, OptionalExtension as _};
use wire::gateway::memory_models::{ModelHealth, ModelJob, ModelRole};

use crate::process::TestResult;

pub(super) const STAMP: &str = "2026-10-10T12:00:00.000Z";
const MENTION: &str = "The tinbox host has 152 GB of RAM.";
const SENTENCE: &str = "Confirmed tinbox has 152 GB of RAM.";

pub(super) fn seed_message(conn: &Connection, conversation: &str, summary: &str) -> TestResult {
    let message = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, created_at, owner_user_id)
         VALUES (?1, ?2, 'rivet', 'claude-code', 'user', ?3, ?4, 'owner')",
        (&message, conversation, MENTION, STAMP),
    )?;
    conn.execute(
        "INSERT INTO ros_summary_sources (summary_id, message_id) VALUES (?1, ?2)",
        (summary, &message),
    )?;
    Ok(())
}

pub(super) fn response(text: &str) -> Result<Option<String>, serde_json::Error> {
    if text.contains("Identify every durable person") {
        let observations = EntityIdentifications {
            observations: vec![EntityIdentification {
                entity_ref: None,
                name: "tinbox".to_owned(),
                type_name: "Hosts".to_owned(),
                aliases: Vec::new(),
                mention: MENTION.to_owned(),
            }],
        };
        return serde_json::to_string(&observations).map(Some);
    }
    if text.contains("Rewrite one entity page") {
        let rewrite = EntityRewrite {
            summary: "tinbox is a host with 152 GB of RAM.".to_owned(),
            article: MENTION.to_owned(),
            current_facts: vec![RewriteFact {
                attribute: "ram".to_owned(),
                value: "152 GB".to_owned(),
                as_of: STAMP.to_owned(),
            }],
            history_sentence: SENTENCE.to_owned(),
            retired: Vec::new(),
        };
        return serde_json::to_string(&rewrite).map(Some);
    }
    Ok(None)
}

pub(super) async fn assert_built(home: &Path, database: &Path, port: u16) -> TestResult {
    let ready = async {
        let mut checks = tokio::time::interval(Duration::from_millis(50));
        loop {
            checks.tick().await;
            if let Some(page) = written(database)? {
                return Ok::<_, rusqlite::Error>(page);
            }
        }
    };
    let (state, article, history) = tokio::time::timeout(Duration::from_secs(10), ready).await??;
    assert_eq!(state, "tinbox is a host with 152 GB of RAM.");
    assert!(article.contains("**ram**: 152 GB"), "{article}");
    assert_eq!(history, SENTENCE);
    let client = reqwest::Client::builder()
        .no_proxy()
        .default_headers(crate::owner::home_headers(home)?)
        .build()?;
    let models = client
        .get(format!("http://127.0.0.1:{port}/api/memory/models"))
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<ModelJob>>()
        .await?;
    let entity = models
        .iter()
        .find(|job| job.role == ModelRole::EntityBuilder)
        .ok_or("entity model missing")?;
    let wiki = models
        .iter()
        .find(|job| job.role == ModelRole::WikiWriter)
        .ok_or("wiki model missing")?;
    assert_eq!(entity.provider.as_deref(), Some("p"));
    assert_eq!(entity.model.as_deref(), Some("m"));
    assert_eq!(
        (&entity.provider, &entity.model),
        (&wiki.provider, &wiki.model)
    );
    assert_eq!(entity.health, ModelHealth::Healthy);
    Ok(())
}

fn written(database: &Path) -> rusqlite::Result<Option<(String, String, String)>> {
    Connection::open(database)?
        .query_row(
            "SELECT w.current_state, w.article, h.sentence FROM ros_wiki_topics w
         JOIN rh_topic_pages p ON p.page_slug = w.slug
         JOIN rh_entity_history h ON h.entity_id = p.topic_id
         JOIN rh_entity_extractions e ON e.summary_id = h.summary_id AND e.status = 'done'
         WHERE w.slug = 'tinbox'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
}
