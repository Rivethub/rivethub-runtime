use jiff::Timestamp;
use kernel::tenancy::Scope;
use kernel::topics::NewTopic;
use kernel::topics::SweepOwners;
use kernel::topics::SyncTopicPage;
use kernel::topics::Topic;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
use kernel::topics::TopicPage;
use kernel::topics::TopicPageError;
use kernel::topics::TopicPageStore as _;
use kernel::topics::TopicStore as _;
use kernel::topics::sync_topic_page;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::consolidate::WikiConsolidateStore as _;
use kernel::wiki::consolidate::WikiFold;
use kernel::wiki::page::FoldMark;
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
async fn a_viewer_cannot_retire_a_page_another_person_redirects_to(pool: PgPool) -> TestResult {
    let (store, _) = seed(&pool).await?;
    let wiki = tempfile::tempdir()?;
    let root = wiki.path().join("wiki");
    let alice = viewer("alice")?;
    let now = Timestamp::now();
    let kitchen = store
        .create(Topic::create(
            TopicId::random(),
            NewTopic {
                kind: TopicKind::Work,
                name: TopicName::new("Kitchen")?,
                aliases: Vec::new(),
                parent: None,
                directory: None,
                owner: Some(alice.principal().clone()),
            },
            now,
        )?)
        .await?;
    let admin = PostgresMemory::new(pool.clone()).with_wiki_directory(root.clone());
    admin
        .save_topic_page(
            kitchen.id,
            &TopicPage::empty(slug("kitchen")?, "Kitchen"),
            now,
        )
        .await?;
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, current_state, owner_user_id, visibility)
         VALUES ('pantry', 'Pantry', 'Flour lives on the top shelf.', 'alice', 'shared');
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id)
         VALUES ('larder', 'pantry', 'bob')",
    )
    .execute(&pool)
    .await?;
    let fold = WikiFold {
        cluster: "kitchen".to_owned(),
        parent: "kitchen".to_owned(),
        children: vec!["pantry".to_owned()],
        mark: FoldMark::Merged,
    };

    let probe = probe(&pool).await?;
    let viewer_store = PostgresMemory::new(probe.clone()).with_wiki_directory(root.clone());
    let owners = Scope::Viewer(alice);
    let folded = owners.clone().run(viewer_store.fold_wiki(&fold, now)).await;
    assert!(
        matches!(&folded, Err(WikiStoreError::RedirectsHeld { page, aliases })
            if page == "pantry" && aliases == &["larder".to_owned()]),
        "a viewer fold over bob's alias is refused, got {folded:?}"
    );
    let removed = owners.run(viewer_store.remove_page(&slug("pantry")?)).await;
    assert!(
        matches!(&removed, Err(WikiStoreError::RedirectsHeld { .. })),
        "a viewer removal under bob's alias is refused, got {removed:?}"
    );
    assert_eq!(
        redirect_row(&pool, "larder").await?,
        ("pantry".to_owned(), Some("bob".to_owned()))
    );
    let pantry: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rh_wiki_topics WHERE slug = 'pantry'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(pantry, 1, "the refused fold kept the page row");
    let kitchen_text = std::fs::read_to_string(root.join("topics/kitchen.md"))?;
    assert!(
        !kitchen_text.contains("Flour"),
        "the refused fold left the parent file alone"
    );

    let maintained = Scope::Maintenance
        .run(viewer_store.fold_wiki(&fold, now))
        .await?;
    assert_eq!(maintained, vec!["pantry".to_owned()]);
    assert_eq!(
        redirect_row(&pool, "larder").await?,
        ("kitchen".to_owned(), Some("bob".to_owned()))
    );
    Ok(())
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

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn deleting_a_page_keeps_the_redirects_that_point_at_it(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_scopes (scope, owner_user_id, visibility, key_prefix, directory)
         VALUES ('bob', 'bob', 'private', 'bob-', 'bob');
         INSERT INTO rh_wiki_topics (slug, title, owner_user_id, visibility, scope, local_slug)
         VALUES ('bob-desk', 'Desk', 'bob', 'private', 'bob', 'desk');
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id)
         VALUES ('desk', 'bob-desk', 'alice'), ('old-desk', 'bob-desk', NULL)",
    )
    .execute(&pool)
    .await?;
    let bob = person(&pool, "bob").await?;
    let deleted = sqlx::query("DELETE FROM rh_wiki_topics WHERE slug = 'bob-desk'")
        .execute(&bob)
        .await;
    let code = deleted
        .as_ref()
        .err()
        .and_then(sqlx::Error::as_database_error)
        .and_then(sqlx::error::DatabaseError::code);
    assert_eq!(
        code.as_deref(),
        Some("23503"),
        "deleting a page other people's redirects point at fails, got {deleted:?}"
    );
    assert_eq!(
        redirect_row(&pool, "desk").await?,
        ("bob-desk".to_owned(), Some("alice".to_owned()))
    );
    assert_eq!(
        redirect_row(&pool, "old-desk").await?,
        ("bob-desk".to_owned(), None)
    );

    let wiki = tempfile::tempdir()?;
    let probe = probe(&pool).await?;
    let store = PostgresMemory::new(probe).with_wiki_directory(wiki.path().join("wiki"));
    Scope::Maintenance
        .run(store.remove_page(&slug("bob-desk")?))
        .await?;
    let left: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM rh_wiki_redirects WHERE to_slug = 'bob-desk')
              + (SELECT count(*) FROM rh_wiki_topics WHERE slug = 'bob-desk')",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        left, 0,
        "a maintenance removal deletes the redirects, then the page"
    );
    Ok(())
}

async fn work_topic(
    store: &PostgresMemory,
    name: &str,
    parent: Option<TopicId>,
) -> Result<Topic, Box<dyn std::error::Error>> {
    Ok(store
        .create(Topic::create(
            TopicId::random(),
            NewTopic {
                kind: TopicKind::Work,
                name: TopicName::new(name)?,
                aliases: Vec::new(),
                parent,
                directory: None,
                owner: Some(viewer("alice")?.principal().clone()),
            },
            Timestamp::now(),
        )?)
        .await?)
}

fn wiki_store(
    pool: PgPool,
    root: &std::path::Path,
    scoped: bool,
) -> Result<PostgresMemory, Box<dyn std::error::Error>> {
    let store = PostgresMemory::new(pool).with_wiki_directory(root.to_owned());
    Ok(if scoped {
        store.with_wiki_owner(kernel::owner::Owner::new("phil".to_owned())?)
    } else {
        store
    })
}

async fn kitchen_with_pantry(
    pool: &PgPool,
    root: &std::path::Path,
    scoped: bool,
) -> Result<TopicId, Box<dyn std::error::Error>> {
    let (store, _) = seed(pool).await?;
    let now = Timestamp::now();
    let kitchen = work_topic(&store, "Kitchen", None).await?;
    let pantry = work_topic(&store, "Pantry", Some(kitchen.id)).await?;
    let admin = wiki_store(pool.clone(), root, scoped)?;
    admin
        .save_topic_page(
            kitchen.id,
            &TopicPage::empty(slug("kitchen")?, "Kitchen"),
            now,
        )
        .await?;
    admin
        .save_topic_page(
            pantry.id,
            &TopicPage {
                summary: "Flour lives on the top shelf.".to_owned(),
                ..TopicPage::empty(slug("pantry")?, "Pantry")
            },
            now,
        )
        .await?;
    Ok(pantry.id)
}

async fn sync_as_alice(
    pool: &PgPool,
    root: &std::path::Path,
    topic: TopicId,
    scoped: bool,
) -> Result<Result<(), TopicPageError>, Box<dyn std::error::Error>> {
    let store = wiki_store(probe(pool).await?, root, scoped)?;
    Ok(Scope::Viewer(viewer("alice")?)
        .run(sync_topic_page(
            &store,
            &store,
            &store,
            &SyncTopicPage::topic(topic),
            SweepOwners::All,
            Timestamp::now(),
        ))
        .await
        .map(drop))
}

fn topic_file(root: &std::path::Path, scoped: bool, slug: &str) -> std::path::PathBuf {
    let topics = if scoped {
        "users/alice/topics"
    } else {
        "topics"
    };
    root.join(topics).join(format!("{slug}.md"))
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_refused_topic_sync_fold_changes_nothing(pool: PgPool) -> TestResult {
    refused_sync(pool, false).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_refused_scoped_topic_sync_fold_changes_nothing(pool: PgPool) -> TestResult {
    refused_sync(pool, true).await
}

async fn refused_sync(pool: PgPool, scoped: bool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let root = wiki.path().join("wiki");
    let pantry = kitchen_with_pantry(&pool, &root, scoped).await?;
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id)
         VALUES ('larder', 'pantry', 'bob')",
    )
    .execute(&pool)
    .await?;

    let synced = sync_as_alice(&pool, &root, pantry, scoped).await?;
    assert!(
        matches!(
            &synced,
            Err(TopicPageError::Wiki(WikiStoreError::RedirectsHeld { page, .. })) if page == "pantry"
        ),
        "a topic sync cannot fold a page bob's alias points at, got {synced:?}"
    );
    let pantry_text: String =
        sqlx::query_scalar("SELECT current_state FROM rh_wiki_topics WHERE slug = 'pantry'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        pantry_text, "Flour lives on the top shelf.",
        "the refused fold kept the sub-topic's text"
    );
    let kitchen_text = std::fs::read_to_string(topic_file(&root, scoped, "kitchen"))?;
    assert!(
        !kitchen_text.contains("Flour"),
        "the refused fold did not copy the text into the parent"
    );
    assert_eq!(
        redirect_row(&pool, "larder").await?,
        ("pantry".to_owned(), Some("bob".to_owned()))
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_topic_sync_fold_moves_the_text_and_retires_the_page_together(
    pool: PgPool,
) -> TestResult {
    synced_fold(pool, false).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_scoped_topic_sync_fold_moves_the_text_and_retires_the_page_together(
    pool: PgPool,
) -> TestResult {
    synced_fold(pool, true).await
}

async fn synced_fold(pool: PgPool, scoped: bool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let root = wiki.path().join("wiki");
    let pantry = kitchen_with_pantry(&pool, &root, scoped).await?;
    sqlx::raw_sql(
        "CREATE FUNCTION test_alias_cleared_pantry() RETURNS trigger
             LANGUAGE plpgsql SECURITY DEFINER AS $$
         BEGIN
             INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id)
             VALUES ('larder', 'pantry', 'bob') ON CONFLICT DO NOTHING;
             RETURN NULL;
         END $$;
         CREATE TRIGGER test_alias_cleared_pantry AFTER UPDATE ON rh_wiki_topics
             FOR EACH ROW WHEN (NEW.slug = 'pantry' AND NEW.current_state = '')
             EXECUTE FUNCTION test_alias_cleared_pantry()",
    )
    .execute(&pool)
    .await?;

    let synced = sync_as_alice(&pool, &root, pantry, scoped).await?;
    assert!(
        synced.is_ok(),
        "the sync folds the sub-topic without a separate clearing write that a new alias could follow, got {synced:?}"
    );
    let pantry_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rh_wiki_topics WHERE slug = 'pantry'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(pantry_rows, 0, "the sub-topic page is retired");
    let kitchen_text = std::fs::read_to_string(topic_file(&root, scoped, "kitchen"))?;
    assert!(
        kitchen_text.contains("Flour lives on the top shelf."),
        "the sub-topic's text moved into the parent page"
    );
    assert!(
        !topic_file(&root, scoped, "pantry").exists(),
        "the sub-topic's page file is removed"
    );
    assert_eq!(redirect_row(&pool, "pantry").await?.0, "kitchen");
    Ok(())
}

async fn blocked_by(pool: &PgPool, blocker: i32) -> Result<(), sqlx::Error> {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(10));
    loop {
        interval.tick().await;
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
                             WHERE datname = current_database()
                               AND $1 = ANY(pg_blocking_pids(pid)))",
        )
        .bind(blocker)
        .fetch_one(pool)
        .await?;
        if waiting {
            return Ok(());
        }
    }
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_fold_and_a_redirect_move_between_its_children_both_finish(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let root = wiki.path().join("wiki");
    kitchen_with_pantry(&pool, &root, false).await?;
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, current_state, owner_user_id)
         VALUES ('larder', 'Larder', 'Jars of jam.', 'alice');
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id)
         VALUES ('cupboard', 'pantry', 'alice')",
    )
    .execute(&pool)
    .await?;
    let fold = WikiFold {
        cluster: "kitchen".to_owned(),
        parent: "kitchen".to_owned(),
        children: vec!["larder".to_owned(), "pantry".to_owned()],
        mark: FoldMark::Merged,
    };
    let store = PostgresMemory::new(probe(&pool).await?).with_wiki_directory(root.clone());
    let mut mover = pool.begin().await?;
    let mover_pid: i32 = sqlx::query_scalar(
        "SELECT pg_backend_pid() FROM rh_wiki_redirects WHERE from_slug = 'cupboard' FOR UPDATE",
    )
    .fetch_one(&mut *mover)
    .await?;

    let folding = Scope::Maintenance.run(store.fold_wiki(&fold, Timestamp::now()));
    let moving = async {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            blocked_by(&pool, mover_pid),
        )
        .await
        .map_err(|_| sqlx::Error::PoolTimedOut)??;
        sqlx::query("UPDATE rh_wiki_redirects SET to_slug = 'larder' WHERE from_slug = 'cupboard'")
            .execute(&mut *mover)
            .await?;
        mover.commit().await
    };
    let (folded, moved) = tokio::join!(folding, moving);
    assert!(
        moved.is_ok(),
        "the redirect move finishes without a deadlock, got {moved:?}"
    );
    assert_eq!(
        folded.map_err(|error| format!("the fold finishes without a deadlock, got {error:?}"))?,
        vec!["larder".to_owned(), "pantry".to_owned()]
    );
    assert_eq!(
        redirect_row(&pool, "cupboard").await?,
        ("kitchen".to_owned(), Some("alice".to_owned()))
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_topic_sync_fold_rebuilds_a_parent_file_from_its_indexed_row(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let root = wiki.path().join("wiki");
    let pantry = kitchen_with_pantry(&pool, &root, false).await?;
    std::fs::remove_file(topic_file(&root, false, "kitchen"))?;
    sqlx::query(
        "UPDATE rh_wiki_topics SET article = 'Pots hang by the stove.', aliases = '{cookhouse}',
                tags = '{rooms}', entities = '{stove}', related = '{dining-room}'
          WHERE slug = 'kitchen'",
    )
    .execute(&pool)
    .await?;

    let synced = sync_as_alice(&pool, &root, pantry, false).await?;
    assert!(
        synced.is_ok(),
        "the sync folds the sub-topic, got {synced:?}"
    );
    let kitchen_text = std::fs::read_to_string(topic_file(&root, false, "kitchen"))?;
    assert!(
        kitchen_text.contains("Flour lives on the top shelf."),
        "the sub-topic's text moved into the rebuilt parent page"
    );
    assert!(
        kitchen_text.contains("Pots hang by the stove."),
        "the rebuilt parent page keeps its indexed text"
    );
    let (aliases, tags, entities, related): (Vec<String>, Vec<String>, Vec<String>, Vec<String>) =
        sqlx::query_as(
            "SELECT aliases, tags, entities, related FROM rh_wiki_topics WHERE slug = 'kitchen'",
        )
        .fetch_one(&pool)
        .await?;
    assert!(
        aliases.contains(&"cookhouse".to_owned()),
        "the rebuilt parent keeps its aliases, got {aliases:?}"
    );
    assert_eq!(
        tags,
        vec!["rooms".to_owned()],
        "the rebuilt parent keeps its tags"
    );
    assert!(
        entities.contains(&"stove".to_owned()),
        "the rebuilt parent keeps its entities, got {entities:?}"
    );
    assert!(
        related.contains(&"dining-room".to_owned()),
        "the rebuilt parent keeps its related pages, got {related:?}"
    );
    let pantry_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rh_wiki_topics WHERE slug = 'pantry'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(pantry_rows, 0, "the sub-topic page is retired");
    assert_eq!(redirect_row(&pool, "pantry").await?.0, "kitchen");
    Ok(())
}

async fn waiting(pool: &PgPool, pid: i32) -> Result<(), sqlx::Error> {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(10));
    loop {
        interval.tick().await;
        let blocked: bool = sqlx::query_scalar("SELECT cardinality(pg_blocking_pids($1)) > 0")
            .bind(pid)
            .fetch_one(pool)
            .await?;
        if blocked {
            return Ok(());
        }
    }
}

async fn within<T>(
    wait: impl std::future::Future<Output = Result<T, sqlx::Error>>,
) -> Result<T, sqlx::Error> {
    tokio::time::timeout(std::time::Duration::from_secs(10), wait)
        .await
        .map_err(|_| sqlx::Error::PoolTimedOut)?
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_redirect_added_while_a_fold_waits_for_its_page_does_not_deadlock(
    pool: PgPool,
) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let root = wiki.path().join("wiki");
    kitchen_with_pantry(&pool, &root, false).await?;
    sqlx::query(
        "INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id)
         VALUES ('cupboard', 'pantry', 'alice')",
    )
    .execute(&pool)
    .await?;
    let fold = WikiFold {
        cluster: "kitchen".to_owned(),
        parent: "kitchen".to_owned(),
        children: vec!["pantry".to_owned()],
        mark: FoldMark::Merged,
    };
    let store = PostgresMemory::new(probe(&pool).await?).with_wiki_directory(root.clone());
    let mut reader = pool.begin().await?;
    let reader_pid: i32 = sqlx::query_scalar(
        "SELECT pg_backend_pid() FROM rh_wiki_topics WHERE slug = 'pantry' FOR KEY SHARE",
    )
    .fetch_one(&mut *reader)
    .await?;

    let folding = Scope::Maintenance.run(store.fold_wiki(&fold, Timestamp::now()));
    let racing = async {
        within(blocked_by(&pool, reader_pid)).await?;
        sqlx::query(
            "INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id)
             VALUES ('shelf', 'pantry', 'alice')",
        )
        .execute(&pool)
        .await?;
        let mut mover = pool.begin().await?;
        let mover_pid: i32 = sqlx::query_scalar(
            "SELECT pg_backend_pid() FROM rh_wiki_redirects WHERE from_slug = 'shelf' FOR UPDATE",
        )
        .fetch_one(&mut *mover)
        .await?;
        let moving = async {
            sqlx::query(
                "UPDATE rh_wiki_redirects SET to_slug = 'pantry' WHERE from_slug = 'cupboard'",
            )
            .execute(&mut *mover)
            .await?;
            mover.commit().await
        };
        let releasing = async {
            within(waiting(&pool, mover_pid)).await?;
            reader.rollback().await
        };
        let (moved, released) = tokio::join!(moving, releasing);
        released?;
        moved
    };
    let (folded, raced) = tokio::join!(folding, racing);
    assert!(
        raced.is_ok(),
        "the redirect move finishes without a deadlock, got {raced:?}"
    );
    assert_eq!(
        folded.map_err(|error| format!("the fold finishes without a deadlock, got {error:?}"))?,
        vec!["pantry".to_owned()]
    );
    for alias in ["cupboard", "shelf"] {
        assert_eq!(redirect_row(&pool, alias).await?.0, "kitchen", "{alias}");
    }
    Ok(())
}

async fn backed_off(pool: &PgPool, blocker: i32) -> Result<(), sqlx::Error> {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(10));
    loop {
        interval.tick().await;
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
                             WHERE datname = current_database()
                               AND (wait_event = 'PgSleep' OR $1 = ANY(pg_blocking_pids(pid))))",
        )
        .bind(blocker)
        .fetch_one(pool)
        .await?;
        if waiting {
            return Ok(());
        }
    }
}

async fn before_redirect_migration(pool: &PgPool) -> TestResult {
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
    before.run(pool).await?;
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, owner_user_id)
         VALUES ('kitchen', 'Kitchen', 'alice'), ('pantry', 'Pantry', 'alice');
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id)
         VALUES ('cupboard', 'pantry', 'alice')",
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn the_redirect_migration_waits_for_an_in_flight_fold(pool: PgPool) -> TestResult {
    before_redirect_migration(&pool).await?;
    let mut fold = pool.begin().await?;
    let fold_pid: i32 = sqlx::query_scalar(
        "SELECT pg_backend_pid() FROM rh_wiki_redirects WHERE to_slug = 'pantry' FOR UPDATE",
    )
    .fetch_one(&mut *fold)
    .await?;

    let migrating = migrate::postgres_pool(&pool);
    let folding = async {
        within(backed_off(&pool, fold_pid)).await?;
        sqlx::query("DELETE FROM rh_wiki_redirects WHERE to_slug = 'pantry'")
            .execute(&mut *fold)
            .await?;
        sqlx::query("DELETE FROM rh_wiki_topics WHERE slug = 'pantry'")
            .execute(&mut *fold)
            .await?;
        fold.commit().await
    };
    let (migrated, folded) = tokio::join!(migrating, folding);
    assert!(
        folded.is_ok(),
        "the in-flight fold finishes without a deadlock, got {folded:?}"
    );
    assert!(
        migrated.is_ok(),
        "the migration waits for the fold and then applies, got {migrated:?}"
    );
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn the_redirect_migration_and_a_fold_that_read_its_pages_both_finish(
    pool: PgPool,
) -> TestResult {
    before_redirect_migration(&pool).await?;
    let mut fold = pool.begin().await?;
    let fold_pid: i32 =
        sqlx::query_scalar("SELECT pg_backend_pid() FROM rh_wiki_topics WHERE slug = 'pantry'")
            .fetch_one(&mut *fold)
            .await?;

    let migrating = migrate::postgres_pool(&pool);
    let folding = async {
        within(backed_off(&pool, fold_pid)).await?;
        sqlx::query("SELECT 1 FROM rh_wiki_redirects WHERE to_slug = 'pantry' FOR UPDATE")
            .execute(&mut *fold)
            .await?;
        sqlx::query("DELETE FROM rh_wiki_redirects WHERE to_slug = 'pantry'")
            .execute(&mut *fold)
            .await?;
        sqlx::query("DELETE FROM rh_wiki_topics WHERE slug = 'pantry'")
            .execute(&mut *fold)
            .await?;
        fold.commit().await
    };
    let (migrated, folded) = tokio::join!(migrating, folding);
    assert!(
        folded.is_ok(),
        "the fold takes its redirect lock after reading pages without a deadlock, got {folded:?}"
    );
    assert!(
        migrated.is_ok(),
        "the migration backs off until the fold commits, then applies, got {migrated:?}"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_fold_across_wiki_homes_is_refused_and_changes_nothing(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let root = wiki.path().join("wiki");
    let (store, _) = seed(&pool).await?;
    let now = Timestamp::now();
    let kitchen = work_topic(&store, "Kitchen", None).await?;
    let pantry = store
        .create(Topic::create(
            TopicId::random(),
            NewTopic {
                kind: TopicKind::Work,
                name: TopicName::new("Pantry")?,
                aliases: Vec::new(),
                parent: Some(kitchen.id),
                directory: None,
                owner: Some(viewer("bob")?.principal().clone()),
            },
            now,
        )?)
        .await?;
    let admin = wiki_store(pool.clone(), &root, true)?;
    admin
        .save_topic_page(
            kitchen.id,
            &TopicPage::empty(slug("kitchen")?, "Kitchen"),
            now,
        )
        .await?;
    admin
        .save_topic_page(
            pantry.id,
            &TopicPage {
                summary: "Flour lives on the top shelf.".to_owned(),
                ..TopicPage::empty(slug("pantry")?, "Pantry")
            },
            now,
        )
        .await?;
    let fold = WikiFold {
        cluster: "kitchen".to_owned(),
        parent: "kitchen".to_owned(),
        children: vec!["pantry".to_owned()],
        mark: FoldMark::Merged,
    };
    let folder = PostgresMemory::new(probe(&pool).await?).with_wiki_directory(root.clone());

    let folded = Scope::Maintenance
        .run(folder.fold_wiki(&fold, Timestamp::now()))
        .await;
    assert!(
        matches!(&folded, Err(WikiStoreError::InvalidPatch(message)) if message.contains("pantry")),
        "a fold whose pages live in two wikis is refused, got {folded:?}"
    );
    let pantry_text: String =
        sqlx::query_scalar("SELECT current_state FROM rh_wiki_topics WHERE slug = 'pantry'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(pantry_text, "Flour lives on the top shelf.");
    let redirects: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_wiki_redirects")
        .fetch_one(&pool)
        .await?;
    assert_eq!(redirects, 0, "the refused fold wrote no redirect");
    assert!(
        !std::fs::read_to_string(topic_file(&root, true, "kitchen"))?.contains("Flour"),
        "the refused fold did not write the parent file"
    );
    assert!(
        root.join("users/bob/topics/pantry.md").exists(),
        "the refused fold kept the child file"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_child_created_in_another_wiki_while_a_fold_waits_is_refused(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let root = wiki.path().join("wiki");
    kitchen_with_pantry(&pool, &root, true).await?;
    sqlx::query("UPDATE rh_wiki_topics SET slug = 'alice-pantry' WHERE slug = 'pantry'")
        .execute(&pool)
        .await?;
    let fold = WikiFold {
        cluster: "kitchen".to_owned(),
        parent: "kitchen".to_owned(),
        children: vec!["pantry".to_owned()],
        mark: FoldMark::Merged,
    };
    let store = PostgresMemory::new(probe(&pool).await?).with_wiki_directory(root.clone());
    let mut creator = pool.begin().await?;
    let creator_pid: i32 = sqlx::query_scalar(
        "SELECT pg_backend_pid() FROM pg_advisory_xact_lock(hashtextextended('pantry', 0))",
    )
    .fetch_one(&mut *creator)
    .await?;

    let folding = Scope::Maintenance.run(store.fold_wiki(&fold, Timestamp::now()));
    let creating = async {
        within(blocked_by(&pool, creator_pid)).await?;
        sqlx::query(
            "INSERT INTO rh_wiki_topics (slug, title, current_state, owner_user_id)
             VALUES ('pantry', 'Pantry', 'Rice by the door.', 'alice')",
        )
        .execute(&mut *creator)
        .await?;
        creator.commit().await
    };
    let (folded, created) = tokio::join!(folding, creating);
    created?;
    assert!(
        matches!(&folded, Err(WikiStoreError::InvalidPatch(_))),
        "a fold whose child appeared in another wiki while it waited is refused, got {folded:?}"
    );
    assert!(
        std::fs::read_to_string(topic_file(&root, true, "pantry"))?
            .contains("Flour lives on the top shelf."),
        "the refused fold kept the same-named file of another page"
    );
    assert!(
        !std::fs::read_to_string(topic_file(&root, true, "kitchen"))?.contains("Flour"),
        "the refused fold did not write the parent file"
    );
    let pantry_text: String =
        sqlx::query_scalar("SELECT current_state FROM rh_wiki_topics WHERE slug = 'pantry'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(pantry_text, "Rice by the door.");
    let redirects: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_wiki_redirects")
        .fetch_one(&pool)
        .await?;
    assert_eq!(redirects, 0, "the refused fold wrote no redirect");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_rebuilt_parent_keeps_a_scoped_link_whose_key_is_another_local_slug(
    pool: PgPool,
) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let root = wiki.path().join("wiki");
    let pantry = kitchen_with_pantry(&pool, &root, true).await?;
    std::fs::remove_file(topic_file(&root, true, "kitchen"))?;
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, scope, local_slug, owner_user_id)
         VALUES ('alice-dining', 'Dining Room', 'alice', 'dining', 'alice'),
                ('alice-alice-dining', 'Alice Dining', 'alice', 'alice-dining', 'alice');
         UPDATE rh_wiki_topics SET related = '{alice-dining}' WHERE slug = 'kitchen'",
    )
    .execute(&pool)
    .await?;

    let synced = sync_as_alice(&pool, &root, pantry, true).await?;
    assert!(
        synced.is_ok(),
        "the sync folds the sub-topic, got {synced:?}"
    );
    let related: Vec<String> =
        sqlx::query_scalar("SELECT related FROM rh_wiki_topics WHERE slug = 'kitchen'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        related,
        vec!["alice-dining".to_owned()],
        "the rebuilt parent still links the page it linked before"
    );
    Ok(())
}
