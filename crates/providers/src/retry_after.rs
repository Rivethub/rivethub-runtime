use std::time::Duration;

use chrono::DateTime;
use chrono::Utc;
use reqwest::header::HeaderMap;
use reqwest::header::RETRY_AFTER;

pub(crate) fn hint(headers: &HeaderMap, now: DateTime<Utc>) -> Option<Duration> {
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

fn http_date(text: &str, now: DateTime<Utc>) -> Option<Duration> {
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let date = DateTime::parse_from_rfc2822(text).ok()?;
    let wait = date.with_timezone(&Utc).signed_duration_since(now);
    Some(Duration::from_secs(
        wait.num_seconds().max(0).unsigned_abs(),
    ))
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

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-19T12:00:00Z")
            .expect("fixed clock")
            .with_timezone(&Utc)
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
}
