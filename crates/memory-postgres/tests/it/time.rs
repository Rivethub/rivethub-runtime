use chrono::{DateTime, Utc};
use sqlx::PgPool;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test]
async fn timestamptz_truncates_at_the_postgres_epoch_and_accepts_arrays_and_nulls(
    pool: PgPool,
) -> TestResult {
    let old: DateTime<Utc> = "1999-12-31T23:59:59.999999001Z".parse()?;
    let new: DateTime<Utc> = "2000-01-01T00:00:00.000001999Z".parse()?;
    let (stored, array, null): (DateTime<Utc>, Vec<DateTime<Utc>>, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT $1::timestamptz, $2::timestamptz[], $3::timestamptz")
            .bind(new)
            .bind(vec![old, new])
            .bind(None::<DateTime<Utc>>)
            .fetch_one(&pool)
            .await?;
    assert_eq!(stored.to_rfc3339(), "2000-01-01T00:00:00.000001+00:00");
    assert_eq!(array[0].to_rfc3339(), "2000-01-01T00:00:00+00:00");
    assert_eq!(array[1], stored);
    assert_eq!(null, None);
    Ok(())
}
