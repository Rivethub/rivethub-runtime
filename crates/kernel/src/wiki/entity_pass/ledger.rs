use super::types::LeafClock;
use jiff::Timestamp;
use jiff::civil::Date;
use values::time::TimestampFormat as _;

#[must_use]
pub fn place_as_of(raw: &str, leaves: &[LeafClock]) -> Timestamp {
    let Some(fallback) = leaves.last().map(|leaf| leaf.start) else {
        return Timestamp::UNIX_EPOCH;
    };
    if let Ok(parsed) = values::time::parse_rfc3339(raw.trim()) {
        let time = parsed;
        if leaves
            .iter()
            .any(|leaf| time >= leaf.start && time <= leaf.end)
        {
            return time;
        }
    }
    let prefix = raw.trim().get(..10);
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let day = prefix.and_then(|text| Date::strptime("%Y-%m-%d", text).ok());
    if let Some(day) = day
        && let Some(leaf) = leaves
            .iter()
            .find(|leaf| leaf.start.utc_date() == day || leaf.end.utc_date() == day)
    {
        return leaf.start;
    }
    fallback
}
