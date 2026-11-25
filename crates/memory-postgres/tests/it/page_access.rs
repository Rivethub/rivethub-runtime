use std::path::Path;

use jiff::Timestamp;
use kernel::memory::MemoryStore as _;
use kernel::owner::Owner;
use kernel::tenancy::Scope;
use kernel::tenancy::SpaceId;
use kernel::tenancy::Viewer;
use kernel::topics::SweepOwners;
use kernel::topics::SyncTopicPage;
use kernel::topics::TopicId;
use kernel::topics::TopicPage;
use kernel::topics::TopicPageStore as _;
use kernel::topics::sync_topic_page;
use kernel::wiki::WikiLookup;
use kernel::wiki::builder::ExtractWiki;
use kernel::wiki::builder::WikiBuilderStore as _;
use kernel::wiki::builder::WikiRouting;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::builder::WikiWork;
use kernel::wiki::consolidate::WikiConsolidateStore as _;
use kernel::wiki::consolidate::consolidation_plan;
use kernel::wiki::page::FoldMark;
use kernel::wiki::page::WikiPage;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::serialize_wiki_page;
use kernel::wiki::recompile::Recompiled;
use kernel::wiki::recompile::WikiRecompileStore as _;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;

use crate::common;

type TestResult = Result<(), Box<dyn std::error::Error>>;

pub(crate) struct Page<'a> {
    pub(crate) key: &'a str,
    pub(crate) local: &'a str,
    pub(crate) scope: &'a str,
    pub(crate) owner: &'a str,
    pub(crate) visibility: &'a str,
    pub(crate) directory: &'a str,
    pub(crate) prose: &'a str,
    pub(crate) row: &'a str,
}

pub(crate) const GPUBOX: Page<'static> = Page {
    key: "gpubox",
    local: "gpubox",
    scope: "phil",
    owner: "phil",
    visibility: "private",
    directory: "",
    prose: "Phil's private GPU host.",
    row: "Phil's GPU host row.",
};

const RUNBOOK: Page<'static> = Page {
    key: "crew-runbook",
    local: "runbook",
    scope: "crew",
    owner: "crew",
    visibility: "crew",
    directory: "spaces/crew",
    prose: "The crew runbook.",
    row: "The crew runbook row.",
};

const COCOS_GPUBOX: Page<'static> = Page {
    key: "coco-gpubox",
    local: "gpubox",
    scope: "coco",
    owner: "coco",
    visibility: "crew",
    directory: "users/coco",
    prose: "Coco's GPU notes.",
    row: "Coco's GPU row.",
};

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_page_reads_only_for_viewers_who_see_its_row(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    seed(&pool, directory.path(), &GPUBOX).await?;
    seed(&pool, directory.path(), &RUNBOOK).await?;

    assert_eq!(
        summary(&memory, "phil", &GPUBOX).await?.as_deref(),
        Some(GPUBOX.prose),
        "the owner reads the page"
    );
    assert_eq!(
        summary(&memory, "coco", &GPUBOX).await?,
        None,
        "a crew member does not read phil's private page"
    );
    assert_eq!(
        summary(&memory, "coco", &RUNBOOK).await?.as_deref(),
        Some(RUNBOOK.prose),
        "a crew member reads the crew page"
    );
    assert_eq!(
        summary(&memory, "dana", &RUNBOOK).await?,
        None,
        "someone outside the crew does not read the crew page"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_archive_reads_only_for_viewers_who_see_its_row(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    seed(&pool, directory.path(), &GPUBOX).await?;
    let archives = directory.path().join("archive");
    std::fs::create_dir_all(&archives)?;
    std::fs::write(archives.join("gpubox.md"), "Phil's old GPU notes.\n")?;
    let slug = WikiSlug::try_from(GPUBOX.key.to_owned())?;

    let owner = viewer("phil")?
        .run(memory.wiki_archive(&slug, None))
        .await?;
    let crewmate = viewer("coco")?
        .run(memory.wiki_archive(&slug, None))
        .await?;

    assert!(
        owner.is_some_and(|text| text.contains("Phil's old GPU notes.")),
        "the owner reads the archive"
    );
    assert_eq!(crewmate, None, "a viewer without the row reads no archive");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_visible_row_in_a_hidden_scope_never_reads_the_primary_wiki(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    seed(&pool, directory.path(), &GPUBOX).await?;
    seed(&pool, directory.path(), &COCOS_GPUBOX).await?;
    let slug = WikiSlug::try_from(COCOS_GPUBOX.key.to_owned())?;
    let rose = viewer("rose")?;

    let page = rose.clone().run(memory.topic_page(&slug)).await?;
    let WikiLookup::Page(read) = rose.run(memory.wiki_page(&slug)).await? else {
        return Err("rose sees coco's crew page row".into());
    };

    assert_eq!(
        page.map(|page| page.summary).as_deref(),
        Some(COCOS_GPUBOX.row),
        "rose reads the row, not a file"
    );
    assert!(
        read.markdown.contains(COCOS_GPUBOX.row)
            && !read.markdown.contains(GPUBOX.prose)
            && !read.markdown.contains(COCOS_GPUBOX.prose),
        "the page read serves the row only, got {:?}",
        read.markdown
    );
    assert_eq!(
        summary(&memory, "coco", &COCOS_GPUBOX).await?.as_deref(),
        Some(COCOS_GPUBOX.prose),
        "coco reads her own page from her wiki"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_legacy_row_reads_the_primary_wiki_only_for_its_owner(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    seed(&pool, directory.path(), &GPUBOX).await?;
    for (key, owner, visibility, row) in [
        ("diary", "coco", "crew", "Coco's diary row."),
        ("notes", "phil", "crew", "Phil's notes row."),
    ] {
        sqlx::query(
            "INSERT INTO rh_wiki_topics (slug, title, current_state, owner_user_id, visibility)
             VALUES ($1, $1, $2, $3, $4)",
        )
        .bind(key)
        .bind(row)
        .bind(owner)
        .bind(visibility)
        .execute(&pool)
        .await?;
    }
    write_file(directory.path(), "", "diary", "Phil's private diary.")?;
    write_file(directory.path(), "", "notes", "Phil's notes file.")?;
    let diary = WikiSlug::try_from("diary".to_owned())?;
    let rose = viewer("rose")?;

    let page = rose.clone().run(memory.topic_page(&diary)).await?;
    let WikiLookup::Page(read) = rose.run(memory.wiki_page(&diary)).await? else {
        return Err("rose sees coco's legacy row".into());
    };
    let notes = viewer("coco")?
        .run(memory.topic_page(&WikiSlug::try_from("notes".to_owned())?))
        .await?;

    assert_eq!(
        page.map(|page| page.summary).as_deref(),
        Some("Coco's diary row."),
        "coco's legacy row never maps to phil's wiki"
    );
    assert!(
        read.markdown.contains("Coco's diary row.")
            && !read.markdown.contains("Phil's private diary."),
        "the page read serves the row only, got {:?}",
        read.markdown
    );
    assert_eq!(
        notes.map(|page| page.summary).as_deref(),
        Some("Phil's notes file."),
        "phil's shared legacy page reads from his wiki"
    );
    Ok(())
}

pub(crate) const ALPHA: Page<'static> = Page {
    key: "crew-alpha",
    local: "alpha",
    scope: "crew",
    owner: "crew",
    visibility: "crew",
    directory: "spaces/crew",
    prose: "Alpha file.",
    row: "Alpha row.",
};

pub(crate) const BETA: Page<'static> = Page {
    key: "crew-beta",
    local: "beta",
    scope: "crew",
    owner: "crew",
    visibility: "crew",
    directory: "spaces/crew",
    prose: "Beta file.",
    row: "Beta row.",
};

const PHILS_ALPHA: Page<'static> = Page {
    key: "crew-alpha-2",
    local: "crew-alpha",
    scope: "phil",
    owner: "phil",
    visibility: "private",
    directory: "",
    prose: "Phil's alpha secret.",
    row: "Phil's alpha row.",
};

const PHILS_BETA: Page<'static> = Page {
    key: "crew-beta-2",
    local: "crew-beta",
    scope: "phil",
    owner: "phil",
    visibility: "private",
    directory: "",
    prose: "Phil's beta secret.",
    row: "Phil's beta row.",
};

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn folding_space_pages_touches_only_the_space_wiki(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    for page in [&ALPHA, &BETA, &PHILS_ALPHA, &PHILS_BETA] {
        seed(&pool, directory.path(), page).await?;
    }
    let phils_alpha = directory.path().join("topics/crew-alpha.md");
    let phils_beta = directory.path().join("topics/crew-beta.md");
    let before = std::fs::read_to_string(&phils_alpha)?;

    viewer("coco")?
        .run(memory.fold_page(
            &WikiSlug::try_from(BETA.key.to_owned())?,
            &WikiSlug::try_from(ALPHA.key.to_owned())?,
            FoldMark::Merged,
            Timestamp::now(),
        ))
        .await?;

    assert_eq!(std::fs::read_to_string(&phils_alpha)?, before);
    assert!(phils_beta.exists(), "phil's beta file stays");
    let alpha = std::fs::read_to_string(directory.path().join("spaces/crew/topics/alpha.md"))?;
    assert!(
        alpha.contains(BETA.prose) && !alpha.contains(PHILS_BETA.prose),
        "the crew alpha page absorbs the crew beta file, got {alpha:?}"
    );
    assert!(
        !directory.path().join("spaces/crew/topics/beta.md").exists(),
        "the crew beta file is folded away"
    );
    let row: String =
        sqlx::query_scalar("SELECT current_state || article FROM rh_wiki_topics WHERE slug = $1")
            .bind(ALPHA.key)
            .fetch_one(&pool)
            .await?;
    assert!(
        !row.contains(PHILS_ALPHA.prose) && !row.contains(PHILS_BETA.prose),
        "the crew row holds none of phil's prose, got {row:?}"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_fold_across_wikis_is_refused(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    seed(&pool, directory.path(), &ALPHA).await?;
    seed(&pool, directory.path(), &COCOS_GPUBOX).await?;
    let cocos = directory.path().join("users/coco/topics/gpubox.md");
    let before = std::fs::read_to_string(&cocos)?;

    let folded = viewer("coco")?
        .run(memory.fold_page(
            &WikiSlug::try_from(COCOS_GPUBOX.key.to_owned())?,
            &WikiSlug::try_from(ALPHA.key.to_owned())?,
            FoldMark::Merged,
            Timestamp::now(),
        ))
        .await;

    assert!(folded.is_err(), "got {folded:?}");
    assert_eq!(std::fs::read_to_string(&cocos)?, before);
    assert_eq!(
        summary(&memory, "coco", &COCOS_GPUBOX).await?.as_deref(),
        Some(COCOS_GPUBOX.prose)
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_topic_sync_never_writes_over_a_file_it_has_no_row_for(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    let runbook = Page {
        key: "runbook",
        local: "runbook",
        scope: "phil",
        owner: "phil",
        visibility: "private",
        directory: "",
        prose: "Phil's runbook.",
        row: "Phil's runbook row.",
    };
    seed(&pool, directory.path(), &RUNBOOK).await?;
    seed(&pool, directory.path(), &runbook).await?;
    let topic = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at, owner_user_id, visibility_mark)
         VALUES ($1, 'work', 'runbook', 'Runbook', now(), now(), 'coco', 'crew')",
    )
    .bind(topic)
    .execute(&pool)
    .await?;
    let repository = directory.path().join("spaces/crew");
    let crew = repository.join("topics/runbook.md");
    let before = std::fs::read_to_string(&crew)?;
    let touched = last_commit_of(&repository, "topics/runbook.md")?;

    viewer("coco")?
        .run(sync_topic_page(
            &memory,
            &memory,
            &memory,
            &SyncTopicPage::topic(TopicId::from_uuid(topic)),
            SweepOwners::All,
            Timestamp::now(),
        ))
        .await?;

    assert_eq!(std::fs::read_to_string(&crew)?, before);
    assert_eq!(last_commit_of(&repository, "topics/runbook.md")?, touched);
    let placed = viewer("coco")?
        .run(memory.topic_page(&WikiSlug::try_from("runbook-2".to_owned())?))
        .await?;
    assert!(placed.is_some(), "the topic takes the next free slug");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn removing_a_page_leaves_a_file_the_remover_cannot_see(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    seed(&pool, directory.path(), &GPUBOX).await?;
    seed(&pool, directory.path(), &COCOS_GPUBOX).await?;
    let file = directory.path().join("topics/gpubox.md");
    let cocos_file = directory.path().join("users/coco/topics/gpubox.md");

    let removed = viewer("coco")?
        .run(memory.remove_page(&WikiSlug::try_from(GPUBOX.key.to_owned())?))
        .await;
    let theirs = viewer("coco")?
        .run(memory.remove_page(&WikiSlug::try_from(COCOS_GPUBOX.key.to_owned())?))
        .await;

    assert!(removed.is_ok(), "got {removed:?}");
    assert!(theirs.is_ok(), "got {theirs:?}");
    assert!(file.exists(), "phil's page file stays");
    assert_eq!(
        summary(&memory, "phil", &GPUBOX).await?.as_deref(),
        Some(GPUBOX.prose),
        "phil still reads his page"
    );
    assert!(!cocos_file.exists(), "coco removes her own page file");
    assert_eq!(summary(&memory, "coco", &COCOS_GPUBOX).await?, None);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_recompile_targets_the_primary_wiki_page_by_its_file_name(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    for page in [&ALPHA, &BETA, &PHILS_ALPHA] {
        seed(&pool, directory.path(), page).await?;
    }
    write_file(directory.path(), "", "crew-beta", "Phil's loose beta file.")?;
    write_file(directory.path(), "", "orphan", "An orphan file.")?;
    sqlx::query(
        "INSERT INTO rh_wiki_topics (slug, title, current_state, owner_user_id, visibility)
         VALUES ('diary', 'diary', 'Coco''s diary row.', 'coco', 'crew')",
    )
    .execute(&pool)
    .await?;
    write_file(directory.path(), "", "diary", "Phil's private diary.")?;
    let key = |local: &str| {
        let local = WikiSlug::try_from(local.to_owned());
        let memory = &memory;
        async move {
            Scope::Maintenance
                .run(memory.primary_page_key(&local?))
                .await
                .map_err(Box::<dyn std::error::Error>::from)
        }
    };

    assert_eq!(
        key("crew-alpha").await?.as_ref().map(AsRef::as_ref),
        Some(PHILS_ALPHA.key)
    );
    assert_eq!(
        key("crew-beta").await?,
        None,
        "a loose file under a space key is skipped"
    );
    assert_eq!(
        key("orphan").await?.as_ref().map(AsRef::as_ref),
        Some("orphan")
    );
    let source = Scope::Maintenance
        .run(memory.recompile_source(
            &WikiSlug::try_from(PHILS_ALPHA.key.to_owned())?,
            Timestamp::UNIX_EPOCH,
        ))
        .await?
        .ok_or("phil's page recompiles")?;
    assert_eq!(source.page.current_state, PHILS_ALPHA.prose);
    let diary = Scope::Maintenance
        .run(memory.recompile_source(
            &WikiSlug::try_from("diary".to_owned())?,
            Timestamp::UNIX_EPOCH,
        ))
        .await?;
    assert_eq!(
        diary, None,
        "coco's legacy row never recompiles phil's file"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_summary_cites_no_page_its_owner_cannot_see(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    seed(&pool, directory.path(), &GPUBOX).await?;
    let secret = Page {
        key: "coco-secret",
        local: "secret",
        scope: "coco",
        owner: "coco",
        visibility: "private",
        directory: "users/coco",
        prose: "Coco's secret.",
        row: "Coco's secret row.",
    };
    seed(&pool, directory.path(), &secret).await?;
    let conversation = uuid::Uuid::new_v4();
    let summary = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO rh_conversations (id, session_key, agent, channel, owner_user_id)
         VALUES ($1, 'phil-session', 'fixture', 'cli', 'phil')",
    )
    .bind(conversation)
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO rh_summaries (id, conversation_id, content, owner_user_id)
         VALUES ($1, $2, 'Phil tuned the GPU host.', 'phil')",
    )
    .bind(summary)
    .bind(conversation)
    .execute(&pool)
    .await?;
    let job = ExtractWiki {
        summary_id: summary,
        conversation_id: Some(conversation),
    };
    let phil = viewer("phil")?;

    phil.run(async {
        let WikiWork::Pending(input) = memory.wiki_work(&job).await? else {
            return Err::<(), Box<dyn std::error::Error>>("phil's summary is pending".into());
        };
        memory
            .apply_wiki(
                &job,
                Vec::new(),
                &WikiRouting {
                    input: Some(input),
                    cited: vec![
                        WikiSlug::try_from(GPUBOX.key.to_owned())?,
                        WikiSlug::try_from(secret.key.to_owned())?,
                    ],
                    ..WikiRouting::default()
                },
                Timestamp::now(),
            )
            .await?;
        Ok(())
    })
    .await?;

    let cited: Vec<String> = sqlx::query_scalar(
        "SELECT topic_slug FROM rh_wiki_provenance WHERE source_id = $1 ORDER BY topic_slug",
    )
    .bind(summary)
    .fetch_all(&pool)
    .await?;
    assert_eq!(cited, vec![GPUBOX.key.to_owned()]);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_legacy_row_never_reads_a_root_wiki_recorded_for_another_owner(
    pool: PgPool,
) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub_rooted(&pool, directory.path(), "coco").await?;
    legacy_row(&pool, "diary", "phil", "private", "Phil's diary row.").await?;
    write_file(directory.path(), "", "diary", "Coco's root diary.")?;
    let diary = WikiSlug::try_from("diary".to_owned())?;
    let phil = viewer("phil")?;

    let page = phil.clone().run(memory.topic_page(&diary)).await?;
    let WikiLookup::Page(read) = phil.run(memory.wiki_page(&diary)).await? else {
        return Err("phil sees his legacy row".into());
    };

    assert_eq!(
        page.map(|page| page.summary).as_deref(),
        Some("Phil's diary row."),
        "the configured owner reads the row, not coco's root file"
    );
    assert!(
        !read.markdown.contains("Coco's root diary."),
        "the page read serves the row only, got {:?}",
        read.markdown
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_new_page_never_claims_an_unreadable_file(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    write_file(directory.path(), "spaces/crew", "seed", "Seed.")?;
    let file = directory.path().join("spaces/crew/topics/runbook.md");
    std::fs::write(&file, "not a wiki page\n")?;

    let saved = save_crew_runbook(&pool, &memory).await;

    assert!(
        matches!(saved, Err(WikiStoreError::SlugConflict(_))),
        "got {saved:?}"
    );
    assert_eq!(std::fs::read_to_string(&file)?, "not a wiki page\n");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_new_page_never_claims_a_file_the_sync_brings_in(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    let remote = directory.path().join("remote.git");
    let other = directory.path().join("other");
    git(
        directory.path(),
        &["init", "-q", "--bare", "-b", "main", "remote.git"],
    )?;
    git(directory.path(), &["clone", "-q", "remote.git", "other"])?;
    write_file(&other, "", "seed", "Seed.")?;
    git(&other, &["push", "-q", "origin", "main"])?;
    std::fs::create_dir_all(directory.path().join("spaces"))?;
    git(
        &directory.path().join("spaces"),
        &["clone", "-q", &remote.to_string_lossy(), "crew"],
    )?;
    write_file(&other, "", "runbook", "Phil's runbook from another hub.")?;
    git(&other, &["push", "-q", "origin", "main"])?;
    let file = directory.path().join("spaces/crew/topics/runbook.md");

    let saved = save_crew_runbook(&pool, &memory).await;

    assert!(
        matches!(saved, Err(WikiStoreError::SlugConflict(_))),
        "got {saved:?}"
    );
    assert!(
        std::fs::read_to_string(&file)?.contains("Phil's runbook from another hub."),
        "the synced file keeps its prose"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_new_page_never_claims_a_file_under_its_truncated_name(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    let truncated = format!("runbook-{}", "x".repeat(72));
    write_file(
        directory.path(),
        "spaces/crew",
        &truncated,
        "Phil's crew notes.",
    )?;
    let crew = directory.path().join("spaces/crew");
    let file = crew.join(format!("topics/{truncated}.md"));
    let before = std::fs::read_to_string(&file)?;
    let head = git_head(&crew)?;
    let long = format!("{truncated}-tail");

    let saved = save_crew_page(&pool, &memory, &long).await;

    assert!(
        matches!(saved, Err(WikiStoreError::SlugConflict(_))),
        "got {saved:?}"
    );
    assert_eq!(std::fs::read_to_string(&file)?, before);
    assert_eq!(git_head(&crew)?, head);
    let read = viewer("coco")?
        .run(memory.topic_page(&WikiSlug::try_from(long)?))
        .await?;
    assert_eq!(read.map(|page| page.summary), None);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_new_page_never_claims_a_slug_with_an_archive_left_behind(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    write_file(directory.path(), "spaces/crew", "seed", "Seed.")?;
    let crew = directory.path().join("spaces/crew");
    leave_archives(&crew, &["archive/runbook.md", "archive/notes/2026-01.md"])?;
    let archives = archive_files(&crew)?;
    let head = git_head(&crew)?;

    for slug in ["runbook", "notes"] {
        let saved = save_crew_page(&pool, &memory, slug).await;

        assert!(
            matches!(saved, Err(WikiStoreError::SlugConflict(_))),
            "{slug}: got {saved:?}"
        );
        assert!(!crew.join(format!("topics/{slug}.md")).exists());
        let read = viewer("coco")?
            .run(memory.wiki_archive(&WikiSlug::try_from(slug.to_owned())?, None))
            .await?;
        assert_eq!(read, None, "{slug}: coco reads no archive");
    }
    assert_eq!(git_head(&crew)?, head);
    assert_eq!(archive_files(&crew)?, archives);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_topic_sync_skips_slugs_with_an_archive_left_behind(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    write_file(directory.path(), "spaces/crew", "seed", "Seed.")?;
    let crew = directory.path().join("spaces/crew");
    leave_archives(
        &crew,
        &["archive/runbook.md", "archive/runbook-2/2026-01.md"],
    )?;
    let archives = archive_files(&crew)?;
    let topic = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at, owner_user_id, visibility_mark)
         VALUES ($1, 'work', 'runbook', 'Runbook', now(), now(), 'coco', 'crew')",
    )
    .bind(topic)
    .execute(&pool)
    .await?;

    viewer("coco")?
        .run(sync_topic_page(
            &memory,
            &memory,
            &memory,
            &SyncTopicPage::topic(TopicId::from_uuid(topic)),
            SweepOwners::All,
            Timestamp::now(),
        ))
        .await?;

    for slug in ["runbook", "runbook-2"] {
        let slug = WikiSlug::try_from(slug.to_owned())?;
        assert!(!crew.join(format!("topics/{}.md", slug.as_ref())).exists());
        let page = viewer("coco")?.run(memory.topic_page(&slug)).await?;
        assert!(page.is_none(), "{slug:?} stays unclaimed");
        let read = viewer("coco")?
            .run(memory.wiki_archive(&slug, None))
            .await?;
        assert_eq!(read, None, "{slug:?}: coco reads no archive");
    }
    let placed = viewer("coco")?
        .run(memory.topic_page(&WikiSlug::try_from("runbook-3".to_owned())?))
        .await?;
    assert!(placed.is_some(), "the topic takes the next free slug");
    assert_eq!(archive_files(&crew)?, archives);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_new_over_long_page_reads_back_from_the_file_it_wrote(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    write_file(directory.path(), "spaces/crew", "seed", "Seed.")?;
    let truncated = format!("runbook-{}", "x".repeat(72));
    let long = format!("{truncated}-tail");

    save_crew_page(&pool, &memory, &long).await?;

    let read = viewer("coco")?
        .run(memory.topic_page(&WikiSlug::try_from(long.clone())?))
        .await?;
    assert_eq!(
        read.map(|page| page.summary).as_deref(),
        Some("Coco's runbook.")
    );
    let topics = directory.path().join("spaces/crew/topics");
    assert!(topics.join(format!("{truncated}.md")).exists());
    assert!(!topics.join(format!("{long}.md")).exists());
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_restored_file_recovers_into_the_primary_owners_scope(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = PostgresMemory::new(common::probe(&pool).await?)
        .with_wiki_directory(directory.path().to_path_buf())
        .with_wiki_owner(Owner::new("phil".to_owned())?);
    write_file(directory.path(), "", "orphan", "A restored file.")?;
    let slug = WikiSlug::try_from("orphan".to_owned())?;

    let source = Scope::Maintenance
        .run(memory.recompile_source(&slug, Timestamp::UNIX_EPOCH))
        .await?
        .ok_or("the restored file recompiles")?;
    Scope::Maintenance
        .run(memory.store_recompiled(
            Recompiled {
                inputs: Vec::new(),
                base: source.page.clone(),
                page: source.page,
                archived: Vec::new(),
                message: "recompile".to_owned(),
            },
            Timestamp::now(),
        ))
        .await?;

    let row: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT scope, owner_user_id, local_slug FROM rh_wiki_topics WHERE slug = 'orphan'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        row,
        (
            Some("phil".to_owned()),
            Some("phil".to_owned()),
            Some("orphan".to_owned())
        )
    );
    let read = viewer("phil")?.run(memory.topic_page(&slug)).await?;
    assert_eq!(
        read.map(|page| page.summary).as_deref(),
        Some("A restored file.")
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_restored_file_is_skipped_while_the_primary_owner_is_contested(
    pool: PgPool,
) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub_rooted(&pool, directory.path(), "coco").await?;
    write_file(directory.path(), "", "orphan", "A restored file.")?;

    let source = Scope::Maintenance
        .run(memory.recompile_source(
            &WikiSlug::try_from("orphan".to_owned())?,
            Timestamp::UNIX_EPOCH,
        ))
        .await?;

    assert_eq!(source, None);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_wiki_topics")
        .fetch_one(&pool)
        .await?;
    assert_eq!(rows, 0);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn consolidation_skips_a_page_without_a_verified_home(pool: PgPool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let memory = hub(&pool, directory.path()).await?;
    for (key, prose) in [
        ("mesh-host", "Mesh host."),
        ("mesh-host-notes", "Mesh notes."),
    ] {
        let page = Page {
            key,
            local: key,
            prose,
            row: prose,
            ..GPUBOX
        };
        seed(&pool, directory.path(), &page).await?;
    }
    legacy_row(
        &pool,
        "mesh-host-diary",
        "coco",
        "crew",
        "Coco's diary row.",
    )
    .await?;
    write_file(
        directory.path(),
        "",
        "mesh-host-diary",
        "Phil's diary file.",
    )?;
    let diary = directory.path().join("topics/mesh-host-diary.md");

    let folded = Scope::Maintenance
        .run(async {
            let stats = memory.wiki_topic_stats().await?;
            let mut folded = Vec::new();
            for fold in consolidation_plan(&stats, &serde_json::from_str("{}")?) {
                folded.extend(memory.fold_wiki(&fold, Timestamp::now()).await?);
            }
            Ok::<_, Box<dyn std::error::Error>>(folded)
        })
        .await?;

    assert_eq!(folded, vec!["mesh-host-notes".to_owned()]);
    assert!(diary.exists(), "the unverified page file stays");
    let kept: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM rh_wiki_topics WHERE slug = 'mesh-host-diary')",
    )
    .fetch_one(&pool)
    .await?;
    assert!(kept, "the unverified page row stays");
    Ok(())
}

pub(crate) async fn legacy_row(
    pool: &PgPool,
    key: &str,
    owner: &str,
    visibility: &str,
    row: &str,
) -> TestResult {
    sqlx::query(
        "INSERT INTO rh_wiki_topics (slug, title, current_state, owner_user_id, visibility)
         VALUES ($1, $1, $2, $3, $4)",
    )
    .bind(key)
    .bind(row)
    .bind(owner)
    .bind(visibility)
    .execute(pool)
    .await?;
    Ok(())
}

async fn save_crew_runbook(pool: &PgPool, memory: &PostgresMemory) -> Result<(), WikiStoreError> {
    save_crew_page(pool, memory, "runbook").await
}

async fn save_crew_page(
    pool: &PgPool,
    memory: &PostgresMemory,
    slug: &str,
) -> Result<(), WikiStoreError> {
    let topic = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at, owner_user_id, visibility_mark)
         VALUES ($1, 'work', $2, 'Runbook', now(), now(), 'coco', 'crew')",
    )
    .bind(topic)
    .bind(slug)
    .execute(pool)
    .await
    .map_err(|error| WikiStoreError::Query(Box::new(error)))?;
    let page = TopicPage {
        slug: WikiSlug::try_from(slug.to_owned())
            .map_err(|error| WikiStoreError::InvalidPatch(error.to_string()))?,
        title: "Runbook".to_owned(),
        summary: "Coco's runbook.".to_owned(),
        article: String::new(),
        sections: Vec::new(),
    };
    let coco = viewer("coco").map_err(|error| WikiStoreError::InvalidPatch(error.to_string()))?;
    coco.run(memory.save_topic_page(TopicId::from_uuid(topic), &page, Timestamp::now()))
        .await
}

fn git(directory: &Path, args: &[&str]) -> TestResult {
    let status = std::process::Command::new("git")
        .current_dir(directory)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .status()?;
    assert!(status.success(), "git {args:?} failed");
    Ok(())
}

pub(crate) async fn hub(
    pool: &PgPool,
    directory: &Path,
) -> Result<PostgresMemory, Box<dyn std::error::Error>> {
    hub_rooted(pool, directory, "phil").await
}

async fn hub_rooted(
    pool: &PgPool,
    directory: &Path,
    root: &str,
) -> Result<PostgresMemory, Box<dyn std::error::Error>> {
    sqlx::query("INSERT INTO rh_spaces (id, kind, name) VALUES ('crew', 'team', 'Crew')")
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO rh_space_members (space_id, user_id) VALUES ('crew', 'phil'), ('crew', 'coco'), ('crew', 'rose')",
    )
    .execute(pool)
    .await?;
    for (scope, visibility) in [("phil", "private"), ("coco", "private"), ("crew", "crew")] {
        let (prefix, home) = match (scope, visibility) {
            (_, "crew") => (format!("{scope}-"), format!("spaces/{scope}")),
            _ if scope == root => (String::new(), String::new()),
            _ => (format!("{scope}-"), format!("users/{scope}")),
        };
        sqlx::query(
            "INSERT INTO rh_wiki_scopes (scope, owner_user_id, visibility, key_prefix, directory)
             VALUES ($1, $1, $2, $3, $4)",
        )
        .bind(scope)
        .bind(visibility)
        .bind(&prefix)
        .bind(&home)
        .execute(pool)
        .await?;
    }
    Ok(PostgresMemory::new(common::probe(pool).await?)
        .with_wiki_directory(directory.to_path_buf())
        .with_wiki_owner(Owner::new("phil".to_owned())?))
}

pub(crate) async fn seed(pool: &PgPool, wiki: &Path, page: &Page<'_>) -> TestResult {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('rivet.defer_embed_enqueue', 'on', true)")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO rh_wiki_topics
            (slug, title, current_state, owner_user_id, scope, local_slug, visibility)
         VALUES ($1, $2, $3, $4, $5, $2, $6)",
    )
    .bind(page.key)
    .bind(page.local)
    .bind(page.row)
    .bind(page.owner)
    .bind(page.scope)
    .bind(page.visibility)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    write_file(wiki, page.directory, page.local, page.prose)
}

pub(crate) fn write_file(wiki: &Path, directory: &str, local: &str, prose: &str) -> TestResult {
    let topics = wiki.join(directory).join("topics");
    std::fs::create_dir_all(&topics)?;
    let mut file = WikiPage::empty(WikiSlug::try_from(local.to_owned())?, local.to_owned());
    prose.clone_into(&mut file.current_state);
    std::fs::write(
        topics.join(format!("{local}.md")),
        serialize_wiki_page(&file)?,
    )?;
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "topics"],
        &["commit", "-q", "-m", local],
    ] {
        let status = std::process::Command::new("git")
            .current_dir(wiki.join(directory))
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .status()?;
        assert!(status.success(), "git {args:?} failed");
    }
    Ok(())
}

type ArchiveFile = (String, String, String);

fn leave_archives(repository: &Path, paths: &[&str]) -> TestResult {
    for path in paths {
        let file = repository.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, format!("Phil's archive at {path}.\n"))?;
    }
    git(repository, &["add", "archive"])?;
    git(repository, &["commit", "-q", "-m", "phil's archives"])
}

fn archive_files(repository: &Path) -> Result<Vec<ArchiveFile>, Box<dyn std::error::Error>> {
    let listed = std::process::Command::new("git")
        .current_dir(repository)
        .args(["ls-files", "--cached", "--others", "--", "archive"])
        .output()?;
    String::from_utf8(listed.stdout)?
        .lines()
        .map(|path| {
            let text = std::fs::read_to_string(repository.join(path))?;
            Ok((path.to_owned(), text, last_commit_of(repository, path)?))
        })
        .collect()
}

fn last_commit_of(repository: &Path, path: &str) -> Result<String, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .current_dir(repository)
        .args(["log", "-1", "--format=%H", "--", path])
        .output()?;
    Ok(String::from_utf8(output.stdout)?)
}

fn git_head(repository: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .current_dir(repository)
        .args(["rev-parse", "HEAD"])
        .output()?;
    Ok(String::from_utf8(output.stdout)?)
}

async fn summary(
    memory: &PostgresMemory,
    name: &str,
    page: &Page<'_>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let slug = WikiSlug::try_from(page.key.to_owned())?;
    let read = viewer(name)?.run(memory.topic_page(&slug)).await?;
    Ok(read.map(|page| page.summary))
}

pub(crate) fn viewer(name: &str) -> Result<Scope, Box<dyn std::error::Error>> {
    let spaces = match name {
        "dana" => Vec::new(),
        _ => vec![SpaceId::new("crew".to_owned())?],
    };
    Ok(Scope::Viewer(Viewer::new(
        Owner::new(name.to_owned())?,
        spaces,
    )))
}
