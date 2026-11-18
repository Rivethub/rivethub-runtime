use super::*;
use jiff::Timestamp;

pub(crate) async fn seed_fts() -> TestResult<Fresh> {
    let fresh = fresh_store()?;
    let now = instant(common::RECORDED_START)?;
    let deployed = fresh
        .store
        .capture(
            deploy_batch("cli:sess-1", "rivet")?,
            &testkit::owner()?,
            None,
            now,
        )
        .await?;
    let helper = fresh
        .store
        .capture(helper_batch()?, &testkit::owner()?, None, now)
        .await?;
    fresh
        .store
        .capture(rollback_batch()?, &testkit::owner()?, None, now)
        .await?;
    let sess1 = deployed.conversation_id;
    let sess2 = helper.conversation_id;
    fresh
        .store
        .with_conn(move |conn| seed_rows(conn, &sess1, &sess2))
        .await?;
    Ok(fresh)
}

fn helper_batch() -> TestResult<CaptureBatchRequest> {
    let mut note = message(
        "h-note",
        Role::User,
        "notes about the staging database migration on db.example.com",
    );
    note.created_at = Some(instant("2026-10-10T12:01:00.000Z")?);
    let mut helper = batch("cli:sess-2", "helper", "cli");
    helper.messages = vec![
        note,
        message("h-fill", Role::User, &filler_release()),
        message("h-empty-1", Role::User, ""),
        message("h-empty-2", Role::User, ""),
    ];
    Ok(helper)
}

fn filler_release() -> String {
    let mut words: Vec<String> = (0..30).map(|index| format!("zz{index}")).collect();
    words.push("release".to_owned());
    words.join(" ")
}

fn rollback_batch() -> TestResult<CaptureBatchRequest> {
    let mut user = message(
        "rb-user",
        Role::User,
        "rollback rollback rollback procedure notes",
    );
    user.created_at = Some(instant("2026-09-01T00:00:00.000Z")?);
    let mut assistant = message(
        "rb-assistant",
        Role::Assistant,
        "the rollback switches traffic back to the previous colour",
    );
    assistant.created_at = Some(instant("2026-10-09T00:00:00.000Z")?);
    let mut rollback = batch("cli:sess-3", "rivet", "cli");
    rollback.messages = vec![user, assistant];
    Ok(rollback)
}

fn seed_rows(conn: &rusqlite::Connection, sess1: &str, sess2: &str) -> Result<(), SqliteError> {
    let stamp = common::RECORDED_START;
    conn.execute(
        "INSERT INTO ros_summaries (id, conversation_id, content, kind, created_at)
         VALUES (?1, ?2, ?3, 'leaf', '2026-10-10T12:00:30.000Z')",
        params![LEAF_ID, sess1, LEAF],
    )
    .map_err(|source| SqliteError::Query {
        operation: "seed leaf",
        source,
    })?;
    conn.execute(
        "INSERT INTO ros_summaries (id, conversation_id, content, kind, created_at)
         VALUES (?1, NULL, ?2, 'branch', '2026-10-10T12:00:40.000Z')",
        params![BRANCH_ID, BRANCH],
    )
    .map_err(|source| SqliteError::Query {
        operation: "seed branch",
        source,
    })?;
    insert_tag(
        conn,
        "tag-acme",
        "conversation",
        sess1,
        ("project", "acmekit"),
        stamp,
    )?;
    insert_tag(
        conn,
        "tag-infra",
        "conversation",
        sess2,
        ("project", "infra"),
        stamp,
    )?;
    insert_tag(
        conn,
        "tag-topic",
        "summary",
        LEAF_ID,
        ("topic", "deploys"),
        stamp,
    )?;
    Ok(())
}

fn insert_tag(
    conn: &rusqlite::Connection,
    id: &str,
    entity_type: &str,
    entity_id: &str,
    tag: (&str, &str),
    stamp: &str,
) -> Result<(), SqliteError> {
    conn.execute(
        "INSERT INTO ros_tags
            (id, entity_type, entity_id, key, value, source, state, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'user', 'accepted', ?6, ?6)",
        params![id, entity_type, entity_id, tag.0, tag.1, stamp],
    )
    .map_err(|source| SqliteError::Query {
        operation: "seed tag",
        source,
    })?;
    Ok(())
}

pub(crate) async fn seed_tag_pool(store: &SqliteStore, now: Timestamp) -> TestResult {
    let tagged = store
        .capture(
            one_message("tagged", TAGGED_NOTE),
            &testkit::owner()?,
            None,
            now,
        )
        .await?;
    store
        .with_conn({
            let conversation = tagged.conversation_id.clone();
            move |conn| {
                insert_tag(
                    conn,
                    "tag-rare",
                    "conversation",
                    &conversation,
                    ("project", "rare"),
                    POOL_CLOCK,
                )
            }
        })
        .await?;
    for index in 0..20 {
        store
            .capture(
                one_message(&format!("short-{index}"), "rollback"),
                &testkit::owner()?,
                None,
                now,
            )
            .await?;
    }
    for index in 0..20 {
        let pad = (0..9)
            .map(|word| format!("pad{index}x{word}"))
            .collect::<Vec<_>>()
            .join(" ");
        store
            .capture(
                one_message(&format!("pad-{index}"), &pad),
                &testkit::owner()?,
                None,
                now,
            )
            .await?;
    }
    Ok(())
}

fn one_message(session_key: &str, content: &str) -> CaptureBatchRequest {
    let mut request = batch(session_key, "rivet", "cli");
    request.messages = vec![message(session_key, Role::User, content)];
    request
}
