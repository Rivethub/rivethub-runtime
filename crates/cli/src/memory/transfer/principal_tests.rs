use kernel::owner::Owner;
use memory_sqlite::SqliteError;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use super::{TestResult, authorization, store};
use crate::memory::transfer::{Direction, run};

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn transfer_remaps_decision_principals_in_both_directions(pool: PgPool) -> TestResult {
    let dir = tempfile::tempdir()?;
    let source = store(&dir, "source.sqlite")?;
    let output = store(&dir, "output.sqlite")?;
    source
        .with_conn(|conn| {
            let fixture = crate::memory::transfer::fixture::MEMORY.replace("'alice'", "'you'");
            conn.execute_batch(&fixture).map_err(SqliteError::Pragma)?;
            conn.execute("UPDATE ros_tags SET decided_by = 'you'", [])
                .map_err(SqliteError::Pragma)?;
            Ok(())
        })
        .await?;
    authorization(&pool).await?;
    let owner = Owner::new("alice".into())?;
    let local_owner = Owner::new("you".into())?;
    let token = CancellationToken::new();
    run(
        &pool,
        &source,
        [&owner, &local_owner],
        Direction::ToPostgres,
        (false, None),
        &token,
    )
    .await?;
    let principals: Vec<String> = sqlx::query_scalar(
        "SELECT decided_by FROM rh_topic_decisions UNION ALL SELECT decided_by FROM rh_tags
         UNION ALL SELECT placed_by FROM rh_session_placements
         UNION ALL SELECT placed_by FROM rh_conversation_placements
         UNION ALL SELECT actor FROM rh_entity_fact_claims",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(principals, ["alice", "alice", "you", "you", "you"]);
    let export_owner = Owner::new("standalone".into())?;
    run(
        &pool,
        &output,
        [&owner, &export_owner],
        Direction::ToSqlite,
        (false, None),
        &token,
    )
    .await?;
    let principals = output.with_conn(|conn| {
        conn.prepare("SELECT decided_by FROM rh_topic_decisions UNION ALL SELECT decided_by FROM ros_tags")
            .and_then(|mut stmt| stmt.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>())
            .map_err(SqliteError::Pragma)
    }).await?;
    assert_eq!(principals, ["standalone", "standalone"]);
    Ok(())
}
