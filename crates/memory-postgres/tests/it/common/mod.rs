use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub(crate) const PROBE: &str = "rh_rls_probe";

pub(crate) async fn probe(pool: &PgPool) -> Result<PgPool, Box<dyn std::error::Error>> {
    probe_with(pool, PgPoolOptions::new().max_connections(2)).await
}

pub(crate) async fn probe_with(
    pool: &PgPool,
    options: PgPoolOptions,
) -> Result<PgPool, Box<dyn std::error::Error>> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DO $$ BEGIN CREATE ROLE {PROBE} NOLOGIN; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
    )))
    .execute(pool)
    .await?;
    for grant in [
        format!("GRANT USAGE ON SCHEMA public TO {PROBE}"),
        format!("GRANT ALL ON ALL TABLES IN SCHEMA public TO {PROBE}"),
        format!("GRANT ALL ON ALL SEQUENCES IN SCHEMA public TO {PROBE}"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(grant))
            .execute(pool)
            .await?;
    }
    let connect = pool
        .connect_options()
        .as_ref()
        .clone()
        .options([("role", PROBE)]);
    Ok(memory_postgres::scoped(options)
        .connect_with(connect)
        .await?)
}

pub(crate) async fn seed_entity_leaf(
    pool: &PgPool,
    leaf: &kernel::wiki::entity_pass::EntityLeaf,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rh_summaries (id, conversation_id, content, owner_user_id, earliest_at, latest_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(leaf.id)
    .bind(leaf.conversation_id)
    .bind(&leaf.content)
    .bind(leaf.owner.as_deref())
    .bind(memory_postgres::time::PgTimestamp::from(leaf.start))
    .bind(memory_postgres::time::PgTimestamp::from(leaf.end))
    .execute(pool)
    .await
    .map(drop)
}
