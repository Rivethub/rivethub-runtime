use jiff::Timestamp;
use jiff::civil::Date;
use jiff::civil::DateTime;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::time::SystemTime;
use wire::time::TimestampFormat as _;

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

fn civil_datetime(text: &str) -> Result<DateTime, jiff::Error> {
    if let Some((_, fraction)) = text.rsplit_once('.')
        && fraction.len() > 9
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        let truncated = text.len() - (fraction.len() - 9);
        let text = text
            .get(..truncated)
            .ok_or_else(|| jiff::Error::from_args(format_args!("invalid fractional seconds")))?;
        return DateTime::strptime("%Y-%m-%dT%H:%M:%S%.f", text);
    }
    DateTime::strptime("%Y-%m-%dT%H:%M:%S%.f", text)
}

pub(crate) fn parse_date(text: &str) -> Option<i64> {
    let text = text.trim();
    if let Ok(at) = wire::time::parse_rfc3339(text) {
        return Some(at.epoch_millis());
    }
    if let Ok(date) = Date::strptime("%Y-%m-%d", text) {
        return match wire::time::date_at_utc(date) {
            Ok(at) => Some(at.epoch_millis()),
            Err(error) => {
                tracing::debug!(%error, "capture date is out of range");
                None
            }
        };
    }
    if let Ok(naive) = civil_datetime(text) {
        return match jiff::tz::TimeZone::UTC.to_timestamp(naive) {
            Ok(at) => Some(at.epoch_millis()),
            Err(error) => {
                tracing::debug!(%error, "capture datetime is out of range");
                None
            }
        };
    }
    None
}

#[expect(clippy::cast_possible_truncation)]
pub(crate) fn floor_ms(value: f64) -> i64 {
    value.floor() as i64
}

pub(crate) fn system_ms(time: SystemTime) -> i64 {
    let (millis, negative) = match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(elapsed) => (elapsed.as_millis(), false),
        Err(error) => (error.duration().as_nanos().div_ceil(1_000_000), true),
    };
    match (i64::try_from(millis), negative) {
        (Ok(ms), false) => ms,
        (Ok(ms), true) => -ms,
        (Err(error), negative) => {
            tracing::debug!(%error, "file timestamp exceeds epoch millisecond range");
            if negative { i64::MIN } else { i64::MAX }
        }
    }
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

pub(crate) fn from_milliseconds(millis: i64) -> Option<Timestamp> {
    match Timestamp::from_millisecond(millis) {
        Ok(at) => Some(at),
        Err(error) => {
            tracing::debug!(%error, millis, "capture timestamp is outside the supported range");
            None
        }
    }
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
            (" 2026-01-01T01:02:03.123456789012", Some(1_767_229_323_123)),
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
