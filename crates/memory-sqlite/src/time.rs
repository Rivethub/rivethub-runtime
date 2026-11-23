use crate::error::SqliteError;
use jiff::SignedDuration;
use jiff::Timestamp;
use wire::time::TimestampFormat;

const UNLOCK_AFTER: i64 = 4 * 3_600;

pub(crate) fn stamp(at: Timestamp) -> String {
    at.rfc3339_millis()
}

pub(crate) fn event(at: Timestamp) -> String {
    at.rfc3339_nanos()
}

pub(crate) fn unlock_before(now: Timestamp) -> Result<Timestamp, SqliteError> {
    now.checked_sub(SignedDuration::from_secs(UNLOCK_AFTER))
        .map_err(|_error| SqliteError::Clock)
}

pub(crate) fn seek_key(at: Timestamp) -> String {
    let seconds = 400_000_000_000 + at.epoch_seconds();
    let nanos = at.nanosecond_of_second();
    format!("{seconds:012}{nanos:09}")
}

pub(crate) fn register(conn: &rusqlite::Connection) -> Result<(), SqliteError> {
    use rusqlite::functions::FunctionFlags;

    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    conn.create_scalar_function("rh_instant", 1, flags, |ctx| {
        ctx.get::<Option<String>>(0)?
            .map(|value| {
                parse_sqlite_timestamp(&value)
                    .map(seek_key)
                    .map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))
            })
            .transpose()
    })
    .map_err(SqliteError::query("register timestamp ordering"))?;
    conn.create_scalar_function("rh_timestamp", 1, flags, |ctx| {
        ctx.get::<Option<String>>(0)?
            .map(|key| {
                from_seek_key(&key)
                    .map(TimestampFormat::rfc3339_z)
                    .map_err(rusqlite::Error::UserFunctionError)
            })
            .transpose()
    })
    .map_err(SqliteError::query("register timestamp projection"))
}

fn from_seek_key(key: &str) -> Result<Timestamp, kernel::BoxError> {
    let (seconds, fraction) = key
        .split_at_checked(12)
        .ok_or_else(|| std::io::Error::other("invalid timestamp key"))?;
    let seconds = i128::from(seconds.parse::<i64>()?) - 400_000_000_000;
    let nanos = i128::from(fraction.parse::<u32>()?);
    Ok(Timestamp::from_nanosecond(seconds * 1_000_000_000 + nanos)?)
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
    wire::time::parse_rfc3339(raw).or_else(|_strict| {
        let (civil, offset) = wire::time::legacy_civil_timestamp(raw, true)?;
        wire::time::legacy_numeric_offset(offset.trim_start(), true)?.to_timestamp(civil)
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
            (
                " 2026- 01- 01 01:02:03.123456789012−01:00",
                "2026-01-01T02:02:03.123Z",
            ),
            ("-0001-01-01 01:02:03+0100", "-0001-01-01T00:02:03.000Z"),
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
