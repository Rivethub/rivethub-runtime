use sqlx::{AssertSqlSafe, Connection as _, PgConnection, PgPool};
use uuid::Uuid;

use super::{Scorer, compact, word_similarity};

type Scores = (f32, f32, f32);
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[sqlx::test]
async fn entity_trigram_scores_match_postgres_c_utf8(pool: PgPool) -> TestResult {
    let cases = [
        ("two words", "word"),
        ("PostgreSQL", "postgressql"),
        ("R2-D2/Unit_4", "r2 d2 unit-5"),
        ("a", "A"),
        ("AB", "a"),
        ("aaaa aaa abaaaa", "aaa aa"),
        ("cat gato car cat", "car cat"),
        ("école française", "ÉCOLE"),
        ("İstanbul City", "istanbul"),
        ("aÉb", "Ae\u{301}b"),
        ("ΑΟΣ City", "αοσ"),
        ("МОСКВА City", "москва"),
        ("中文城市", "中文"),
        ("鏎议仒", "瘍鼕肁"),
        ("hello🚀world", "hello world"),
        ("a²b", "a b"),
        ("a٢b", "a b"),
        ("a\u{0363}b", "a b"),
        ("A\u{1c89}B", "a b"),
        ("", ""),
        ("🚀", "a"),
    ];
    let scorer = Scorer::new()?;
    let expected = postgres_scores(&pool, &cases).await?;
    for ((name, term), (full, word, best)) in cases.into_iter().zip(expected) {
        let actual = (
            scorer.similarity(name, term),
            word_similarity(&scorer.unique(term), &scorer.trigrams(name)),
            scorer.score(term, name),
        );
        assert_eq!(actual, (full, word, best), "{name:?} / {term:?}");
    }
    assert_eq!(
        scorer.score("istanbul", "İstanbul City").to_bits(),
        1.0_f32.to_bits()
    );
    Ok(())
}

#[test]
fn multibyte_trigrams_use_postgres_legacy_crc() {
    let [first, second, third, _] = 0xa854_9be6_u32.to_ne_bytes();
    assert_eq!(compact([' ', ' ', 'é']), [first, second, third]);
}

async fn postgres_scores(pool: &PgPool, cases: &[(&str, &str)]) -> TestResult<Vec<Scores>> {
    let database = format!("rh_trigrams_{}", Uuid::new_v4().simple());
    sqlx::query(AssertSqlSafe(format!(
        "CREATE DATABASE {database} TEMPLATE template0 LOCALE 'C.UTF-8' ENCODING 'UTF8'"
    )))
    .execute(pool)
    .await?;
    let options = pool.connect_options().as_ref().clone().database(&database);
    let scores = async {
        let mut conn = PgConnection::connect_with(&options).await?;
        sqlx::query("CREATE EXTENSION pg_trgm")
            .execute(&mut conn)
            .await?;
        let mut scores = Vec::new();
        for &(name, term) in cases {
            scores.push(postgres_score(&mut conn, name, term).await?);
        }
        Ok::<_, sqlx::Error>(scores)
    }
    .await;
    sqlx::query(AssertSqlSafe(format!(
        "DROP DATABASE {database} WITH (FORCE)"
    )))
    .execute(pool)
    .await?;
    Ok(scores?)
}

async fn postgres_score(conn: &mut PgConnection, name: &str, term: &str) -> sqlx::Result<Scores> {
    sqlx::query_as(
        "SELECT similarity(lower($1), lower($2)), word_similarity(lower($2), lower($1)),
                greatest(similarity(lower($1), lower($2)), word_similarity(lower($2), lower($1)))",
    )
    .bind(name)
    .bind(term)
    .fetch_one(conn)
    .await
}
