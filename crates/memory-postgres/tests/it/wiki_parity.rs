use std::path::Path;

use kernel::owner::Owner;
use kernel::wiki::builder::ExtractWiki;
use kernel::wiki::page::PagePatch;
use kernel::wiki::page::WikiAction;
use memory_postgres::CaptureOptions;
use memory_postgres::PostgresMemory;
use memory_postgres::PostgresQueue;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct WikiHome {
    _directory: tempfile::TempDir,
    memory: PostgresMemory,
    jobs: [ExtractWiki; 2],
}

impl WikiHome {
    async fn new(pool: PgPool) -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("wiki");
        tokio::fs::create_dir_all(&root).await?;
        let jobs = seed_summaries(&pool).await?;
        let memory = PostgresMemory::new(pool).with_wiki_directory(root);
        Ok(Self {
            _directory: directory,
            memory,
            jobs,
        })
    }
}

async fn seed_summaries(pool: &PgPool) -> TestResult<[ExtractWiki; 2]> {
    let conversation = Uuid::new_v4();
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL rivet.defer_embed_enqueue = 'on'")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO rh_conversations (id, session_key, agent, title, owner_user_id)
         VALUES ($1, 'cli:wiki-parity', 'rivet', 'AcmeKit', 'owner')",
    )
    .bind(conversation)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO rh_summaries (id, conversation_id, content, kind, owner_user_id)
         VALUES ($1, $2, 'Ships 1.0.', 'leaf', 'owner'),
                ($3, $2, 'Builds moved.', 'leaf', 'owner')",
    )
    .bind(first)
    .bind(conversation)
    .bind(second)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok([
        ExtractWiki {
            summary_id: first,
            conversation_id: Some(conversation),
        },
        ExtractWiki {
            summary_id: second,
            conversation_id: Some(conversation),
        },
    ])
}

async fn seed_stem_pages(pool: &PgPool, root: &Path) -> TestResult {
    let verified_at = "2026-10-28T12:00:00Z".parse()?;
    let repository = memory_postgres::wiki_writer::WikiRepository::new(root.to_path_buf());
    for (slug, title, state) in [
        ("acme-kit", "Acme Kit", "Acme Kit ships the runtime."),
        (
            "acme-kit-notes",
            "Rollout diary",
            "The Friday notes stayed in the diary.",
        ),
    ] {
        let applied = write_stem_page(&repository, slug, title, state, verified_at).await?;
        sqlx::query(
            "INSERT INTO rh_wiki_topics (slug, title, current_state, search_text, history_count, git_sha)
             VALUES ($1, $2, $3, $3, 0, $4)",
        )
        .bind(slug)
        .bind(title)
        .bind(state)
        .bind(applied)
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn write_stem_page(
    repository: &memory_postgres::wiki_writer::WikiRepository,
    slug: &str,
    title: &str,
    state: &str,
    verified_at: jiff::Timestamp,
) -> TestResult<String> {
    let (applied, released) = {
        let mut locked = repository.lock().await?;
        let applied = locked
            .apply(stem_patch(slug, title, state, verified_at), slug, None)
            .await;
        (applied, locked.release().await)
    };
    released?;
    Ok(applied?.git_sha)
}

fn stem_patch(slug: &str, title: &str, state: &str, verified_at: jiff::Timestamp) -> PagePatch {
    PagePatch {
        action: WikiAction::Create,
        slug: slug.to_owned(),
        verified_at,
        title: Some(title.to_owned()),
        add_aliases: Vec::new(),
        add_tags: Vec::new(),
        add_entities: Vec::new(),
        add_related: Vec::new(),
        summary_delta: Some(state.to_owned()),
        current_state: None,
        article: None,
        article_patches: Vec::new(),
        history_entry: None,
        add_citations: Vec::new(),
        add_sources: Vec::new(),
        allow_shrink: false,
    }
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_stem_cluster_folds_into_the_parent(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("wiki");
    seed_stem_pages(&pool, &root).await?;
    let memory = PostgresMemory::new(pool).with_wiki_directory(root);
    testkit::maintenance::a_stem_cluster_folds_into_the_parent(&memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_second_extraction_keeps_the_stable_summary(pool: PgPool) -> TestResult {
    let home = WikiHome::new(pool).await?;
    testkit::maintenance::a_second_extraction_keeps_the_stable_summary(&home.memory, &home.jobs)
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_eval_task_is_automated(pool: PgPool) -> TestResult {
    let task: Uuid = sqlx::query_scalar(
        "INSERT INTO rh_tasks_archive (goal, executor, agent_id, origin)
         VALUES ('score the page', 'chat-loop', 'rivet', 'eval')
         RETURNING id",
    )
    .fetch_one(&pool)
    .await?;
    let memory = PostgresMemory::new(pool.clone());
    let owner = Owner::new("alice".to_owned())?;
    let captured = memory
        .capture_batch(
            &crate::visibility::batch(&format!("task:{task}"), "Score the page"),
            &CaptureOptions::owned_by(&owner),
        )
        .await?;
    let id = Uuid::parse_str(&captured.conversation_id)?;
    let readonly = PgPoolOptions::new()
        .connect_with(
            pool.connect_options()
                .as_ref()
                .clone()
                .options([("default_transaction_read_only", "on")]),
        )
        .await?;
    testkit::maintenance::an_eval_task_is_automated(&PostgresMemory::new(readonly), id).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn exhausted_extract_wiki_leaves_the_queue(pool: PgPool) -> TestResult {
    testkit::graphile_schema::install_graphile_worker(&pool).await?;
    let jobs = seed_summaries(&pool).await?;
    let summary = jobs.first().ok_or("a live summary")?.summary_id;
    let queue = PostgresQueue::connect(pool.clone()).await?;
    let admin = PostgresMemory::new(pool);
    testkit::maintenance::exhausted_extract_wiki_leaves_the_queue(
        &queue,
        &admin,
        summary,
        async || Ok(()),
    )
    .await
}
