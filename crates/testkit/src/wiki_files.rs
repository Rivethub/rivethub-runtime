use std::path::Path;

use jiff::Timestamp;
use kernel::memory::MemoryStore;
use kernel::memory::NewSummary;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryStore;
use kernel::topics::NewTopic;
use kernel::topics::PageEdit;
use kernel::topics::Topic;
use kernel::topics::TopicId;
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
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::ExtractEntities;
use kernel::wiki::entity_pass::FactState;
use kernel::wiki::entity_pass::PreparedEntity;
use kernel::wiki::entity_pass::PreparedFact;
use kernel::wiki::entity_pass::PreparedHistory;
use kernel::wiki::entity_pass::PublishOutcome;
use kernel::wiki::entity_pass::StoredRewrite;
use kernel::wiki::page::FoldMark;
use kernel::wiki::page::WikiAction;
use kernel::wiki::page::WikiPatch;
use kernel::wiki::page::WikiSlug;
use uuid::Uuid;
use wire::time::TimestampFormat as _;

use crate::topics::Outcome;

pub const CONVERSATION: &str = "00000000-0000-4000-8000-0000000000c1";

const CREATE: &str = "00000000-0000-4000-8000-0000000000d1";
const UPDATE: &str = "00000000-0000-4000-8000-0000000000d2";
const CHILD: &str = "00000000-0000-4000-8000-0000000000d3";
const TIN: &str = "00000000-0000-4000-8000-0000000000e1";
const OUT: &str = "00000000-0000-4000-8000-0000000000e2";
const TOPIC: &str = "00000000-0000-4000-8000-0000000000f1";
const T1: &str = "2026-10-10T12:00:00Z";
const T2: &str = "2026-10-10T13:00:00Z";
const T3: &str = "2026-10-10T14:00:00Z";
const T4: &str = "2026-10-10T15:00:00Z";
const T5: &str = "2026-10-10T16:00:00Z";
const T6: &str = "2026-10-10T17:00:00Z";
const T7: &str = "2026-10-10T18:00:00Z";
const T8: &str = "2026-10-10T19:00:00Z";
const T9: &str = "2026-10-10T20:00:00Z";

pub trait PageFileStore:
    WikiBuilderStore
    + WikiConsolidateStore
    + TopicPageStore
    + TopicStore
    + EntityStore
    + MemoryStore
    + SummaryStore
{
}

impl<T> PageFileStore for T where
    T: WikiBuilderStore
        + WikiConsolidateStore
        + TopicPageStore
        + TopicStore
        + EntityStore
        + MemoryStore
        + SummaryStore
{
}

struct Taken {
    slug: String,
    tree: String,
    commits: String,
    tip: String,
    message: String,
    body: String,
}

pub async fn same_commits(store: &impl PageFileStore, wiki: &Path) -> Outcome {
    prepare_wiki(wiki)?;
    let mut steps = Vec::new();
    wiki_pages(store, wiki, &mut steps).await?;
    topic_page(store, wiki, &mut steps).await?;
    let (tin, out) = entities(store, wiki, &mut steps).await?;
    let merged = store.merge_entities(tin.id, out.id, at(T9)?).await?;
    let merged = merged.ok_or("entity merge produced nothing")?;
    assert!(merged.performed);
    assert_eq!(merged.survivor, tin.id);
    assert_eq!(merged.absorbed, out.id);
    absent(wiki, "outbox");
    let removal = git(wiki, &["log", "-1", "--format=%B"])?;
    assert_eq!(removal.trim_end(), "entity merge: remove outbox");
    let archive = store
        .wiki_archive(&WikiSlug::try_from("acmekit-deploys".to_owned())?, None)
        .await?;
    assert_eq!(archive, None);
    steps.push(cover::removed(wiki, "outbox")?);
    cover::rest(store, wiki, &tin, &mut steps).await?;
    dependent_patches(store, wiki, &mut steps).await?;
    cover::missing_file(store, wiki).await?;
    check(&steps);
    Ok(())
}

async fn dependent_patches(
    store: &impl PageFileStore,
    wiki: &Path,
    steps: &mut Vec<Taken>,
) -> Outcome {
    const PAIR: &str = "00000000-0000-4000-8000-0000000000d7";
    const WHEN: &str = "2026-10-11T01:00:00Z";
    write_leaf(store, PAIR, "Bench one. Bench two.", WHEN).await?;
    let touched = store
        .apply_wiki(
            &ExtractWiki {
                summary_id: uid(PAIR)?,
                conversation_id: Some(uid(CONVERSATION)?),
            },
            vec![
                patch(
                    "bench",
                    "Bench",
                    "Bench one.",
                    Some("First cut."),
                    WikiAction::Create,
                )?,
                patch(
                    "bench",
                    "Bench",
                    "Bench two.",
                    Some("Second cut."),
                    WikiAction::Update,
                )?,
            ],
            &WikiRouting::default(),
            at(WHEN)?,
        )
        .await?;
    assert_eq!(touched, vec!["bench".to_owned()]);
    let commits = git(
        wiki,
        &["rev-list", "--count", "HEAD", "--", "topics/bench.md"],
    )?;
    assert_eq!(commits.trim(), "2");
    steps.push(take(store, wiki, "bench").await?);
    Ok(())
}

async fn wiki_pages(store: &impl PageFileStore, wiki: &Path, steps: &mut Vec<Taken>) -> Outcome {
    write_leaf(store, CREATE, "AcmeKit deploys on Fridays.", T1).await?;
    let created = apply_patch(
        store,
        CREATE,
        patch(
            "acmekit-deploys",
            "AcmeKit deploys",
            "AcmeKit deploys on Fridays.",
            Some("Deploy notes."),
            WikiAction::Create,
        )?,
        T1,
    )
    .await?;
    assert_eq!(created, vec!["acmekit-deploys".to_owned()]);
    steps.push(take(store, wiki, "acmekit-deploys").await?);
    write_leaf(store, UPDATE, "AcmeKit also deploys on Mondays.", T2).await?;
    apply_patch(
        store,
        UPDATE,
        patch(
            "acmekit-deploys",
            "AcmeKit deploys",
            "AcmeKit also deploys on Mondays.",
            Some("Deploy notes."),
            WikiAction::Update,
        )?,
        T2,
    )
    .await?;
    steps.push(take(store, wiki, "acmekit-deploys").await?);
    write_leaf(store, CHILD, "The label printer jams on Mondays.", T3).await?;
    let child = apply_patch(
        store,
        CHILD,
        patch(
            "label-printer",
            "Label printer",
            "The label printer jams on Mondays.",
            None,
            WikiAction::Create,
        )?,
        T3,
    )
    .await?;
    assert_eq!(child, vec!["label-printer".to_owned()]);
    steps.push(take(store, wiki, "label-printer").await?);
    let folded = store
        .fold_wiki(
            &WikiFold {
                cluster: "acmekit-deploys".to_owned(),
                parent: "acmekit-deploys".to_owned(),
                children: vec!["label-printer".to_owned()],
                mark: FoldMark::Merged,
            },
            at(T4)?,
        )
        .await?;
    assert_eq!(folded, vec!["label-printer".to_owned()]);
    absent(wiki, "label-printer");
    steps.push(take(store, wiki, "acmekit-deploys").await?);
    Ok(())
}

async fn topic_page(store: &impl PageFileStore, wiki: &Path, steps: &mut Vec<Taken>) -> Outcome {
    let topic = Topic::create(
        TopicId::from_uuid(uid(TOPIC)?),
        NewTopic {
            kind: TopicKind::Work,
            name: TopicName::new("Field notes")?,
            aliases: Vec::new(),
            parent: None,
            directory: None,
            owner: Some(crate::owner()?),
        },
        at(T5)?,
    )?;
    store.create(topic).await?;
    let slug = WikiSlug::try_from("field-notes".to_owned())?;
    let page = TopicPage {
        slug: slug.clone(),
        title: "Field notes".to_owned(),
        summary: "Notes from the bench.".to_owned(),
        article: "The bench faces the window.".to_owned(),
        sections: Vec::new(),
    };
    store
        .save_topic_page(TopicId::from_uuid(uid(TOPIC)?), &page, at(T5)?)
        .await?;
    steps.push(take(store, wiki, "field-notes").await?);
    let edited = store
        .edit_page(
            &slug,
            &PageEdit {
                current_state: Some("Notes from the repaired bench.".to_owned()),
                article: None,
            },
            at(T6)?,
        )
        .await?;
    assert!(edited);
    steps.push(take(store, wiki, "field-notes").await?);
    Ok(())
}

async fn entities(
    store: &impl PageFileStore,
    wiki: &Path,
    steps: &mut Vec<Taken>,
) -> Result<(EntityView, EntityView), Box<dyn std::error::Error>> {
    let tin = publish_entity(store, TIN, "tinbox", "A tin box.", "Noted tinbox.", T7).await?;
    assert_eq!(tin.slug, "tinbox");
    steps.push(take(store, wiki, "tinbox").await?);
    let out = publish_entity(store, OUT, "outbox", "An out box.", "Noted outbox.", T8).await?;
    assert_eq!(out.slug, "outbox");
    steps.push(take(store, wiki, "outbox").await?);
    Ok((tin, out))
}

#[path = "wiki_file_cover.rs"]
mod cover;

pub use cover::FAILED_MENTION;
pub use cover::FAILED_SENTENCE;
pub use cover::commit_failure_keeps_rows;
pub use cover::stale_publish_keeps_the_file;

pub async fn publish_entity(
    store: &impl PageFileStore,
    summary: &str,
    name: &str,
    prose: &str,
    sentence: &str,
    when: &str,
) -> Result<EntityView, Box<dyn std::error::Error>> {
    write_leaf(store, summary, prose, when).await?;
    let id = uid(summary)?;
    let conversation = uid(CONVERSATION)?;
    let leaf = match store
        .entity_work(&ExtractEntities {
            summary_id: id,
            conversation_id: Some(conversation),
            session_at: None,
        })
        .await?
    {
        kernel::wiki::entity_pass::EntityWork::Pending(leaf) => leaf,
        kernel::wiki::entity_pass::EntityWork::Complete { entities } => {
            return Err(format!("summary {summary} was already an entity: {entities:?}").into());
        }
    };
    let view = store
        .commit(
            std::slice::from_ref(&leaf),
            &PreparedEntity {
                entity_ref: None,
                name: name.to_owned(),
                type_name: "Tools".to_owned(),
                aliases: Vec::new(),
                facts: Vec::new(),
                history: Vec::new(),
            },
            at(when)?,
        )
        .await?
        .ok_or("entity commit was skipped")?;
    let body = StoredRewrite {
        summary: prose.to_owned(),
        article: format!("{prose} Kept on the bench."),
        facts: vec![PreparedFact {
            attribute: "place".to_owned(),
            value: "bench".to_owned(),
            as_of: at(when)?,
            status: FactState::Current,
            summary_id: id,
            conversation_id: Some(conversation),
        }],
        history: Some(PreparedHistory {
            summary_id: id,
            conversation_id: Some(conversation),
            session_on: at(when)?.utc_date(),
            sentence: sentence.to_owned(),
        }),
        owner: crate::OWNER.to_owned(),
        session_at: at(when)?,
        session_id: id,
        evidence: sentence.to_owned(),
    };
    let mut lease = store.lease_entity(view.id).await?;
    let published = lease.apply(&body, at(when)?).await?;
    lease.release().await?;
    match published {
        PublishOutcome::Stored { slug, .. } => assert_eq!(slug, name),
        PublishOutcome::Stale(_) => return Err("entity page went stale".into()),
    }
    Ok(view)
}

fn prepare_wiki(wiki: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let init = std::process::Command::new("git")
        .current_dir(wiki)
        .args(["init", "-b", "main"])
        .output()?;
    if !init.status.success() {
        return Err(String::from_utf8_lossy(&init.stderr).into());
    }
    for (key, value) in [("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
        let set = std::process::Command::new("git")
            .current_dir(wiki)
            .args(["config", "--local", key, value])
            .output()?;
        if !set.status.success() {
            return Err(String::from_utf8_lossy(&set.stderr).into());
        }
    }
    Ok(())
}

fn check(steps: &[Taken]) {
    assert_eq!(
        render(steps),
        include_str!("../tests/fixtures/wiki_files_expected.txt")
    );
}

pub(super) fn redact_commits(
    wiki: &Path,
    text: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut text = text.to_owned();
    for short in commit_abbreviations(wiki)? {
        text = text.replace(&short, "<commit>");
    }
    Ok(text)
}

fn render(steps: &[Taken]) -> String {
    steps.iter().map(render_step).collect()
}

fn render_step(step: &Taken) -> String {
    format!(
        "SLUG {slug}\nTREE {tree}\nCOMMITS {commits}\nTIP {tip}\nMESSAGE\n{message}BODY\n{body}\n",
        slug = step.slug,
        tree = step.tree,
        commits = step.commits,
        tip = step.tip,
        message = step.message,
        body = step.body
    )
}

async fn take(
    store: &impl MemoryStore,
    wiki: &Path,
    slug: &str,
) -> Result<Taken, Box<dyn std::error::Error>> {
    let head = git(wiki, &["rev-parse", "HEAD"])?;
    let head = head.trim();
    let tree = pinned_tree(wiki)?;
    let commits = git(wiki, &["rev-list", "--count", "HEAD"])?;
    let spec = format!("topics/{slug}.md");
    let touched = git(wiki, &["log", "-1", "--format=%H", "--", &spec])?;
    let message = git(wiki, &["log", "-1", "--format=%B"])?;
    let recorded = match store
        .wiki_page(&WikiSlug::try_from(slug.to_owned())?)
        .await?
    {
        WikiLookup::Page(page) => page.git_sha.unwrap_or_default(),
        WikiLookup::Redirect(target) => return Err(format!("{slug} redirects to {target}").into()),
        WikiLookup::Missing => return Err(format!("{slug} is missing").into()),
    };
    assert_eq!(recorded, touched.trim(), "{slug}");
    let tip = if recorded == head { "yes" } else { "no" };
    Ok(Taken {
        slug: slug.to_owned(),
        tree: tree.trim().to_owned(),
        commits: commits.trim().to_owned(),
        tip: tip.to_owned(),
        message,
        body: redact_commits(
            wiki,
            &std::fs::read_to_string(wiki.join("topics").join(format!("{slug}.md")))?,
        )?,
    })
}

pub(super) fn pinned_tree(wiki: &Path) -> Result<String, Box<dyn std::error::Error>> {
    if worktree_embeds_commit(wiki)? {
        return Ok("<embeds-commit>".to_owned());
    }
    Ok(git(wiki, &["rev-parse", "HEAD^{tree}"])?.trim().to_owned())
}

fn worktree_embeds_commit(wiki: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let shorts = commit_abbreviations(wiki)?;
    for path in markdown_files(wiki)? {
        let text = std::fs::read_to_string(path)?;
        if shorts.iter().any(|short| text.contains(short)) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn commit_abbreviations(wiki: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let listed = git(wiki, &["rev-list", "HEAD"])?;
    Ok(listed
        .lines()
        .map(|sha| sha.chars().take(10).collect())
        .filter(|short: &String| short.len() == 10)
        .collect())
}

fn markdown_files(wiki: &Path) -> Result<Vec<std::path::PathBuf>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    collect_markdown(&wiki.join("topics"), &mut files)?;
    collect_markdown(&wiki.join("archive"), &mut files)?;
    Ok(files)
}

fn collect_markdown(
    dir: &Path,
    files: &mut Vec<std::path::PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_markdown(&path, files)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
    Ok(())
}

pub(super) async fn stored_commit(
    store: &impl MemoryStore,
    slug: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    match store
        .wiki_page(&WikiSlug::try_from(slug.to_owned())?)
        .await?
    {
        WikiLookup::Page(page) => Ok(page.git_sha.unwrap_or_default()),
        WikiLookup::Redirect(target) => Err(format!("{slug} redirects to {target}").into()),
        WikiLookup::Missing => Err(format!("{slug} is missing").into()),
    }
}

fn absent(wiki: &Path, slug: &str) {
    assert!(!wiki.join("topics").join(format!("{slug}.md")).exists());
}

async fn apply_patch(
    store: &impl WikiBuilderStore,
    summary: &str,
    patch: WikiPatch,
    when: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(store
        .apply_wiki(
            &ExtractWiki {
                summary_id: uid(summary)?,
                conversation_id: Some(uid(CONVERSATION)?),
            },
            vec![patch],
            &WikiRouting::default(),
            at(when)?,
        )
        .await?)
}

fn patch(
    slug: &str,
    title: &str,
    summary: &str,
    article: Option<&str>,
    action: WikiAction,
) -> Result<WikiPatch, Box<dyn std::error::Error>> {
    Ok(WikiPatch {
        action,
        slug: WikiSlug::try_from(slug.to_owned())?,
        title: title.to_owned(),
        aliases: Vec::new(),
        tags: vec!["ops".to_owned()],
        entities: Vec::new(),
        summary: summary.to_owned(),
        article: article.map(str::to_owned),
        related: None,
        history_entry: None,
        entity_type: None,
        facts: None,
    })
}

pub async fn write_leaf(
    store: &impl SummaryStore,
    id: &str,
    content: &str,
    when: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let stamp = at(when)?;
    store
        .write_summary(NewSummary {
            id: uid(id)?,
            conversation_id: uid(CONVERSATION)?,
            kind: SummaryKind::Leaf,
            depth: 0,
            content: content.to_owned(),
            message_count: 0,
            earliest_at: stamp,
            latest_at: stamp,
            model: "fixture".to_owned(),
            pipeline_version: 6,
            created_at: stamp,
            sources: Vec::new(),
            child_ids: Vec::new(),
        })
        .await?;
    Ok(())
}

fn at(text: &str) -> Result<Timestamp, jiff::Error> {
    text.parse()
}

fn uid(text: &str) -> Result<Uuid, uuid::Error> {
    Uuid::parse_str(text)
}

fn git(root: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    if output.status.success() {
        Ok(String::from_utf8(output.stdout)?)
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )
        .into())
    }
}
