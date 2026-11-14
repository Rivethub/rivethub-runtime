use super::*;
use std::collections::BTreeSet;

fn row(id: &str, session: &str) -> ArmRow {
    ArmRow {
        chunk: None,
        id: id.into(),
        source: MemoryHitSource::Message,
        content_key: id.into(),
        reasoning: false,
        role: Some(Role::User),
        agent: None,
        kind: None,
        tool_name: None,
        conversation_id: None,
        session_id: Some(session.into()),
        created_at: DateTime::from_timestamp(0, 0).expect("time"),
        arm_score: 1.0,
        semantic: 0.0,
        boost: 0.0,
    }
}

#[test]
fn own_weight_breaks_a_tie_but_does_not_override_strong_relevance() {
    let own = vec![row("z", "own")];
    let team = vec![row("a", "team")];
    let owners = BTreeSet::from(["own".into()]);
    assert_eq!(
        fuse(&own, None, Some(&team), &owners, 1.25)
            .first()
            .expect("hit")
            .hit
            .id,
        "z"
    );
    assert_eq!(
        fuse(&own, None, Some(&team), &owners, 1.0)
            .first()
            .expect("hit")
            .hit
            .id,
        "a"
    );
    let mixed = vec![row("a", "team"), row("z", "own")];
    assert_eq!(
        fuse(&mixed, None, Some(&team), &owners, 1.25)
            .first()
            .expect("hit")
            .hit
            .id,
        "a"
    );
    let personal = fuse(&own, None, None, &BTreeSet::new(), 1.25);
    let disabled = fuse(&own, None, None, &BTreeSet::new(), 1.0);
    assert_eq!(
        personal.first().expect("hit").hit.score,
        disabled.first().expect("hit").hit.score
    );
}
