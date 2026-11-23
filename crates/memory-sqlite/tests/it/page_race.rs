use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use kernel::memory::MemoryStore;
use kernel::topics::NewTopic;
use kernel::topics::PageEdit;
use kernel::topics::Topic;
use kernel::topics::TopicId;
use kernel::topics::TopicKey;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
use kernel::topics::TopicPage;
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
use kernel::wiki::page::WikiAction;
use kernel::wiki::page::WikiPatch;
use kernel::wiki::page::WikiSlug;
use memory_sqlite::SqliteError;
use memory_sqlite::SqliteStore;
use parking_lot::Condvar;
use parking_lot::Mutex;
use tempfile::TempDir;
use testkit::wiki_files::CONVERSATION;
use tokio::sync::Notify;
use wiki_files::wiki_writer::WikiRepository;
use wire::time::TimestampFormat as _;

type Sent<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
type PauseHook = Arc<dyn Fn() + Send + Sync>;
type PauseSet = fn(Option<PauseHook>);

fn share<T>(result: Result<T, Box<dyn std::error::Error>>) -> Sent<T> {
    result.map_err(|error| error.to_string().into())
}

struct Opened {
    _dir: TempDir,
    store: SqliteStore,
    wiki: PathBuf,
}

struct Pause {
    gate: Arc<(Mutex<Flag>, Condvar)>,
    entered: Arc<Notify>,
    clear: PauseSet,
}

#[tokio::test]
async fn a_replaced_summary_does_not_publish_its_page() -> Sent {
    let opened = open()?;
    let slug = WikiSlug::try_from("acmekit-deploys".to_owned())?;
    apply(
        &opened.store,
        "00000000-0000-4000-8000-0000000000a1",
        &slug,
        "First.",
        WikiAction::Create,
    )
    .await?;
    let summary = "00000000-0000-4000-8000-0000000000a4";
    share(
        testkit::wiki_files::write_leaf(
            &opened.store,
            summary,
            "Replaced while waiting.",
            "2026-10-10T12:00:00Z",
        )
        .await,
    )?;
    let pause = Pause::install(memory_sqlite::set_lock_pause);
    let store = opened.store.clone();
    let slug_job = slug.clone();
    let task: tokio::task::JoinHandle<Sent> = tokio::spawn(async move {
        let patch = WikiPatch {
            action: WikiAction::Update,
            slug: slug_job,
            title: "AcmeKit deploys".to_owned(),
            aliases: Vec::new(),
            tags: vec!["ops".to_owned()],
            entities: Vec::new(),
            summary: "Replaced while waiting.".to_owned(),
            article: Some("Replaced while waiting.".to_owned()),
            related: None,
            history_entry: None,
            entity_type: None,
            facts: None,
        };
        store
            .apply_wiki(
                &ExtractWiki {
                    summary_id: summary.parse()?,
                    conversation_id: Some(CONVERSATION.parse()?),
                },
                vec![patch],
                &WikiRouting::default(),
                stamp()?,
            )
            .await?;
        Ok(())
    });
    pause.wait().await?;
    let removed = summary.to_owned();
    opened
        .store
        .with_conn(move |conn| {
            conn.execute("DELETE FROM ros_summaries WHERE id = ?1", [removed])
                .map_err(|source| SqliteError::Query {
                    operation: "delete summary",
                    source,
                })?;
            Ok(())
        })
        .await?;
    drop(pause);
    let error = match task.await? {
        Ok(()) => return Err("published a page for a summary that was gone".into()),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("is gone"), "{error}");
    let text = std::fs::read_to_string(opened.wiki.join("topics").join("acmekit-deploys.md"))?;
    assert!(!text.contains("Replaced while waiting."), "{text}");
    let page = filed(&opened.store, &slug).await?;
    assert_eq!(page.article.as_deref(), Some("First."));
    assert_eq!(page.git_sha.as_deref(), Some(head(&opened.wiki)?.as_str()));
    Ok(())
}

struct Flag {
    entered: bool,
    release: bool,
}

#[tokio::test]
async fn two_editors_keep_the_later_page() -> Sent {
    let opened = open()?;
    let slug = WikiSlug::try_from("field-notes".to_owned())?;
    save_page(&opened.store, &slug, "The bench faces the window.").await?;
    let left = opened.store.clone();
    let right = opened.store.clone();
    let slug_left = slug.clone();
    let slug_right = slug.clone();
    race(
        &opened.wiki,
        async move { edit(&left, &slug_left, "from writer a").await },
        async move { edit(&right, &slug_right, "from writer b").await },
    )
    .await?;
    let page = filed(&opened.store, &slug).await?;
    assert_eq!(page.current_state, "from writer b");
    assert_eq!(page.git_sha.as_deref(), Some(head(&opened.wiki)?.as_str()));
    let text = std::fs::read_to_string(opened.wiki.join("topics").join("field-notes.md"))?;
    assert!(text.contains("from writer b"), "{text}");
    Ok(())
}

#[tokio::test]
async fn two_extractions_keep_the_later_page() -> Sent {
    let opened = open()?;
    let slug = WikiSlug::try_from("acmekit-deploys".to_owned())?;
    apply(
        &opened.store,
        "00000000-0000-4000-8000-0000000000a1",
        &slug,
        "First.",
        WikiAction::Create,
    )
    .await?;
    let left = opened.store.clone();
    let right = opened.store.clone();
    let slug_left = slug.clone();
    let slug_right = slug.clone();
    race(
        &opened.wiki,
        async move {
            apply(
                &left,
                EARLIER_SUMMARY,
                &slug_left,
                "from writer a",
                WikiAction::Update,
            )
            .await
        },
        async move {
            apply(
                &right,
                LATER_SUMMARY,
                &slug_right,
                "from writer b",
                WikiAction::Update,
            )
            .await
        },
    )
    .await?;
    let page = filed(&opened.store, &slug).await?;
    let log = commits(&opened.wiki)?;
    let later = log
        .iter()
        .find(|(_, body)| body.contains(LATER_SUMMARY))
        .ok_or("writer b left no commit")?;
    let earlier = log
        .iter()
        .find(|(_, body)| body.contains(EARLIER_SUMMARY))
        .ok_or("writer a left no commit")?;
    assert_eq!(page.current_state, "First.");
    assert_eq!(page.article.as_deref(), Some("from writer b"));
    assert_eq!(page.git_sha.as_deref(), Some(later.0.as_str()));
    assert_eq!(later.0, head(&opened.wiki)?);
    assert_ne!(page.git_sha.as_deref(), Some(earlier.0.as_str()));
    assert!(
        page.citations
            .iter()
            .any(|citation| citation.summary_id == LATER_SUMMARY)
    );
    Ok(())
}

#[tokio::test]
async fn a_change_holds_the_repository_through_the_row() -> Sent {
    let opened = open()?;
    let slug = WikiSlug::try_from("field-notes".to_owned())?;
    save_page(&opened.store, &slug, "The bench faces the window.").await?;
    let store = opened.store.clone();
    let topic = topic_id()?;
    during_pause(&opened.wiki, async move {
        store
            .change_page(
                topic,
                &slug,
                &|page| page.summary = "Changed while locked.".to_owned(),
                stamp()?,
            )
            .await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_second_save_holds_the_repository_through_the_row() -> Sent {
    let opened = open()?;
    let slug = WikiSlug::try_from("field-notes".to_owned())?;
    save_page(&opened.store, &slug, "The bench faces the window.").await?;
    let store = opened.store.clone();
    during_pause(&opened.wiki, async move {
        save_page(&store, &slug, "Saved while locked.").await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_cancelled_save_keeps_the_repository_until_the_row_is_stored() -> Sent {
    let opened = open()?;
    let slug = WikiSlug::try_from("field-notes".to_owned())?;
    save_page(&opened.store, &slug, "The bench faces the window.").await?;
    let pause = Pause::install(memory_sqlite::set_row_pause);
    let store = opened.store.clone();
    let slug_job = slug.clone();
    let task: tokio::task::JoinHandle<Sent> = tokio::spawn(async move {
        save_page(&store, &slug_job, "Saved after cancel.").await?;
        Ok(())
    });
    pause.wait().await?;
    assert_held(&opened.wiki).await?;
    task.abort();
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            sleep,
            reason = "cancellation must be observed before the lock is checked again"
        )
    )]
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_held(&opened.wiki).await?;
    drop(pause);
    wait_until_free(&opened.wiki).await?;
    let page = filed(&opened.store, &slug).await?;
    assert_eq!(page.article.as_deref(), Some("Saved after cancel."));
    Ok(())
}

async fn wait_until_free(wiki: &Path) -> Sent {
    for _ in 0..50 {
        if lock_is_free(wiki).await? {
            return Ok(());
        }
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(sleep, reason = "the detached save finishes after cancellation")
        )]
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err("the cancelled save did not finish the row".into())
}

async fn lock_is_free(wiki: &Path) -> Sent<bool> {
    Ok(WikiRepository::for_store(wiki.to_path_buf())
        .try_lock()
        .await?
        .is_some())
}

#[tokio::test]
async fn a_fold_holds_the_repository_through_the_row() -> Sent {
    let opened = open()?;
    apply(
        &opened.store,
        "00000000-0000-4000-8000-0000000000b1",
        &WikiSlug::try_from("parent-page".to_owned())?,
        "Parent.",
        WikiAction::Create,
    )
    .await?;
    apply(
        &opened.store,
        "00000000-0000-4000-8000-0000000000b2",
        &WikiSlug::try_from("child-page".to_owned())?,
        "Child.",
        WikiAction::Create,
    )
    .await?;
    let store = opened.store.clone();
    during_pause(&opened.wiki, async move {
        store
            .fold_wiki(
                &WikiFold {
                    cluster: "parent-page".to_owned(),
                    parent: "parent-page".to_owned(),
                    children: vec!["child-page".to_owned()],
                    mark: FoldMark::Merged,
                },
                stamp()?,
            )
            .await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn entity_publication_holds_the_repository_through_the_row() -> Sent {
    let opened = open()?;
    let view = share(
        testkit::wiki_files::publish_entity(
            &opened.store,
            "00000000-0000-4000-8000-0000000000c2",
            "lockbox",
            "A locked box.",
            "Noted lockbox.",
            "2026-10-10T18:00:00Z",
        )
        .await,
    )?;
    let store = opened.store.clone();
    during_pause(&opened.wiki, async move {
        let when = stamp()?;
        let id = uuid::Uuid::parse_str("00000000-0000-4000-8000-0000000000c3")?;
        share(
            testkit::wiki_files::write_leaf(
                &store,
                "00000000-0000-4000-8000-0000000000c3",
                "Again.",
                "2026-10-10T18:00:00Z",
            )
            .await,
        )?;
        let mut lease = store.lease_entity(view.id).await?;
        lease
            .apply(
                &StoredRewrite {
                    summary: "Again.".to_owned(),
                    article: "Still locked.".to_owned(),
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
                        sentence: "Noted again.".to_owned(),
                    }),
                    owner: testkit::OWNER.to_owned(),
                    session_at: when,
                    session_id: id,
                    evidence: "Noted again.".to_owned(),
                },
                when,
            )
            .await?;
        lease.release().await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_second_fold_skips_a_child_already_folded() -> Sent {
    let opened = open()?;
    let parent_one = WikiSlug::try_from("parent-one".to_owned())?;
    let parent_two = WikiSlug::try_from("parent-two".to_owned())?;
    let child = WikiSlug::try_from("child-page".to_owned())?;
    apply(
        &opened.store,
        "00000000-0000-4000-8000-0000000000b1",
        &parent_one,
        "Parent one.",
        WikiAction::Create,
    )
    .await?;
    apply(
        &opened.store,
        "00000000-0000-4000-8000-0000000000b2",
        &parent_two,
        "Parent two.",
        WikiAction::Create,
    )
    .await?;
    apply(
        &opened.store,
        "00000000-0000-4000-8000-0000000000b3",
        &child,
        "Only the child.",
        WikiAction::Create,
    )
    .await?;
    let pause = PairPause::install();
    let left = opened.store.clone();
    let right = opened.store.clone();
    let first: tokio::task::JoinHandle<Sent> = tokio::spawn(async move {
        left.fold_wiki(
            &WikiFold {
                cluster: "parent-one".to_owned(),
                parent: "parent-one".to_owned(),
                children: vec!["child-page".to_owned()],
                mark: FoldMark::Merged,
            },
            stamp()?,
        )
        .await?;
        Ok(())
    });
    let second: tokio::task::JoinHandle<Sent> = tokio::spawn(async move {
        right
            .fold_wiki(
                &WikiFold {
                    cluster: "parent-two".to_owned(),
                    parent: "parent-two".to_owned(),
                    children: vec!["child-page".to_owned()],
                    mark: FoldMark::Merged,
                },
                stamp()?,
            )
            .await?;
        Ok(())
    });
    pause.wait_both().await?;
    first.await??;
    second.await??;
    let one = std::fs::read_to_string(opened.wiki.join("topics").join("parent-one.md"))?;
    let two = std::fs::read_to_string(opened.wiki.join("topics").join("parent-two.md"))?;
    kept_by_one_parent(&one, &two, opened.store.wiki_page(&child).await?)
}

fn kept_by_one_parent(one: &str, two: &str, lookup: WikiLookup) -> Sent {
    let one_has = one.contains("Only the child.");
    let two_has = two.contains("Only the child.");
    assert!(
        one_has != two_has,
        "both parents absorbed the child\n{one}\n{two}"
    );
    match lookup {
        WikiLookup::Redirect(target) => {
            let keeper = if one_has { "parent-one" } else { "parent-two" };
            assert_eq!(target, keeper);
            Ok(())
        }
        WikiLookup::Page(page) => Err(format!("child page was recreated: {}", page.slug).into()),
        WikiLookup::Missing => Err("child page disappeared without a redirect".into()),
    }
}

struct PairPause {
    gate: Arc<(Mutex<u8>, Condvar)>,
}

impl PairPause {
    fn install() -> Self {
        let gate = Arc::new((Mutex::new(0_u8), Condvar::new()));
        let hooked = gate.clone();
        memory_sqlite::set_lock_pause(Some(Arc::new(move || {
            let (lock, cv) = &*hooked;
            let mut count = lock.lock();
            *count = count.saturating_add(1);
            cv.notify_all();
            while *count < 2 {
                cv.wait(&mut count);
            }
        })));
        Self { gate }
    }

    async fn wait_both(&self) -> Sent {
        let gate = Arc::clone(&self.gate);
        tokio::task::spawn_blocking(move || {
            let (lock, cv) = &*gate;
            let mut count = lock.lock();
            while *count < 2 {
                cv.wait(&mut count);
            }
        })
        .await
        .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> {
            error.to_string().into()
        })?;
        Ok(())
    }
}

impl Drop for PairPause {
    fn drop(&mut self) {
        let (lock, cv) = &*self.gate;
        let mut count = lock.lock();
        if *count < 2 {
            *count = 2;
        }
        drop(count);
        cv.notify_all();
        memory_sqlite::set_lock_pause(None);
    }
}

async fn during_pause(
    wiki: &Path,
    work: impl std::future::Future<Output = Sent> + Send + 'static,
) -> Sent {
    let pause = Pause::install(memory_sqlite::set_row_pause);
    let task = tokio::spawn(work);
    pause.wait().await?;
    assert_held(wiki).await?;
    drop(pause);
    task.await?
}

async fn race(
    wiki: &Path,
    first: impl std::future::Future<Output = Sent> + Send + 'static,
    second: impl std::future::Future<Output = Sent> + Send + 'static,
) -> Sent {
    let pause = Pause::install(memory_sqlite::set_row_pause);
    let first = tokio::spawn(first);
    pause.wait().await?;
    assert_held(wiki).await?;
    let finished = Arc::new(AtomicBool::new(false));
    let done = finished.clone();
    let second = tokio::spawn(async move {
        let result = second.await;
        done.store(true, Ordering::SeqCst);
        result
    });
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            sleep,
            reason = "the second writer must still be waiting on the repository lock"
        )
    )]
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !finished.load(Ordering::SeqCst),
        "the second writer finished while the first still held the repository"
    );
    assert_held(wiki).await?;
    drop(pause);
    first.await??;
    second.await??;
    Ok(())
}

impl Pause {
    fn install(set: PauseSet) -> Self {
        let gate = Arc::new((
            Mutex::new(Flag {
                entered: false,
                release: false,
            }),
            Condvar::new(),
        ));
        let entered = Arc::new(Notify::new());
        let hooked = gate.clone();
        let signal = entered.clone();
        set(Some(Arc::new(move || {
            let (lock, cv) = &*hooked;
            let mut flag = lock.lock();
            flag.entered = true;
            cv.notify_all();
            signal.notify_one();
            while !flag.release {
                cv.wait(&mut flag);
            }
        })));
        Self {
            gate,
            entered,
            clear: set,
        }
    }

    async fn wait(&self) -> Sent {
        let notified = self.entered.notified();
        if self.gate.0.lock().entered {
            return Ok(());
        }
        tokio::time::timeout(Duration::from_secs(10), notified)
            .await
            .map_err(|_| "the row save was not reached while the repository lock was held")?;
        Ok(())
    }
}

impl Drop for Pause {
    fn drop(&mut self) {
        let (lock, cv) = &*self.gate;
        lock.lock().release = true;
        cv.notify_all();
        (self.clear)(None);
    }
}

async fn assert_held(wiki: &Path) -> Sent {
    let held = WikiRepository::for_store(wiki.to_path_buf())
        .try_lock()
        .await?
        .is_none();
    assert!(
        held,
        "repository lock was released before the row was saved"
    );
    Ok(())
}

fn open() -> Sent<Opened> {
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("memory.sqlite");
    migrate::sqlite(&database)?;
    let wiki = dir.path().join("wiki");
    std::fs::create_dir(&wiki)?;
    let conn = rusqlite::Connection::open(&database)?;
    conn.execute(
        "INSERT INTO ros_conversations
            (id, session_key, agent, title, created_at, updated_at, owner_user_id)
         VALUES (?1, 'claude-code:page-race', 'rivet', 'page race', '2026-10-10T12:00:00.000Z',
                 '2026-10-10T12:00:00.000Z', 'owner')",
        [CONVERSATION],
    )?;
    let store = SqliteStore::open(&database)?.with_wiki_directory(wiki.clone());
    Ok(Opened {
        _dir: dir,
        store,
        wiki,
    })
}

async fn save_page(store: &SqliteStore, slug: &WikiSlug, article: &str) -> Sent {
    let topic = Topic::create(
        topic_id()?,
        NewTopic {
            kind: TopicKind::Work,
            name: TopicName::new("Field notes")?,
            aliases: Vec::new(),
            parent: None,
            directory: None,
            owner: Some(testkit::owner()?),
        },
        stamp()?,
    )?;
    if store.find(&TopicKey::Id(topic.id)).await?.is_none() {
        store.create(topic).await?;
    }
    store
        .save_topic_page(
            topic_id()?,
            &TopicPage {
                slug: slug.clone(),
                title: "Field notes".to_owned(),
                summary: "Notes from the bench.".to_owned(),
                article: article.to_owned(),
                sections: Vec::new(),
            },
            stamp()?,
        )
        .await?;
    Ok(())
}

async fn edit(store: &SqliteStore, slug: &WikiSlug, state: &str) -> Sent {
    store
        .edit_page(
            slug,
            &PageEdit {
                current_state: Some(state.to_owned()),
                article: None,
            },
            stamp()?,
        )
        .await?;
    Ok(())
}

async fn apply(
    store: &SqliteStore,
    summary: &str,
    slug: &WikiSlug,
    text: &str,
    action: WikiAction,
) -> Sent {
    share(testkit::wiki_files::write_leaf(store, summary, text, "2026-10-10T12:00:00Z").await)?;
    let patch = WikiPatch {
        action,
        slug: slug.clone(),
        title: "AcmeKit deploys".to_owned(),
        aliases: Vec::new(),
        tags: vec!["ops".to_owned()],
        entities: Vec::new(),
        summary: text.to_owned(),
        article: Some(text.to_owned()),
        related: None,
        history_entry: None,
        entity_type: None,
        facts: None,
    };
    store
        .apply_wiki(
            &ExtractWiki {
                summary_id: summary.parse()?,
                conversation_id: Some(CONVERSATION.parse()?),
            },
            vec![patch],
            &WikiRouting::default(),
            stamp()?,
        )
        .await?;
    Ok(())
}

async fn filed(store: &SqliteStore, slug: &WikiSlug) -> Sent<kernel::wiki::WikiPageResponse> {
    match store.wiki_page(slug).await? {
        WikiLookup::Page(page) => Ok(*page),
        WikiLookup::Redirect(target) => Err(format!("redirects to {target}").into()),
        WikiLookup::Missing => Err("page is missing".into()),
    }
}

fn topic_id() -> Sent<TopicId> {
    Ok(TopicId::from_uuid(uuid::Uuid::parse_str(
        "00000000-0000-4000-8000-0000000000f1",
    )?))
}

fn stamp() -> Sent<jiff::Timestamp> {
    Ok("2026-10-10T18:00:00Z".parse()?)
}

const EARLIER_SUMMARY: &str = "00000000-0000-4000-8000-0000000000a2";
const LATER_SUMMARY: &str = "00000000-0000-4000-8000-0000000000a3";

fn commits(wiki: &Path) -> Sent<Vec<(String, String)>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(wiki)
        .args(["log", "--format=%H%n%B%x1e"])
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into());
    }
    let text = String::from_utf8(output.stdout)?;
    let mut rows = Vec::new();
    for record in text.split('\u{1e}') {
        let record = record.trim();
        if record.is_empty() {
            continue;
        }
        let mut lines = record.lines();
        let Some(sha) = lines.next() else {
            continue;
        };
        let body = lines.collect::<Vec<_>>().join("\n");
        rows.push((sha.to_owned(), body));
    }
    Ok(rows)
}

fn head(wiki: &Path) -> Sent<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(wiki)
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
