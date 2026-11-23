use std::path::Path;
use std::path::PathBuf;

use kernel::memory::MemoryStore;
use kernel::topics::NewTopic;
use kernel::topics::PageEdit;
use kernel::topics::Topic;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
use kernel::topics::TopicPageStore;
use kernel::topics::TopicStore;
use kernel::wiki::WikiLookup;
use kernel::wiki::builder::ExtractWiki;
use kernel::wiki::builder::WikiBuilderStore;
use kernel::wiki::builder::WikiRouting;
use kernel::wiki::consolidate::WikiConsolidateStore;
use kernel::wiki::consolidate::WikiFold;
use kernel::wiki::entity_pass::EntityStore;
use kernel::wiki::entity_pass::FactState;
use kernel::wiki::entity_pass::PreparedFact;
use kernel::wiki::entity_pass::PreparedHistory;
use kernel::wiki::entity_pass::StoredRewrite;
use kernel::wiki::page::FoldMark;
use kernel::wiki::page::ReadSection;
use kernel::wiki::page::WikiAction;
use kernel::wiki::page::WikiPatch;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::format_wiki_read;
use memory_sqlite::SqliteError;
use memory_sqlite::SqliteStore;
use rusqlite::params;
use tempfile::TempDir;
use testkit::wiki_files::CONVERSATION;
use wire::time::TimestampFormat as _;

use crate::common::TestResult;

struct Opened {
    _dir: TempDir,
    store: SqliteStore,
    wiki: PathBuf,
}

#[tokio::test]
async fn database_only_pages_keep_their_articles() -> TestResult {
    let opened = open()?;
    extraction_keeps_the_article(&opened).await?;
    edit_and_change_keep_the_article(&opened).await?;
    fold_keeps_the_child_section(&opened).await?;
    entity_keeps_the_old_article(&opened).await?;
    let lookup = opened
        .store
        .wiki_page(&WikiSlug::try_from("legacy-read".to_owned())?)
        .await?;
    let kernel::wiki::WikiLookup::Page(page) = lookup else {
        return Err("database-only page was not readable".into());
    };
    let article = format_wiki_read(&page.markdown, "legacy-read", Some(ReadSection::Article))?;
    assert!(article.contains("Readable article."), "{article}");
    assert!(!article.contains("malformed"), "{article}");
    assert!(
        page.markdown.contains("Readable section."),
        "{}",
        page.markdown
    );
    Ok(())
}

#[tokio::test]
async fn database_only_page_keeps_every_stored_field() -> TestResult {
    let opened = open()?;
    let topic = owned(&opened, "Stored fields").await?;
    insert_page(
        &opened.store,
        StoredPage {
            slug: "stored-fields",
            title: "Stored fields",
            summary: "Old summary.",
            article: "Row article.",
            sections: Some(
                r#"[{"topic":null,"heading":"Bench notes","body":"Section from the row."}]"#,
            ),
            topic: Some(&topic),
        },
    )
    .await?;
    stamp_fields(&opened.store, "stored-fields").await?;
    let slug = WikiSlug::try_from("stored-fields".to_owned())?;
    let edited = opened
        .store
        .edit_page(
            &slug,
            &PageEdit {
                current_state: Some("Edited summary.".to_owned()),
                article: None,
            },
            stamp()?,
        )
        .await?;
    assert!(edited);
    let text = std::fs::read_to_string(opened.wiki.join("topics").join("stored-fields.md"))?;
    assert!(text.contains("bench-tag"), "{text}");
    assert!(text.contains("Kept alias"), "{text}");
    assert!(text.contains("Keptbox"), "{text}");
    assert!(text.contains("[[other-page]]"), "{text}");
    assert!(text.contains("Section from the row."), "{text}");
    assert!(text.contains("Row article."), "{text}");
    assert!(
        text.contains("last_verified: 2026-10-10T12:00:00.000Z"),
        "{text}"
    );
    assert!(text.contains("Edited summary."), "{text}");
    let shown = git(
        &opened.wiki,
        &["log", "-p", "--all", "--grep=record the stored page"],
    )?;
    assert!(shown.contains("Cited from the row."), "{shown}");
    assert!(shown.contains("row-source"), "{shown}");
    Ok(())
}

#[tokio::test]
async fn a_dirty_worktree_file_is_not_replaced_from_the_database() -> TestResult {
    let opened = open()?;
    let topic = owned(&opened, "Dirty page").await?;
    insert_page(
        &opened.store,
        StoredPage {
            slug: "dirty-page",
            title: "Dirty page",
            summary: "Database summary.",
            article: "Database article.",
            sections: None,
            topic: Some(&topic),
        },
    )
    .await?;
    let path = opened.wiki.join("topics").join("dirty-page.md");
    std::fs::create_dir_all(opened.wiki.join("topics"))?;
    let draft = draft("dirty-page", "Dirty page", "Worktree draft.");
    std::fs::write(&path, &draft)?;
    let slug = WikiSlug::try_from("dirty-page".to_owned())?;
    let error = opened
        .store
        .edit_page(
            &slug,
            &PageEdit {
                current_state: Some("Edited summary.".to_owned()),
                article: None,
            },
            stamp()?,
        )
        .await;
    let Err(error) = error else {
        return Err("a dirty worktree file was accepted".into());
    };
    let message = error.to_string();
    assert!(message.contains("topics/dirty-page.md"), "{message}");
    assert_eq!(std::fs::read_to_string(&path)?, draft);
    Ok(())
}

#[tokio::test]
async fn a_file_matching_head_is_replaced_from_the_database() -> TestResult {
    let opened = open()?;
    let topic = owned(&opened, "Matched page").await?;
    insert_page(
        &opened.store,
        StoredPage {
            slug: "matched-page",
            title: "Matched page",
            summary: "Database summary.",
            article: "Database article.",
            sections: None,
            topic: Some(&topic),
        },
    )
    .await?;
    stamp_fields(&opened.store, "matched-page").await?;
    let path = opened.wiki.join("topics").join("matched-page.md");
    std::fs::create_dir_all(opened.wiki.join("topics"))?;
    std::fs::write(
        &path,
        draft("matched-page", "Matched page", "Committed article."),
    )?;
    git(&opened.wiki, &["init", "-b", "main"])?;
    git(&opened.wiki, &["config", "user.email", "wiki@rivetos.dev"])?;
    git(&opened.wiki, &["config", "user.name", "RivetOS Wiki"])?;
    git(&opened.wiki, &["add", "topics/matched-page.md"])?;
    git(&opened.wiki, &["commit", "-m", "seed"])?;
    let slug = WikiSlug::try_from("matched-page".to_owned())?;
    let edited = opened
        .store
        .edit_page(
            &slug,
            &PageEdit {
                current_state: Some("Edited summary.".to_owned()),
                article: None,
            },
            stamp()?,
        )
        .await?;
    assert!(edited);
    let text = std::fs::read_to_string(&path)?;
    assert!(text.contains("bench-tag"), "{text}");
    assert!(text.contains("Database article."), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_stored_commit_does_not_invent_a_file() -> TestResult {
    let opened = open()?;
    let topic = owned(&opened, "Filed already").await?;
    insert_page(
        &opened.store,
        StoredPage {
            slug: "filed-already",
            title: "Filed already",
            summary: "Database summary.",
            article: "Database article.",
            sections: None,
            topic: Some(&topic),
        },
    )
    .await?;
    opened
        .store
        .with_conn(|conn| {
            conn.execute(
                "UPDATE ros_wiki_topics SET git_sha = ?1 WHERE slug = 'filed-already'",
                ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
            )
            .map_err(|source| SqliteError::Query {
                operation: "store a commit id",
                source,
            })?;
            Ok(())
        })
        .await?;
    let slug = WikiSlug::try_from("filed-already".to_owned())?;
    let error = opened
        .store
        .edit_page(
            &slug,
            &PageEdit {
                current_state: Some("Edited summary.".to_owned()),
                article: None,
            },
            stamp()?,
        )
        .await;
    let Err(error) = error else {
        return Err("a stored commit id invented a file from the database".into());
    };
    let message = error.to_string();
    assert!(message.contains("filed-already"), "{message}");
    assert!(
        !opened.wiki.join("topics").join("filed-already.md").exists(),
        "{message}"
    );
    Ok(())
}

#[tokio::test]
async fn removing_a_missing_file_in_a_new_repository_deletes_the_row() -> TestResult {
    let opened = open()?;
    insert_page(
        &opened.store,
        StoredPage {
            slug: "unborn-page",
            title: "Unborn page",
            summary: "Never filed.",
            article: "Never filed.",
            sections: None,
            topic: None,
        },
    )
    .await?;
    let slug = WikiSlug::try_from("unborn-page".to_owned())?;
    opened.store.remove_page(&slug).await?;
    let lookup = opened.store.wiki_page(&slug).await?;
    assert!(matches!(lookup, WikiLookup::Missing));
    Ok(())
}

async fn extraction_keeps_the_article(opened: &Opened) -> TestResult {
    insert_page(
        &opened.store,
        StoredPage {
            slug: "legacy-extract",
            title: "Legacy extract",
            summary: "Old summary.",
            article: "Legacy extract article.",
            sections: None,
            topic: None,
        },
    )
    .await?;
    let slug = WikiSlug::try_from("legacy-extract".to_owned())?;
    testkit::wiki_files::write_leaf(
        &opened.store,
        "00000000-0000-4000-8000-0000000000aa",
        "Fresh summary.",
        "2026-10-10T12:00:00Z",
    )
    .await?;
    opened
        .store
        .apply_wiki(
            &ExtractWiki {
                summary_id: "00000000-0000-4000-8000-0000000000aa".parse()?,
                conversation_id: Some(CONVERSATION.parse()?),
            },
            vec![WikiPatch {
                action: WikiAction::Update,
                slug,
                title: "Legacy extract".to_owned(),
                aliases: Vec::new(),
                tags: Vec::new(),
                entities: Vec::new(),
                summary: "Fresh summary.".to_owned(),
                article: None,
                related: None,
                history_entry: None,
                entity_type: None,
                facts: None,
            }],
            &WikiRouting::default(),
            stamp()?,
        )
        .await?;
    let text = std::fs::read_to_string(opened.wiki.join("topics").join("legacy-extract.md"))?;
    assert!(text.contains("Legacy extract article."), "{text}");
    assert!(text.contains("Old summary."), "{text}");
    Ok(())
}

async fn edit_and_change_keep_the_article(opened: &Opened) -> TestResult {
    let topic = topic_id()?;
    opened
        .store
        .create(Topic::create(
            topic,
            NewTopic {
                kind: TopicKind::Work,
                name: TopicName::new("Legacy edit")?,
                aliases: Vec::new(),
                parent: None,
                directory: None,
                owner: Some(testkit::owner()?),
            },
            stamp()?,
        )?)
        .await?;
    let topic_id = topic.to_string();
    insert_page(
        &opened.store,
        StoredPage {
            slug: "legacy-edit",
            title: "Legacy edit",
            summary: "Old edit summary.",
            article: "Legacy edit article.",
            sections: Some(
                r#"[{"topic":null,"heading":"Bench notes","body":"Section from the database."}]"#,
            ),
            topic: Some(&topic_id),
        },
    )
    .await?;
    let slug = WikiSlug::try_from("legacy-edit".to_owned())?;
    let edited = opened
        .store
        .edit_page(
            &slug,
            &PageEdit {
                current_state: Some("Edited summary.".to_owned()),
                article: None,
            },
            stamp()?,
        )
        .await?;
    assert!(edited);
    opened
        .store
        .change_page(
            topic,
            &slug,
            &|page| "Changed summary.".clone_into(&mut page.summary),
            stamp()?,
        )
        .await?;
    let text = std::fs::read_to_string(opened.wiki.join("topics").join("legacy-edit.md"))?;
    assert!(text.contains("Legacy edit article."), "{text}");
    assert!(text.contains("Section from the database."), "{text}");
    assert!(text.contains("Changed summary."), "{text}");
    Ok(())
}

async fn fold_keeps_the_child_section(opened: &Opened) -> TestResult {
    let parent = WikiSlug::try_from("legacy-parent".to_owned())?;
    testkit::wiki_files::write_leaf(
        &opened.store,
        "00000000-0000-4000-8000-0000000000ab",
        "Parent page.",
        "2026-10-10T12:00:00Z",
    )
    .await?;
    opened
        .store
        .apply_wiki(
            &ExtractWiki {
                summary_id: "00000000-0000-4000-8000-0000000000ab".parse()?,
                conversation_id: Some(CONVERSATION.parse()?),
            },
            vec![WikiPatch {
                action: WikiAction::Create,
                slug: parent,
                title: "Legacy parent".to_owned(),
                aliases: Vec::new(),
                tags: Vec::new(),
                entities: Vec::new(),
                summary: "Parent page.".to_owned(),
                article: Some("Parent article.".to_owned()),
                related: None,
                history_entry: None,
                entity_type: None,
                facts: None,
            }],
            &WikiRouting::default(),
            stamp()?,
        )
        .await?;
    insert_page(
        &opened.store,
        StoredPage {
            slug: "legacy-child",
            title: "Legacy child",
            summary: "Child summary.",
            article: "Child article.",
            sections: Some(
                r#"[{"topic":null,"heading":"Child section","body":"Child section body."}]"#,
            ),
            topic: None,
        },
    )
    .await?;
    opened
        .store
        .fold_wiki(
            &WikiFold {
                cluster: "legacy-parent".to_owned(),
                parent: "legacy-parent".to_owned(),
                children: vec!["legacy-child".to_owned()],
                mark: FoldMark::Merged,
            },
            stamp()?,
        )
        .await?;
    let text = std::fs::read_to_string(opened.wiki.join("topics").join("legacy-parent.md"))?;
    assert!(text.contains("Child article."), "{text}");
    assert!(text.contains("Child section body."), "{text}");
    Ok(())
}

async fn entity_keeps_the_old_article(opened: &Opened) -> TestResult {
    let view = testkit::wiki_files::publish_entity(
        &opened.store,
        "00000000-0000-4000-8000-0000000000ac",
        "keptbox",
        "Old entity prose.",
        "Noted keptbox.",
        "2026-10-10T12:00:00Z",
    )
    .await?;
    std::fs::remove_file(opened.wiki.join("topics").join("keptbox.md"))?;
    let when = stamp()?;
    let id = uuid::Uuid::parse_str("00000000-0000-4000-8000-0000000000ad")?;
    testkit::wiki_files::write_leaf(
        &opened.store,
        "00000000-0000-4000-8000-0000000000ad",
        "New entity prose.",
        "2026-10-10T13:00:00Z",
    )
    .await?;
    let mut lease = opened.store.lease_entity(view.id).await?;
    lease
        .apply(
            &StoredRewrite {
                summary: "New entity prose.".to_owned(),
                article: "New entity article.".to_owned(),
                facts: vec![PreparedFact {
                    attribute: "place".to_owned(),
                    value: "bench".to_owned(),
                    as_of: when,
                    status: FactState::Current,
                    summary_id: id,
                    conversation_id: Some(CONVERSATION.parse()?),
                }],
                history: Some(PreparedHistory {
                    summary_id: id,
                    conversation_id: Some(CONVERSATION.parse()?),
                    session_on: when.utc_date(),
                    sentence: "Rewrote keptbox.".to_owned(),
                }),
                owner: testkit::OWNER.to_owned(),
                session_at: when,
                session_id: id,
                evidence: "Rewrote keptbox.".to_owned(),
            },
            when,
        )
        .await?;
    lease.release().await?;
    let shown = git(
        &opened.wiki,
        &["log", "-p", "--all", "--", "topics/keptbox.md"],
    )?;
    assert!(
        shown.contains("Old entity prose. Kept on the bench."),
        "{shown}"
    );
    let current = std::fs::read_to_string(opened.wiki.join("topics").join("keptbox.md"))?;
    assert!(current.contains("New entity article."), "{current}");
    Ok(())
}

async fn owned(opened: &Opened, name: &str) -> TestResult<String> {
    let topic = topic_id()?;
    opened
        .store
        .create(Topic::create(
            topic,
            NewTopic {
                kind: TopicKind::Work,
                name: TopicName::new(name)?,
                aliases: Vec::new(),
                parent: None,
                directory: None,
                owner: Some(testkit::owner()?),
            },
            stamp()?,
        )?)
        .await?;
    Ok(topic.to_string())
}

async fn stamp_fields(store: &SqliteStore, slug: &str) -> TestResult {
    let slug = slug.to_owned();
    store
        .with_conn(move |conn| {
            let cited = slug.clone();
            let sourced = slug.clone();
            conn.execute(
                "UPDATE ros_wiki_topics
                    SET aliases = ?1, tags = ?2, entities = ?3, related = ?4,
                        last_verified_at = ?5
                  WHERE slug = ?6",
                params![
                    r#"["Kept alias"]"#,
                    r#"["bench-tag"]"#,
                    r#"["Keptbox"]"#,
                    r#"["other-page"]"#,
                    "2026-10-10T12:00:00.000Z",
                    slug,
                ],
            )
            .map_err(|source| SqliteError::Query {
                operation: "stamp stored fields",
                source,
            })?;
            conn.execute(
                "INSERT INTO ros_wiki_citations
                    (topic_slug, summary_id, kind, note, cited_at)
                 VALUES (?1, '00000000-0000-4000-8000-0000000000aa', 'summary', ?2, ?3)",
                params![cited, "Cited from the row.", "2026-10-10T12:00:00.000Z",],
            )
            .map_err(|source| SqliteError::Query {
                operation: "stamp citation",
                source,
            })?;
            conn.execute(
                "INSERT INTO ros_wiki_provenance
                    (topic_slug, source_kind, source_id, created_at)
                 VALUES (?1, 'summary', 'row-source', ?2)",
                params![sourced, "2026-10-10T12:00:00.000Z"],
            )
            .map_err(|source| SqliteError::Query {
                operation: "stamp provenance",
                source,
            })?;
            Ok(())
        })
        .await?;
    Ok(())
}

fn draft(slug: &str, title: &str, summary: &str) -> String {
    format!(
        "---\ntitle: {title}\nslug: {slug}\naliases: []\ntags: []\nentities: []\nrelated: []\nsources: []\n---\n\n## Summary\n\n{summary}\n\n## Article\n\n{summary}\n\n## History\n\n"
    )
}

struct StoredPage<'a> {
    slug: &'a str,
    title: &'a str,
    summary: &'a str,
    article: &'a str,
    sections: Option<&'a str>,
    topic: Option<&'a str>,
}

async fn insert_page(store: &SqliteStore, page: StoredPage<'_>) -> TestResult {
    let slug = page.slug.to_owned();
    let title = page.title.to_owned();
    let summary = page.summary.to_owned();
    let article = page.article.to_owned();
    let sections = page.sections.unwrap_or("[]").to_owned();
    let topic = page.topic.map(str::to_owned);
    let page_slug = slug.clone();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_wiki_topics
                    (slug, title, current_state, article, search_text, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?2, '2026-10-10T12:00:00.000Z', '2026-10-10T12:00:00.000Z')",
                params![slug, title, summary, article],
            )
            .map_err(|source| SqliteError::Query {
                operation: "insert stored page",
                source,
            })?;
            conn.execute(
                "INSERT INTO rh_topic_pages (page_slug, topic_id, sections) VALUES (?1, ?2, ?3)",
                params![page_slug, topic, sections],
            )
            .map_err(|source| SqliteError::Query {
                operation: "insert stored sections",
                source,
            })?;
            Ok(())
        })
        .await?;
    Ok(())
}

fn open() -> TestResult<Opened> {
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("memory.sqlite");
    migrate::sqlite(&database)?;
    let wiki = dir.path().join("wiki");
    std::fs::create_dir(&wiki)?;
    let conn = rusqlite::Connection::open(&database)?;
    conn.execute(
        "INSERT INTO ros_conversations
            (id, session_key, agent, title, created_at, updated_at, owner_user_id)
         VALUES (?1, 'claude-code:page-upgrade', 'rivet', 'upgrade', '2026-10-10T12:00:00.000Z',
                 '2026-10-10T12:00:00.000Z', 'owner')",
        [CONVERSATION],
    )?;
    conn.execute(
        "INSERT INTO ros_wiki_topics
            (slug, title, current_state, article, search_text, created_at, updated_at)
         VALUES ('legacy-read', 'Legacy read', 'Readable summary.', 'Readable article.',
                 'legacy', '2026-10-10T12:00:00.000Z', '2026-10-10T12:00:00.000Z')",
        [],
    )?;
    conn.execute(
        "INSERT INTO rh_topic_pages (page_slug, sections) VALUES ('legacy-read', ?1)",
        [r#"[{"topic":null,"heading":"Readable","body":"Readable section."}]"#],
    )?;
    let store = SqliteStore::open(&database)?.with_wiki_directory(wiki.clone());
    Ok(Opened {
        _dir: dir,
        store,
        wiki,
    })
}

fn topic_id() -> TestResult<TopicId> {
    Ok(TopicId::from_uuid(uuid::Uuid::parse_str(
        "00000000-0000-4000-8000-0000000000f2",
    )?))
}

fn stamp() -> TestResult<jiff::Timestamp> {
    Ok("2026-10-10T13:00:00Z".parse()?)
}

fn git(root: &Path, args: &[&str]) -> TestResult<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}
