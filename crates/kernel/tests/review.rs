use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use kernel::facts::Claim;
use kernel::facts::Evidence;
use kernel::facts::FactId;
use kernel::facts::FactKey;
use kernel::facts::FactValue;
use kernel::facts::Said;
use kernel::memory::TagId;
use kernel::owner::Owner;
use kernel::review::FACT_NEW_FOR;
use kernel::review::FilingTarget;
use kernel::review::Mark;
use kernel::review::Marks;
use kernel::review::ReviewKey;
use kernel::review::ReviewMark;
use kernel::review::SNOOZE;
use kernel::review::fact_reviews;
use kernel::tidy::ProposalId;
use kernel::topics::Slug;
use kernel::topics::TopicId;
use uuid::Uuid;
use values::HarnessId;
use values::SessionId;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn hours(count: i64) -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH + TimeDelta::days(20_000) + TimeDelta::hours(count)
}

fn session(n: u8) -> SessionId {
    SessionId::new(
        HarnessId::ClaudeCode,
        &format!("00000000-0000-4000-8000-0000000000{n:02}"),
    )
}

struct Key {
    fact: FactId,
    key: FactKey,
}

impl Key {
    fn new(key: &str) -> TestResult<Self> {
        Ok(Self {
            fact: FactId::random(),
            key: FactKey::new(key)?,
        })
    }

    fn writer(&self, value: &str, from: u8, at: i64) -> TestResult<Claim> {
        self.claim(
            value,
            Said::Writer {
                session: session(from),
                summary: Uuid::new_v4(),
                evidence: Evidence::quote(&format!("we moved it to {value} today")),
                settled: false,
            },
            at,
        )
    }

    fn settled(&self, value: &str, from: u8, at: i64) -> TestResult<Claim> {
        let mut claim = self.writer(value, from, at)?;
        if let Said::Writer { settled, .. } = &mut claim.said {
            *settled = true;
        }
        Ok(claim)
    }

    fn you(&self, value: &str, pinned: bool, at: i64) -> TestResult<Claim> {
        self.claim(
            value,
            Said::You {
                by: Owner::new("phil".into())?,
                pinned,
            },
            at,
        )
    }

    fn claim(&self, value: &str, said: Said, at: i64) -> TestResult<Claim> {
        Ok(Claim {
            fact: self.fact,
            key: self.key.clone(),
            value: FactValue::new(value)?,
            said,
            at: hours(at),
            recorded_at: hours(at),
        })
    }
}

#[test]
fn every_review_key_reads_back_as_itself() -> TestResult {
    let keys = [
        ReviewKey::Conflict {
            fact: FactId::random(),
            version: 1_700_000_000_123,
        },
        ReviewKey::Fact {
            fact: FactId::random(),
            version: 42,
        },
        ReviewKey::Filing {
            session: session(7),
            into: FilingTarget::Topic(TopicId::random()),
        },
        ReviewKey::Filing {
            session: SessionId::new(HarnessId::Codex, "rollout.2026-11-07.v2"),
            into: FilingTarget::New(Slug::fold("Home lab backups")?),
        },
        ReviewKey::Tag(TagId::from_uuid(Uuid::new_v4())),
        ReviewKey::Tidy("00000000-0000-4000-8000-0000000000aa".parse::<ProposalId>()?),
    ];
    for key in keys {
        let text = key.to_string();
        assert_eq!(text.parse::<ReviewKey>()?, key, "{text} round trips");
    }
    Ok(())
}

#[test]
fn a_key_that_names_no_item_is_refused() {
    for text in [
        "",
        "conflict",
        "conflict.not-a-uuid.1",
        "fact.00000000-0000-4000-8000-0000000000aa.soon",
        "filing.claude-code:abc.new-Not A Slug",
        "filing.nobody.00000000-0000-4000-8000-0000000000aa",
        "tag.7",
        "entity.00000000-0000-4000-8000-0000000000aa",
    ] {
        assert!(text.parse::<ReviewKey>().is_err(), "{text:?} is refused");
    }
}

#[test]
fn sources_that_disagree_make_one_conflict_with_quoted_evidence() -> TestResult {
    let backup = Key::new("backup time")?;
    let claims = vec![backup.writer("02:30", 1, 0)?, backup.writer("03:00", 2, 2)?];
    let reviews = fact_reviews(&claims, hours(3));
    let [review] = reviews.as_slice() else {
        return Err(format!("one review expected, got {reviews:?}").into());
    };
    assert_eq!(
        review.key,
        ReviewKey::Conflict {
            fact: backup.fact,
            version: hours(2).timestamp_millis(),
        }
    );
    let quoted: Vec<(&str, Option<&str>)> = review
        .quotes
        .iter()
        .map(|quote| {
            (
                quote.support.value.as_str(),
                quote.words.as_ref().map(Evidence::as_str),
            )
        })
        .collect();
    assert_eq!(
        quoted,
        [
            ("03:00", Some("we moved it to 03:00 today")),
            ("02:30", Some("we moved it to 02:30 today")),
        ]
    );
    assert_eq!(review.at, hours(2));
    Ok(())
}

#[test]
fn a_new_writer_fact_is_reviewed_until_it_is_old_or_yours() -> TestResult {
    let os = Key::new("os")?;
    let fresh = vec![
        os.writer("Ubuntu 22.04", 1, 0)?,
        os.writer("Ubuntu 24.04", 2, 48)?,
    ];
    let reviews = fact_reviews(&fresh, hours(50));
    let [review] = reviews.as_slice() else {
        return Err(format!("one review expected, got {reviews:?}").into());
    };
    assert_eq!(
        review.key,
        ReviewKey::Fact {
            fact: os.fact,
            version: hours(48).timestamp_millis(),
        }
    );
    assert_eq!(review.state.value.as_str(), "Ubuntu 24.04");
    assert_eq!(
        review
            .state
            .previous
            .as_ref()
            .map(|previous| previous.value.as_str()),
        Some("Ubuntu 22.04")
    );

    let later = hours(48) + FACT_NEW_FOR + TimeDelta::hours(1);
    assert!(
        fact_reviews(&fresh, later).is_empty(),
        "a fact recorded more than the window ago is no longer new"
    );

    let mut agreed = fresh;
    agreed.insert(0, os.you("Ubuntu 24.04", false, 1)?);
    assert!(
        fact_reviews(&agreed, hours(50)).is_empty(),
        "a value you set yourself is not news"
    );

    let set = vec![
        os.writer("Ubuntu 22.04", 1, 0)?,
        os.you("Debian 12", false, 5)?,
    ];
    assert!(
        fact_reviews(&set, hours(6)).is_empty(),
        "a value you hold is not reviewed"
    );
    Ok(())
}

#[test]
fn a_pinned_value_disputed_by_a_writer_is_a_conflict() -> TestResult {
    let address = Key::new("address")?;
    let claims = vec![
        address.you("192.0.2.4", true, 0)?,
        address.writer("192.0.2.9", 1, 10)?,
    ];
    let reviews = fact_reviews(&claims, hours(11));
    let [review] = reviews.as_slice() else {
        return Err(format!("one review expected, got {reviews:?}").into());
    };
    assert!(matches!(review.key, ReviewKey::Conflict { .. }));
    assert_eq!(review.state.value.as_str(), "192.0.2.4");
    assert!(review.state.pinned);
    Ok(())
}

#[test]
fn settled_writer_facts_are_not_reviewed_unless_they_conflict() -> TestResult {
    let os = Key::new("os")?;
    let ordinary = vec![
        os.settled("Ubuntu 22.04", 1, 0)?,
        os.settled("Ubuntu 24.04", 2, 48)?,
    ];
    assert!(
        fact_reviews(&ordinary, hours(50)).is_empty(),
        "ordinary settled facts are not review items"
    );

    let backup = Key::new("backup time")?;
    let split = vec![
        backup.settled("02:30", 1, 0)?,
        backup.settled("03:00", 2, 2)?,
    ];
    let reviews = fact_reviews(&split, hours(3));
    let [review] = reviews.as_slice() else {
        return Err(format!("one conflict expected, got {reviews:?}").into());
    };
    assert!(matches!(review.key, ReviewKey::Conflict { .. }));

    let address = Key::new("address")?;
    let disputed = vec![
        address.you("192.0.2.4", true, 0)?,
        address.settled("192.0.2.9", 1, 10)?,
    ];
    let reviews = fact_reviews(&disputed, hours(11));
    let [review] = reviews.as_slice() else {
        return Err(format!("one conflict expected, got {reviews:?}").into());
    };
    assert!(matches!(review.key, ReviewKey::Conflict { .. }));
    assert_eq!(review.state.value.as_str(), "192.0.2.4");
    assert!(review.state.pinned);
    Ok(())
}

#[test]
fn a_decided_item_stays_closed_and_a_snoozed_one_returns() {
    let decided = ReviewKey::Tag(TagId::from_uuid(Uuid::new_v4()));
    let snoozed = ReviewKey::Tag(TagId::from_uuid(Uuid::new_v4()));
    let untouched = ReviewKey::Tag(TagId::from_uuid(Uuid::new_v4()));
    let marks = Marks::of(vec![
        ReviewMark {
            key: decided.clone(),
            mark: Mark::Decided { at: hours(0) },
        },
        ReviewMark {
            key: snoozed.clone(),
            mark: Mark::Snoozed {
                until: hours(0) + SNOOZE,
            },
        },
    ]);
    assert!(!marks.open(&decided, hours(100)));
    assert!(marks.decided(&decided));
    assert!(!marks.open(&snoozed, hours(23)));
    assert!(marks.open(&snoozed, hours(24)));
    assert!(!marks.decided(&snoozed));
    assert!(marks.open(&untouched, hours(0)));
}
