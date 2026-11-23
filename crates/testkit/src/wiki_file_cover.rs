use std::path::Path;

use kernel::topics::TopicId;
use kernel::wiki::WikiLookup;
use kernel::wiki::builder::ExtractWiki;
use kernel::wiki::builder::WikiRouting;
use kernel::wiki::entity_pass::EntityLease;
use kernel::wiki::entity_pass::EntityProse;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::MentionNote;
use kernel::wiki::entity_pass::PreparedHistory;
use kernel::wiki::entity_pass::PublishOutcome;
use kernel::wiki::entity_pass::StoredRewrite;
use kernel::wiki::page::WikiAction;
use kernel::wiki::page::WikiSlug;
use wire::time::TimestampFormat as _;

use super::PageFileStore;
use super::Taken;
use crate::topics::Outcome;

const ORDER_ID: &str = "00000000-0000-4000-8000-0000000000d4";
const CROWD: &str = "00000000-0000-4000-8000-0000000000d6";
const FAIL_ID: &str = "00000000-0000-4000-8000-0000000000e3";
const FAIL_REWRITE: &str = "00000000-0000-4000-8000-0000000000e4";
const FAIL_MENTION: &str = "00000000-0000-4000-8000-0000000000e5";
const STALE_ID: &str = "00000000-0000-4000-8000-0000000000e6";
const MENTION: &str = "00000000-0000-4000-8000-0000000000e7";
const ORDER: &str = "2026-10-10T21:00:00Z";
const CHANGE: &str = "2026-10-10T17:30:00Z";
const PUBLISH: &str = "2026-10-10T22:00:00Z";
const MENTIONED: &str = "2026-10-10T23:00:00Z";
const CROWDED: &str = "2026-10-11T00:00:00Z";
const FAIL_TIME: &str = "2026-10-12T12:00:00Z";
const FAIL_LATER: &str = "2026-10-12T13:00:00Z";
const STALE_TIME: &str = "2026-10-12T14:00:00Z";
const STALE_LATER: &str = "2026-10-12T15:00:00Z";

pub const FAILED_SENTENCE: &str = "This rewrite must not land.";
pub const FAILED_MENTION: &str = "This mention must not land.";

pub(super) async fn rest(
    store: &impl PageFileStore,
    wiki: &Path,
    tin: &EntityView,
    steps: &mut Vec<Taken>,
) -> Outcome {
    order(store, wiki, steps).await?;
    change_and_remove(store, wiki, steps).await?;
    direct_publish(store, wiki, tin, steps).await?;
    mention(store, wiki, tin, steps).await?;
    crowd(store, wiki, steps).await?;
    Ok(())
}

pub(super) fn removed(wiki: &Path, slug: &str) -> Result<Taken, Box<dyn std::error::Error>> {
    let head = head(wiki)?;
    let spec = format!("topics/{slug}.md");
    let touched = super::git(wiki, &["log", "-1", "--format=%H", "--", &spec])?;
    assert_eq!(touched.trim(), head, "{slug}");
    let tree = super::pinned_tree(wiki)?;
    let commits = super::git(wiki, &["rev-list", "--count", "HEAD"])?;
    Ok(Taken {
        slug: slug.to_owned(),
        tree: tree.trim().to_owned(),
        commits: commits.trim().to_owned(),
        tip: "yes".to_owned(),
        message: super::git(wiki, &["log", "-1", "--format=%B"])?,
        body: "<removed>\n".to_owned(),
    })
}

pub async fn commit_failure_keeps_rows<G>(
    store: &impl PageFileStore,
    wiki: &Path,
    mut arm: impl FnMut() -> G,
) -> Outcome {
    let view = super::publish_entity(
        store,
        FAIL_ID,
        "faultbox",
        "A fault box.",
        "Noted faultbox.",
        FAIL_TIME,
    )
    .await?;
    let path = wiki.join("topics").join("faultbox.md");
    let head = super::git(wiki, &["rev-parse", "HEAD"])?;
    let head = head.trim().to_owned();
    let message = super::git(wiki, &["log", "-1", "--format=%B"])?;
    assert_eq!(super::stored_commit(store, "faultbox").await?, head);
    let mut lease = store.lease_entity(view.id).await?;
    let before = lease.applied_through().await?;
    refuse_git(&mut *lease, &mut arm, rewrite_body(store).await?).await?;
    assert_eq!(lease.applied_through().await?, before);
    stayed(&path, "Kept off the page.", wiki, &head, &message)?;
    assert_eq!(super::stored_commit(store, "faultbox").await?, head);
    refuse_git(&mut *lease, &mut arm, mention_body(store).await?).await?;
    assert_eq!(lease.applied_through().await?, before);
    stayed(&path, FAILED_MENTION, wiki, &head, &message)?;
    assert_eq!(super::stored_commit(store, "faultbox").await?, head);
    lease.release().await?;
    Ok(())
}

fn stayed(path: &Path, needle: &str, wiki: &Path, head: &str, message: &str) -> Outcome {
    let text = String::from_utf8(std::fs::read(path)?)?;
    assert!(text.contains(needle), "{text}");
    assert_eq!(super::git(wiki, &["rev-parse", "HEAD"])?.trim(), head);
    assert_eq!(super::git(wiki, &["log", "-1", "--format=%B"])?, message);
    Ok(())
}

pub async fn stale_publish_keeps_the_file(store: &impl PageFileStore, wiki: &Path) -> Outcome {
    let view = super::publish_entity(
        store,
        STALE_ID,
        "stalebox",
        "First prose.",
        "Noted stalebox.",
        STALE_TIME,
    )
    .await?;
    let path = wiki.join("topics").join("stalebox.md");
    let bytes = std::fs::read(&path)?;
    let outcome = store
        .publish(
            &view,
            &EntityProse {
                summary: "Replacement.".to_owned(),
                article: "This prose must not land.".to_owned(),
            },
            super::at(STALE_LATER)?,
        )
        .await?;
    match outcome {
        PublishOutcome::Stale(_) => {}
        PublishOutcome::Stored { .. } => {
            return Err("stale publish replaced the page".into());
        }
    }
    let after = std::fs::read(&path)?;
    assert_eq!(after, bytes);
    let text = String::from_utf8(after)?;
    assert!(!text.contains("must not land"));
    Ok(())
}

async fn order(store: &impl PageFileStore, wiki: &Path, steps: &mut Vec<Taken>) -> Outcome {
    super::write_leaf(store, ORDER_ID, "Shelves were labelled.", ORDER).await?;
    let patches = vec![
        super::patch(
            "zeta-shelf",
            "Zeta shelf",
            "Zeta shelf holds the jars.",
            None,
            WikiAction::Create,
        )?,
        super::patch(
            "alpha-shelf",
            "Alpha shelf",
            "Alpha shelf holds the tins.",
            None,
            WikiAction::Create,
        )?,
    ];
    store
        .apply_wiki(
            &ExtractWiki {
                summary_id: super::uid(ORDER_ID)?,
                conversation_id: Some(super::uid(super::CONVERSATION)?),
            },
            patches,
            &WikiRouting::default(),
            super::at(ORDER)?,
        )
        .await?;
    let subjects = super::git(wiki, &["log", "-2", "--format=%s"])?;
    let mut lines = subjects.lines();
    assert!(
        lines
            .next()
            .is_some_and(|line| line.contains("alpha-shelf")),
        "{subjects}",
    );
    assert!(
        lines.next().is_some_and(|line| line.contains("zeta-shelf")),
        "{subjects}",
    );
    steps.push(own_commit(store, wiki, "zeta-shelf").await?);
    steps.push(super::take(store, wiki, "alpha-shelf").await?);
    Ok(())
}

async fn change_and_remove(
    store: &impl PageFileStore,
    wiki: &Path,
    steps: &mut Vec<Taken>,
) -> Outcome {
    let slug = WikiSlug::try_from("field-notes".to_owned())?;
    store
        .change_page(
            TopicId::from_uuid(super::uid(super::TOPIC)?),
            &slug,
            &|page| {
                "Notes from the late bench.".clone_into(&mut page.summary);
            },
            super::at(CHANGE)?,
        )
        .await?;
    steps.push(super::take(store, wiki, "field-notes").await?);
    store.remove_page(&slug).await?;
    super::absent(wiki, "field-notes");
    steps.push(removed(wiki, "field-notes")?);
    Ok(())
}

async fn direct_publish(
    store: &impl PageFileStore,
    wiki: &Path,
    tin: &EntityView,
    steps: &mut Vec<Taken>,
) -> Outcome {
    let path = wiki.join("topics").join("tinbox.md");
    let bytes = std::fs::read(&path)?;
    let outcome = store
        .publish(
            tin,
            &EntityProse {
                summary: "Stale summary.".to_owned(),
                article: "This prose must not land.".to_owned(),
            },
            super::at(PUBLISH)?,
        )
        .await?;
    let PublishOutcome::Stale(fresh) = outcome else {
        return Err("direct publish of a stale view replaced tinbox".into());
    };
    assert_eq!(std::fs::read(&path)?, bytes);
    let stored = store
        .publish(
            &fresh,
            &EntityProse {
                summary: "Direct summary.".to_owned(),
                article: "Direct article.".to_owned(),
            },
            super::at(PUBLISH)?,
        )
        .await?;
    let PublishOutcome::Stored { slug, .. } = stored else {
        return Err("fresh direct publish was stale".into());
    };
    assert_eq!(slug, "tinbox");
    steps.push(super::take(store, wiki, "tinbox").await?);
    Ok(())
}

async fn mention(
    store: &impl PageFileStore,
    wiki: &Path,
    tin: &EntityView,
    steps: &mut Vec<Taken>,
) -> Outcome {
    super::write_leaf(store, MENTION, "Tinbox was mentioned again.", MENTIONED).await?;
    let when = super::at(MENTIONED)?;
    let mut lease = store.lease_entity(tin.id).await?;
    let sha = lease
        .record_mention(
            &MentionNote {
                history: Some(PreparedHistory {
                    summary_id: super::uid(MENTION)?,
                    conversation_id: Some(super::uid(super::CONVERSATION)?),
                    session_on: when.utc_date(),
                    sentence: "Mentioned after publication.".to_owned(),
                }),
                session_at: when,
                session_id: super::uid(MENTION)?,
                owner: crate::OWNER.to_owned(),
            },
            when,
        )
        .await?;
    assert!(sha.is_some());
    lease.release().await?;
    steps.push(super::take(store, wiki, "tinbox").await?);
    Ok(())
}

async fn crowd(store: &impl PageFileStore, wiki: &Path, steps: &mut Vec<Taken>) -> Outcome {
    super::write_leaf(store, CROWD, "The shelf ran out of room.", CROWDED).await?;
    let mut patch = super::patch(
        "crowded-shelf",
        "Crowded shelf",
        "Every jar has a label.",
        Some("The labels face out."),
        WikiAction::Create,
    )?;
    patch.entities = (1..=65)
        .map(|number| format!("entity-{number:02}"))
        .collect();
    store
        .apply_wiki(
            &ExtractWiki {
                summary_id: super::uid(CROWD)?,
                conversation_id: Some(super::uid(super::CONVERSATION)?),
            },
            vec![patch],
            &WikiRouting::default(),
            super::at(CROWDED)?,
        )
        .await?;
    let mut step = super::take(store, wiki, "crowded-shelf").await?;
    let slug = WikiSlug::try_from("crowded-shelf".to_owned())?;
    let archive = store
        .wiki_archive(&slug, None)
        .await?
        .ok_or("crowded shelf archive is missing")?;
    assert!(archive.contains("entity-01"), "{archive}");
    let archive = super::redact_commits(wiki, &archive)?;
    assert!(archive.contains("last inline at <commit>"), "{archive}");
    step.body.push_str("ARCHIVE\n");
    step.body.push_str(&archive);
    steps.push(step);
    Ok(())
}

async fn own_commit(
    store: &impl PageFileStore,
    wiki: &Path,
    slug: &str,
) -> Result<Taken, Box<dyn std::error::Error>> {
    let spec = format!("topics/{slug}.md");
    let message = super::git(wiki, &["log", "-1", "--format=%B", "--", &spec])?;
    let mut step = super::take(store, wiki, slug).await?;
    step.message = message;
    Ok(step)
}

fn head(wiki: &Path) -> Result<String, Box<dyn std::error::Error>> {
    Ok(super::git(wiki, &["rev-parse", "HEAD"])?.trim().to_owned())
}

enum GitAttempt {
    Rewrite(StoredRewrite),
    Mention(MentionNote),
}

async fn rewrite_body(
    store: &impl PageFileStore,
) -> Result<GitAttempt, Box<dyn std::error::Error>> {
    super::write_leaf(store, FAIL_REWRITE, "A later fault.", FAIL_LATER).await?;
    let when = super::at(FAIL_LATER)?;
    let id = super::uid(FAIL_REWRITE)?;
    let conversation = super::uid(super::CONVERSATION)?;
    Ok(GitAttempt::Rewrite(StoredRewrite {
        summary: "A later fault.".to_owned(),
        article: "Kept off the page.".to_owned(),
        facts: Vec::new(),
        history: Some(PreparedHistory {
            summary_id: id,
            conversation_id: Some(conversation),
            session_on: when.utc_date(),
            sentence: FAILED_SENTENCE.to_owned(),
        }),
        owner: crate::OWNER.to_owned(),
        session_at: when,
        session_id: id,
        evidence: FAILED_SENTENCE.to_owned(),
    }))
}

async fn mention_body(
    store: &impl PageFileStore,
) -> Result<GitAttempt, Box<dyn std::error::Error>> {
    super::write_leaf(store, FAIL_MENTION, "A mention that fails.", FAIL_LATER).await?;
    let when = super::at(FAIL_LATER)?;
    let id = super::uid(FAIL_MENTION)?;
    Ok(GitAttempt::Mention(MentionNote {
        history: Some(PreparedHistory {
            summary_id: id,
            conversation_id: Some(super::uid(super::CONVERSATION)?),
            session_on: when.utc_date(),
            sentence: FAILED_MENTION.to_owned(),
        }),
        session_at: when,
        session_id: id,
        owner: crate::OWNER.to_owned(),
    }))
}

async fn refuse_git<G>(
    lease: &mut dyn EntityLease,
    arm: &mut impl FnMut() -> G,
    attempt: GitAttempt,
) -> Outcome {
    let _guard = arm();
    let failed = match attempt {
        GitAttempt::Rewrite(body) => lease
            .apply(&body, super::at(FAIL_LATER)?)
            .await
            .map(|_| ())
            .err(),
        GitAttempt::Mention(note) => lease
            .record_mention(&note, super::at(FAIL_LATER)?)
            .await
            .map(|_| ())
            .err(),
    };
    let Some(error) = failed else {
        return Err("git failure still published the entity page".into());
    };
    let rendered = format!("{error} {error:?}");
    assert!(rendered.contains("injected git failure"), "{rendered}");
    Ok(())
}

pub(super) async fn missing_file(store: &impl PageFileStore, wiki: &Path) -> Outcome {
    const ID: &str = "00000000-0000-4000-8000-0000000000d8";
    const WHEN: &str = "2026-10-11T02:00:00Z";
    super::write_leaf(store, ID, "Nowhere.", WHEN).await?;
    let slug = WikiSlug::try_from("nowhere".to_owned())?;
    store
        .apply_wiki(
            &ExtractWiki {
                summary_id: super::uid(ID)?,
                conversation_id: Some(super::uid(super::CONVERSATION)?),
            },
            vec![super::patch(
                "nowhere",
                "Nowhere",
                "Nowhere.",
                Some("No file."),
                WikiAction::Create,
            )?],
            &WikiRouting::default(),
            super::at(WHEN)?,
        )
        .await?;
    let path = wiki.join("topics").join("nowhere.md");
    std::fs::remove_file(&path)?;
    store.remove_page(&slug).await?;
    let lookup = store.wiki_page(&slug).await?;
    assert!(matches!(lookup, WikiLookup::Missing));
    assert!(!path.exists());
    Ok(())
}
