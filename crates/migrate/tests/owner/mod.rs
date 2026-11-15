pub(crate) async fn migrate_owned_by(
    pool: &sqlx::PgPool,
    owner: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = pool.acquire().await?;
    sqlx::query("SELECT set_config('rh.store_owner', $1, false)")
        .bind(owner)
        .execute(&mut *conn)
        .await?;
    migrate::POSTGRES.run(&mut *conn).await?;
    sqlx::query("RESET rh.store_owner")
        .execute(&mut *conn)
        .await?;
    Ok(())
}
