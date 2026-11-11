use crate::common;
use crate::import::at;
use crate::import::import;
use crate::import::phils_session;
use crate::import::receiver_key;

use kernel::memory::SessionActivity;
use kernel::memory::SessionCatalog as _;
use kernel::memory::SessionDeletion as _;
use kernel::memory::SessionQuery;
use kernel::memory::SessionView;
use kernel::owner::Owner;
use kernel::tenancy::Scope;
use kernel::tenancy::Viewer;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn person(name: &str) -> Result<Scope, Box<dyn std::error::Error>> {
    Ok(Scope::Viewer(Viewer::new(
        Owner::new(name.to_owned())?,
        Vec::new(),
    )))
}

async fn listed(
    store: &PostgresMemory,
    scope: Scope,
    view: SessionView,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let page = scope.run(store.list(&SessionQuery::new(view))).await?;
    Ok(page.items.iter().map(|item| item.id.to_string()).collect())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn another_person_neither_lists_nor_marks_an_import(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let session = phils_session()?;
    import(&PostgresMemory::new(pool.clone()), "alice", &session).await?;
    let key = receiver_key("alice", &session)?;
    let scoped = PostgresMemory::new(common::probe(&pool).await?);
    let keys = [key.clone()];

    assert_eq!(
        listed(&scoped, person("alice")?, SessionView::All).await?,
        keys
    );
    assert_eq!(
        listed(&scoped, person("bob")?, SessionView::All).await?,
        Vec::<String>::new()
    );
    let recorded = |scope: Scope| {
        let scoped = &scoped;
        let keys = &keys;
        async move {
            let found = scope.run(scoped.session_activity(keys)).await?;
            Ok::<_, Box<dyn std::error::Error>>(
                found.get(&keys[0]).is_some_and(SessionActivity::recorded),
            )
        }
    };
    assert!(recorded(person("alice")?).await?);
    assert!(!recorded(person("bob")?).await?);

    let refused = person("bob")?
        .run(scoped.trash_session(&key, Some("bob"), at("2026-11-02T00:00:00Z")?))
        .await;
    let error = refused.err().ok_or("another person's import is refused")?;
    assert!(
        error.to_string().contains("belongs to another person"),
        "{error}"
    );
    assert_eq!(
        listed(&scoped, person("alice")?, SessionView::Trash).await?,
        Vec::<String>::new()
    );

    person("alice")?
        .run(scoped.trash_session(&key, Some("alice"), at("2026-11-02T00:00:00Z")?))
        .await?;
    assert_eq!(
        listed(&scoped, person("alice")?, SessionView::Trash).await?,
        keys
    );
    assert_eq!(
        listed(&scoped, person("bob")?, SessionView::Trash).await?,
        Vec::<String>::new()
    );
    person("bob")?.run(scoped.restore_session(&key)).await?;
    assert_eq!(
        listed(&scoped, person("alice")?, SessionView::Trash).await?,
        keys,
        "another person's restore changes nothing"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn only_the_owner_keeps_an_import(pool: PgPool) -> TestResult {
    common::install_graphile(&pool).await?;
    let session = phils_session()?;
    import(&PostgresMemory::new(pool.clone()), "alice", &session).await?;
    let key = receiver_key("alice", &session)?;
    let scoped = PostgresMemory::new(common::probe(&pool).await?);
    let keys = [key.clone()];

    let refused = person("bob")?
        .run(scoped.keep_session(&key, Some("bob"), at("2026-11-02T00:00:00Z")?))
        .await;
    let error = refused.err().ok_or("another person's keep is refused")?;
    assert!(
        error.to_string().contains("belongs to another person"),
        "{error}"
    );
    assert_eq!(
        listed(&scoped, person("alice")?, SessionView::Kept).await?,
        Vec::<String>::new()
    );

    person("alice")?
        .run(scoped.keep_session(&key, Some("alice"), at("2026-11-02T00:00:00Z")?))
        .await?;
    assert_eq!(
        listed(&scoped, person("alice")?, SessionView::Kept).await?,
        keys
    );
    Ok(())
}
