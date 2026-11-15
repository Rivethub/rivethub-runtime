use std::collections::BTreeMap;
use std::collections::HashMap;
use std::num::TryFromIntError;

use kernel::transcript::TranscriptReaderError;
use serde::Deserialize;
use serde_with::DefaultOnError;
use serde_with::serde_as;

use crate::readers::scan::RawLine;

const TURN_STRIDE: i64 = 1_000_000;
const OTHER_BASE: i64 = 10_000;
const USER_CHUNK: &str = "user_message_chunk";

#[serde_as]
#[derive(Debug, Deserialize)]
struct Event {
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    params: Option<Params>,
}

#[serde_as]
#[derive(Debug, Deserialize)]
struct Params {
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "_meta", default)]
    meta: Option<LineMeta>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    update: Option<Update>,
}

#[serde_as]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LineMeta {
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    prompt_id: Option<String>,
}

#[serde_as]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Update {
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    session_update: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "_meta", default)]
    meta: Option<UpdateMeta>,
}

#[serde_as]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateMeta {
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    prompt_index: Option<i64>,
}

impl Params {
    fn prompt_id(&self) -> Option<&str> {
        self.meta.as_ref()?.prompt_id.as_deref()
    }
}

pub(super) fn ordinals(records: &[RawLine]) -> Result<BTreeMap<u64, i64>, TranscriptReaderError> {
    let mut events: Vec<(u64, Params)> = Vec::new();
    for record in records {
        #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
        let event = serde_json::from_str::<Event>(&record.text).ok();
        if let Some(params) = event.and_then(|event| event.params) {
            events.push((record.number, params));
        }
    }
    let mut turns: HashMap<&str, i64> = HashMap::new();
    for (_, params) in &events {
        if let Some(id) = params.prompt_id() {
            let next = signed(turns.len())?;
            turns.entry(id).or_insert(next);
        }
    }
    let mut ordinals = BTreeMap::new();
    let mut user_chunks: HashMap<i64, i64> = HashMap::new();
    let mut current = -1_i64;
    for (number, params) in &events {
        let Some(update) = &params.update else {
            continue;
        };
        let Some(kind) = update
            .session_update
            .as_deref()
            .filter(|kind| !kind.is_empty())
        else {
            continue;
        };
        let (turn, sub) = if kind == USER_CHUNK {
            let turn = update
                .meta
                .as_ref()
                .and_then(|meta| meta.prompt_index)
                .unwrap_or_else(|| current.max(0));
            let count = user_chunks.entry(turn).or_insert(0);
            let order = *count;
            *count = count.saturating_add(1);
            current = turn;
            (turn, order)
        } else {
            if let Some(turn) = params.prompt_id().and_then(|id| turns.get(id)) {
                current = *turn;
            }
            (
                current,
                OTHER_BASE.saturating_add(signed(number.saturating_sub(1))?),
            )
        };
        ordinals.insert(
            *number,
            turn.saturating_mul(TURN_STRIDE).saturating_add(sub),
        );
    }
    Ok(ordinals)
}

fn signed<T: TryInto<i64, Error = TryFromIntError>>(
    value: T,
) -> Result<i64, TranscriptReaderError> {
    value
        .try_into()
        .map_err(|error| TranscriptReaderError::Read {
            message: format!("grok ordinal does not fit: {error}"),
            source: Box::new(error),
        })
}
