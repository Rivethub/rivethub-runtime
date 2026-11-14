use chrono::Utc;
use kernel::memory::BrowseFilter;
use kernel::memory::Limit;
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::MemoryScope;
use kernel::memory::MemoryStore as _;
use kernel::memory::SearchMode;
use kernel::memory::SearchQuery;
use kernel::memory::SessionDeletion as _;
use kernel::owner::Owner;
use kernel::tenancy::Scope;
use kernel::tenancy::TenancyStore as _;
use kernel::tenancy::TenancyStoreError;
use kernel::tenancy::Visibility;
use memory_postgres::CaptureOptions;
use memory_postgres::PostgresMemory;
use memory_postgres::PostgresTenancy;
use sqlx::PgPool;
use wire::session_id::{SessionId, SessionKey};

use crate::visibility::{batch, household, person, seed, viewer};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SHARED: &str = "claude-code:00000000-0000-4000-8000-0000000000c1";
const PRIVATE: &str = "claude-code:00000000-0000-4000-8000-0000000000b2";
const TEXT: &str = "alice wrote down how the quartzlantern relay reaches the hub";

fn search() -> SearchQuery {
    SearchQuery {
        since: None,
        before: None,
        window: None,
        expand: None,
        synthesize: None,
        text: "quartzlantern".to_owned(),
        mode: SearchMode::Fts,
        scope: MemoryScope::Messages,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(Some(20), 20, 1..=50),
    }
}

fn browse() -> BrowseFilter {
    BrowseFilter {
        conversation_id: None,
        agent: None,
        role: None,
        tool_name: None,
        tag: None,
        window: None,
        since: None,
        before: None,
        include_tools: false,
        order: MemoryBrowseOrder::Desc,
        limit: Limit::clamped(Some(50), 50, 1..=200),
    }
}

async fn recalled(reader: &PostgresMemory) -> Result<[bool; 2], Box<dyn std::error::Error>> {
    let scope = Scope::Viewer(viewer("bob")?);
    let found = scope.clone().run(reader.search(search())).await?;
    let browsed = scope.run(reader.browse(browse())).await?;
    Ok([
        found
            .results
            .iter()
            .any(|hit| hit.session_id.as_deref() == Some(SHARED)),
        browsed.iter().any(|entry| entry.content == TEXT),
    ])
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_trashed_session_leaves_every_reader_s_recall_on_a_pinned_login(
    pool: PgPool,
) -> TestResult {
    let (store, tenancy) = seed(&pool).await?;
    let alice = viewer("alice")?;
    let owner = Scope::Viewer(alice.clone());
    store
        .capture_batch(
            &batch(SHARED, TEXT),
            &CaptureOptions::owned_by(&Owner::new("alice".to_owned())?),
        )
        .await?;
    let shared: SessionId = SHARED.parse()?;
    owner
        .clone()
        .run(tenancy.mark_session(
            &alice,
            &SessionKey::from(&shared),
            Some(&Visibility::Space(household()?)),
            Utc::now(),
        ))
        .await?;
    let bob = PostgresMemory::new(person(&pool, "bob").await?);
    assert_eq!(
        recalled(&bob).await?,
        [true, true],
        "bob reads the shared session"
    );

    owner
        .clone()
        .run(store.trash_session(SHARED, Some("alice"), Utc::now()))
        .await?;
    assert_eq!(
        recalled(&bob).await?,
        [false, false],
        "the trash is the session's, not alice's"
    );

    owner.run(store.restore_session(SHARED)).await?;
    assert_eq!(
        recalled(&bob).await?,
        [true, true],
        "a restore returns it to bob"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_reader_cannot_mark_a_session_and_a_stray_mark_changes_nothing(
    pool: PgPool,
) -> TestResult {
    let (store, tenancy) = seed(&pool).await?;
    let alice = viewer("alice")?;
    let bob = viewer("bob")?;
    store
        .capture_batch(
            &batch(SHARED, TEXT),
            &CaptureOptions::owned_by(&Owner::new("alice".to_owned())?),
        )
        .await?;
    let shared: SessionId = SHARED.parse()?;
    let private: SessionId = PRIVATE.parse()?;
    Scope::Viewer(alice.clone())
        .run(tenancy.mark_session(
            &alice,
            &SessionKey::from(&shared),
            Some(&Visibility::Space(household()?)),
            Utc::now(),
        ))
        .await?;
    let as_bob = Scope::Viewer(bob.clone());
    let read = as_bob
        .clone()
        .run(tenancy.mark_session(
            &bob,
            &SessionKey::from(&shared),
            Some(&Visibility::Private),
            Utc::now(),
        ))
        .await;
    assert!(
        matches!(read, Err(TenancyStoreError::NotOwner(_))),
        "{read:?}"
    );
    let pinned = person(&pool, "bob").await?;
    let unseen = as_bob
        .clone()
        .run(PostgresTenancy::new(pinned.clone()).mark_session(
            &bob,
            &SessionKey::from(&private),
            Some(&Visibility::Private),
            Utc::now(),
        ))
        .await;
    assert!(
        matches!(unseen, Err(TenancyStoreError::NotFound(_))),
        "{unseen:?}"
    );
    let planted = sqlx::query(
        "INSERT INTO rh_session_marks (session_key, owner_user_id, visibility, marked_at)
         VALUES ($1, 'bob', 'private', now())",
    )
    .bind(PRIVATE)
    .execute(&pinned)
    .await;
    assert!(
        planted.is_err(),
        "the claim trigger refuses a pinned login too"
    );
    let marks: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rh_session_marks WHERE owner_user_id = $1")
            .bind(bob.principal().as_str())
            .fetch_one(&pool)
            .await?;
    assert_eq!(marks, 0, "nothing was stored for bob");

    sqlx::raw_sql(
        "ALTER TABLE rh_session_marks DISABLE TRIGGER rh_mark_claim;
         INSERT INTO rh_session_marks (session_key, owner_user_id, visibility, marked_at)
              VALUES ('claude-code:00000000-0000-4000-8000-0000000000c1', 'bob', 'private', now());
         ALTER TABLE rh_session_marks ENABLE TRIGGER rh_mark_claim;",
    )
    .execute(&pool)
    .await?;
    let key = SessionKey::from(&shared);
    let seen = as_bob
        .run(tenancy.session_visibility(&bob, std::slice::from_ref(&key)))
        .await?;
    let state = seen.get(&key).ok_or("bob reads the session's state")?;
    assert_eq!(
        state.mark,
        Some(Visibility::Space(household()?)),
        "the mark reported is the owner's, never the stray one"
    );
    assert_eq!(state.effective, Visibility::Space(household()?));
    Ok(())
}
