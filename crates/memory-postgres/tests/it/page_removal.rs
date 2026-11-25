use std::path::Path;
use std::path::PathBuf;

use jiff::Timestamp;
use kernel::tenancy::Scope;
use kernel::topics::TopicPageStore as _;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::consolidate::WikiConsolidateStore as _;
use kernel::wiki::consolidate::WikiFold;
use kernel::wiki::page::FoldMark;
use kernel::wiki::page::WikiSlug;
use sqlx::PgPool;

use crate::common;
use crate::page_access::ALPHA;
use crate::page_access::BETA;
use crate::page_access::GPUBOX;
use crate::page_access::hub;
use crate::page_access::legacy_row;
use crate::page_access::seed;
use crate::page_access::viewer;
use crate::page_access::write_file;
use crate::wiki_redirects::blocked_by;
use crate::wiki_redirects::within;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const DIARY_ROW: &str =
    "SELECT pg_backend_pid() FROM rh_wiki_topics WHERE slug = 'diary' FOR KEY SHARE";
const DIARY_KEY: &str =
    "SELECT pg_backend_pid() FROM pg_advisory_xact_lock(hashtextextended('diary', 0))";
const PRIMARY_CHANGES_HANDS: &str =
    "UPDATE rh_wiki_scopes SET directory = 'users/phil' WHERE scope = 'phil';
     UPDATE rh_wiki_scopes SET directory = '' WHERE scope = 'coco'";

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_pinned_viewer_never_removes_a_shared_page_it_cannot_delete(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    legacy_row(&pool, "diary", "phil", "crew", "Phil's diary row.").await?;
    write_file(directory.path(), "", "diary", "Phil's diary.")?;
    redirect(&pool, "journal", "diary", "coco").await?;
    pin(&pool, "coco").await?;

    let removed = viewer("coco")?
        .run(memory.remove_page(&slug("diary")?))
        .await;

    assert!(
        matches!(&removed, Err(WikiStoreError::InvalidPatch(_))),
        "coco's pinned login cannot delete phil's row, so the removal is refused, got {removed:?}"
    );
    assert!(
        primary_file(directory.path(), "diary").exists(),
        "phil's file stays"
    );
    assert_eq!(rows(&pool, "diary").await?, 1, "phil's row stays");
    assert_eq!(redirects(&pool).await?, [redirected("journal", "diary")]);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_pinned_fold_never_retires_a_child_it_cannot_delete(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    seed(&pool, directory.path(), &GPUBOX).await?;
    legacy_row(&pool, "gpubox-notes", "phil", "crew", "Phil's notes row.").await?;
    write_file(directory.path(), "", "gpubox-notes", "Phil's GPU notes.")?;
    pin(&pool, "phil").await?;
    let parent = primary_file(directory.path(), GPUBOX.local);
    let before = std::fs::read_to_string(&parent)?;

    let folded = viewer("phil")?
        .run(memory.fold_page(
            &slug("gpubox-notes")?,
            &slug(GPUBOX.key)?,
            FoldMark::Merged,
            Timestamp::now(),
        ))
        .await;

    assert!(
        matches!(&folded, Err(WikiStoreError::InvalidPatch(_))),
        "the pinned login cannot delete the legacy child row, so the fold is refused, got {folded:?}"
    );
    assert!(
        primary_file(directory.path(), "gpubox-notes").exists(),
        "the child file stays"
    );
    assert_eq!(
        std::fs::read_to_string(&parent)?,
        before,
        "the parent file stays"
    );
    assert_eq!(rows(&pool, "gpubox-notes").await?, 1, "the child row stays");
    assert!(
        redirects(&pool).await?.is_empty(),
        "the refused fold wrote no redirect"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_removal_leaves_a_page_that_disappears_while_it_waits(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = diary(&pool, directory.path()).await?;

    let removed = paused(
        &pool,
        DIARY_ROW,
        "DELETE FROM rh_wiki_topics WHERE slug = 'diary'",
        Scope::Maintenance.run(memory.remove_page(&slug("diary")?)),
    )
    .await?;

    assert!(removed.is_ok(), "got {removed:?}");
    assert!(
        primary_file(directory.path(), "diary").exists(),
        "a removal whose page vanished leaves the file"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_removal_leaves_a_page_that_loses_its_home_while_it_waits(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = diary(&pool, directory.path()).await?;
    redirect(&pool, "journal", "diary", "phil").await?;

    let removed = paused(
        &pool,
        DIARY_ROW,
        PRIMARY_CHANGES_HANDS,
        Scope::Maintenance.run(memory.remove_page(&slug("diary")?)),
    )
    .await?;

    assert!(removed.is_ok(), "got {removed:?}");
    assert!(
        primary_file(directory.path(), "diary").exists(),
        "the root wiki now belongs to coco, so phil's legacy file stays"
    );
    assert_eq!(rows(&pool, "diary").await?, 1, "the row stays");
    assert_eq!(redirects(&pool).await?, [redirected("journal", "diary")]);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_removal_leaves_a_page_that_moves_while_it_waits(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = diary(&pool, directory.path()).await?;
    redirect(&pool, "journal", "diary", "phil").await?;

    let removed = paused(
        &pool,
        DIARY_ROW,
        "UPDATE rh_wiki_topics
            SET scope = 'crew', owner_user_id = 'crew', visibility = 'crew', local_slug = 'diary'
          WHERE slug = 'diary'",
        Scope::Maintenance.run(memory.remove_page(&slug("diary")?)),
    )
    .await?;

    assert!(
        matches!(&removed, Err(WikiStoreError::InvalidPatch(_))),
        "got {removed:?}"
    );
    assert!(
        primary_file(directory.path(), "diary").exists(),
        "the file of the moved page's old home stays"
    );
    assert_eq!(rows(&pool, "diary").await?, 1, "the moved row stays");
    assert_eq!(redirects(&pool).await?, [redirected("journal", "diary")]);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_fold_whose_root_wiki_changes_hands_while_it_waits_is_refused(
    pool: PgPool,
) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = diary(&pool, directory.path()).await?;
    legacy_row(&pool, "diary-notes", "phil", "private", "Phil's notes row.").await?;
    write_file(directory.path(), "", "diary-notes", "Phil's diary notes.")?;
    let parent = primary_file(directory.path(), "diary");
    let before = std::fs::read_to_string(&parent)?;
    let fold = WikiFold {
        cluster: "diary".to_owned(),
        parent: "diary".to_owned(),
        children: vec!["diary-notes".to_owned()],
        mark: FoldMark::Merged,
    };

    let folded = paused(
        &pool,
        DIARY_KEY,
        PRIMARY_CHANGES_HANDS,
        Scope::Maintenance.run(memory.fold_wiki(&fold, Timestamp::now())),
    )
    .await?;

    assert!(folded.is_err(), "got {folded:?}");
    assert!(
        primary_file(directory.path(), "diary-notes").exists(),
        "the child file stays"
    );
    assert_eq!(
        std::fs::read_to_string(&parent)?,
        before,
        "the parent file stays"
    );
    assert_eq!(rows(&pool, "diary-notes").await?, 1, "the child row stays");
    assert!(
        redirects(&pool).await?.is_empty(),
        "the refused fold wrote no redirect"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn maintenance_folds_and_removes_space_pages_by_their_file_names(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    seed(&pool, directory.path(), &ALPHA).await?;
    seed(&pool, directory.path(), &BETA).await?;
    redirect(&pool, "old-beta", BETA.key, "coco").await?;
    let topics = directory.path().join("spaces/crew/topics");
    let fold = WikiFold {
        cluster: ALPHA.key.to_owned(),
        parent: ALPHA.key.to_owned(),
        children: vec![BETA.key.to_owned()],
        mark: FoldMark::Merged,
    };

    let folded = Scope::Maintenance
        .run(memory.fold_wiki(&fold, Timestamp::now()))
        .await?;

    assert_eq!(folded, [BETA.key.to_owned()]);
    assert!(
        !topics.join("beta.md").exists(),
        "the child file is folded away"
    );
    let alpha = std::fs::read_to_string(topics.join("alpha.md"))?;
    assert!(alpha.contains(BETA.prose), "got {alpha:?}");
    assert_eq!(rows(&pool, BETA.key).await?, 0, "the child row is retired");
    assert_eq!(
        redirects(&pool).await?,
        [
            redirected(BETA.key, ALPHA.key),
            redirected("old-beta", ALPHA.key),
        ]
    );

    Scope::Maintenance
        .run(memory.remove_page(&slug(ALPHA.key)?))
        .await?;

    assert!(
        !topics.join("alpha.md").exists(),
        "the parent file is removed"
    );
    assert_eq!(
        rows(&pool, ALPHA.key).await?,
        0,
        "the parent row is removed"
    );
    assert!(
        redirects(&pool).await?.is_empty(),
        "its redirects go with it"
    );
    Ok(())
}

async fn diary(
    pool: &PgPool,
    directory: &Path,
) -> Result<memory_postgres::PostgresMemory, Box<dyn std::error::Error>> {
    let memory = hub(pool, directory).await?;
    legacy_row(pool, "diary", "phil", "private", "Phil's diary row.").await?;
    write_file(directory, "", "diary", "Phil's diary.")?;
    Ok(memory)
}

async fn paused<T>(
    pool: &PgPool,
    hold: &'static str,
    change: &'static str,
    work: impl Future<Output = T>,
) -> Result<T, Box<dyn std::error::Error>> {
    let mut holder = pool.begin().await?;
    let holder_pid: i32 = sqlx::query_scalar(hold).fetch_one(&mut *holder).await?;
    let changing = async {
        within(blocked_by(pool, holder_pid)).await?;
        sqlx::raw_sql(change).execute(&mut *holder).await?;
        holder.commit().await
    };
    let (done, changed) = tokio::join!(work, changing);
    changed?;
    Ok(done)
}

async fn pin(pool: &PgPool, person: &str) -> TestResult {
    sqlx::query("INSERT INTO rh_role_principals (role_name, user_id) VALUES ($1, $2)")
        .bind(common::PROBE)
        .bind(person)
        .execute(pool)
        .await?;
    Ok(())
}

async fn redirect(pool: &PgPool, from: &str, to: &str, owner: &str) -> TestResult {
    sqlx::query(
        "INSERT INTO rh_wiki_redirects (from_slug, to_slug, owner_user_id) VALUES ($1, $2, $3)",
    )
    .bind(from)
    .bind(to)
    .bind(owner)
    .execute(pool)
    .await?;
    Ok(())
}

async fn rows(pool: &PgPool, key: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM rh_wiki_topics WHERE slug = $1")
        .bind(key)
        .fetch_one(pool)
        .await
}

async fn redirects(pool: &PgPool) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as("SELECT from_slug, to_slug FROM rh_wiki_redirects ORDER BY from_slug")
        .fetch_all(pool)
        .await
}

fn redirected(from: &str, to: &str) -> (String, String) {
    (from.to_owned(), to.to_owned())
}

fn primary_file(directory: &Path, local: &str) -> PathBuf {
    directory.join("topics").join(format!("{local}.md"))
}

fn slug(text: &str) -> Result<WikiSlug, Box<dyn std::error::Error>> {
    Ok(WikiSlug::try_from(text.to_owned())?)
}
