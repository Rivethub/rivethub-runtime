use std::time::Duration;

use jiff::Timestamp;
use jiff::fmt::rfc2822;
use reqwest::header::HeaderMap;
use reqwest::header::RETRY_AFTER;

pub(crate) fn hint(headers: &HeaderMap, now: Timestamp) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?;
    let text = String::from_utf8_lossy(value.as_bytes());
    let text = text.trim();
    delay_seconds(text).or_else(|| http_date(text, now))
}

fn delay_seconds(text: &str) -> Option<Duration> {
    if text.is_empty() {
        return None;
    }
    text.bytes()
        .try_fold(0_u64, |total, byte| {
            let digit = byte.checked_sub(b'0').filter(|digit| *digit <= 9)?;
            total.checked_mul(10)?.checked_add(u64::from(digit))
        })
        .map(Duration::from_secs)
}

fn http_date(text: &str, now: Timestamp) -> Option<Duration> {
    let date = parse_http_date(text)?;
    let wait = date.duration_since(now);
    Some(Duration::from_secs(wait.as_secs().max(0).unsigned_abs()))
}

fn parse_http_date(text: &str) -> Option<Timestamp> {
    let (date, comments) = match text.find('(') {
        Some(start) => text.split_at(start),
        None => (text, ""),
    };
    if !valid_comments(comments) {
        return None;
    }
    let (clock, zone) = date.trim_end().rsplit_once(char::is_whitespace)?;
    let spaced_weekday = clock
        .split_once(',')
        .is_some_and(|(weekday, _)| weekday.ends_with(char::is_whitespace));
    let spaced_second = clock
        .split_once(':')
        .and_then(|(_, minute)| minute.split_once(':'))
        .is_some_and(|(_, second)| second.starts_with(char::is_whitespace));
    if spaced_weekday || spaced_second || !legacy_zone(zone) {
        return None;
    }
    let (weekday, date) = match clock.split_once(',') {
        Some((weekday, date)) => (format!("{weekday},"), date),
        None => (String::new(), clock),
    };
    let mut fields = date.split_whitespace();
    let day = fields.next()?;
    let month = fields.next()?;
    let raw_year = fields.next()?;
    let year = if raw_year.len() > 4 {
        format!("{:0>4}", raw_year.trim_start_matches('0'))
    } else {
        raw_year.to_owned()
    };
    let time = fields.collect::<Vec<_>>().join(" ");
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    rfc2822::DateTimeParser::new()
        .parse_timestamp(format!("{weekday}{day} {month} {year} {time} {zone}"))
        .ok()
}

fn legacy_zone(zone: &str) -> bool {
    match zone.to_ascii_uppercase().as_bytes() {
        b"UT"
        | b"GMT"
        | b"EST"
        | b"EDT"
        | b"CST"
        | b"CDT"
        | b"MST"
        | b"MDT"
        | b"PST"
        | b"PDT"
        | [b'A'..=b'I' | b'K'..=b'Z'] => true,
        [
            b'+' | b'-',
            tens @ b'0'..=b'2',
            ones @ b'0'..=b'9',
            b'0'..=b'5',
            b'0'..=b'9',
        ] => *tens < b'2' || *ones < b'4',
        _ => false,
    }
}

fn valid_comments(comments: &str) -> bool {
    let mut depth = 0;
    let mut escaped = false;
    for character in comments.chars() {
        match character {
            _ if escaped => escaped = false,
            '\\' if depth > 0 => escaped = true,
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ if depth > 0 || character.is_whitespace() => {}
            _ => return false,
        }
    }
    depth == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn headers(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static(value));
        headers
    }

    fn now() -> Timestamp {
        "2026-10-19T12:00:00Z".parse().expect("fixed clock")
    }

    #[test]
    fn delay_seconds_are_the_hint() {
        assert_eq!(hint(&headers("120"), now()), Some(Duration::from_secs(120)));
        assert_eq!(hint(&headers(" 7 "), now()), Some(Duration::from_secs(7)));
        assert_eq!(hint(&headers("0"), now()), Some(Duration::ZERO));
    }

    #[test]
    fn an_http_date_is_the_time_until_it() {
        assert_eq!(
            hint(&headers("Mon, 19 Oct 2026 12:01:30 GMT"), now()),
            Some(Duration::from_secs(90))
        );
        assert_eq!(
            hint(&headers("Mon, 19 Oct 2026 11:00:00 GMT"), now()),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn a_missing_or_malformed_header_is_no_hint() {
        assert_eq!(hint(&HeaderMap::new(), now()), None);
        for value in ["", "-5", "1.5", "soon", "99999999999999999999999"] {
            assert_eq!(hint(&headers(value), now()), None, "{value}");
        }
    }

    #[test]
    fn retry_dates_preserve_legacy_zones_and_comments() {
        for value in [
            "Mon,19 Oct 2026 12:01:30 GMT(note)(nested (note))",
            "Mon,19 Oct 02026 12:01:30 GMT",
            "19 Oct 26 12:01:30 Z",
            "Mon, 19 Oct 2026 13:01:30 +0100",
        ] {
            assert_eq!(hint(&headers(value), now()), Some(Duration::from_secs(90)));
        }
        for value in [
            "Mon, 19 Oct 2026 12:01:30 UTC",
            "Mon, 19 Oct 2026 12:01:30 +2400",
            "Mon, 19 Oct 2026 12:01: 30 GMT",
            "Mon , 19 Oct 2026 12:01:30 GMT",
        ] {
            assert_eq!(hint(&headers(value), now()), None, "{value}");
        }
    }
}
