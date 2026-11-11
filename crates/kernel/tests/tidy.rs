use std::collections::BTreeSet;

use chrono::DateTime;
use chrono::Utc;
use kernel::filing::Refusal;
use kernel::filing::Target;
use kernel::queue::WorkerRole;
use kernel::tidy::Guard;
use kernel::tidy::Refused;
use kernel::tidy::RoleActivity;
use kernel::tidy::Suggested;
use kernel::tidy::TaskActivity;
use kernel::tidy::TreeEdit;
use kernel::tidy::clusters;
use kernel::tidy::duplicates;
use kernel::topics::NewTopic;
use kernel::topics::Slug;
use kernel::topics::Topic;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::topics::TopicListing;
use kernel::topics::TopicName;
use kernel::wiki::page::WikiSlug;
use values::SessionId;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn listing(
    name: &str,
    kind: TopicKind,
    parent: Option<TopicId>,
    sessions: u64,
) -> TestResult<TopicListing> {
    let new = NewTopic {
        kind,
        name: TopicName::new(name)?,
        aliases: Vec::new(),
        parent,
        directory: None,
        owner: testkit::owner().ok(),
    };
    Ok(TopicListing {
        topic: Topic::create(TopicId::random(), new, DateTime::<Utc>::UNIX_EPOCH)?,
        session_count: sessions,
        last_active: None,
    })
}

fn members(count: u8) -> Vec<Target> {
    (0..count)
        .map(|n| {
            Target::Session(SessionId::new(
                values::HarnessId::ClaudeCode,
                &format!("00000000-0000-4000-8000-0000000000{n:02}"),
            ))
        })
        .collect()
}

fn create(name: &str, count: u8) -> TestResult<TreeEdit> {
    Ok(TreeEdit::CreateTopic {
        topic: TopicId::random(),
        name: TopicName::new(name)?,
        parent: None,
        aliases: Vec::new(),
        members: members(count),
    })
}

#[test]
fn fingerprints_ignore_merge_order_and_a_new_topic_s_parent() -> TestResult {
    let (a, b) = (TopicId::random(), TopicId::random());
    let merge = |source, into| TreeEdit::Merge { source, into };
    assert_eq!(
        merge(a, b).fingerprint().unwrap(),
        merge(b, a).fingerprint().unwrap()
    );
    let under = TreeEdit::CreateTopic {
        topic: TopicId::random(),
        name: TopicName::new("Release tooling").unwrap(),
        parent: Some(a),
        aliases: Vec::new(),
        members: Vec::new(),
    };
    assert_eq!(
        under.fingerprint().unwrap(),
        create("Release Tooling", 3)?.fingerprint().unwrap()
    );
    Ok(())
}

#[test]
fn a_filing_s_fingerprint_is_its_set_of_members() {
    let topic = TopicId::random();
    let file = |members: Vec<Target>| TreeEdit::FileInto { topic, members }.fingerprint();
    let six = members(6);
    let (first, second) = six.split_at(3);
    assert_ne!(
        file(first.to_vec()).unwrap(),
        file(second.to_vec()).unwrap()
    );
    let mut reordered = first.to_vec();
    reordered.reverse();
    assert_eq!(file(first.to_vec()).unwrap(), file(reordered).unwrap());
}

#[test]
fn a_page_fold_s_fingerprint_keeps_its_direction() -> TestResult {
    let fold = |page: &str, into: &str| -> TestResult<_> {
        Ok(TreeEdit::MergePage {
            page: WikiSlug::try_from(page.to_owned())?,
            into: WikiSlug::try_from(into.to_owned())?,
        }
        .fingerprint()?)
    };
    assert_ne!(
        fold("acmekit-deploys", "acmekit")?,
        fold("acmekit", "acmekit-deploys")?
    );
    Ok(())
}

#[test]
fn the_guard_refuses_small_titled_tombstoned_and_known_changes() -> TestResult {
    let app = listing("Desktop App", TopicKind::Work, None, 4)?;
    let closed = {
        let mut closed = listing("Old Spike", TopicKind::Work, None, 0)?;
        closed.topic.closed_at = Some(DateTime::<Utc>::UNIX_EPOCH);
        closed
    };
    let people = listing("People", TopicKind::Type, None, 0)?;
    let ada = listing("Ada", TopicKind::Entity, Some(people.topic.id), 0)?;
    let topics = vec![app.clone(), closed.clone(), people, ada.clone()];
    let rejected = create("Rejected Area", 3)?;
    let known = BTreeSet::from([rejected.fingerprint().unwrap()]);
    let tombstoned = BTreeSet::from([(TopicKind::Work, Slug::fold("Gone Area").unwrap())]);
    let guard = Guard::new(&topics, &known, &tombstoned);
    let cases = [
        (create("Release Tooling", 3)?, Ok(())),
        (
            create("Release Tooling", 2)?,
            Err(Refused::TooFewSessions { found: 2 }),
        ),
        (
            create("Fix the flaky release script today", 3)?,
            Err(Refused::Name(Refusal::Title)),
        ),
        (
            create("v2 Rollout", 3)?,
            Err(Refused::Name(Refusal::Version)),
        ),
        (create("Gone Area", 3)?, Err(Refused::Tombstoned)),
        (create("Desktop App", 3)?, Err(Refused::Taken)),
        (rejected, Err(Refused::Known)),
        (
            TreeEdit::Close {
                topic: closed.topic.id,
            },
            Err(Refused::Closed),
        ),
        (
            TreeEdit::Delete {
                topic: app.topic.id,
            },
            Err(Refused::NotEmpty),
        ),
        (
            TreeEdit::Move {
                topic: ada.topic.id,
                parent: None,
            },
            Err(Refused::Misplaced),
        ),
        (
            TreeEdit::Merge {
                source: ada.topic.id,
                into: app.topic.id,
            },
            Err(Refused::Misplaced),
        ),
    ];
    for (change, expected) in cases {
        assert_eq!(guard.check(&change), expected, "{change:?}");
    }
    Ok(())
}

#[test]
fn suggestions_cluster_by_name_and_duplicates_fold_into_the_busier_topic() -> TestResult {
    let parent = TopicId::random();
    let suggested = |n: u8, name: &str, under: Option<TopicId>| Suggested {
        session: SessionId::new(
            values::HarnessId::ClaudeCode,
            &format!("00000000-0000-4000-8000-0000000001{n:02}"),
        ),
        name: TopicName::new(name).unwrap(),
        parent: under,
        title: None,
    };
    let grouped = clusters(vec![
        suggested(1, "Release tooling", Some(parent)),
        suggested(2, "release-tooling", Some(parent)),
        suggested(3, "Release Tooling", None),
        suggested(4, "Voice", None),
    ]);
    assert_eq!(
        grouped
            .iter()
            .map(|cluster| (cluster.sessions.len(), cluster.parent))
            .collect::<Vec<_>>(),
        [(3, Some(parent)), (1, None)]
    );
    let busy = listing("Desktop App", TopicKind::Work, None, 9)?;
    let quiet = listing("Desktop Apps", TopicKind::Work, None, 1)?;
    let other = listing("Voice", TopicKind::Work, None, 1)?;
    assert_eq!(
        duplicates(&[quiet.clone(), busy.clone(), other]),
        [(quiet.topic.id, busy.topic.id)]
    );
    Ok(())
}

#[test]
fn a_role_reports_its_newest_finish_and_its_failing_jobs() {
    let at = |seconds| DateTime::<Utc>::from_timestamp(seconds, 0);
    let tasks = [
        TaskActivity {
            task: "embed-target".to_owned(),
            last_finished: at(10),
            failing: 2,
            queued: 3,
        },
        TaskActivity {
            task: "enqueue-unembedded".to_owned(),
            last_finished: at(20),
            failing: 0,
            queued: 4,
        },
        TaskActivity {
            task: "file-conversation".to_owned(),
            last_finished: at(30),
            failing: 1,
            queued: 5,
        },
    ];
    let embedder = RoleActivity::of(WorkerRole::Embedder, &tasks);
    assert_eq!(
        (embedder.last_finished, embedder.failing, embedder.queued),
        (at(20), 2, 7)
    );
    let wiki = RoleActivity::of(WorkerRole::WikiBuilder, &tasks);
    assert_eq!(
        (wiki.last_finished, wiki.failing, wiki.queued),
        (None, 0, 0)
    );
}
