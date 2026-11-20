use crate::search_support;
use jiff::Timestamp;

use kernel::memory::SearchMode;
use search_support::{TestResult, failed, query, store};

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn trigram_similarity_and_threshold_match_postgres(pool: sqlx::PgPool) -> TestResult {
    let (_dir, store) = store(None)?;
    for (left, right) in [
        ("word", "two words"),
        ("postgresql", "postgressql"),
        ("cat", "car"),
        ("a", "a"),
        ("A_B-C", "a b c"),
        ("aaaa", "aaa"),
        ("foo|bar", "foo bar"),
        ("", ""),
        ("a", "b"),
    ] {
        let expected: f32 = sqlx::query_scalar("SELECT similarity($1,$2)")
            .bind(left)
            .bind(right)
            .fetch_one(&pool)
            .await?;
        let (left, right) = (left.to_owned(), right.to_owned());
        let actual: f64 = store
            .with_conn(move |conn| {
                conn.query_row("SELECT rh_similarity(?1,?2)", (left, right), |row| {
                    row.get(0)
                })
                .map_err(failed)
            })
            .await?;
        assert!(
            (actual - f64::from(expected)).abs() < 1e-7,
            "{actual} != {expected}"
        );
    }
    for (text, needle) in [
        ("cat", "car"),
        ("PostgreSQL", "postgressql"),
        ("a", "a"),
        ("abcdefghijkl", "abc"),
        ("hello world", "world"),
        ("a b c", "a"),
        ("ab c d ef", "ab"),
    ] {
        let expected: bool = sqlx::query_scalar("SELECT similarity($1,$2)>0.3")
            .bind(text)
            .bind(needle)
            .fetch_one(&pool)
            .await?;
        let text = text.to_owned();
        store.with_conn(move |conn|{
            conn.execute("INSERT INTO ros_summaries(id,content,kind,created_at) VALUES ('s',?1,'leaf','2026-10-10T00:00:00.000Z') ON CONFLICT(id) DO UPDATE SET content=excluded.content",[text]).map_err(failed)?;
            Ok(())
        }).await?;
        let result = store
            .search_at(
                query(needle, SearchMode::Trigram),
                Timestamp::now(),
                &jiff::tz::TimeZone::UTC,
            )
            .await?;
        assert_eq!(result.results.len(), usize::from(expected), "{needle}");
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn supported_regex_patterns_match_postgres_case_insensitive(
    pool: sqlx::PgPool,
) -> TestResult {
    let (_dir, store) = store(None)?;
    for (text, pattern) in [
        ("Deploy SUCCESS 42", r"^deploy.*\d+$"),
        ("abc.def", r"abc\.def"),
        ("a'b", "a'b"),
        ("two\nlines", "two.*lines"),
        ("no match", "^match$"),
    ] {
        let expected: bool = sqlx::query_scalar("SELECT $1::text ~* $2::text")
            .bind(text)
            .bind(pattern)
            .fetch_one(&pool)
            .await?;
        let (text, pattern) = (text.to_owned(), pattern.to_owned());
        let actual: bool = store
            .with_conn(move |conn| {
                conn.query_row("SELECT ?1 REGEXP ?2", (text, pattern), |row| row.get(0))
                    .map_err(failed)
            })
            .await?;
        assert_eq!(actual, expected);
    }
    Ok(())
}

#[tokio::test]
async fn summary_search_hides_the_legacy_rows_of_a_session_tree() -> TestResult {
    let (_dir, store) = store(None)?;
    store
        .with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at)
                 VALUES ('mixed', 'cli:mixed', 'fixture','2026-10-10T00:00:00Z','2026-10-10T00:00:00Z');
                 INSERT INTO ros_summaries (id, conversation_id, kind, content, created_at, latest_at)
                 VALUES ('old', 'mixed', 'leaf', 'invented tray legacy', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z'),
                        ('new', 'mixed', 'session', 'invented tray session', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');",
            )
            .map_err(failed)
        })
        .await?;
    for mode in [SearchMode::Fts, SearchMode::Regex] {
        let result = store
            .search_at(
                query("tray", mode),
                Timestamp::now(),
                &jiff::tz::TimeZone::UTC,
            )
            .await?;
        assert_eq!(result.results.len(), 1, "{mode}");
        assert_eq!(
            result
                .results
                .first()
                .ok_or("expected a session hit")?
                .content,
            "invented tray session"
        );
    }
    Ok(())
}
