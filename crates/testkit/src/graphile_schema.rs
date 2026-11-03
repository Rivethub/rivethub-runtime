use sqlx::PgPool;

const SCHEMA: &str = include_str!("../tests/fixtures/graphile/worker-schema.txt");

pub async fn install_graphile_worker(pool: &PgPool) -> Result<(), sqlx::Error> {
    let present: bool = sqlx::query_scalar("SELECT to_regnamespace('graphile_worker') IS NOT NULL")
        .fetch_one(pool)
        .await?;
    if !present {
        sqlx::raw_sql(SCHEMA).execute(pool).await?;
    }
    Ok(())
}
