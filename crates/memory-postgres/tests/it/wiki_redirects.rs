use jiff::Timestamp;
use kernel::tenancy::Scope;
use kernel::topics::TopicPageStore as _;
use kernel::wiki::page::WikiSlug;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;

use crate::common::probe;
use crate::visibility::person;
use crate::visibility::refused;
use crate::visibility::seed;
use crate::visibility::viewer;

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn redirect_row(pool: &PgPool, from: &str) -> Result<(String, Option<String>), sqlx::Error> {
    sqlx::query_as("SELECT to_slug, owner_user_id FROM rh_wiki_redirects WHERE from_slug = $1")
        .bind(from)
        .fetch_one(pool)
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_space_member_cannot_rewrite_another_owners_wiki_redirect(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, owner_user_id, visibility)
         VALUES ('shared-kitchen', 'Shared Kitchen', 'alice', 'shared'),
                ('other-room', 'Other Room', 'alice', 'shared');
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id)
         VALUES ('old-kitchen', 'shared-kitchen', 'alice'),
                ('legacy-kitchen', 'shared-kitchen', NULL)",
    )
    .execute(&pool)
    .await?;
    let bob = person(&pool, "bob").await?;

    let seen: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM rh_wiki_redirects WHERE to_slug = 'shared-kitchen'",
    )
    .fetch_one(&bob)
    .await?;
    assert_eq!(
        seen, 2,
        "a space member still reads redirects to a shared page"
    );

    for (name, sql) in [
        (
            "INSERT as another owner",
            "INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id)
             VALUES ('forged-kitchen', 'shared-kitchen', 'alice')",
        ),
        (
            "upsert over another owner's redirect",
            "INSERT INTO rh_wiki_redirects (from_slug, to_slug)
             VALUES ('old-kitchen', 'other-room')
             ON CONFLICT (from_slug) DO UPDATE SET to_slug = EXCLUDED.to_slug",
        ),
        (
            "upsert over a redirect with no owner",
            "INSERT INTO rh_wiki_redirects (from_slug, to_slug)
             VALUES ('legacy-kitchen', 'other-room')
             ON CONFLICT (from_slug) DO UPDATE SET to_slug = EXCLUDED.to_slug",
        ),
    ] {
        let outcome = sqlx::query(sql).execute(&bob).await;
        assert!(
            outcome.as_ref().is_err_and(refused),
            "{name} by a space member should fail with 42501, got {outcome:?}"
        );
    }
    for (name, sql) in [
        (
            "UPDATE",
            "UPDATE rh_wiki_redirects SET to_slug = 'other-room'
              WHERE from_slug IN ('old-kitchen', 'legacy-kitchen')",
        ),
        (
            "DELETE",
            "DELETE FROM rh_wiki_redirects WHERE from_slug IN ('old-kitchen', 'legacy-kitchen')",
        ),
    ] {
        let done = sqlx::query(sql).execute(&bob).await?;
        assert_eq!(
            done.rows_affected(),
            0,
            "{name} by a space member changed a redirect"
        );
    }
    assert_eq!(
        redirect_row(&pool, "old-kitchen").await?,
        ("shared-kitchen".to_owned(), Some("alice".to_owned()))
    );
    assert_eq!(
        redirect_row(&pool, "legacy-kitchen").await?,
        ("shared-kitchen".to_owned(), None)
    );

    sqlx::query("INSERT INTO rh_wiki_redirects (from_slug, to_slug) VALUES ('bob-kitchen', 'shared-kitchen')")
        .execute(&bob)
        .await?;
    let moved = sqlx::query(
        "UPDATE rh_wiki_redirects SET to_slug = 'other-room' WHERE from_slug = 'bob-kitchen'",
    )
    .execute(&bob)
    .await?;
    assert_eq!(
        moved.rows_affected(),
        1,
        "a person rewrites their own redirect"
    );
    assert_eq!(
        redirect_row(&pool, "bob-kitchen").await?,
        ("other-room".to_owned(), Some("bob".to_owned()))
    );

    let probe = probe(&pool).await?;
    let alice = Scope::Viewer(viewer("alice")?);
    let owned = alice
        .clone()
        .run(
            sqlx::query("UPDATE rh_wiki_redirects SET to_slug = 'other-room' WHERE from_slug = 'old-kitchen'")
                .execute(&probe),
        )
        .await?;
    assert_eq!(
        owned.rows_affected(),
        1,
        "the owner rewrites their redirect"
    );
    let unowned = alice
        .run(
            sqlx::query("UPDATE rh_wiki_redirects SET to_slug = 'other-room' WHERE from_slug = 'legacy-kitchen'")
                .execute(&probe),
        )
        .await?;
    assert_eq!(
        unowned.rows_affected(),
        0,
        "a redirect with no owner is maintenance-only"
    );
    for sql in [
        "UPDATE rh_wiki_redirects SET to_slug = 'other-room' WHERE from_slug = 'legacy-kitchen'",
        "DELETE FROM rh_wiki_redirects WHERE from_slug = 'old-kitchen'",
    ] {
        let done = Scope::Maintenance
            .run(sqlx::query(sql).execute(&probe))
            .await?;
        assert_eq!(
            done.rows_affected(),
            1,
            "maintenance scope writes any redirect: {sql}"
        );
    }
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn an_upgrade_keeps_legacy_wiki_redirects_writable_by_the_page_owner(
    pool: PgPool,
) -> TestResult {
    let through_85 = migrate::POSTGRES
        .migrations
        .partition_point(|migration| migration.version <= 85);
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Borrowed(
            migrate::POSTGRES
                .migrations
                .get(..through_85)
                .ok_or("missing migrations through 85")?,
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    before.run(&pool).await?;
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, owner_user_id) VALUES ('page', 'Page', 'alice');
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id)
         VALUES ('legacy', 'page', NULL)",
    )
    .execute(&pool)
    .await?;
    migrate::postgres_pool(&pool).await?;

    let probe = probe(&pool).await?;
    Scope::Viewer(viewer("alice")?)
        .run(PostgresMemory::new(probe).redirect_page(
            &WikiSlug::try_from("legacy".to_owned())?,
            &WikiSlug::try_from("page".to_owned())?,
            Timestamp::now(),
        ))
        .await?;
    assert_eq!(
        redirect_row(&pool, "legacy").await?,
        ("page".to_owned(), Some("alice".to_owned()))
    );
    Ok(())
}
