use std::ops::Range;

use kernel::model::{FinishReason, LlmResponse, ModelOptions, ModelRequest};
use thiserror::Error;
use wire::message::{Message, MessageContent, Role};

use super::plan::{Source, tag};
use super::prompt::Prompt;
use super::python_whitespace;

pub(crate) const NON_STOP: u32 = 1 << 0;
pub(crate) const OVER_CEILING: u32 = 1 << 1;
pub(crate) const THINK_TAG: u32 = 1 << 2;
pub(crate) const REFUSAL: u32 = 1 << 3;

pub(crate) fn model_request(model: &str, prompt: &Prompt) -> ModelRequest {
    let text = |role, content: &String| Message {
        role,
        content: MessageContent::Text(content.clone()),
        reasoning: None,
        tool_calls: None,
        tool_call_id: None,
    };
    ModelRequest {
        model: model.to_owned(),
        options: ModelOptions {
            temperature: Some(0.0),
            enable_thinking: Some(prompt.thinking),
            ..ModelOptions::default()
        },
        messages: vec![
            text(Role::System, &prompt.system),
            text(Role::User, &prompt.user),
        ],
        thinking: None,
        max_tokens: Some(prompt.max_tokens),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(crate) struct Section {
    pub(crate) heading: String,
    pub(crate) chars: Range<usize>,
    pub(crate) source_first: usize,
    pub(crate) source_last: usize,
    pub(crate) range_verified: bool,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(crate) struct Reply {
    pub(crate) text: String,
    pub(crate) sections: Vec<Section>,
    pub(crate) quality_flags: u32,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("empty top-down summary")]
pub(crate) struct EmptyReply {
    pub(crate) quality_flags: u32,
}

pub(crate) fn parse_model_reply(
    response: &LlmResponse,
    editorial_ceiling: u32,
    sources: &[Source],
) -> Result<Reply, EmptyReply> {
    parse_reply(
        response.content.as_deref().unwrap_or_default(),
        response.finish_reason == Some(FinishReason::Stop),
        response.usage.as_ref().map(|usage| usage.completion_tokens),
        editorial_ceiling,
        sources,
    )
}

pub(crate) fn parse_reply(
    text: &str,
    stopped: bool,
    completion_tokens: Option<u64>,
    editorial_ceiling: u32,
    sources: &[Source],
) -> Result<Reply, EmptyReply> {
    let text = text.trim_matches(python_whitespace);
    let mut quality_flags = 0;
    if !stopped {
        quality_flags |= NON_STOP;
    }
    if completion_tokens.is_some_and(|tokens| tokens > u64::from(editorial_ceiling)) {
        quality_flags |= OVER_CEILING;
    }
    if text.starts_with("<think>") || text.contains("</think>") {
        quality_flags |= THINK_TAG;
    }
    if refusal(text) {
        quality_flags |= REFUSAL;
    }
    if text.is_empty() {
        return Err(EmptyReply { quality_flags });
    }
    Ok(Reply {
        text: text.to_owned(),
        sections: sections(text, sources),
        quality_flags,
    })
}

fn refusal(text: &str) -> bool {
    let prefix: String = text
        .chars()
        .take(20)
        .map(|ch| match ch {
            'İ' | 'ı' => 'i',
            'ſ' => 's',
            ch => ch.to_ascii_lowercase(),
        })
        .collect();
    [
        "i'm sorry",
        "im sorry",
        "i cannot",
        "i can't help",
        "i cant help",
        "i can't assist",
        "i cant assist",
        "as an ai",
    ]
    .iter()
    .any(|start| prefix.starts_with(start))
}

pub(crate) fn sections(text: &str, sources: &[Source]) -> Vec<Section> {
    let (Some(first), Some(last)) = (sources.first(), sources.last()) else {
        return Vec::new();
    };
    let fallback = (first.ordinal, last.ordinal);
    let mut sections: Vec<Section> = Vec::new();
    let mut offset = 0;
    let mut preamble = false;
    for line in text.split_inclusive('\n') {
        if let Some(raw_heading) = line.strip_prefix("## ") {
            if let Some(previous) = sections.last_mut() {
                previous.chars.end = offset;
            } else if preamble {
                sections.push(section("Overview", 0..offset, fallback, false));
            }
            let (heading, bounds) = heading_range(raw_heading);
            let valid = bounds.filter(|&(start, end)| owns_range(sources, start, end));
            sections.push(section(
                heading,
                offset..offset,
                valid.unwrap_or(fallback),
                valid.is_some(),
            ));
        } else if sections.is_empty() {
            preamble |= !line.trim_matches(python_whitespace).is_empty();
        }
        offset += line.chars().count();
    }
    if let Some(last) = sections.last_mut() {
        last.chars.end = offset;
    } else if offset != 0 {
        sections.push(section("Overview", 0..offset, fallback, false));
    }
    sections
}

fn section(heading: &str, chars: Range<usize>, bounds: (usize, usize), verified: bool) -> Section {
    Section {
        heading: heading.to_owned(),
        chars,
        source_first: bounds.0,
        source_last: bounds.1,
        range_verified: verified,
    }
}

fn heading_range(raw: &str) -> (&str, Option<(usize, usize)>) {
    let heading = raw.trim_end_matches(python_whitespace);
    if let Some((title, suffix)) = heading.rsplit_once(" [")
        && !title.trim_matches(python_whitespace).is_empty()
        && let Some((first, last)) = suffix
            .strip_suffix(']')
            .and_then(|suffix| suffix.split_once('-'))
        && let (Some(first), Some(last)) = (ordinal(first), ordinal(last))
    {
        return (title, Some((first, last)));
    }
    (heading, None)
}

fn ordinal(text: &str) -> Option<usize> {
    let digits = text.strip_prefix('m')?;
    let number = digits.bytes().try_fold(0_usize, |number, byte| {
        let digit = usize::from(byte.checked_sub(b'0').filter(|digit| *digit <= 9)?);
        number.checked_mul(10)?.checked_add(digit)
    })?;
    (tag(number) == text).then_some(number)
}

fn owns_range(sources: &[Source], start: usize, end: usize) -> bool {
    if start > end {
        return false;
    }
    let first = sources.partition_point(|source| source.ordinal < start);
    let mut next = start;
    for source in sources.iter().skip(first) {
        if source.ordinal == next {
            if next == end {
                return true;
            }
            next += 1;
        } else if source.ordinal > next {
            return false;
        }
    }
    false
}
