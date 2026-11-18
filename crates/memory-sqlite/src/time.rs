use crate::error::SqliteError;
use jiff::Timestamp;
use wire::time::TimestampFormat as _;

pub(crate) fn stamp(at: Timestamp) -> String {
    at.rfc3339_millis()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stamp {
    Source(Timestamp),
    CaptureTime,
}

impl From<Option<Timestamp>> for Stamp {
    fn from(source: Option<Timestamp>) -> Self {
        match source {
            Some(at) => Self::Source(at),
            None => Self::CaptureTime,
        }
    }
}

impl Stamp {
    pub(crate) const fn at(self, now: Timestamp) -> Timestamp {
        match self {
            Self::Source(at) => at,
            Self::CaptureTime => now,
        }
    }
}

pub(crate) fn stored(text: &str, column: &'static str) -> Result<Timestamp, SqliteError> {
    parse_sqlite_timestamp(text).map_err(|source| SqliteError::StoredTimestamp {
        column,
        value: text.to_owned(),
        source,
    })
}

fn parse_sqlite_timestamp(raw: &str) -> Result<Timestamp, jiff::Error> {
    match wire::time::parse_rfc3339(raw) {
        Ok(at) => Ok(at),
        Err(strict) => {
            let Some((date, suffix)) = raw.split_once(' ') else {
                return Err(strict);
            };
            let Some(offset_start) = suffix.find(['+', '-', 'Z', 'z']) else {
                return Err(strict);
            };
            let (time, offset) = suffix.split_at(offset_start);
            let offset = offset.trim();
            if matches!(offset.as_bytes(), [b'+' | b'-', b'0'..=b'9', b'0'..=b'9']) {
                wire::time::parse_wire_timestamp(&format!("{date}T{}{offset}00", time.trim_end()))
            } else {
                wire::time::parse_wire_timestamp(&format!("{date}T{time}{offset}"))
            }
        }
    }
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

    #[test]
    fn leap_seconds_are_normalized_and_expanded_years_fail_storage_decoding() {
        assert_eq!(
            stamp(stored("2016-12-31T23:59:60.500Z", "created_at").expect("leap second")),
            "2016-12-31T23:59:59.500Z"
        );
        assert!(matches!(
            stored("+10000-01-01T00:00:00Z", "created_at"),
            Err(crate::error::SqliteError::StoredTimestamp {
                column: "created_at",
                ..
            })
        ));
    }
}
