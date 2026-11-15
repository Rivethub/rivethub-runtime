use chrono::DateTime;
use chrono::NaiveDate;
use chrono::Utc;

use super::types::LeafClock;

#[must_use]
pub fn place_as_of(raw: &str, leaves: &[LeafClock]) -> DateTime<Utc> {
    let Some(fallback) = leaves.last().map(|leaf| leaf.start) else {
        return DateTime::UNIX_EPOCH;
    };
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw.trim()) {
        let time = parsed.with_timezone(&Utc);
        if leaves
            .iter()
            .any(|leaf| time >= leaf.start && time <= leaf.end)
        {
            return time;
        }
    }
    let prefix = raw.trim().get(..10);
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let day = prefix.and_then(|text| NaiveDate::parse_from_str(text, "%Y-%m-%d").ok());
    if let Some(day) = day
        && let Some(leaf) = leaves
            .iter()
            .find(|leaf| leaf.start.date_naive() == day || leaf.end.date_naive() == day)
    {
        return leaf.start;
    }
    fallback
}
