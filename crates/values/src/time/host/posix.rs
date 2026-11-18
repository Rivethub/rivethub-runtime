use std::fmt::Write as _;

use jiff::tz::TimeZone;

pub(super) fn parse(tz: &str) -> Option<TimeZone> {
    let (offsets, rules) = match tz.split_once(',') {
        Some((offsets, rules)) => (offsets, Some(rules)),
        None => (tz, None),
    };
    let (standard, offset) = name(offsets)?;
    let (offset, daylight) = signed_clock(offset)?;
    let normalized = match rules {
        None if daylight.is_empty() => format!("{standard}{offset}"),
        Some(rules) => {
            let (daylight, daylight_offset) = name(daylight)?;
            let daylight_offset = if daylight_offset.is_empty() {
                String::new()
            } else {
                let (value, tail) = signed_clock(daylight_offset)?;
                if !tail.is_empty() {
                    return None;
                }
                value
            };
            let (start, end) = rules.split_once(',')?;
            format!(
                "{standard}{offset}{daylight}{daylight_offset},{},{}",
                rule(start)?,
                rule(end)?
            )
        }
        None => return None,
    };
    if let Ok(zone) = TimeZone::posix(&normalized) {
        return Some(zone);
    }
    None
}

fn name(text: &str) -> Option<(&str, &str)> {
    let length = match text.strip_prefix('<') {
        Some(quoted) => {
            let (name, _) = quoted.split_once('>')?;
            if !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-'))
            {
                return None;
            }
            name.len() + 2
        }
        None => text.bytes().take_while(u8::is_ascii_alphabetic).count(),
    };
    let (name, tail) = text.split_at(length);
    let width = if name.starts_with('<') {
        length - 2
    } else {
        length
    };
    (3..=7).contains(&width).then_some((name, tail))
}

fn signed_clock(text: &str) -> Option<(String, &str)> {
    let (sign, digits) = match text.as_bytes() {
        [b'+' | b'-', ..] => text.split_at(1),
        _ => ("", text),
    };
    let (clock, tail) = clock(digits, 23)?;
    Some((format!("{sign}{clock}"), tail))
}

fn clock(text: &str, max_hour: u16) -> Option<(String, &str)> {
    let (hour, mut tail) = number(text)?;
    if hour > max_hour {
        return None;
    }
    let mut clock = hour.to_string();
    for _ in 0..2 {
        let Some(digits) = tail.strip_prefix(':') else {
            break;
        };
        let (value, rest) = number(digits)?;
        if value > 59 {
            return None;
        }
        if write!(clock, ":{value:02}").is_err() {
            return None;
        }
        tail = rest;
    }
    Some((clock, tail))
}

fn rule(text: &str) -> Option<String> {
    let (day, time) = match text.split_once('/') {
        Some((day, time)) => (day, Some(time)),
        None => (text, None),
    };
    let (prefix, day) = match day.as_bytes() {
        [b'M' | b'J', ..] => day.split_at(1),
        _ => ("", day),
    };
    let fields = day
        .split('.')
        .map(|field| {
            let (number, rest) = number(field)?;
            rest.is_empty().then(|| number.to_string())
        })
        .collect::<Option<Vec<_>>>()?;
    let date = format!("{prefix}{}", fields.join("."));
    match time {
        Some(time) => {
            let (clock, tail) = clock(time, 24)?;
            tail.is_empty().then(|| format!("{date}/{clock}"))
        }
        None => Some(date),
    }
}

fn number(text: &str) -> Option<(u16, &str)> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    let (value, tail) = text.split_at(digits);
    match value.parse() {
        Ok(value) => Some((value, tail)),
        Err(_invalid) => None,
    }
}
