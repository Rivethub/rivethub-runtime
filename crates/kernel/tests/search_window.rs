use jiff::Timestamp;
use kernel::memory::{BrowseWindow, MemoryScope};
use kernel::memory::{Limit, SearchMode, SearchQuery};
use kernel::memory_core::TimeRange;

fn query() -> SearchQuery {
    SearchQuery {
        text: "release".into(),
        mode: SearchMode::Hybrid,
        scope: MemoryScope::Both,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(None, 10, 1..=50),
        since: None,
        before: None,
        window: Some(BrowseWindow::Yesterday),
        expand: None,
        synthesize: None,
    }
}

#[test]
fn search_windows_use_local_calendar_and_explicit_bounds_take_precedence() {
    let now: Timestamp = "2026-03-09T12:00:00Z".parse().expect("now");
    let start = "2026-03-08T05:00:00Z".parse().expect("start");
    let end = "2026-03-09T04:00:00Z".parse().expect("end");
    let mut query = query();
    assert_eq!(
        query.time_range(
            now,
            &values::time::bundled_zone("America/New_York").expect("bundled zone")
        ),
        TimeRange {
            since: Some(start),
            before: Some(end)
        }
    );
    for (since, before) in [
        (Some(now), None),
        (None, Some(now)),
        (Some(start), Some(end)),
    ] {
        query.since = since;
        query.before = before;
        assert_eq!(
            query.time_range(
                now,
                &values::time::bundled_zone("America/New_York").expect("bundled zone")
            ),
            TimeRange { since, before }
        );
    }
    query.since = None;
    query.before = None;
    query.window = None;
    assert_eq!(
        query.time_range(
            now,
            &values::time::bundled_zone("America/New_York").expect("bundled zone")
        ),
        TimeRange {
            since: None,
            before: None
        }
    );
}
