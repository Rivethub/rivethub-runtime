#![cfg(test)]

use crate::wiki_support;

use kernel::owner::Owner;
use kernel::tenancy::Scope;
use kernel::tenancy::SpaceId;
use kernel::tenancy::Viewer;
use kernel::wiki::builder::{ExtractWiki, WikiBuilderStore, WikiRouting};
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use wiki_support::{TestResult, now, patch};

const PROBE: &str = "rh_wiki_probe";

async fn summary(pool: &PgPool, owner: &str, visibility: &str) -> Result<ExtractWiki, sqlx::Error> {
    let conversation_id = Uuid::new_v4();
    let summary_id = Uuid::new_v4();
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL rivet.defer_embed_enqueue = 'on'")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO rh_conversations (id, session_key, agent, owner_user_id) VALUES ($1, $2, 'rivet', $3)",
    )
    .bind(conversation_id)
    .bind(format!("claude-code:{conversation_id}"))
    .bind(owner)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE rh_conversations SET visibility = $2 WHERE id = $1")
        .bind(conversation_id)
        .bind(visibility)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO rh_summaries (id, conversation_id, content, kind, owner_user_id)
         VALUES ($1, $2, 'Deploy with a health gate.', 'leaf', $3)",
    )
    .bind(summary_id)
    .bind(conversation_id)
    .bind(owner)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ExtractWiki {
        summary_id,
        conversation_id: Some(conversation_id),
    })
}

async fn viewer_pool(pool: &PgPool) -> Result<PgPool, Box<dyn std::error::Error>> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DO $$ BEGIN CREATE ROLE {PROBE} NOLOGIN; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
    )))
    .execute(pool)
    .await?;
    for grant in [
        format!("GRANT USAGE ON SCHEMA public TO {PROBE}"),
        format!("GRANT ALL ON ALL TABLES IN SCHEMA public TO {PROBE}"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(grant))
            .execute(pool)
            .await?;
    }
    let options = pool
        .connect_options()
        .as_ref()
        .clone()
        .options([("role", PROBE)]);
    Ok(
        memory_postgres::scoped(PgPoolOptions::new().max_connections(2))
            .connect_with(options)
            .await?,
    )
}

async fn pages(pool: &PgPool, who: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let viewer = Viewer::new(
        Owner::new(who.to_owned())?,
        vec![SpaceId::new("shared".to_owned())?],
    );
    Ok(Scope::Viewer(viewer)
        .run(async {
            sqlx::query_scalar::<_, String>(
                "SELECT slug || ' ' || owner_user_id || ' ' || visibility FROM rh_wiki_topics ORDER BY slug",
            )
            .fetch_all(pool)
            .await
        })
        .await?)
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn private_and_shared_notes_land_on_separate_pages(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool.clone())
        .with_wiki_directory(wiki.path().to_path_buf())
        .with_wiki_owner(Owner::new("dana".to_owned())?);
    for (owner, visibility) in [("dana", "private"), ("cleo", "private"), ("cleo", "shared")] {
        let job = summary(&pool, owner, visibility).await?;
        store
            .apply_wiki(&job, vec![patch()], &WikiRouting::default(), now())
            .await?;
    }
    let all: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT t.slug, t.owner_user_id, t.visibility, s.directory FROM rh_wiki_topics t
           JOIN rh_wiki_scopes s ON s.scope = t.scope ORDER BY t.slug",
    )
    .fetch_all(&pool)
    .await?;
    let owned = |slug: &str, owner: &str, visibility: &str, directory: &str| {
        (
            slug.to_owned(),
            owner.to_owned(),
            visibility.to_owned(),
            directory.to_owned(),
        )
    };
    assert_eq!(
        all,
        vec![
            owned("acmekit-deploys", "dana", "private", ""),
            owned("cleo-acmekit-deploys", "cleo", "private", "users/cleo"),
            owned(
                "shared-acmekit-deploys",
                "shared",
                "shared",
                "spaces/shared"
            ),
        ]
    );
    for directory in ["", "users/cleo", "spaces/shared"] {
        let file = wiki
            .path()
            .join(directory)
            .join("topics/acmekit-deploys.md");
        assert!(file.is_file(), "{}", file.display());
        assert!(wiki.path().join(directory).join(".git").is_dir());
    }
    let probe = viewer_pool(&pool).await?;
    assert_eq!(
        pages(&probe, "dana").await?,
        vec![
            "acmekit-deploys dana private".to_owned(),
            "shared-acmekit-deploys shared shared".to_owned(),
        ]
    );
    assert_eq!(
        pages(&probe, "cleo").await?,
        vec![
            "cleo-acmekit-deploys cleo private".to_owned(),
            "shared-acmekit-deploys shared shared".to_owned(),
        ]
    );
    Ok(())
}

async fn seed_shared_space(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO rh_spaces (id, kind, name) VALUES ('shared', 'household', 'Shared')")
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO rh_space_members (space_id, user_id, role) VALUES ('shared', 'dana', 'member'), ('shared', 'cleo', 'member')",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn shared_topic(pool: &PgPool) -> Result<kernel::topics::TopicId, sqlx::Error> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO rh_topics (id, slug, name, created_at, updated_at, owner_user_id, visibility_mark)
         VALUES ($1, 'kitchen-shelves', 'Kitchen shelves', now(), now(), 'dana', 'shared')",
    )
    .bind(id)
    .execute(pool)
    .await?;
    sqlx::query("UPDATE rh_topics SET visibility = rh_topic_visibility(id) WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(kernel::topics::TopicId::from_uuid(id))
}

async fn note(
    store: &PostgresMemory,
    topic: kernel::topics::TopicId,
    job: &ExtractWiki,
    text: &str,
) -> TestResult {
    use kernel::topics::TopicPageStore as _;
    let slug = kernel::wiki::page::WikiSlug::try_from("kitchen-shelves".to_owned())?;
    kernel::wiki::origin::NoteOrigin::run(job.summary_id, async {
        let mut page = store
            .topic_page(&slug)
            .await?
            .ok_or("the topic page is readable")?;
        text.clone_into(&mut page.summary);
        store.save_topic_page(topic, &page, now()).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn private_notes_on_a_shared_topic_stay_on_the_owners_own_page(pool: PgPool) -> TestResult {
    use kernel::topics::TopicPageStore as _;
    let wiki = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool.clone())
        .with_wiki_directory(wiki.path().to_path_buf())
        .with_wiki_owner(Owner::new("dana".to_owned())?);
    seed_shared_space(&pool).await?;
    let topic = shared_topic(&pool).await?;
    let slug = kernel::wiki::page::WikiSlug::try_from("kitchen-shelves".to_owned())?;
    store
        .save_topic_page(
            topic,
            &kernel::topics::TopicPage::empty(slug, "Kitchen shelves"),
            now(),
        )
        .await?;
    note(
        &store,
        topic,
        &summary(&pool, "dana", "shared").await?,
        "Brackets every 40 cm.",
    )
    .await?;
    note(
        &store,
        topic,
        &summary(&pool, "dana", "private").await?,
        "Budget stays under 90 euro.",
    )
    .await?;

    let written: Vec<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT t.slug, t.owner_user_id, t.visibility, s.directory, t.current_state FROM rh_wiki_topics t
           JOIN rh_wiki_scopes s ON s.scope = t.scope ORDER BY t.slug",
    )
    .fetch_all(&pool)
    .await?;
    let row = |slug: &str, owner: &str, visibility: &str, directory: &str, state: &str| {
        (
            slug.to_owned(),
            owner.to_owned(),
            visibility.to_owned(),
            directory.to_owned(),
            state.to_owned(),
        )
    };
    assert_eq!(
        written,
        vec![
            row(
                "kitchen-shelves",
                "shared",
                "shared",
                "spaces/shared",
                "Brackets every 40 cm."
            ),
            row(
                "kitchen-shelves-2",
                "dana",
                "private",
                "",
                "Budget stays under 90 euro."
            ),
        ]
    );
    let shared =
        std::fs::read_to_string(wiki.path().join("spaces/shared/topics/kitchen-shelves.md"))?;
    let own = std::fs::read_to_string(wiki.path().join("topics/kitchen-shelves.md"))?;
    assert!(shared.contains("Brackets every 40 cm.") && !shared.contains("90 euro"));
    assert!(own.contains("90 euro"));
    let probe = viewer_pool(&pool).await?;
    assert_eq!(
        pages(&probe, "cleo").await?,
        vec!["kitchen-shelves shared shared".to_owned()],
        "the private note never reaches the shared space"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_personal_page_stays_bounded_under_repeated_updates(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool.clone())
        .with_wiki_directory(wiki.path().to_path_buf())
        .with_wiki_owner(Owner::new("dana".to_owned())?);
    for round in 0..24 {
        let job = summary(&pool, "cleo", "private").await?;
        let mut update = patch();
        if round > 0 {
            update.action = kernel::wiki::page::WikiAction::Update;
        }
        let body = (0..60)
            .map(|step| format!("Deploy r{round}s{step} kept the health gate green."))
            .collect::<Vec<_>>()
            .join(" ");
        update.article = Some(format!("## Round {round}\n{body}"));
        update.summary = format!("AcmeKit deploys, round {round}.");
        store
            .apply_wiki(&job, vec![update], &WikiRouting::default(), now())
            .await?;
    }
    let (article, scope): (String, String) = sqlx::query_as(
        "SELECT t.article, s.directory FROM rh_wiki_topics t JOIN rh_wiki_scopes s ON s.scope = t.scope
          WHERE t.slug = 'cleo-acmekit-deploys'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(scope, "users/cleo");
    assert!(
        article.len() <= kernel::wiki::page::bound::ARTICLE_BUDGET,
        "{} bytes",
        article.len()
    );
    let page = std::fs::read_to_string(wiki.path().join("users/cleo/topics/acmekit-deploys.md"))?;
    assert!(page.len() < 48 * 1024, "{} bytes on disk", page.len());
    assert!(
        wiki.path()
            .join("users/cleo/archive/acmekit-deploys")
            .is_dir(),
        "what the rewrites dropped is archived in her own repository"
    );
    assert!(!wiki.path().join("topics/acmekit-deploys.md").exists());
    Ok(())
}
