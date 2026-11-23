use std::time::Duration;

use jiff::Timestamp;
use uuid::Uuid;
use wire::JsonObject;

use kernel::cleanup::Automated;
use kernel::cleanup::CleanupSource;
use kernel::cleanup::TaskOrigin;
use kernel::flagged::FlaggedJobs;
use kernel::memory::MemoryStore;
use kernel::queue::ClaimSelector;
use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use kernel::queue::admin::MAINTENANCE_TASKS;
use kernel::queue::admin::QueueAdmin;
use kernel::queue::admin::empty_health;
use kernel::queue::pause::PausableRole;
use kernel::queue::pause::WorkerPauses;
use kernel::wiki::WikiLookup;
use kernel::wiki::WikiPageResponse;
use kernel::wiki::builder::ExtractWiki;
use kernel::wiki::builder::WikiBuilderStore;
use kernel::wiki::builder::WikiRouting;
use kernel::wiki::consolidate::ConsolidateWiki;
use kernel::wiki::consolidate::WikiConsolidateStore;
use kernel::wiki::consolidate::WikiFold;
use kernel::wiki::consolidate::consolidation_plan;
use kernel::wiki::page::FoldMark;
use kernel::wiki::page::WikiAction;
use kernel::wiki::page::WikiPatch;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::lead::stable_summary;

use crate::topics::Outcome;

pub async fn pausing_twice_keeps_the_first_and_resume_is_harmless<S>(store: &S) -> Outcome
where
    S: WorkerPauses,
{
    assert_eq!(store.paused().await?, Vec::new());
    let first = store.pause(PausableRole::EntityBuilder, "owner").await?;
    let again = store
        .pause(PausableRole::EntityBuilder, "someone-else")
        .await?;
    assert_eq!(again, first);
    assert_eq!(first.paused_by.as_deref(), Some("owner"));
    store.pause(PausableRole::Categorizer, "owner").await?;
    let roles: Vec<_> = store
        .paused()
        .await?
        .into_iter()
        .map(|pause| pause.role)
        .collect();
    assert_eq!(
        roles,
        [PausableRole::Categorizer, PausableRole::EntityBuilder]
    );
    store.resume(PausableRole::EntityBuilder).await?;
    store.resume(PausableRole::EntityBuilder).await?;
    store.resume(PausableRole::WikiBuilder).await?;
    let roles: Vec<_> = store
        .paused()
        .await?
        .into_iter()
        .map(|pause| pause.role)
        .collect();
    assert_eq!(roles, [PausableRole::Categorizer]);
    Ok(())
}

pub async fn another_connection_sees_pause_and_resume<S>(reader: &S, writer: &S) -> Outcome
where
    S: WorkerPauses,
{
    assert_eq!(reader.paused().await?, Vec::new());
    writer.pause(PausableRole::WikiBuilder, "other").await?;
    let seen: Vec<_> = reader
        .paused()
        .await?
        .into_iter()
        .map(|pause| (pause.role, pause.paused_by))
        .collect();
    assert_eq!(
        seen,
        vec![(PausableRole::WikiBuilder, Some("other".to_owned()))]
    );
    writer.resume(PausableRole::WikiBuilder).await?;
    assert_eq!(reader.paused().await?, Vec::new());
    Ok(())
}

pub async fn resume_from_another_connection_wakes<Q, P>(listening: &Q, writer: &P) -> Outcome
where
    Q: JobQueue,
    P: WorkerPauses,
{
    writer.pause(PausableRole::WikiBuilder, "other").await?;
    let mut notes = listening.subscribe().await?;
    writer.resume(PausableRole::WikiBuilder).await?;
    tokio::time::timeout(Duration::from_secs(3), notes.recv()).await??;
    assert_eq!(writer.paused().await?, Vec::new());
    Ok(())
}

pub async fn a_stem_cluster_folds_into_the_parent<S>(store: &S) -> Outcome
where
    S: WikiConsolidateStore + MemoryStore,
{
    let created: Timestamp = "2026-10-28T12:00:00Z".parse()?;
    let plan = consolidation_plan(&store.wiki_topic_stats().await?, &stem_options());
    let fold = one_stem_fold(&plan)?;
    assert_eq!(
        store.fold_wiki(&fold, created).await?,
        vec!["acme-kit-notes".to_owned()]
    );
    let slugs: Vec<String> = store
        .wiki_topic_stats()
        .await?
        .into_iter()
        .map(|topic| topic.slug)
        .collect();
    assert_eq!(slugs, vec!["acme-kit".to_owned()]);
    assert_eq!(redirect_target(store, "acme-kit-notes").await?, "acme-kit");
    Ok(())
}

pub async fn maintenance_tasks_start_empty<A>(admin: &A) -> Outcome
where
    A: QueueAdmin,
{
    let expected = MAINTENANCE_TASKS
        .into_iter()
        .map(|task| empty_health(JobName::Fixed(task)))
        .collect::<Vec<_>>();
    assert_eq!(admin.admin_status().await?, expected);
    Ok(())
}

pub async fn retry_resets_only_wiki_errors<A>(admin: &A) -> Outcome
where
    A: QueueAdmin,
{
    let task = "extract-wiki".to_owned();
    assert_eq!(
        admin
            .admin_retry(std::slice::from_ref(&task), Some("Wiki"), None, true)
            .await?,
        0
    );
    assert_eq!(
        admin
            .admin_retry(std::slice::from_ref(&task), Some("wiki"), Some(1), true)
            .await?,
        1
    );
    assert_eq!(
        admin
            .admin_retry(std::slice::from_ref(&task), Some("wiki"), Some(1), false)
            .await?,
        1
    );
    assert_eq!(
        admin
            .admin_retry(std::slice::from_ref(&task), Some("wiki"), None, false)
            .await?,
        1
    );
    Ok(())
}

pub async fn requeue_revives_two_idle_jobs<A>(admin: &A) -> Outcome
where
    A: QueueAdmin,
{
    let task = JobName::Fixed(FixedJob::ExtractWiki);
    assert_eq!(admin.admin_requeue(&task, 200, false).await?, 2);
    Ok(())
}

pub async fn cleanup_reads_the_opening<S>(store: &S, id: Uuid, text: &str) -> Outcome
where
    S: CleanupSource,
{
    let snapshot = store.snapshot().await?;
    let row = snapshot
        .conversations
        .iter()
        .find(|row| row.head.id == id)
        .ok_or("projected row")?;
    assert_eq!(row.opening.as_deref(), Some(text));
    assert_eq!(row.summary.as_deref(), Some(text));
    Ok(())
}

pub async fn a_second_extraction_keeps_the_stable_summary<S>(
    store: &S,
    jobs: &[ExtractWiki; 2],
) -> Outcome
where
    S: WikiBuilderStore + MemoryStore,
{
    let first_summary = "AcmeKit ships the runtime from the build box.";
    let second_summary = "The gateway now routes sessions through the mesh.";
    let slug = WikiSlug::try_from("acmekit".to_owned())?;
    let first = jobs.first().ok_or("two summaries")?;
    let second = jobs.get(1).ok_or("two summaries")?;
    let created: Timestamp = "2026-10-28T12:00:00Z".parse()?;
    let updated: Timestamp = "2026-10-29T12:00:00Z".parse()?;
    store
        .apply_wiki(
            first,
            vec![summary_patch(WikiAction::Create, first_summary)?],
            &WikiRouting::default(),
            created,
        )
        .await?;
    store
        .apply_wiki(
            second,
            vec![summary_patch(WikiAction::Update, second_summary)?],
            &WikiRouting::default(),
            updated,
        )
        .await?;
    let page = stored_page(store, &slug).await?;
    let expected = stable_summary(first_summary, second_summary);
    assert_ne!(expected, second_summary);
    assert_eq!(page.current_state, expected);
    assert_eq!(page.title, "AcmeKit");
    Ok(())
}

pub async fn an_eval_task_is_automated<S>(store: &S, id: Uuid) -> Outcome
where
    S: CleanupSource,
{
    let snapshot = store.snapshot().await?;
    let row = snapshot
        .conversations
        .iter()
        .find(|row| row.head.id == id)
        .ok_or("projected row")?;
    assert_eq!(row.task_origin, Some(TaskOrigin::Eval));
    let automated = Automated::detect(
        snapshot.conversations.iter(),
        &std::collections::BTreeSet::new(),
    );
    assert!(automated.runs.contains(&id));
    assert_eq!(automated.by_task, 1);
    Ok(())
}

pub async fn exhausted_extract_wiki_leaves_the_queue<Q, A>(
    queue: &Q,
    admin: &A,
    summary_id: Uuid,
    make_due: impl AsyncFnOnce() -> Outcome,
) -> Outcome
where
    Q: JobQueue,
    A: QueueAdmin + FlaggedJobs,
{
    let task = JobName::Fixed(FixedJob::ExtractWiki);
    let key = format!("wiki-ext-{summary_id}");
    queue
        .enqueue(Job {
            name: task.clone(),
            payload: JsonObject::from_serialize(&ExtractWiki {
                summary_id,
                conversation_id: None,
            })?,
            key: Some(key.clone()),
            key_mode: JobKeyMode::Replace,
            priority: 0,
            run_at: None,
            max_attempts: None,
        })
        .await?;
    let first = claim_extract(queue).await?;
    queue.fail(first.id, "wiki extract failed").await?;
    make_due().await?;
    let second = claim_extract(queue).await?;
    let last_error = "wiki extract failed again";
    queue.fail(second.id, last_error).await?;
    let flagged = admin.flagged().await?;
    let [job] = flagged.as_slice() else {
        return Err("expected exactly one flagged job".into());
    };
    assert_eq!(job.task, task.to_string());
    assert_eq!(job.key.as_deref(), Some(key.as_str()));
    assert_eq!(job.error, last_error);
    assert_eq!(job.failures, 2);
    let status = admin.admin_status().await?;
    let row = status
        .into_iter()
        .find(|row| row.task == task)
        .ok_or("extract-wiki status")?;
    assert_eq!(row, empty_health(task.clone()));
    let name = task.to_string();
    for dry_run in [true, false] {
        assert_eq!(
            admin
                .admin_retry(std::slice::from_ref(&name), None, None, dry_run)
                .await?,
            0
        );
        assert_eq!(admin.admin_requeue(&task, 200, dry_run).await?, 0);
    }
    assert_eq!(admin.flagged().await?, flagged);
    Ok(())
}

async fn claim_extract<Q>(
    queue: &Q,
) -> Result<kernel::queue::ClaimedJob, Box<dyn std::error::Error>>
where
    Q: JobQueue,
{
    let selector = ClaimSelector::matching(vec![JobName::Fixed(FixedJob::ExtractWiki)]);
    for _ in 0..40 {
        if let Some(claimed) = queue.claim(&selector).await? {
            return Ok(claimed);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err("extract-wiki did not become claimable".into())
}

fn stem_options() -> ConsolidateWiki {
    ConsolidateWiki {
        dry_run: false,
        limit_clusters: 50,
        min_cluster_size: 2,
    }
}

fn one_stem_fold(plan: &[WikiFold]) -> Result<WikiFold, Box<dyn std::error::Error>> {
    let expected = WikiFold {
        cluster: "acme-kit".to_owned(),
        parent: "acme-kit".to_owned(),
        children: vec!["acme-kit-notes".to_owned()],
        mark: FoldMark::Merged,
    };
    if plan.len() == 1 && plan.first() == Some(&expected) {
        return Ok(expected);
    }
    Err(format!("stem plan was {plan:?}").into())
}

async fn redirect_target<S>(store: &S, slug: &str) -> Result<String, Box<dyn std::error::Error>>
where
    S: MemoryStore,
{
    match store
        .wiki_page(&WikiSlug::try_from(slug.to_owned())?)
        .await?
    {
        WikiLookup::Redirect(target) => Ok(target),
        WikiLookup::Page(_) => Err("child page remained".into()),
        WikiLookup::Missing => Err("child redirect is missing".into()),
    }
}

fn summary_patch(
    action: WikiAction,
    summary: &str,
) -> Result<WikiPatch, Box<dyn std::error::Error>> {
    Ok(WikiPatch {
        action,
        slug: WikiSlug::try_from("acmekit".to_owned())?,
        title: "AcmeKit".to_owned(),
        aliases: Vec::new(),
        tags: Vec::new(),
        entities: Vec::new(),
        summary: summary.to_owned(),
        article: Some("Ships from the build box.".to_owned()),
        related: None,
        history_entry: None,
        entity_type: None,
        facts: None,
    })
}

async fn stored_page<S>(
    store: &S,
    slug: &WikiSlug,
) -> Result<WikiPageResponse, Box<dyn std::error::Error>>
where
    S: MemoryStore,
{
    match store.wiki_page(slug).await? {
        WikiLookup::Page(page) => Ok(*page),
        WikiLookup::Redirect(target) => Err(format!("redirect to {target}").into()),
        WikiLookup::Missing => Err("wiki page is missing".into()),
    }
}
