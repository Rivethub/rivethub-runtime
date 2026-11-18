use jiff::SignedDuration;
use jiff::Timestamp;
use kernel::memory::{
    SessionActivity, Speaker, SpokenTurn, TrashReason, TrashSignals, trash_verdict,
};

const fn turn(speaker: Speaker, text: &str) -> SpokenTurn<'_> {
    SpokenTurn {
        speaker,
        text,
        worked: false,
    }
}

fn now() -> Timestamp {
    Timestamp::UNIX_EPOCH + values::time::elapsed_days(20_000)
}

fn verdict(
    activity: &SessionActivity,
    transcript: &[SpokenTurn<'_>],
    title: Option<&str>,
    age: SignedDuration,
) -> Option<(TrashReason, u64, Option<String>)> {
    trash_verdict(&TrashSignals {
        native: "native-1",
        title,
        updated_at: now() - age,
        now: now(),
        activity,
        transcript,
    })
    .map(|verdict| (verdict.reason, verdict.user_turns, verdict.reply))
}

#[test]
fn an_id_blank_or_harness_default_title_names_no_session() {
    for title in [
        None,
        Some(""),
        Some("  "),
        Some("native-1"),
        Some("Untitled"),
        Some("new SESSION"),
        Some("Grok Build session"),
        Some("omp session (imported)"),
    ] {
        assert_eq!(
            verdict(
                &SessionActivity::default(),
                &[],
                title,
                SignedDuration::from_hours(2)
            ),
            Some((TrashReason::Empty, 0, None)),
            "{title:?}"
        );
    }
}

#[test]
fn an_empty_discovery_session_remembered_without_messages_is_empty() {
    let remembered = SessionActivity {
        conversations: 1,
        ..SessionActivity::default()
    };
    assert_eq!(
        verdict(
            &remembered,
            &[],
            Some("Grok Build session"),
            SignedDuration::from_hours(2)
        ),
        Some((TrashReason::Empty, 0, None))
    );
    let worked = [SpokenTurn {
        speaker: Speaker::Assistant,
        text: "",
        worked: true,
    }];
    assert_eq!(
        verdict(&remembered, &worked, None, SignedDuration::from_hours(2)),
        None
    );
}

#[test]
fn an_old_silent_session_with_a_default_title_is_empty() {
    let quiet = SessionActivity::default();
    assert_eq!(
        verdict(&quiet, &[], None, SignedDuration::from_hours(2)),
        Some((TrashReason::Empty, 0, None))
    );
    assert_eq!(
        verdict(&quiet, &[], Some("Plan"), SignedDuration::from_hours(2)),
        None
    );
    assert_eq!(
        verdict(&quiet, &[], None, SignedDuration::from_mins(59)),
        None
    );
    assert_eq!(
        verdict(&quiet, &[], None, -SignedDuration::from_hours(2)),
        None
    );
}

#[test]
fn a_one_word_reply_is_trimmed_of_punctuation_and_previewed() {
    let transcript = [
        turn(Speaker::User, "say hi"),
        turn(Speaker::Assistant, "  Hi!  "),
    ];
    assert_eq!(
        verdict(
            &SessionActivity::default(),
            &transcript,
            None,
            SignedDuration::from_hours(2)
        ),
        Some((TrashReason::OneWordReply, 1, Some("Hi!".to_owned())))
    );
}

#[test]
fn an_unanswered_or_long_prompt_is_kept() {
    let unanswered = [turn(Speaker::User, "hello?")];
    assert_eq!(
        verdict(
            &SessionActivity::default(),
            &unanswered,
            None,
            SignedDuration::from_hours(2)
        ),
        None
    );
    let blank_reply = [
        turn(Speaker::User, "hello?"),
        turn(Speaker::Assistant, "  "),
    ];
    assert_eq!(
        verdict(
            &SessionActivity::default(),
            &blank_reply,
            None,
            SignedDuration::from_hours(2)
        ),
        None
    );
    let long_prompt = "a".repeat(281);
    let noted = [
        turn(Speaker::User, &long_prompt),
        turn(Speaker::Assistant, "Noted."),
    ];
    assert_eq!(
        verdict(
            &SessionActivity::default(),
            &noted,
            None,
            SignedDuration::from_hours(2)
        ),
        None
    );
    let short_prompt = "a".repeat(280);
    let ok = [
        turn(Speaker::User, &short_prompt),
        turn(Speaker::Assistant, "Noted."),
    ];
    assert_eq!(
        verdict(
            &SessionActivity::default(),
            &ok,
            None,
            SignedDuration::from_hours(2)
        ),
        Some((TrashReason::OneWordReply, 1, Some("Noted.".to_owned())))
    );
}

#[test]
fn any_doubt_keeps_the_session() {
    let age = SignedDuration::from_hours(2);
    let two_words = [
        turn(Speaker::User, "hi"),
        turn(Speaker::Assistant, "Hello there"),
    ];
    assert_eq!(
        verdict(&SessionActivity::default(), &two_words, None, age),
        None
    );
    let longer = [
        turn(Speaker::User, "hi"),
        turn(Speaker::Assistant, "Hi"),
        turn(Speaker::User, "and then"),
    ];
    assert_eq!(
        verdict(&SessionActivity::default(), &longer, None, age),
        None
    );
    let memory_has_more = SessionActivity {
        conversations: 1,
        messages: 4,
        user_messages: 2,
        assistant_messages: 2,
        reply: Some("Sure".to_owned()),
        ..SessionActivity::default()
    };
    assert_eq!(verdict(&memory_has_more, &[], None, age), None);
    let transcript_has_more = SessionActivity {
        conversations: 1,
        messages: 2,
        user_messages: 1,
        assistant_messages: 1,
        reply: Some("Sure".to_owned()),
        ..SessionActivity::default()
    };
    assert_eq!(verdict(&transcript_has_more, &longer, None, age), None);
    for kept in [
        SessionActivity {
            tool_rows: 1,
            ..SessionActivity::default()
        },
        SessionActivity {
            attachments: 1,
            ..SessionActivity::default()
        },
        SessionActivity {
            placed: true,
            ..SessionActivity::default()
        },
        SessionActivity {
            cited: true,
            ..SessionActivity::default()
        },
        SessionActivity {
            tasked: true,
            ..SessionActivity::default()
        },
    ] {
        assert_eq!(verdict(&kept, &[], None, age), None, "{kept:?}");
    }
}
