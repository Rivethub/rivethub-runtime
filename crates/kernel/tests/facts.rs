use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use kernel::facts::Claim;
use kernel::facts::FactChange;
use kernel::facts::FactId;
use kernel::facts::FactKey;
use kernel::facts::FactState;
use kernel::facts::FactValue;
use kernel::facts::Said;
use kernel::facts::SessionLink;
use kernel::facts::connected;
use kernel::facts::facts_as_of;
use kernel::facts::timeline;
use kernel::owner::Owner;
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
                evidence: None,
                settled: false,
            },
            at,
        )
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

fn only(claims: &[Claim], as_of: Option<DateTime<Utc>>) -> FactState {
    let mut facts = facts_as_of(claims, as_of);
    assert_eq!(facts.len(), 1, "one fact");
    facts.remove(0)
}

fn values(state: &FactState) -> Vec<&str> {
    state
        .candidates
        .iter()
        .map(|candidate| candidate.value.as_str())
        .collect()
}

#[test]
fn keys_fold_case_and_spacing_and_refuse_blanks() -> TestResult {
    assert_eq!(FactKey::new("  Primary   Role ")?.as_str(), "primary role");
    assert!(FactKey::new(" -- ").is_err());
    assert!(FactValue::new("   ").is_err());
    assert!(FactKey::new(&"k".repeat(65)).is_err());
    Ok(())
}

#[test]
fn a_newer_source_a_day_later_supersedes_and_keeps_the_previous_value() -> TestResult {
    let os = Key::new("os")?;
    let claims = [
        os.writer("Ubuntu 22.04", 1, 0)?,
        os.writer("Ubuntu 22.04", 2, 5)?,
        os.writer("Ubuntu 24.04", 3, 48)?,
    ];
    let state = only(&claims, None);
    assert_eq!(state.value.as_str(), "Ubuntu 24.04");
    assert_eq!(state.since, hours(48));
    assert!(state.candidates.is_empty(), "no conflict past the window");
    let previous = state.previous.unwrap();
    assert_eq!(previous.value.as_str(), "Ubuntu 22.04");
    assert_eq!(previous.until, hours(48));
    assert_eq!(state.sources.len(), 1);
    Ok(())
}

#[test]
fn agreeing_sources_support_the_value_from_its_first_claim() -> TestResult {
    let os = Key::new("os")?;
    let claims = [
        os.writer("Debian 12", 1, 0)?,
        os.writer("Debian 12", 2, 30)?,
    ];
    let state = only(&claims, None);
    assert_eq!(state.since, hours(0));
    let supporting: Vec<&SessionId> = state.sources.iter().map(|source| &source.session).collect();
    assert_eq!(supporting, [&session(2), &session(1)], "newest first");
    Ok(())
}

#[test]
fn two_sessions_disagreeing_within_a_day_are_a_conflict() -> TestResult {
    let ip = Key::new("address")?;
    let claims = [ip.writer("192.0.2.4", 1, 0)?, ip.writer("192.0.2.9", 2, 3)?];
    let state = only(&claims, None);
    assert_eq!(state.value.as_str(), "192.0.2.9");
    assert_eq!(values(&state), ["192.0.2.9", "192.0.2.4"]);
    Ok(())
}

#[test]
fn one_session_correcting_itself_is_no_conflict() -> TestResult {
    let ip = Key::new("address")?;
    let claims = [ip.writer("192.0.2.4", 1, 0)?, ip.writer("192.0.2.9", 1, 3)?];
    assert_eq!(only(&claims, None).candidates.len(), 0);
    Ok(())
}

#[test]
fn your_edit_resolves_a_conflict_and_later_sources_supersede_it_unless_pinned() -> TestResult {
    let ip = Key::new("address")?;
    let mut claims = vec![
        ip.writer("192.0.2.4", 1, 0)?,
        ip.writer("192.0.2.9", 2, 3)?,
        ip.you("192.0.2.9", false, 4)?,
    ];
    let state = only(&claims, None);
    assert!(state.candidates.is_empty(), "resolved");
    assert!(!state.pinned);
    claims.push(ip.writer("192.0.2.12", 3, 60)?);
    assert_eq!(only(&claims, None).value.as_str(), "192.0.2.12");
    claims.push(ip.you("192.0.2.9", true, 61)?);
    claims.push(ip.writer("192.0.2.12", 4, 90)?);
    let pinned = only(&claims, None);
    assert_eq!(
        pinned.value.as_str(),
        "192.0.2.9",
        "the writer never overwrites a pin"
    );
    assert!(pinned.pinned);
    assert_eq!(
        values(&pinned),
        ["192.0.2.12"],
        "it adds a candidate instead"
    );
    Ok(())
}

#[test]
fn a_pin_counts_later_agreeing_sources() -> TestResult {
    let role = Key::new("role")?;
    let claims = [
        role.you("maintainer", true, 0)?,
        role.writer("maintainer", 1, 10)?,
    ];
    let state = only(&claims, None);
    assert_eq!(state.candidates.len(), 0);
    assert_eq!(state.sources.len(), 1);
    Ok(())
}

#[test]
fn as_of_shows_the_state_as_it_stood() -> TestResult {
    let os = Key::new("os")?;
    let claims = [
        os.writer("Ubuntu 22.04", 1, 0)?,
        os.writer("Ubuntu 24.04", 2, 48)?,
    ];
    assert_eq!(
        only(&claims, Some(hours(24))).value.as_str(),
        "Ubuntu 22.04"
    );
    assert!(
        facts_as_of(&claims, Some(hours(-1))).is_empty(),
        "not known yet"
    );
    Ok(())
}

#[test]
fn the_timeline_lists_changes_pins_and_conflicts_newest_first() -> TestResult {
    let ip = Key::new("address")?;
    let claims = [
        ip.writer("192.0.2.4", 1, 0)?,
        ip.writer("192.0.2.4", 2, 1)?,
        ip.writer("192.0.2.9", 3, 2)?,
        ip.you("192.0.2.9", true, 5)?,
    ];
    let events: Vec<(DateTime<Utc>, FactChange)> = timeline(&claims, None)
        .into_iter()
        .map(|event| (event.at, event.change))
        .collect();
    let value = |text: &str| FactValue::new(text).unwrap();
    assert_eq!(
        events,
        [
            (
                hours(5),
                FactChange::Pinned {
                    value: value("192.0.2.9")
                }
            ),
            (
                hours(2),
                FactChange::Became {
                    value: value("192.0.2.9"),
                    session: Some(session(3)),
                }
            ),
            (
                hours(2),
                FactChange::Disputed {
                    values: vec![value("192.0.2.9"), value("192.0.2.4")],
                    session: Some(session(3)),
                }
            ),
            (
                hours(0),
                FactChange::Became {
                    value: value("192.0.2.4"),
                    session: Some(session(1)),
                }
            ),
        ]
    );
    Ok(())
}

#[test]
fn connected_entities_rank_by_shared_sessions() {
    let host = TopicId::from_uuid(Uuid::from_u128(1));
    let person = TopicId::from_uuid(Uuid::from_u128(2));
    let model = TopicId::from_uuid(Uuid::from_u128(3));
    let link = |entity, n| SessionLink {
        entity,
        session: session(n),
    };
    let links = [
        link(host, 1),
        link(host, 2),
        link(person, 1),
        link(person, 2),
        link(model, 2),
        link(model, 9),
    ];
    assert_eq!(connected(host, &links), [(person, 2), (model, 1)]);
}
