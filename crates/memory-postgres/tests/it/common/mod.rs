use sqlx::PgPool;

pub(crate) async fn install_graphile(pool: &PgPool) -> Result<(), sqlx::Error> {
    let present: bool = sqlx::query_scalar("SELECT to_regnamespace('graphile_worker') IS NOT NULL")
        .fetch_one(pool)
        .await?;
    if present {
        return Ok(());
    }
    let mut transaction = pool.begin().await?;
    let path: String = sqlx::query_scalar("SHOW search_path")
        .fetch_one(&mut *transaction)
        .await?;
    sqlx::raw_sql(include_str!(
        "../../../../rivethub/tests/fixtures/graphile_worker.txt"
    ))
    .execute(&mut *transaction)
    .await?;
    sqlx::query("SELECT set_config('search_path', $1, false)")
        .bind(path)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await
}
