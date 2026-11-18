use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::time::SystemTime;

use chrono::DateTime;
use chrono::NaiveDate;
use chrono::NaiveDateTime;
use chrono::Utc;
use rusqlite::types::Value as SqlValue;

use super::field::Field;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RawTime {
    Number(f64),
    Text(String),
    Absent,
}

impl RawTime {
    pub(crate) fn from_field(field: Option<&Field>) -> Self {
        let Some(field) = field else {
            return Self::Absent;
        };
        if let Some(number) = field.number() {
            return Self::Number(number);
        }
        field.text().map_or(Self::Absent, Self::Text)
    }

    #[expect(clippy::cast_precision_loss)]
    pub(crate) fn from_sql(value: &SqlValue) -> Self {
        match value {
            SqlValue::Integer(number) => Self::Number(*number as f64),
            SqlValue::Real(number) => Self::Number(*number),
            SqlValue::Text(text) => Self::Text(text.clone()),
            SqlValue::Null | SqlValue::Blob(_) => Self::Absent,
        }
    }
}

pub(crate) const UNKNOWN_TIME: i64 = 0;

pub(crate) fn epoch_flex(time: &RawTime) -> i64 {
    match time {
        RawTime::Number(value) if *value > 1e12 => floor_ms(*value),
        RawTime::Number(value) if *value > 1e9 => floor_ms(*value * 1000.0),
        RawTime::Number(value) => floor_ms(*value),
        RawTime::Text(text) => parse_date(text).unwrap_or(UNKNOWN_TIME),
        RawTime::Absent => UNKNOWN_TIME,
    }
}

pub(crate) fn epoch_opencode(time: &RawTime) -> i64 {
    match time {
        RawTime::Number(value) if *value < 1e12 => floor_ms(*value * 1000.0),
        RawTime::Number(value) => floor_ms(*value),
        RawTime::Text(text) => parse_date(text).unwrap_or(UNKNOWN_TIME),
        RawTime::Absent => UNKNOWN_TIME,
    }
}

pub(crate) fn finite_or_date(time: &RawTime) -> Option<i64> {
    match time {
        RawTime::Number(value) if value.is_finite() => Some(floor_ms(*value)),
        RawTime::Text(text) => parse_date(text),
        RawTime::Number(_) | RawTime::Absent => None,
    }
}

pub(crate) fn parse_date(text: &str) -> Option<i64> {
    let text = text.trim();
    if let Ok(at) = DateTime::parse_from_rfc3339(text) {
        return Some(at.timestamp_millis());
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return Some(
            date.and_time(chrono::NaiveTime::MIN)
                .and_utc()
                .timestamp_millis(),
        );
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some(naive.and_utc().timestamp_millis());
    }
    None
}

#[expect(clippy::cast_possible_truncation)]
pub(crate) fn floor_ms(value: f64) -> i64 {
    value.floor() as i64
}

pub(crate) fn system_ms(time: SystemTime) -> i64 {
    DateTime::<Utc>::from(time).timestamp_millis()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileTimes {
    pub(crate) mtime: i64,
    pub(crate) birth: i64,
    pub(crate) created: Option<i64>,
}

impl FileTimes {
    pub(crate) fn of(metadata: &Metadata) -> Self {
        let mtime = match metadata.modified() {
            Ok(time) => system_ms(time),
            Err(error) => {
                tracing::trace!(%error, "file modification time is not available, ctime stands in");
                ctime_ms(metadata)
            }
        };
        let created = match metadata.created() {
            Ok(time) => Some(system_ms(time)).filter(|ms| *ms != 0),
            Err(error) => {
                tracing::trace!(%error, "file creation time is not available");
                None
            }
        };
        let birth = created
            .or_else(|| Some(ctime_ms(metadata)).filter(|ms| *ms != 0))
            .unwrap_or(mtime);
        Self {
            mtime,
            birth,
            created,
        }
    }

    pub(crate) fn birth_or_mtime(self) -> i64 {
        self.created.unwrap_or(self.mtime)
    }
}

fn ctime_ms(metadata: &Metadata) -> i64 {
    metadata
        .ctime()
        .saturating_mul(1000)
        .saturating_add(metadata.ctime_nsec() / 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::{RawTime, epoch_flex, epoch_opencode, parse_date};

    #[test]
    fn source_time_forms_normalize_to_epoch_milliseconds() {
        for (raw, expected) in [
            (" 1970-01-01 ", Some(0)),
            ("1969-12-31T23:59:59.999999999Z", Some(-1)),
            ("1970-01-01t01:00:00.5+01:00", Some(500)),
            ("1970-01-01T00:00:00.000999", Some(0)),
            ("1970-01-01 00:00:00.5", None),
            ("1970-01-01T00:00", None),
            ("1970-02-29", None),
        ] {
            assert_eq!(parse_date(raw), expected, "{raw}");
        }
        assert_eq!(
            epoch_flex(&RawTime::Number(1_000_000_001.5)),
            1_000_000_001_500
        );
        assert_eq!(
            epoch_opencode(&RawTime::Number(1_000_000_001.5)),
            1_000_000_001_500
        );
        assert_eq!(epoch_flex(&RawTime::Number(-0.5)), -1);
    }
}
