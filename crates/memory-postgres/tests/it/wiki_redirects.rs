use jiff::Timestamp;
use kernel::tenancy::Scope;
use kernel::topics::TopicPageStore as _;
use kernel::wiki::builder::WikiStoreError;
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
        "INSERT INTO rh_spaces (id, kind, name) VALUES ('shared', 'team', 'Shared');
         INSERT INTO rh_wiki_topics (slug, title, owner_user_id, visibility)
         VALUES ('page', 'Page', 'alice', 'private'),
                ('space-page', 'Space Page', 'shared', 'shared'),
                ('member-page', 'Member Page', 'alice', 'shared');
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id)
         VALUES ('legacy', 'page', NULL),
                ('space-legacy', 'space-page', NULL),
                ('member-legacy', 'member-page', NULL)",
    )
    .execute(&pool)
    .await?;
    migrate::postgres_pool(&pool).await?;
    for from in ["space-legacy", "member-legacy"] {
        assert_eq!(
            redirect_row(&pool, from).await?.1,
            None,
            "an alias of a shared page stays maintenance-only: {from}"
        );
    }

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

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_owner_writes_only_redirects_to_pages_they_can_see(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, owner_user_id)
         VALUES ('bob-desk', 'Desk', 'bob'), ('bob-shelf', 'Shelf', 'bob'),
                ('alice-diary', 'Diary', 'alice');
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id)
         VALUES ('old-desk', 'bob-desk', 'bob'), ('old-diary', 'alice-diary', 'bob')",
    )
    .execute(&pool)
    .await?;
    let bob = person(&pool, "bob").await?;

    let seen: Vec<String> =
        sqlx::query_scalar("SELECT from_slug FROM rh_wiki_redirects ORDER BY from_slug")
            .fetch_all(&bob)
            .await?;
    assert_eq!(
        seen,
        vec!["old-desk".to_owned()],
        "an owner does not read their redirect to a page they can no longer see"
    );
    for (name, sql) in [
        (
            "UPDATE",
            "UPDATE rh_wiki_redirects SET to_slug = 'bob-desk' WHERE from_slug = 'old-diary'",
        ),
        (
            "DELETE",
            "DELETE FROM rh_wiki_redirects WHERE from_slug = 'old-diary'",
        ),
    ] {
        let done = sqlx::query(sql).execute(&bob).await?;
        assert_eq!(
            done.rows_affected(),
            0,
            "{name} of a redirect to a hidden page changed it"
        );
    }
    for (name, sql) in [
        (
            "INSERT to a hidden page",
            "INSERT INTO rh_wiki_redirects (from_slug, to_slug) VALUES ('new-diary', 'alice-diary')",
        ),
        (
            "UPDATE onto a hidden page",
            "UPDATE rh_wiki_redirects SET to_slug = 'alice-diary' WHERE from_slug = 'old-desk'",
        ),
    ] {
        let outcome = sqlx::query(sql).execute(&bob).await;
        assert!(
            outcome.as_ref().is_err_and(refused),
            "{name} should fail with 42501, got {outcome:?}"
        );
    }
    assert_eq!(
        redirect_row(&pool, "old-diary").await?,
        ("alice-diary".to_owned(), Some("bob".to_owned()))
    );

    let moved = sqlx::query(
        "UPDATE rh_wiki_redirects SET to_slug = 'bob-shelf' WHERE from_slug = 'old-desk'",
    )
    .execute(&bob)
    .await?;
    assert_eq!(
        moved.rows_affected(),
        1,
        "an owner moves a redirect between pages they see"
    );
    sqlx::query(
        "INSERT INTO rh_wiki_redirects (from_slug, to_slug) VALUES ('new-desk', 'bob-desk')",
    )
    .execute(&bob)
    .await?;
    assert_eq!(
        redirect_row(&pool, "new-desk").await?,
        ("bob-desk".to_owned(), Some("bob".to_owned()))
    );
    let dropped = sqlx::query("DELETE FROM rh_wiki_redirects WHERE from_slug = 'new-desk'")
        .execute(&bob)
        .await?;
    assert_eq!(
        dropped.rows_affected(),
        1,
        "an owner deletes their redirect"
    );
    Ok(())
}

fn slug(text: &str) -> Result<WikiSlug, Box<dyn std::error::Error>> {
    Ok(WikiSlug::try_from(text.to_owned())?)
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn topic_sync_leaves_an_alias_that_already_points_right(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, owner_user_id, visibility)
         VALUES ('shared-kitchen', 'Shared Kitchen', 'shared', 'shared'),
                ('alice-desk', 'Desk', 'alice', 'private');
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id, created_at)
         VALUES ('kitchen', 'shared-kitchen', NULL, '2026-01-01T00:00:00Z'),
                ('cooking', 'shared-kitchen', 'bob', '2026-01-01T00:00:00Z'),
                ('desk', 'alice-desk', 'alice', '2026-01-01T00:00:00Z'),
                ('stale', 'alice-desk', 'bob', '2026-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await?;
    let probe = probe(&pool).await?;
    let store = PostgresMemory::new(probe);
    let alice = Scope::Viewer(viewer("alice")?);
    let later = Timestamp::now();
    for (from, to) in [
        ("kitchen", "shared-kitchen"),
        ("cooking", "shared-kitchen"),
        ("desk", "alice-desk"),
    ] {
        alice
            .clone()
            .run(store.redirect_page(&slug(from)?, &slug(to)?, later))
            .await?;
    }
    let taken = alice
        .run(store.redirect_page(&slug("stale")?, &slug("shared-kitchen")?, later))
        .await;
    assert!(
        matches!(&taken, Err(WikiStoreError::Query(_))),
        "topic sync does not take over bob's alias, got {taken:?}"
    );
    let rows: Vec<(String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT from_slug, to_slug, owner_user_id, created_at::date::text
           FROM rh_wiki_redirects ORDER BY from_slug",
    )
    .fetch_all(&pool)
    .await?;
    let row = |from: &str, to: &str, owner: Option<&str>| {
        (
            from.to_owned(),
            to.to_owned(),
            owner.map(str::to_owned),
            "2026-01-01".to_owned(),
        )
    };
    assert_eq!(
        rows,
        vec![
            row("cooking", "shared-kitchen", Some("bob")),
            row("desk", "alice-desk", Some("alice")),
            row("kitchen", "shared-kitchen", None),
            row("stale", "alice-desk", Some("bob")),
        ],
        "an alias that already points at the page is left as it was"
    );
    Ok(())
}
