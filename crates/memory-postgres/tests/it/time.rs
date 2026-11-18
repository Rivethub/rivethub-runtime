use jiff::Timestamp;
use sqlx::PgPool;
use wire::time::TimestampFormat as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test]
async fn timestamptz_truncates_at_the_postgres_epoch_and_accepts_arrays_and_nulls(
    pool: PgPool,
) -> TestResult {
    let old: Timestamp = "1999-12-31T23:59:59.999999001Z".parse()?;
    let new: Timestamp = "2000-01-01T00:00:00.000001999Z".parse()?;
    let (stored, array, null): (
        jiff_sqlx::Timestamp,
        Vec<jiff_sqlx::Timestamp>,
        Option<jiff_sqlx::Timestamp>,
    ) = sqlx::query_as("SELECT $1::timestamptz, $2::timestamptz[], $3::timestamptz")
        .bind(memory_postgres::time::PgTimestamp::from(new))
        .bind(vec![
            jiff_sqlx::Timestamp::from(old),
            jiff_sqlx::Timestamp::from(new),
        ])
        .bind(None::<jiff_sqlx::Timestamp>)
        .fetch_one(&pool)
        .await?;
    assert_eq!(
        stored.to_jiff().rfc3339_offset(),
        "2000-01-01T00:00:00.000001+00:00"
    );
    let [first, second] = array.as_slice() else {
        return Err("expected two timestamps".into());
    };
    assert_eq!(
        first.to_jiff().rfc3339_offset(),
        "2000-01-01T00:00:00+00:00"
    );
    assert_eq!(*second, stored);
    assert_eq!(null, None);
    Ok(())
}

#[sqlx::test]
async fn a_stored_year_outside_the_supported_range_fails_the_query(pool: PgPool) -> TestResult {
    let times = sqlx::query_scalar::<_, jiff_sqlx::Timestamp>(
        "SELECT at FROM (VALUES ('2026-01-01 00:00:00+00'::timestamptz),
                                ('10000-01-01 00:00:00+00'::timestamptz)) AS times(at)
          ORDER BY at",
    )
    .fetch_all(&pool)
    .await;
    assert!(matches!(times, Err(sqlx::Error::ColumnDecode { .. })));
    Ok(())
}
