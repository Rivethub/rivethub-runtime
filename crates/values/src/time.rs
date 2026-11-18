use std::borrow::Cow;

use jiff::civil::{Date, DateTime};
use jiff::tz::{Offset, TimeZone};
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

fn legacy_year(text: String) -> String {
    match text.strip_prefix("-00") {
        Some(year_and_time) => format!("-{year_and_time}"),
        None => text,
    }
}

pub trait LegacyStrftime {
    fn legacy_strftime(&self, pattern: &str) -> String;
}

fn legacy_year_pattern(pattern: &str, year: i16) -> Cow<'_, str> {
    if year < 0 && pattern.contains("%Y") {
        Cow::Owned(pattern.replace("%Y", &format!("-{:04}", year.unsigned_abs())))
    } else {
        Cow::Borrowed(pattern)
    }
}

impl LegacyStrftime for Timestamp {
    fn legacy_strftime(&self, pattern: &str) -> String {
        let pattern = legacy_year_pattern(pattern, self.utc_date().year());
        self.strftime(pattern.as_ref()).to_string()
    }
}

impl LegacyStrftime for Date {
    fn legacy_strftime(&self, pattern: &str) -> String {
        let pattern = legacy_year_pattern(pattern, self.year());
        self.strftime(pattern.as_ref()).to_string()
    }
}

impl LegacyStrftime for jiff::ZonedDateTime {
    fn legacy_strftime(&self, pattern: &str) -> String {
        let pattern = legacy_year_pattern(pattern, self.year());
        self.strftime(pattern.as_ref()).to_string()
    }
}

impl TimestampFormat for Timestamp {
    fn rfc3339_offset(self) -> String {
        let precision = automatic_precision(self);
        let base = self.display_with_offset(jiff::tz::Offset::UTC);
        legacy_year(format!("{base:.precision$}"))
    }

    fn rfc3339_z(self) -> String {
        let precision = automatic_precision(self);
        legacy_year(format!("{self:.precision$}"))
    }

    fn rfc3339_millis(self) -> String {
        legacy_year(format!("{self:.3}"))
    }

    fn rfc3339_seconds(self) -> String {
        legacy_year(format!("{self:.0}"))
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
    let normalized = offset.strip_prefix('−').map(|digits| format!("-{digits}"));
    let offset = normalized.as_deref().unwrap_or(offset);
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
    if fraction_digits > 9 || normalized.is_some() {
        let prefix = raw
            .get(..19 + offset_start.min(10))
            .ok_or_else(invalid_timestamp)?;
        return format!("{prefix}{offset}").parse();
    }
    raw.parse()
}

pub fn parse_wire_timestamp(raw: &str) -> Result<Timestamp, Error> {
    parse_rfc3339(raw).or_else(|_strict| {
        let (civil, offset) = legacy_civil_timestamp(raw, false)?;
        legacy_numeric_offset(offset.trim(), false)?.to_timestamp(civil)
    })
}

pub fn legacy_civil_timestamp(raw: &str, space_separator: bool) -> Result<(DateTime, &str), Error> {
    let raw = raw.trim_start();
    let (sign, digits, width) = match raw.as_bytes() {
        [b'+', ..] => (1, raw.get(1..).ok_or_else(invalid_timestamp)?, usize::MAX),
        [b'-', ..] => (-1, raw.get(1..).ok_or_else(invalid_timestamp)?, usize::MAX),
        _ => (1, raw, 4),
    };
    let (year, tail) = decimal::<i16>(digits, width)?;
    let (month, tail) = decimal(delimiter(tail, '-', space_separator)?, 2)?;
    let (day, tail) = decimal(delimiter(tail, '-', space_separator)?, 2)?;
    let clock = if space_separator {
        tail.trim_start()
    } else {
        tail.strip_prefix(['T', 't', ' '])
            .ok_or_else(invalid_timestamp)?
    };
    let (hour, tail) = decimal(clock.trim_start(), 2)?;
    let (minute, tail) = decimal(delimiter(tail, ':', space_separator)?, 2)?;
    let (second, tail) = decimal(delimiter(tail, ':', space_separator)?, 2)?;
    let (nanos, offset) = parse_fraction(tail)?;
    let second = if second == 60 { 59 } else { second };
    let civil = DateTime::new(year * sign, month, day, hour, minute, second, nanos)?;
    Ok((civil, offset))
}

fn delimiter(raw: &str, separator: char, space_separator: bool) -> Result<&str, Error> {
    let raw = if space_separator {
        raw
    } else {
        raw.trim_start()
    };
    raw.strip_prefix(separator)
        .map(str::trim_start)
        .ok_or_else(invalid_timestamp)
}

fn decimal<T: std::str::FromStr>(raw: &str, width: usize) -> Result<(T, &str), Error> {
    let len = raw
        .bytes()
        .take(width)
        .take_while(u8::is_ascii_digit)
        .count();
    let (digits, tail) = raw.split_at(len);
    let value = digits.parse().map_err(|_error| invalid_timestamp())?;
    Ok((value, tail))
}

fn parse_fraction(raw: &str) -> Result<(i32, &str), Error> {
    let Some(fraction) = raw.strip_prefix('.') else {
        return Ok((0, raw));
    };
    let length = fraction.bytes().take_while(u8::is_ascii_digit).count();
    let (digits, tail) = fraction.split_at(length);
    let (nanos, excess) = decimal::<i32>(digits, 9)?;
    let used = u32::try_from(digits.len() - excess.len()).map_err(|_error| invalid_timestamp())?;
    Ok((nanos * 10_i32.pow(9 - used), tail))
}

pub fn legacy_numeric_offset(raw: &str, allow_hours_only: bool) -> Result<Offset, Error> {
    if raw == "Z" || raw == "z" || (!allow_hours_only && raw.eq_ignore_ascii_case("UTC")) {
        return Ok(Offset::UTC);
    }
    let (sign, tail) = if let Some(tail) = raw.strip_prefix('+') {
        (1, tail)
    } else if let Some(tail) = raw.strip_prefix(['-', '−']) {
        (-1, tail)
    } else {
        return Err(invalid_timestamp());
    };
    let (hours, tail) = offset_digits(tail)?;
    let tail = tail.trim_start_matches(|ch: char| ch == ':' || ch.is_whitespace());
    let (minutes, tail) = if allow_hours_only && tail.is_empty() {
        (0, tail)
    } else {
        offset_digits(tail)?
    };
    if hours > 23 || minutes > 59 || !tail.is_empty() {
        return Err(invalid_timestamp());
    }
    Offset::from_seconds(sign * (hours * 3600 + minutes * 60))
}

fn offset_digits(raw: &str) -> Result<(i32, &str), Error> {
    if !matches!(raw.as_bytes(), [b'0'..=b'9', b'0'..=b'9', ..]) {
        return Err(invalid_timestamp());
    }
    decimal(raw, 2)
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
        serializer.serialize_str(&value.legacy_strftime("%Y-%m-%d"))
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
