use sqlx::{AssertSqlSafe, PgConnection, PgPool};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CATALOG: &str = "
    INSERT INTO rh_topics (id, kind, slug, name, owner_user_id, created_at, updated_at)
    SELECT gen_random_uuid(), 'entity', 'transfer-plan-' || n, 'Transfer plan ' || n, 'owner', now(), now()
    FROM generate_series(1, 25000) n;
    INSERT INTO rh_wiki_topics (slug, title, owner_user_id)
    SELECT 'transfer-plan-' || n, 'Transfer plan ' || n, 'owner'
    FROM generate_series(1, 25000) n;
    ANALYZE rh_topics;
    ANALYZE rh_wiki_topics;
";

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn transfer_ownership_writes_use_indexed_keys(pool: PgPool) -> TestResult {
    let mut tx = pool.begin().await?;
    sqlx::raw_sql(CATALOG).execute(&mut *tx).await?;
    let old = explain(
        &mut tx,
        "DELETE FROM rh_wiki_topics WHERE slug = 'transfer-plan-1'",
    )
    .await?;
    let new = explain(
        &mut tx,
        "WITH retired AS (
            DELETE FROM rh_wiki_topics WHERE slug = 'transfer-plan-1' RETURNING owner_user_id
         )
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at, owner_user_id)
         SELECT 'transfer-plan-1', 'transfer-plan-2', now(),
             coalesce(rh_scope_principal(), owner_user_id) FROM retired
         ON CONFLICT (from_slug) DO UPDATE SET to_slug = EXCLUDED.to_slug",
    )
    .await?;
    for plan in [&old, &new] {
        assert!(plan.contains("Index Scan"), "{plan}");
        assert!(plan.contains("Index Cond: (slug = "), "{plan}");
        assert!(!plan.contains("Seq Scan"), "{plan}");
    }
    assert!(new.contains("Conflict Arbiter Indexes:"), "{new}");
    let owner = explain(
        &mut tx,
        "SELECT owner_user_id FROM rh_topics WHERE kind = 'entity' AND slug = 'transfer-plan-1'",
    )
    .await?;
    assert!(owner.contains("Index Scan"), "{owner}");
    assert!(owner.contains("Index Cond:"), "{owner}");
    for statement in [
        "INSERT INTO rh_topic_tombstones (kind, slug, deleted_at)
         VALUES ('entity', 'transfer-plan-1', now())
         ON CONFLICT (kind, slug) DO UPDATE SET deleted_at = EXCLUDED.deleted_at",
        "INSERT INTO rh_topic_tombstones (kind, slug, deleted_at)
         VALUES ('entity', 'transfer-plan-1', now())
         ON CONFLICT (kind, slug) DO UPDATE SET deleted_at = EXCLUDED.deleted_at,
             owner_user_id = EXCLUDED.owner_user_id",
        "INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at)
         VALUES ('transfer-plan-1', 'transfer-plan-2', now())
         ON CONFLICT (from_slug) DO UPDATE SET to_slug = EXCLUDED.to_slug, created_at = EXCLUDED.created_at",
        "INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at, owner_user_id)
         VALUES ('transfer-plan-1', 'transfer-plan-2', now(), coalesce(rh_scope_principal(), nullif('owner', '')))
         ON CONFLICT (from_slug) DO UPDATE SET to_slug = EXCLUDED.to_slug, created_at = EXCLUDED.created_at",
    ] {
        let plan = explain(&mut tx, statement).await?;
        assert!(plan.contains("Conflict Arbiter Indexes:"), "{plan}");
        assert!(!plan.contains("Seq Scan"), "{plan}");
    }
    tx.rollback().await?;
    Ok(())
}

async fn explain(conn: &mut PgConnection, statement: &str) -> Result<String, sqlx::Error> {
    sqlx::query("SAVEPOINT transfer_plan")
        .execute(&mut *conn)
        .await?;
    let rows: Vec<String> = sqlx::query_scalar(AssertSqlSafe(format!(
        "EXPLAIN (ANALYZE, BUFFERS) {statement}"
    )))
    .fetch_all(&mut *conn)
    .await?;
    sqlx::query("ROLLBACK TO SAVEPOINT transfer_plan")
        .execute(&mut *conn)
        .await?;
    sqlx::query("RELEASE SAVEPOINT transfer_plan")
        .execute(&mut *conn)
        .await?;
    Ok(rows.join("\n"))
}
