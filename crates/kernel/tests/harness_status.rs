use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use kernel::harness_setup::CaptureActivity;
use kernel::harness_setup::CaptureHistory;
use kernel::harness_setup::CaptureMethod;
use kernel::harness_setup::CaptureState;
use kernel::harness_setup::HarnessProbe;
use kernel::harness_setup::RecallState;
use kernel::harness_setup::RecallStatus;

fn probe(installed: bool) -> HarnessProbe {
    HarnessProbe {
        id: "codex".to_owned(),
        name: "Codex".to_owned(),
        session_prefix: "codex".to_owned(),
        found: true,
        path: Some("/usr/bin/codex".to_owned()),
        version: None,
        method: CaptureMethod::Hook,
        capture_installed: installed,
        install_detail: "hook installed".to_owned(),
        recall: RecallStatus {
            state: RecallState::Connected,
            detail: "rivethub runs rh mcp".to_owned(),
        },
        can_connect: false,
    }
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-06T12:00:00Z")
        .map(|time| time.with_timezone(&Utc))
        .unwrap_or_default()
}

fn state(installed: bool, history: CaptureHistory) -> CaptureState {
    probe(installed).into_status(history, now()).capture.state
}

#[test]
fn a_capture_inside_three_days_is_receiving_whatever_the_install_check_says() {
    let recent = CaptureHistory::Known(Some(now() - Duration::days(2)));
    assert_eq!(state(true, recent), CaptureState::Receiving);
    assert_eq!(state(false, recent), CaptureState::Receiving);
}

#[test]
fn an_installed_entry_with_nothing_in_three_days_is_stale() {
    let old = CaptureHistory::Known(Some(now() - Duration::days(4)));
    assert_eq!(state(true, old), CaptureState::Stale);
    assert_eq!(
        state(true, CaptureHistory::Known(None)),
        CaptureState::Stale
    );
    assert_eq!(state(true, CaptureHistory::Unknown), CaptureState::Stale);
}

#[test]
fn nothing_installed_and_nothing_recent_is_not_connected() {
    let old = CaptureHistory::Known(Some(now() - Duration::days(30)));
    assert_eq!(state(false, old), CaptureState::NotConnected);
    assert_eq!(
        state(false, CaptureHistory::Unknown),
        CaptureState::NotConnected
    );
}

#[test]
fn the_history_of_a_harness_is_its_newest_capture_under_its_prefix() {
    let early = now() - Duration::days(9);
    let late = now() - Duration::hours(3);
    let activity = [
        CaptureActivity {
            key_prefix: "codex".to_owned(),
            last_at: early,
        },
        CaptureActivity {
            key_prefix: "codex".to_owned(),
            last_at: late,
        },
        CaptureActivity {
            key_prefix: "cursor".to_owned(),
            last_at: now(),
        },
    ];
    assert_eq!(
        CaptureHistory::of(&activity, "codex"),
        CaptureHistory::Known(Some(late))
    );
    assert_eq!(
        CaptureHistory::of(&activity, "kiro"),
        CaptureHistory::Known(None)
    );
}

#[test]
fn the_status_carries_the_last_capture_time_and_both_details() {
    let at = now() - Duration::hours(1);
    let status = probe(true).into_status(CaptureHistory::Known(Some(at)), now());
    assert_eq!(status.capture.last_capture_at, Some(at));
    assert_eq!(
        status.capture.detail,
        "hook installed; last capture 2026-11-06T11:00:00Z"
    );
    assert_eq!(status.recall.detail, "rivethub runs rh mcp");
}

#[test]
fn history_is_mentioned_only_when_the_line_can_say_something_definite() {
    let at = now() - Duration::hours(1);
    let installed = probe(true);
    assert_eq!(
        installed
            .clone()
            .into_status(CaptureHistory::Known(None), now())
            .capture
            .detail,
        "hook installed; nothing captured yet"
    );
    assert_eq!(
        probe(false)
            .into_status(CaptureHistory::Known(None), now())
            .capture
            .detail,
        "hook installed"
    );
    assert_eq!(
        installed
            .into_status(CaptureHistory::Unknown, now())
            .capture
            .detail,
        "hook installed"
    );
    let mut missing = probe(true);
    missing.found = false;
    assert_eq!(
        missing
            .into_status(CaptureHistory::Known(Some(at)), now())
            .capture
            .detail,
        "hook installed"
    );
}
