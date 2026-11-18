use crate::memory::BrowseWindow;
use jiff::SignedDuration;
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub since: Option<Timestamp>,
    pub before: Option<Timestamp>,
}

#[must_use]
pub fn resolve_window(window: BrowseWindow, now: Timestamp, zone: &TimeZone) -> TimeRange {
    let today = now.to_zoned_datetime(zone.clone()).date();
    match window {
        BrowseWindow::Today => TimeRange {
            since: midnight(today, zone),
            before: None,
        },
        BrowseWindow::Yesterday => TimeRange {
            since: yesterday(today, zone),
            before: midnight(today, zone),
        },
        BrowseWindow::ThisMorning => {
            let start = midnight(today, zone);
            TimeRange {
                since: start,
                before: start.and_then(|at| shifted(at, SignedDuration::from_hours(12))),
            }
        }
        BrowseWindow::ThisWeek => TimeRange {
            since: calendar_date(today.checked_sub(
                jiff::Span::new().days(i64::from(today.weekday().to_monday_zero_offset())),
            ))
            .and_then(|date| midnight(date, zone)),
            before: None,
        },
        BrowseWindow::Last24h => TimeRange {
            since: shifted(now, -SignedDuration::from_hours(24)),
            before: None,
        },
        BrowseWindow::Last7d => TimeRange {
            since: shifted(now, -values::time::elapsed_days(7)),
            before: None,
        },
        BrowseWindow::Last14d => TimeRange {
            since: shifted(now, -values::time::elapsed_days(14)),
            before: None,
        },
    }
}

#[must_use]
pub fn day_window(date: Date, zone: &TimeZone) -> TimeRange {
    TimeRange {
        since: midnight(date, zone),
        before: calendar_date(date.tomorrow()).and_then(|date| midnight(date, zone)),
    }
}

fn yesterday(date: Date, zone: &TimeZone) -> Option<Timestamp> {
    calendar_date(date.yesterday()).and_then(|date| midnight(date, zone))
}

fn calendar_date(result: Result<Date, jiff::Error>) -> Option<Date> {
    match result {
        Ok(date) => Some(date),
        Err(error) => {
            tracing::debug!(%error, "calendar window exceeds the supported date range");
            None
        }
    }
}

fn shifted(at: Timestamp, by: SignedDuration) -> Option<Timestamp> {
    window_timestamp(at.checked_add(by))
}

fn midnight(date: Date, zone: &TimeZone) -> Option<Timestamp> {
    window_timestamp(midnight_utc(date, zone))
}

fn window_timestamp(result: Result<Timestamp, jiff::Error>) -> Option<Timestamp> {
    match result {
        Ok(at) => Some(at),
        Err(error) => {
            tracing::debug!(%error, "calendar window exceeds the supported timestamp range");
            None
        }
    }
}

fn midnight_utc(date: Date, zone: &TimeZone) -> Result<Timestamp, jiff::Error> {
    let ambiguous = zone.to_ambiguous_timestamp(date.at(0, 0, 0, 0));
    let earlier = ambiguous.earlier()?;
    if matches!(ambiguous.offset(), jiff::tz::AmbiguousOffset::Gap { .. }) {
        let later = ambiguous.later()?;
        if let Some(transition) = zone.following(earlier).next()
            && transition.timestamp() <= later
        {
            return Ok(transition.timestamp());
        }
        return Ok(later);
    }
    Ok(earlier)
}

#[cfg(test)]
mod tests {
    use super::TimeRange;
    use super::resolve_window;
    use crate::memory::BrowseWindow;
    use jiff::SignedDuration;
    use jiff::Timestamp;

    fn utc(text: &str) -> Timestamp {
        text.parse().expect("an RFC 3339 instant")
    }

    fn range(since: &str, before: Option<&str>) -> TimeRange {
        TimeRange {
            since: Some(utc(since)),
            before: before.map(utc),
        }
    }

    #[test]
    fn rolling_windows_count_back_from_now_in_any_zone() {
        let now = utc("2026-03-08T07:30:00Z");
        assert_eq!(
            resolve_window(
                BrowseWindow::Last24h,
                now,
                &values::time::bundled_zone("America/New_York").expect("bundled zone")
            ),
            range("2026-03-07T07:30:00Z", None)
        );
        assert_eq!(
            resolve_window(
                BrowseWindow::Last7d,
                now,
                &values::time::bundled_zone("Asia/Kolkata").expect("bundled zone")
            ),
            range("2026-03-01T07:30:00Z", None)
        );
        assert_eq!(
            resolve_window(BrowseWindow::Last14d, now, &jiff::tz::TimeZone::UTC),
            range("2026-02-22T07:30:00Z", None)
        );
    }

    #[test]
    fn today_starts_at_local_midnight() {
        let now = utc("2026-10-13T03:00:00Z");
        assert_eq!(
            resolve_window(BrowseWindow::Today, now, &jiff::tz::TimeZone::UTC),
            range("2026-10-13T00:00:00Z", None)
        );
        assert_eq!(
            resolve_window(
                BrowseWindow::Today,
                now,
                &values::time::bundled_zone("America/New_York").expect("bundled zone")
            ),
            range("2026-10-12T04:00:00Z", None)
        );
        assert_eq!(
            resolve_window(
                BrowseWindow::Today,
                now,
                &values::time::bundled_zone("Asia/Kolkata").expect("bundled zone")
            ),
            range("2026-10-12T18:30:00Z", None)
        );
    }

    #[test]
    fn this_morning_is_twelve_hours_from_local_midnight() {
        let now = utc("2026-10-13T15:00:00Z");
        assert_eq!(
            resolve_window(
                BrowseWindow::ThisMorning,
                now,
                &values::time::bundled_zone("America/New_York").expect("bundled zone")
            ),
            range("2026-10-13T04:00:00Z", Some("2026-10-13T16:00:00Z"))
        );
    }

    #[test]
    fn this_week_starts_on_monday() {
        let monday = utc("2026-10-12T04:00:00Z");
        for now in [
            "2026-10-12T04:00:00Z",
            "2026-10-14T12:00:00Z",
            "2026-10-19T03:59:59Z",
        ] {
            assert_eq!(
                resolve_window(
                    BrowseWindow::ThisWeek,
                    utc(now),
                    &values::time::bundled_zone("America/New_York").expect("bundled zone")
                ),
                TimeRange {
                    since: Some(monday),
                    before: None
                },
                "{now}"
            );
        }
        assert_eq!(
            resolve_window(
                BrowseWindow::ThisWeek,
                utc("2026-10-19T04:00:00Z"),
                &values::time::bundled_zone("America/New_York").expect("bundled zone")
            ),
            range("2026-10-19T04:00:00Z", None)
        );
    }

    #[test]
    fn yesterday_spans_twenty_three_hours_when_the_clock_springs_forward() {
        let window = resolve_window(
            BrowseWindow::Yesterday,
            utc("2026-03-09T12:00:00Z"),
            &values::time::bundled_zone("America/New_York").expect("bundled zone"),
        );
        assert_eq!(
            window,
            range("2026-03-08T05:00:00Z", Some("2026-03-09T04:00:00Z"))
        );
        assert_eq!(
            window
                .before
                .zip(window.since)
                .map(|(end, start)| end - start),
            Some(SignedDuration::from_hours(23))
        );
    }

    #[test]
    fn yesterday_spans_twenty_five_hours_when_the_clock_falls_back() {
        let window = resolve_window(
            BrowseWindow::Yesterday,
            utc("2026-11-02T12:00:00Z"),
            &values::time::bundled_zone("America/New_York").expect("bundled zone"),
        );
        assert_eq!(
            window,
            range("2026-11-01T04:00:00Z", Some("2026-11-02T05:00:00Z"))
        );
        assert_eq!(
            window
                .before
                .zip(window.since)
                .map(|(end, start)| end - start),
            Some(SignedDuration::from_hours(25))
        );
    }

    #[test]
    fn this_week_keeps_its_monday_offset_across_the_transition() {
        assert_eq!(
            resolve_window(
                BrowseWindow::ThisWeek,
                utc("2026-11-01T18:00:00Z"),
                &values::time::bundled_zone("America/New_York").expect("bundled zone")
            ),
            range("2026-10-26T04:00:00Z", None)
        );
    }

    #[test]
    fn this_morning_counts_elapsed_hours_on_the_transition_day() {
        assert_eq!(
            resolve_window(
                BrowseWindow::ThisMorning,
                utc("2026-03-08T15:00:00Z"),
                &values::time::bundled_zone("America/New_York").expect("bundled zone")
            ),
            range("2026-03-08T05:00:00Z", Some("2026-03-08T17:00:00Z"))
        );
    }

    #[test]
    fn a_skipped_midnight_starts_the_day_at_the_end_of_the_gap() {
        assert_eq!(
            resolve_window(
                BrowseWindow::Today,
                utc("2026-09-06T12:00:00Z"),
                &values::time::bundled_zone("America/Santiago").expect("bundled zone")
            ),
            range("2026-09-06T04:00:00Z", None)
        );
    }

    #[test]
    fn a_repeated_midnight_starts_the_day_at_its_first_occurrence() {
        assert_eq!(
            resolve_window(
                BrowseWindow::Today,
                utc("2026-11-01T12:00:00Z"),
                &values::time::bundled_zone("America/Havana").expect("bundled zone")
            ),
            range("2026-11-01T04:00:00Z", None)
        );
    }
}
