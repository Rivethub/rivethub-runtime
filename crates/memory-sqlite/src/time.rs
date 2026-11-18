use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;

use crate::error::SqliteError;

pub(crate) fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stamp {
    Source(DateTime<Utc>),
    CaptureTime,
}

impl From<Option<DateTime<Utc>>> for Stamp {
    fn from(source: Option<DateTime<Utc>>) -> Self {
        match source {
            Some(at) => Self::Source(at),
            None => Self::CaptureTime,
        }
    }
}

impl Stamp {
    pub(crate) const fn at(self, now: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Self::Source(at) => at,
            Self::CaptureTime => now,
        }
    }
}

pub(crate) fn stored(text: &str, column: &'static str) -> Result<DateTime<Utc>, SqliteError> {
    DateTime::parse_from_rfc3339(text)
        .or_else(|_| DateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f%#z"))
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|source| SqliteError::StoredTimestamp {
            column,
            value: text.to_owned(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::{stamp, stored};

    #[test]
    fn stored_time_keeps_fixed_milliseconds_and_accepts_existing_offset_forms() {
        for (raw, expected) in [
            ("1969-12-31T23:59:59.999999999Z", "1969-12-31T23:59:59.999Z"),
            ("2026-01-01 01:00:00.5+01", "2026-01-01T00:00:00.500Z"),
            ("2026-01-01 01:00:00+0100", "2026-01-01T00:00:00.000Z"),
            ("2026-01-01T00:00:00+00:00", "2026-01-01T00:00:00.000Z"),
        ] {
            assert_eq!(stamp(stored(raw, "created_at").expect(raw)), expected);
        }
        for raw in [
            "2026-01-01",
            "2026-01-01 00:00:00",
            "2026-01-01T00:00:00Z[UTC]",
        ] {
            assert!(stored(raw, "created_at").is_err(), "{raw}");
        }
    }
}
