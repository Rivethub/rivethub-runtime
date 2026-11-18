use jiff::Timestamp;
use std::collections::BTreeSet;

use kernel::filing::ConversationPlacement;
use kernel::filing::Correction;
use kernel::filing::Evidence;
use kernel::filing::Filable;
use kernel::filing::Refusal;
use kernel::filing::Rule;
use kernel::filing::Step;
use kernel::filing::Target;
use kernel::filing::conversation_keys;
use kernel::filing::session_key;
use kernel::filing::stored_keys;
use kernel::memory::ConversationHead;
use kernel::topics::Alias;
use kernel::topics::NewTopic;
use kernel::topics::PlacementSource;
use kernel::topics::Redirect;
use kernel::topics::Root;
use kernel::topics::Slug;
use kernel::topics::Topic;
use kernel::topics::TopicError;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::topics::TopicListing;
use kernel::topics::TopicName;
use uuid::Uuid;
use values::SessionId;

type TestResult = Result<(), Box<dyn std::error::Error>>;

static NO_GENERIC: BTreeSet<String> = BTreeSet::new();

const TASK: &str = "0199e2a0-7b3c-7a00-8000-0000000000a1";

fn at(minute: i8) -> Timestamp {
    values::time::timestamp_at_utc(2026, 10, 29, 12, minute, 0).unwrap_or_default()
}

fn head(session_key: &str, channel: &str) -> ConversationHead {
    ConversationHead {
        id: Uuid::nil(),
        session_key: session_key.to_owned(),
        agent: "rivet".to_owned(),
        channel: channel.to_owned(),
        title: None,
    }
}

fn topic(name: &str, aliases: &[&str], directory: Option<&str>) -> Result<Topic, TopicError> {
    Topic::create(
        TopicId::random(),
        NewTopic {
            kind: TopicKind::Work,
            name: TopicName::new(name)?,
            aliases: aliases
                .iter()
                .map(|alias| Alias::new(alias))
                .collect::<Result<_, _>>()?,
            parent: None,
            directory: directory.map(|path| Root::Shared {
                path: path.to_owned(),
            }),
            owner: testkit::owner().ok(),
        },
        at(0),
    )
}

fn listed(topics: &[&Topic]) -> Vec<TopicListing> {
    topics
        .iter()
        .map(|topic| TopicListing {
            topic: (*topic).clone(),
            session_count: 0,
            last_active: None,
        })
        .collect()
}

fn evidence<'a>(title: Option<&'a str>, summary: &'a str) -> Evidence<'a> {
    Evidence {
        placed: None,
        corrected: false,
        child: false,
        parent_topic: None,
        cwd: None,
        title,
        summary: Some(summary),
        generic: &NO_GENERIC,
    }
}

#[test]
fn keys_sort_into_sessions_tasks_conversations_and_skips() -> TestResult {
    let dsh: SessionId = "dsh:7f3a".parse()?;
    assert_eq!(Filable::of(&head("dsh:7f3a", "dsh")), Filable::Session(dsh));
    let bare = "1ec40c91-7133-4fe1-bafc-30da0693ccb2";
    let claude: SessionId = format!("claude-code:{bare}").parse()?;
    assert_eq!(
        Filable::of(&head(bare, "claude-code")),
        Filable::Session(claude.clone())
    );
    assert_eq!(Filable::of(&head(bare, "gateway")), Filable::Conversation);
    assert_eq!(
        Filable::of(&head(&format!("task:{TASK}"), "gateway")),
        Filable::Task(Uuid::parse_str(TASK)?)
    );
    assert_eq!(
        Filable::of(&head("task:soon", "gateway")),
        Filable::Conversation
    );
    assert_eq!(
        Filable::of(&head("qwen-code:41", "qwen-code")),
        Filable::Conversation
    );
    for (key, channel) in [
        ("grokbot-run-sand-subagent-5b1f-v4", "grokbot"),
        ("heartbeat:rivet", "gateway"),
        ("claude-code:abc", "test-channel"),
        ("smoke", "env-test-channel"),
    ] {
        assert_eq!(Filable::of(&head(key, channel)), Filable::Never, "{key}");
    }
    assert_eq!(conversation_keys(&claude), [claude.as_str(), bare]);
    assert_eq!(session_key(Some("claude-code"), bare), claude.as_str());
    assert_eq!(session_key(Some("gateway"), bare), bare);
    assert_eq!(session_key(None, bare), bare);
    assert_eq!(session_key(Some("dsh"), "dsh:7f3a"), "dsh:7f3a");
    assert_eq!(stored_keys(claude.as_str()), [claude.as_str(), bare]);
    assert_eq!(stored_keys("dsh:7f3a"), ["dsh:7f3a"]);
    Ok(())
}

#[test]
fn filing_follows_placement_then_parent_then_folder_then_name() -> TestResult {
    let rivetos = topic("RivetOS", &["rivet"], Some("/srv/shared/RivetOS"))?;
    let desktop = topic(
        "Desktop App",
        &["rivethub-desktop"],
        Some("/home/dana/work/rhd"),
    )?;
    let topics = listed(&[&rivetos, &desktop]);
    let summary = "Reworked the rivethub-desktop topic screen.";
    let placed = Evidence {
        placed: Some(PlacementSource::You),
        parent_topic: Some(rivetos.id),
        ..evidence(None, summary)
    };
    assert_eq!(
        placed.decide(&topics, &[]),
        Step::Placed(PlacementSource::You)
    );
    let task = Evidence {
        parent_topic: Some(rivetos.id),
        ..evidence(None, summary)
    };
    assert_eq!(
        task.decide(&topics, &[]),
        Step::File {
            topic: rivetos.id,
            rule: Rule::Parent
        }
    );
    let child = Evidence {
        child: true,
        ..evidence(None, summary)
    };
    assert_eq!(child.decide(&topics, &[]), Step::Inherits);
    let folder = Evidence {
        cwd: Some("/srv/shared/RivetOS/packages/core"),
        ..evidence(None, summary)
    };
    assert_eq!(
        folder.decide(&topics, &[]),
        Step::File {
            topic: rivetos.id,
            rule: Rule::Folder
        }
    );
    let sibling = Evidence {
        cwd: Some("/home/dana/work/rhd-next"),
        ..evidence(None, summary)
    };
    assert_eq!(
        sibling.decide(&topics, &[]),
        Step::File {
            topic: desktop.id,
            rule: Rule::Name
        }
    );
    let named = evidence(Some("Topic screen"), summary);
    assert_eq!(
        named.decide(&topics, &[]),
        Step::File {
            topic: desktop.id,
            rule: Rule::Name
        }
    );
    let both = evidence(None, "Ran rivet capture against rivethub-desktop.");
    assert_eq!(both.decide(&topics, &[]), Step::Ask { named: None });
    let renamed = Redirect {
        slug: Slug::fold("Tauri Shell")?,
        topic: desktop.id,
    };
    let redirected = evidence(None, "Moved the tauri shell window.");
    assert_eq!(
        redirected.decide(&topics, &[renamed]),
        Step::File {
            topic: desktop.id,
            rule: Rule::Name
        }
    );
    Ok(())
}

#[test]
fn repos_decide_before_names_and_common_or_parent_names_leave_it_to_the_model() -> TestResult {
    let acme = topic("AcmeKit", &["acme"], None)?;
    let desktop = topic("Desktop App", &["acme-desktop"], None)?;
    let mut nightly = topic("Nightly Builds", &[], None)?;
    nightly.parent = Some(acme.id);
    let topics = listed(&[&acme, &desktop, &nightly]);
    let review = evidence(None, "Reviewed acme-desktop PR #41 for the AcmeKit team.");
    assert_eq!(
        review.decide(&topics, &[]),
        Step::File {
            topic: desktop.id,
            rule: Rule::Repo
        }
    );
    let parent = evidence(None, "Tuned the AcmeKit release notes.");
    assert_eq!(
        parent.decide(&topics, &[]),
        Step::Ask {
            named: Some(acme.id)
        }
    );
    let generic = BTreeSet::from(["acme".to_owned(), "acmekit".to_owned()]);
    let common = Evidence {
        generic: &generic,
        ..evidence(None, "The acme agent opened the Desktop App settings.")
    };
    assert_eq!(
        common.decide(&topics, &[]),
        Step::File {
            topic: desktop.id,
            rule: Rule::Name
        }
    );
    Ok(())
}

#[test]
fn the_deepest_owned_directory_wins_and_a_tie_falls_through() -> TestResult {
    let rivetos = topic("RivetOS", &[], Some("/srv/shared/RivetOS/"))?;
    let capture = topic("Capture", &[], Some("/srv/shared/RivetOS/packages/capture"))?;
    let twin = topic(
        "Capture Twin",
        &[],
        Some("/srv/shared/RivetOS/packages/capture"),
    )?;
    let deeper = Evidence {
        cwd: Some("/srv/shared/RivetOS/packages/capture/src"),
        ..evidence(None, "")
    };
    assert_eq!(
        deeper.decide(&listed(&[&rivetos, &capture]), &[]),
        Step::File {
            topic: capture.id,
            rule: Rule::Folder
        }
    );
    assert_eq!(
        deeper.decide(&listed(&[&rivetos, &capture, &twin]), &[]),
        Step::Ask { named: None }
    );
    let prefix_only = Evidence {
        cwd: Some("/srv/shared/RivetOSX"),
        ..evidence(None, "")
    };
    assert_eq!(
        prefix_only.decide(&listed(&[&rivetos]), &[]),
        Step::Ask { named: None }
    );
    Ok(())
}

#[test]
fn suggested_names_that_read_like_junk_are_refused() -> TestResult {
    let refused = |name: &str, title: Option<&str>| -> Result<Option<Refusal>, TopicError> {
        Ok(Refusal::of(&TopicName::new(name)?, title))
    };
    assert_eq!(refused("Local Model Dev", None)?, None);
    assert_eq!(refused("Tompkins Family Hub", None)?, None);
    assert_eq!(refused("1cat-vllm", None)?, None);
    assert_eq!(
        refused(
            "Counting with sleep delays",
            Some("Counting with sleep delays")
        )?,
        Some(Refusal::Title)
    );
    assert_eq!(
        refused("Four item shell todo date uptime", None)?,
        Some(Refusal::Title)
    );
    assert_eq!(refused("Fix the build.", None)?, Some(Refusal::Title));
    assert_eq!(refused("rivet-shared/wiki", None)?, Some(Refusal::Path));
    assert_eq!(refused("chat.tsx", None)?, Some(Refusal::Path));
    assert_eq!(refused("Qwen3.5 eval", None)?, Some(Refusal::Version));
    assert_eq!(refused("client-sync.mjs", None)?, Some(Refusal::Path));
    assert_eq!(refused("families.app", None)?, None);
    assert_eq!(refused("V100 GPU Cluster", None)?, None);
    assert_eq!(refused("PR 1145", None)?, Some(Refusal::Ticket));
    assert_eq!(refused("ENG-1203", None)?, Some(Refusal::Ticket));
    assert_eq!(refused("issue #971", None)?, Some(Refusal::Ticket));
    assert_eq!(refused("Runtime v2", None)?, Some(Refusal::Version));
    assert_eq!(refused("feat-entities", None)?, Some(Refusal::Branch));
    assert_eq!(refused("d39da662", None)?, Some(Refusal::Identifier));
    Ok(())
}

#[test]
fn a_filing_you_corrected_is_left_alone() -> TestResult {
    let rivetos = topic("RivetOS", &[], None)?;
    let topics = listed(&[&rivetos]);
    let corrected = Evidence {
        corrected: true,
        ..evidence(Some("RivetOS performance lag"), "RivetOS felt slow.")
    };
    assert_eq!(corrected.decide(&topics, &[]), Step::Corrected);
    Ok(())
}

#[test]
fn only_moving_a_categorizer_filing_is_a_correction() -> TestResult {
    let (from, to) = (TopicId::random(), TopicId::random());
    let target = Target::Conversation(Uuid::parse_str(TASK)?);
    let categorizer = Some((PlacementSource::Categorizer, from));
    assert_eq!(
        Correction::of(target.clone(), categorizer, Some(to), at(5)),
        Some(Correction {
            target: target.clone(),
            from,
            to: Some(to),
            at: at(5),
        })
    );
    assert!(Correction::of(target.clone(), categorizer, None, at(5)).is_some());
    assert_eq!(
        Correction::of(target.clone(), categorizer, Some(from), at(5)),
        None
    );
    let yours = Some((PlacementSource::You, from));
    assert_eq!(Correction::of(target.clone(), yours, Some(to), at(5)), None);
    assert_eq!(Correction::of(target, None, Some(to), at(5)), None);
    let conversation = Uuid::parse_str(TASK)?;
    let filed = |by, topic, minute| ConversationPlacement {
        conversation,
        topic,
        by,
        at: at(minute),
    };
    let mine = filed(PlacementSource::You, from, 1);
    assert_eq!(
        filed(PlacementSource::Categorizer, to, 2).admit(Some(mine.clone())),
        mine
    );
    let moved = filed(PlacementSource::You, to, 3);
    assert_eq!(moved.clone().admit(Some(mine)), moved);
    Ok(())
}
