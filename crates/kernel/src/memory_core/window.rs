use crate::memory::BrowseWindow;
use chrono::DateTime;
use chrono::Datelike as _;
use chrono::Duration;
use chrono::LocalResult;
use chrono::NaiveDate;
use chrono::NaiveDateTime;
use chrono::NaiveTime;
use chrono::Offset as _;
use chrono::TimeZone;
use chrono::Utc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub since: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
}

#[must_use]
pub fn resolve_window<Tz: TimeZone>(
    window: BrowseWindow,
    now: DateTime<Utc>,
    zone: &Tz,
) -> TimeRange {
    let today = now.with_timezone(zone).date_naive();
    match window {
        BrowseWindow::Today => TimeRange {
            since: Some(midnight_utc(today, zone)),
            before: None,
        },
        BrowseWindow::Yesterday => TimeRange {
            since: Some(midnight_utc(today - Duration::days(1), zone)),
            before: Some(midnight_utc(today, zone)),
        },
        BrowseWindow::ThisMorning => {
            let start = midnight_utc(today, zone);
            TimeRange {
                since: Some(start),
                before: Some(start + Duration::hours(12)),
            }
        }
        BrowseWindow::ThisWeek => TimeRange {
            since: Some(midnight_utc(
                today - Duration::days(i64::from(today.weekday().num_days_from_monday())),
                zone,
            )),
            before: None,
        },
        BrowseWindow::Last24h => TimeRange {
            since: Some(now - Duration::hours(24)),
            before: None,
        },
        BrowseWindow::Last7d => TimeRange {
            since: Some(now - Duration::days(7)),
            before: None,
        },
        BrowseWindow::Last14d => TimeRange {
            since: Some(now - Duration::days(14)),
            before: None,
        },
    }
}

#[must_use]
pub fn day_window<Tz: TimeZone>(date: NaiveDate, zone: &Tz) -> TimeRange {
    TimeRange {
        since: Some(midnight_utc(date, zone)),
        before: date.succ_opt().map(|next| midnight_utc(next, zone)),
    }
}

fn midnight_utc<Tz: TimeZone>(date: NaiveDate, zone: &Tz) -> DateTime<Utc> {
    let naive = NaiveDateTime::new(date, NaiveTime::MIN);
    match zone.from_local_datetime(&naive) {
        LocalResult::Single(local) => local.with_timezone(&Utc),
        LocalResult::Ambiguous(first, second) => first.min(second).with_timezone(&Utc),
        LocalResult::None => after_gap(naive, zone),
    }
}

fn after_gap<Tz: TimeZone>(local: NaiveDateTime, zone: &Tz) -> DateTime<Utc> {
    let after = zone
        .offset_from_utc_datetime(&(local + Duration::days(1)))
        .fix();
    let before = zone
        .offset_from_utc_datetime(&(local - Duration::days(1)))
        .fix();
    let gap = i64::from(after.local_minus_utc()) - i64::from(before.local_minus_utc());
    let first = local + Duration::seconds(seconds_until_existing(zone, local, gap));
    (first - after).and_utc()
}

fn seconds_until_existing<Tz: TimeZone>(zone: &Tz, local: NaiveDateTime, gap: i64) -> i64 {
    let mut lo = 0i64;
    let mut hi = gap.max(0);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let candidate = local + Duration::seconds(mid);
        if zone.from_local_datetime(&candidate).earliest().is_some() {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::TimeRange;
    use super::resolve_window;
    use crate::memory::BrowseWindow;
    use chrono::DateTime;
    use chrono::Duration;
    use chrono::Utc;
    use chrono_tz::America::Havana;
    use chrono_tz::America::New_York;
    use chrono_tz::America::Santiago;
    use chrono_tz::Asia::Kolkata;

    fn utc(text: &str) -> DateTime<Utc> {
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
            resolve_window(BrowseWindow::Last24h, now, &New_York),
            range("2026-03-07T07:30:00Z", None)
        );
        assert_eq!(
            resolve_window(BrowseWindow::Last7d, now, &Kolkata),
            range("2026-03-01T07:30:00Z", None)
        );
        assert_eq!(
            resolve_window(BrowseWindow::Last14d, now, &Utc),
            range("2026-02-22T07:30:00Z", None)
        );
    }

    #[test]
    fn today_starts_at_local_midnight() {
        let now = utc("2026-10-13T03:00:00Z");
        assert_eq!(
            resolve_window(BrowseWindow::Today, now, &Utc),
            range("2026-10-13T00:00:00Z", None)
        );
        assert_eq!(
            resolve_window(BrowseWindow::Today, now, &New_York),
            range("2026-10-12T04:00:00Z", None)
        );
        assert_eq!(
            resolve_window(BrowseWindow::Today, now, &Kolkata),
            range("2026-10-12T18:30:00Z", None)
        );
    }

    #[test]
    fn this_morning_is_twelve_hours_from_local_midnight() {
        let now = utc("2026-10-13T15:00:00Z");
        assert_eq!(
            resolve_window(BrowseWindow::ThisMorning, now, &New_York),
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
                resolve_window(BrowseWindow::ThisWeek, utc(now), &New_York),
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
                &New_York
            ),
            range("2026-10-19T04:00:00Z", None)
        );
    }

    #[test]
    fn yesterday_spans_twenty_three_hours_when_the_clock_springs_forward() {
        let window = resolve_window(
            BrowseWindow::Yesterday,
            utc("2026-03-09T12:00:00Z"),
            &New_York,
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
            Some(Duration::hours(23))
        );
    }

    #[test]
    fn yesterday_spans_twenty_five_hours_when_the_clock_falls_back() {
        let window = resolve_window(
            BrowseWindow::Yesterday,
            utc("2026-11-02T12:00:00Z"),
            &New_York,
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
            Some(Duration::hours(25))
        );
    }

    #[test]
    fn this_week_keeps_its_monday_offset_across_the_transition() {
        assert_eq!(
            resolve_window(
                BrowseWindow::ThisWeek,
                utc("2026-11-01T18:00:00Z"),
                &New_York
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
                &New_York
            ),
            range("2026-03-08T05:00:00Z", Some("2026-03-08T17:00:00Z"))
        );
    }

    #[test]
    fn a_skipped_midnight_starts_the_day_at_the_end_of_the_gap() {
        assert_eq!(
            resolve_window(BrowseWindow::Today, utc("2026-09-06T12:00:00Z"), &Santiago),
            range("2026-09-06T04:00:00Z", None)
        );
    }

    #[test]
    fn a_repeated_midnight_starts_the_day_at_its_first_occurrence() {
        assert_eq!(
            resolve_window(BrowseWindow::Today, utc("2026-11-01T12:00:00Z"), &Havana),
            range("2026-11-01T04:00:00Z", None)
        );
    }
}
