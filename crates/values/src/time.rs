use std::borrow::Cow;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Error, SignedDuration, Timestamp};
use serde::{Deserialize, Deserializer, Serializer};
use serde_with::schemars_1::JsonSchemaAs;
use serde_with::{DeserializeAs, SerializeAs};

pub trait TimestampFormat {
    fn rfc3339_offset(self) -> String;
    fn rfc3339_z(self) -> String;
    fn rfc3339_millis(self) -> String;
    fn rfc3339_seconds(self) -> String;
    fn utc_date(self) -> Date;
    fn epoch_seconds(self) -> i64;
    fn epoch_millis(self) -> i64;
    fn epoch_micros(self) -> i64;
    fn nanosecond_of_second(self) -> u32;
    fn truncate_subseconds(self, digits: u32) -> Timestamp;
}

fn automatic_precision(at: Timestamp) -> usize {
    let fraction = at.nanosecond_of_second();
    if fraction == 0 {
        0
    } else if fraction.is_multiple_of(1_000_000) {
        3
    } else if fraction.is_multiple_of(1_000) {
        6
    } else {
        9
    }
}

impl TimestampFormat for Timestamp {
    fn rfc3339_offset(self) -> String {
        let precision = automatic_precision(self);
        let base = self.display_with_offset(jiff::tz::Offset::UTC);
        format!("{base:.precision$}")
    }

    fn rfc3339_z(self) -> String {
        let precision = automatic_precision(self);
        format!("{self:.precision$}")
    }

    fn rfc3339_millis(self) -> String {
        format!("{self:.3}")
    }

    fn rfc3339_seconds(self) -> String {
        format!("{self:.0}")
    }

    fn utc_date(self) -> Date {
        TimeZone::UTC.to_datetime(self).date()
    }

    fn epoch_seconds(self) -> i64 {
        if self.subsec_nanosecond() < 0 {
            self.as_second() - 1
        } else {
            self.as_second()
        }
    }

    fn epoch_millis(self) -> i64 {
        if self.as_nanosecond().rem_euclid(1_000_000) != 0 && self < Self::UNIX_EPOCH {
            self.as_millisecond() - 1
        } else {
            self.as_millisecond()
        }
    }

    fn epoch_micros(self) -> i64 {
        if self.as_nanosecond().rem_euclid(1_000) != 0 && self < Self::UNIX_EPOCH {
            self.as_microsecond() - 1
        } else {
            self.as_microsecond()
        }
    }

    fn nanosecond_of_second(self) -> u32 {
        self.subsec_nanosecond()
            .rem_euclid(1_000_000_000)
            .unsigned_abs()
    }

    fn truncate_subseconds(self, digits: u32) -> Timestamp {
        let quantum = 10_i32.pow(9 - digits);
        let nanos = self.subsec_nanosecond().rem_euclid(1_000_000_000);
        Self::constant(self.epoch_seconds(), nanos - nanos % quantum)
    }
}

pub const fn elapsed_days(days: i64) -> SignedDuration {
    SignedDuration::from_hours(days * 24)
}

pub const fn elapsed_weeks(weeks: i64) -> SignedDuration {
    elapsed_days(weeks * 7)
}

pub fn bundled_zone(name: &str) -> Result<TimeZone, Error> {
    let (canonical, data) = jiff_tzdb::get(name)
        .filter(|(canonical, _)| *canonical == name)
        .ok_or_else(|| Error::from_args(format_args!("unknown time zone {name}")))?;
    TimeZone::tzif(canonical, data)
}

fn invalid_timestamp() -> Error {
    Error::from_args(format_args!("invalid timestamp"))
}

pub fn parse_rfc3339(raw: &str) -> Result<Timestamp, Error> {
    let bytes = raw.as_bytes();
    if !matches!(
        bytes,
        [
            b'0'..=b'9',
            b'0'..=b'9',
            b'0'..=b'9',
            b'0'..=b'9',
            b'-',
            b'0'..=b'9',
            b'0'..=b'9',
            b'-',
            b'0'..=b'9',
            b'0'..=b'9',
            b'T' | b't' | b' ',
            b'0'..=b'9',
            b'0'..=b'9',
            b':',
            b'0'..=b'9',
            b'0'..=b'9',
            b':',
            b'0'..=b'9',
            b'0'..=b'9',
            ..
        ]
    ) {
        return Err(invalid_timestamp());
    }
    let tail = raw.get(19..).ok_or_else(invalid_timestamp)?;
    let fraction_digits = tail.strip_prefix('.').map_or(0, |digits| {
        digits.bytes().take_while(u8::is_ascii_digit).count()
    });
    let offset_start = if tail.starts_with('.') {
        if fraction_digits == 0 {
            return Err(invalid_timestamp());
        }
        fraction_digits + 1
    } else {
        0
    };
    let offset = tail.get(offset_start..).ok_or_else(invalid_timestamp)?;
    if !matches!(
        offset.as_bytes(),
        [b'Z' | b'z']
            | [
                b'+' | b'-',
                b'0'..=b'9',
                b'0'..=b'9',
                b':',
                b'0'..=b'9',
                b'0'..=b'9'
            ]
    ) {
        return Err(invalid_timestamp());
    }
    if fraction_digits > 9 {
        let prefix = raw.get(..29).ok_or_else(invalid_timestamp)?;
        return format!("{prefix}{offset}").parse();
    }
    raw.parse()
}

pub fn parse_wire_timestamp(raw: &str) -> Result<Timestamp, Error> {
    match parse_rfc3339(raw) {
        Ok(at) => Ok(at),
        Err(_strict) => {
            let raw = raw.trim_end();
            if let Some(body) = raw.strip_suffix(['Z', 'z']) {
                return jiff::fmt::strtime::parse("%Y-%m-%d%T%.f", body)
                    .and_then(|time| TimeZone::UTC.to_timestamp(time.to_datetime()?))
                    .or_else(|_error| parse_wire_offset(body, "+0000"));
            }
            if let Some(body) = raw.get(..raw.len().saturating_sub(3))
                && raw
                    .get(body.len()..)
                    .is_some_and(|suffix| suffix.eq_ignore_ascii_case("UTC"))
            {
                return parse_wire_offset(body, "+0000");
            }
            let separator = raw
                .get(10..)
                .and_then(|tail| tail.rfind(['+', '-']))
                .map(|index| index + 10)
                .ok_or_else(invalid_timestamp)?;
            let (body, offset) = raw.split_at(separator);
            parse_wire_offset(body, offset)
        }
    }
}

fn parse_wire_offset(body: &str, offset: &str) -> Result<Timestamp, Error> {
    let body = body.trim_end();
    let separator = body.rfind(['T', 't', ' ']).ok_or_else(invalid_timestamp)?;
    let boundary = body.get(..separator).ok_or_else(invalid_timestamp)?;
    let clock = body.get(separator + 1..).ok_or_else(invalid_timestamp)?;
    let offset: String = offset
        .chars()
        .filter(|ch| *ch != ':' && !ch.is_whitespace())
        .collect();
    if !matches!(
        offset.as_bytes(),
        [
            b'+' | b'-',
            b'0'..=b'9',
            b'0'..=b'9',
            b'0'..=b'9',
            b'0'..=b'9'
        ]
    ) {
        return Err(invalid_timestamp());
    }
    Timestamp::strptime("%Y-%m-%dT%T%.f%z", format!("{boundary}T{clock}{offset}"))
}

#[derive(Debug)]
pub struct Millis;

#[derive(Debug)]
pub struct Rfc3339;

#[derive(Debug)]
pub struct EpochMillis;

#[derive(Debug)]
pub struct CivilDate;

impl SerializeAs<Timestamp> for Millis {
    fn serialize_as<S: Serializer>(value: &Timestamp, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.rfc3339_millis())
    }
}

impl<'de> DeserializeAs<'de, Timestamp> for Millis {
    fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<Timestamp, D::Error> {
        let raw = String::deserialize(deserializer)?;
        parse_rfc3339(&raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchemaAs<Timestamp> for Millis {
    fn schema_name() -> Cow<'static, str> {
        "Millis".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "format": "date-time" })
    }
}

impl SerializeAs<Timestamp> for Rfc3339 {
    fn serialize_as<S: Serializer>(value: &Timestamp, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.rfc3339_z())
    }
}

impl<'de> DeserializeAs<'de, Timestamp> for Rfc3339 {
    fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<Timestamp, D::Error> {
        let raw = String::deserialize(deserializer)?;
        parse_wire_timestamp(&raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchemaAs<Timestamp> for Rfc3339 {
    fn schema_name() -> Cow<'static, str> {
        "DateTime".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        Millis::json_schema(generator)
    }
}

impl SerializeAs<Timestamp> for EpochMillis {
    fn serialize_as<S: Serializer>(value: &Timestamp, serializer: S) -> Result<S::Ok, S::Error> {
        let nanos = value.as_nanosecond();
        let rounded = (nanos + nanos.signum() * 500_000) / 1_000_000;
        let millis = i64::try_from(rounded).map_err(serde::ser::Error::custom)?;
        serializer.serialize_i64(millis)
    }
}

impl<'de> DeserializeAs<'de, Timestamp> for EpochMillis {
    fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<Timestamp, D::Error> {
        Timestamp::from_millisecond(i64::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchemaAs<Timestamp> for EpochMillis {
    fn schema_name() -> Cow<'static, str> {
        "int64".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <i64 as schemars::JsonSchema>::json_schema(generator)
    }
}

impl SerializeAs<Date> for CivilDate {
    fn serialize_as<S: Serializer>(value: &Date, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.strftime("%Y-%m-%d").to_string())
    }
}

impl<'de> DeserializeAs<'de, Date> for CivilDate {
    fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<Date, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Date::strptime("%Y-%m-%d", &raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchemaAs<Date> for CivilDate {
    fn schema_name() -> Cow<'static, str> {
        "NaiveDate".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "format": "date" })
    }
}

pub fn date_at_utc(date: Date) -> Result<Timestamp, Error> {
    TimeZone::UTC.to_timestamp(date.at(0, 0, 0, 0))
}

pub fn timestamp_at_utc(
    year: i16,
    month: i8,
    day: i8,
    hour: i8,
    minute: i8,
    second: i8,
) -> Result<Timestamp, Error> {
    TimeZone::UTC.to_timestamp(jiff::civil::DateTime::new(
        year, month, day, hour, minute, second, 0,
    )?)
}
