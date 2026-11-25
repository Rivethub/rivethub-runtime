use sqlx::{AssertSqlSafe, PgConnection, PgPool};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CATALOG: &str = "
    INSERT INTO rh_topics (id, kind, slug, name, owner_user_id, created_at, updated_at)
    SELECT gen_random_uuid(), 'entity', 'transfer-plan-' || n, 'Transfer plan ' || n, 'owner', now(), now()
    FROM generate_series(1, 25000) n;
    INSERT INTO rh_wiki_topics (slug, title, owner_user_id)
    SELECT 'transfer-plan-' || n, 'Transfer plan ' || n, 'owner' FROM generate_series(1, 25000) n;
    INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id)
    SELECT 'transfer-alias-' || n, 'transfer-plan-2', NULL FROM generate_series(1, 25000) n;
    INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id)
    VALUES ('transfer-plan-1', 'transfer-plan-2', NULL);
    INSERT INTO rh_topic_tombstones (kind, slug, deleted_at, owner_user_id)
    SELECT 'entity', 'transfer-plan-' || n, now(), 'owner' FROM generate_series(1, 25000) n;
    INSERT INTO rh_topic_tombstones (kind, slug, deleted_at, owner_user_id)
    VALUES ('work', 'transfer-plan-1', now(), 'owner');
    ANALYZE rh_topics;
    ANALYZE rh_wiki_topics;
    ANALYZE rh_wiki_redirects;
    ANALYZE rh_topic_tombstones;
";

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn transfer_ownership_writes_use_indexed_keys(pool: PgPool) -> TestResult {
    let mut tx = pool.begin().await?;
    sqlx::raw_sql(CATALOG).execute(&mut *tx).await?;
    for statement in [
        "INSERT INTO rh_topic_tombstones (kind, slug, deleted_at, owner_user_id)
         VALUES ('work', 'transfer-plan-1', now(), 'owner')
         ON CONFLICT (kind, slug) DO UPDATE SET deleted_at = EXCLUDED.deleted_at,
             owner_user_id = coalesce(EXCLUDED.owner_user_id, rh_topic_tombstones.owner_user_id)",
        "INSERT INTO rh_topic_tombstones (kind, slug, deleted_at, owner_user_id)
         VALUES ('entity', 'transfer-plan-1', now(), nullif('owner', ''))
         ON CONFLICT (kind, slug) DO UPDATE SET deleted_at = EXCLUDED.deleted_at,
             owner_user_id = coalesce(EXCLUDED.owner_user_id, rh_topic_tombstones.owner_user_id)",
    ] {
        assert_arbiter(
            &explain(&mut tx, statement).await?,
            "rh_topic_tombstones_pkey",
        );
    }
    let merged = explain(&mut tx,
        "INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at, owner_user_id)
         VALUES ('transfer-plan-1', 'transfer-plan-2', now(), coalesce(rh_scope_principal(), nullif('owner', '')))
         ON CONFLICT (from_slug) DO UPDATE SET to_slug = EXCLUDED.to_slug,
             created_at = EXCLUDED.created_at,
             owner_user_id = coalesce(rh_wiki_redirects.owner_user_id, EXCLUDED.owner_user_id)",
    ).await?;
    assert_arbiter(&merged, "ros_wiki_redirects_pkey");
    let redirected = explain(
        &mut tx,
        "INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at, owner_user_id)
         SELECT 'transfer-alias-1', 'transfer-plan-2', now(), coalesce(rh_scope_principal(),
             (SELECT owner_user_id FROM rh_wiki_topics WHERE slug = 'transfer-plan-2'))
          WHERE NOT EXISTS (SELECT 1 FROM rh_wiki_topics WHERE slug = 'transfer-alias-1')
         ON CONFLICT (from_slug) DO UPDATE SET to_slug = EXCLUDED.to_slug,
             owner_user_id = coalesce(rh_wiki_redirects.owner_user_id, EXCLUDED.owner_user_id)
          WHERE rh_wiki_redirects.to_slug <> EXCLUDED.to_slug
             OR (rh_wiki_redirects.owner_user_id IS NULL AND EXCLUDED.owner_user_id IS NOT NULL)",
    )
    .await?;
    assert_arbiter(&redirected, "ros_wiki_redirects_pkey");
    assert!(
        redirected.contains("Index Scan using ros_wiki_topics_pkey"),
        "{redirected}"
    );
    assert!(
        redirected.contains("Index Only Scan using ros_wiki_topics_pkey"),
        "{redirected}"
    );
    let folded = explain(
        &mut tx,
        "WITH retired AS (
            DELETE FROM rh_wiki_topics WHERE slug = 'transfer-plan-1' RETURNING owner_user_id
         )
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at, owner_user_id)
         SELECT 'transfer-plan-1', 'transfer-plan-2', now(),
             coalesce(rh_scope_principal(), (SELECT owner_user_id FROM retired))
         ON CONFLICT (from_slug) DO UPDATE SET to_slug = EXCLUDED.to_slug,
             owner_user_id = coalesce(rh_wiki_redirects.owner_user_id, EXCLUDED.owner_user_id)",
    )
    .await?;
    assert_arbiter(&folded, "ros_wiki_redirects_pkey");
    assert!(
        folded.contains("Index Scan using ros_wiki_topics_pkey"),
        "{folded}"
    );
    tx.rollback().await?;
    Ok(())
}

fn assert_arbiter(plan: &str, index: &str) {
    assert!(
        plan.contains(&format!("Conflict Arbiter Indexes: {index}")),
        "{plan}"
    );
    assert!(!plan.contains("Seq Scan"), "{plan}");
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

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn merging_entities_adopts_a_legacy_redirect(pool: PgPool) -> TestResult {
    use kernel::wiki::entity_pass::EntityStore as _;

    let ids: (uuid::Uuid, uuid::Uuid) = sqlx::query_as(
        "WITH added AS (
           INSERT INTO rh_topics (id, kind, slug, name, owner_user_id, created_at, updated_at)
           VALUES (gen_random_uuid(), 'entity', 'older', 'Older', 'alice', now() - interval '1 day', now()),
                  (gen_random_uuid(), 'entity', 'younger', 'Younger', 'alice', now(), now())
           RETURNING id, slug
         )
         SELECT a.id, b.id FROM added a CROSS JOIN added b WHERE a.slug = 'older' AND b.slug = 'younger'",
    ).fetch_one(&pool).await?;
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, owner_user_id) VALUES ('older', 'Older', 'alice');
         INSERT INTO rh_wiki_redirects (from_slug, to_slug) VALUES ('younger', 'older')",
    )
        .execute(&pool)
        .await?;
    let merged = memory_postgres::PostgresMemory::new(pool.clone())
        .merge_entities(ids.1, ids.0, jiff::Timestamp::now())
        .await?
        .ok_or("missing merge")?;
    assert!(merged.performed);
    let owner: Option<String> = sqlx::query_scalar(
        "SELECT owner_user_id FROM rh_wiki_redirects WHERE from_slug = 'younger'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(owner.as_deref(), Some("alice"));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn redirect_page_leaves_a_legacy_redirect_with_the_same_target(pool: PgPool) -> TestResult {
    use kernel::topics::TopicPageStore as _;
    use kernel::wiki::page::WikiSlug;

    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, owner_user_id) VALUES ('page', 'Page', 'alice');
         INSERT INTO rh_wiki_redirects (from_slug, to_slug) VALUES ('legacy', 'page')",
    )
    .execute(&pool)
    .await?;
    memory_postgres::PostgresMemory::new(pool.clone())
        .redirect_page(
            &WikiSlug::try_from("legacy".to_owned())?,
            &WikiSlug::try_from("page".to_owned())?,
            jiff::Timestamp::now(),
        )
        .await?;
    let owner: Option<String> = sqlx::query_scalar(
        "SELECT owner_user_id FROM rh_wiki_redirects WHERE from_slug = 'legacy'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        owner, None,
        "redirect_page does not write an alias that already points at the page"
    );
    Ok(())
}
