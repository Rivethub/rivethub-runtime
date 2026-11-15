use sqlx::PgPool;
use sqlx::Row as _;
use sqlx::Transaction;
use sqlx::postgres::Postgres;

async fn queue_present(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let row =
        sqlx::query("select to_regclass('graphile_worker._private_jobs') is not null as present")
            .fetch_one(pool)
            .await?;
    row.try_get::<bool, _>("present")
}

pub async fn capture_tx(pool: &PgPool) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mode = if queue_present(pool).await? {
        "off"
    } else {
        "on"
    };
    sqlx::query("select set_config('rivet.defer_embed_enqueue', $1, true) as mode")
        .bind(mode)
        .fetch_one(&mut *tx)
        .await?;
    Ok(tx)
}
