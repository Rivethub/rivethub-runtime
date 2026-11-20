use jiff::Timestamp;
use std::fmt::Write as _;
use std::ops::Range;
use wire::time::LegacyStrftime as _;

use kernel::memory::MemoryEntry;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use wire::message::Role;

use super::prompt::{self, Mode, Prompt};
use super::settings::{CARRY_CEILING, Limits};

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(crate) struct Source {
    pub(crate) id: Uuid,
    pub(crate) ordinal: usize,
    #[serde_as(as = "wire::time::Rfc3339")]
    pub(crate) created_at: Timestamp,
    pub(crate) render_key: [u8; 32],
}

#[derive(Debug, Clone)]
pub(crate) struct Block {
    pub(crate) source: Source,
    pub(crate) text: String,
    role: Role,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum PlanError {
    #[error("renderable transcript row needs a UUID and timestamp")]
    MissingSource,
    #[error("invalid estimated characters per token")]
    InvalidMeter,
    #[error("chunk budget and meter leave no room for a character")]
    InvalidCut,
    #[error("composition needs at least one record")]
    EmptyComposition,
    #[error("one composition record exceeds its context budget")]
    ComposeContext,
}

pub(crate) fn render(ordered: &[MemoryEntry]) -> Result<Vec<Block>, PlanError> {
    let mut blocks = Vec::new();
    for message in ordered {
        let name = message.tool_name.as_deref().filter(|name| !name.is_empty());
        if message.content.is_empty() && name.is_none() {
            continue;
        }
        let created_at = message.created_at.ok_or(PlanError::MissingSource)?;
        let id = Uuid::parse_str(message.id.as_deref().ok_or(PlanError::MissingSource)?)
            .map_err(|_| PlanError::MissingSource)?;
        let ordinal = blocks.len() + 1;
        let tag = tag(ordinal);
        let stamp = created_at.legacy_strftime("%Y-%m-%d %H:%M");
        let mut text = String::new();
        let mut untagged = Sha256::new();
        let mut line = |header: &str, body: &str| {
            if !text.is_empty() {
                text.push('\n');
                untagged.update(b"\n");
            }
            let _infallible = write!(text, "[{tag} {header}] {body}");
            untagged.update(b"[");
            untagged.update(header);
            untagged.update(b"] ");
            untagged.update(body);
        };
        if !message.content.is_empty() {
            line(&format!("{stamp} {}", message.role), &message.content);
        }
        if let Some(name) = name {
            let mut args = String::new();
            if let Some(object) = &message.tool_args {
                json_object(object.as_map().iter(), &mut args);
            }
            line(&format!("{stamp} tool call {name}"), &args);
            if let Some(result) = message
                .tool_result
                .as_deref()
                .filter(|text| !text.is_empty())
            {
                line("tool result", result);
            }
        }
        blocks.push(Block {
            source: Source {
                id,
                ordinal,
                created_at,
                render_key: untagged.finalize().into(),
            },
            text,
            role: message.role,
        });
    }
    Ok(blocks)
}

pub(crate) fn tag(ordinal: usize) -> String {
    format!("m{ordinal:05}")
}

fn json_object<'a>(entries: impl Iterator<Item = (&'a String, &'a Value)>, out: &mut String) {
    out.push('{');
    for (index, (key, value)) in entries.enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        let _infallible = write!(out, "{}: ", Value::String(key.clone()));
        python_json(value, out);
    }
    out.push('}');
}

fn python_json(value: &Value, out: &mut String) {
    match value {
        Value::Object(entries) => json_object(entries.iter(), out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                python_json(item, out);
            }
            out.push(']');
        }
        Value::Number(number) if number.is_f64() => python_float(&number.to_string(), out),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            let _infallible = write!(out, "{value}");
        }
    }
}

fn python_float(formatted: &str, out: &mut String) {
    if let Some((mantissa, exponent)) = formatted.split_once('e')
        && let Ok(exponent) = exponent.parse::<i32>()
    {
        let _infallible = write!(out, "{mantissa}e{exponent:+03}");
        return;
    }
    let (sign, magnitude) = formatted
        .strip_prefix('-')
        .map_or(("", formatted), |magnitude| ("-", magnitude));
    if let Some(fraction) = magnitude.strip_prefix("0.0000")
        && let Some((first, rest)) = fraction.split_at_checked(1)
        && first != "0"
    {
        let _infallible = write!(out, "{sign}{first}");
        if !rest.is_empty() {
            out.push('.');
            out.push_str(rest);
        }
        out.push_str("e-05");
    } else {
        out.push_str(formatted);
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Meter(f64);

impl Meter {
    pub(crate) const fn uncalibrated() -> Self {
        Self(3.0)
    }

    pub(crate) const fn ratio(self) -> f64 {
        self.0
    }

    pub(crate) fn new(chars_per_token: f64) -> Result<Self, PlanError> {
        if !chars_per_token.is_finite() || chars_per_token <= 0.0 {
            return Err(PlanError::InvalidMeter);
        }
        Ok(Self(chars_per_token))
    }

    #[expect(clippy::cast_precision_loss)]
    pub(crate) fn calibrate(sample: &str, exact_tokens: usize) -> Result<Self, PlanError> {
        Self::new(sample.chars().count() as f64 / exact_tokens as f64 * 0.9)
    }

    pub(crate) fn estimate(self, text: &str) -> usize {
        self.estimate_chars(text.chars().count())
    }

    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn estimate_chars(self, chars: usize) -> usize {
        ((chars as f64 / self.0) as usize).saturating_add(1)
    }

    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub(crate) fn cut_chars(self, budget: usize) -> Result<usize, PlanError> {
        let cut = (budget as f64 * self.0) as usize;
        if cut == 0 {
            return Err(PlanError::InvalidCut);
        }
        Ok(cut)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(crate) struct Fragment {
    pub(crate) source: Source,
    pub(crate) block_chars: Range<usize>,
    pub(crate) chunk_chars: Range<usize>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct Chunk {
    pub(crate) text: String,
    pub(crate) input_token_estimate: usize,
    pub(crate) fragments: Vec<Fragment>,
}

#[derive(Debug, Clone)]
pub(crate) struct TranscriptPlan {
    pub(crate) chunks: Vec<Chunk>,
    pub(crate) total_tokens: usize,
}

pub(crate) enum ExactCount {
    Pending,
    Known(usize),
    OverContext,
}

pub(crate) type ExactCounter<'a> = dyn FnMut(&str) -> ExactCount + 'a;

#[derive(Default)]
struct Packed {
    text: String,
    chars: usize,
    fragments: Vec<Fragment>,
}

impl Packed {
    fn push(&mut self, block: &Block, text: &str, block_chars: Range<usize>) {
        if !self.fragments.is_empty() {
            self.text.push('\n');
            self.chars += 1;
        }
        let start = self.chars;
        self.text.push_str(text);
        self.chars += block_chars.len();
        self.fragments.push(Fragment {
            source: block.source.clone(),
            block_chars,
            chunk_chars: start..self.chars,
        });
    }

    fn finish(
        self,
        meter: Meter,
        budget: usize,
        exact: &mut Option<&mut ExactCounter<'_>>,
        output: &mut Vec<Chunk>,
    ) {
        if self.fragments.is_empty() {
            return;
        }
        let mut pieces = Vec::new();
        split_exact(&self.text, budget, exact, &mut pieces);
        let mut start = 0;
        let mut first_fragment = 0;
        for piece in pieces {
            let chars = piece.chars().count();
            let end = start + chars;
            first_fragment += self
                .fragments
                .iter()
                .skip(first_fragment)
                .take_while(|fragment| fragment.chunk_chars.end <= start)
                .count();
            let fragments = self
                .fragments
                .iter()
                .skip(first_fragment)
                .take_while(|fragment| fragment.chunk_chars.start < end)
                .filter_map(|fragment| {
                    let a = start.max(fragment.chunk_chars.start);
                    let b = end.min(fragment.chunk_chars.end);
                    (a < b).then(|| {
                        let block_start =
                            fragment.block_chars.start + a - fragment.chunk_chars.start;
                        Fragment {
                            source: fragment.source.clone(),
                            block_chars: block_start..block_start + (b - a),
                            chunk_chars: (a - start)..(b - start),
                        }
                    })
                })
                .collect();
            output.push(Chunk {
                text: piece.to_owned(),
                input_token_estimate: meter.estimate_chars(chars),
                fragments,
            });
            start = end;
        }
    }
}

fn byte_at(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(index, _)| index)
}

fn split_exact<'a>(
    text: &'a str,
    budget: usize,
    exact: &mut Option<&mut ExactCounter<'_>>,
    output: &mut Vec<&'a str>,
) {
    if let Some(count) = exact {
        let split = match count(text) {
            ExactCount::Pending => false,
            ExactCount::Known(count) => count == 0 || count > budget,
            ExactCount::OverContext => true,
        };
        if split && text.chars().count() >= 2_000 {
            let midpoint = byte_at(text, text.chars().count() / 2);
            let cut = text[..midpoint]
                .rfind('\n')
                .filter(|cut| *cut > 0)
                .unwrap_or(midpoint);
            let (left, right) = text.split_at(cut);
            split_exact(left, budget, exact, output);
            split_exact(right, budget, exact, output);
            return;
        }
    }
    output.push(text);
}

pub(crate) fn pack(
    blocks: &[Block],
    meter: Meter,
    limits: Limits,
    mut exact: Option<&mut ExactCounter<'_>>,
) -> Result<TranscriptPlan, PlanError> {
    let budget = limits.chunk_tokens();
    let mut packed = Packed::default();
    let mut chunks = Vec::new();
    let mut used = 0;
    for block in blocks {
        let mut text = block.text.as_str();
        let mut chars = text.chars().count();
        let mut offset = 0;
        while meter.estimate_chars(chars) > budget {
            let cut = meter.cut_chars(budget)?.min(chars);
            let (piece, rest) = text.split_at(byte_at(text, cut));
            std::mem::take(&mut packed).finish(meter, budget, &mut exact, &mut chunks);
            packed.push(block, piece, offset..offset + cut);
            std::mem::take(&mut packed).finish(meter, budget, &mut exact, &mut chunks);
            text = rest;
            chars -= cut;
            offset += cut;
            used = 0;
        }
        let size = meter.estimate_chars(chars);
        if !packed.fragments.is_empty()
            && (used + size > budget || (block.role == Role::User && used * 5 > budget * 4))
        {
            std::mem::take(&mut packed).finish(meter, budget, &mut exact, &mut chunks);
            used = 0;
        }
        packed.push(block, text, offset..offset + chars);
        used += size;
    }
    packed.finish(meter, budget, &mut exact, &mut chunks);
    let total_tokens = chunks.iter().map(|chunk| chunk.input_token_estimate).sum();
    Ok(TranscriptPlan {
        chunks,
        total_tokens,
    })
}

#[derive(Debug)]
pub(crate) enum ComposePlan {
    Call(Prompt),
    Reduce {
        groups: Box<[Self; 2]>,
        parent: Prompt,
    },
}

pub(crate) fn compose(
    ordered_records: &[impl AsRef<str>],
    total_tokens: usize,
    meter: Meter,
    limits: Limits,
) -> Result<ComposePlan, PlanError> {
    condense(ordered_records, Some(total_tokens), meter, limits)
}

fn condense(
    records: &[impl AsRef<str>],
    total_tokens: Option<usize>,
    meter: Meter,
    limits: Limits,
) -> Result<ComposePlan, PlanError> {
    if records.is_empty() {
        return Err(PlanError::EmptyComposition);
    }
    let count = records.len();
    let mode = total_tokens.map_or(Mode::Carry { count }, |total_tokens| Mode::Final {
        count,
        total_tokens,
    });
    let ceiling = total_tokens.map_or(CARRY_CEILING, prompt::session_ceiling) as usize;
    let user = prompt::join_records(records);
    if meter.estimate(&user).saturating_add(ceiling * 2 + 2_000) < limits.context_tokens() {
        return Ok(ComposePlan::Call(prompt::build(mode, user, limits)));
    }
    if count == 1 {
        return Err(PlanError::ComposeContext);
    }
    let (left, right) = records.split_at(count / 2);
    Ok(ComposePlan::Reduce {
        groups: Box::new([
            condense(left, None, meter, limits)?,
            condense(right, None, meter, limits)?,
        ]),
        parent: prompt::build(mode, String::new(), limits),
    })
}

pub(crate) fn after_reduction(mut parent: Prompt, records: [&str; 2]) -> Prompt {
    parent.user = prompt::join_records(&records);
    parent
}

#[cfg(test)]
mod probes {
    use super::*;

    fn block(ordinal: usize, role: Role, text: &str) -> Block {
        Block {
            source: Source {
                id: Uuid::from_u128(u128::from(u64::try_from(ordinal).expect("ordinal"))),
                ordinal,
                created_at: Timestamp::UNIX_EPOCH,
                render_key: [0; 32],
            },
            text: text.to_owned(),
            role,
        }
    }

    #[test]
    fn preflight_splits_context_overflow_before_collecting_pending_counts() {
        let limits = Limits::new(10_000, 1_000, false).expect("limits");
        let mut pending_lengths = Vec::new();
        let mut exact = |text: &str| {
            let length = text.chars().count();
            if length == 2_500 {
                ExactCount::OverContext
            } else {
                pending_lengths.push(length);
                ExactCount::Pending
            }
        };
        pack(
            &[block(1, Role::Assistant, &"🌿".repeat(2_500))],
            Meter::uncalibrated(),
            limits,
            Some(&mut exact),
        )
        .expect("packed");
        assert_eq!(pending_lengths, [1_250, 1_250]);
    }

    #[test]
    fn zero_count_splits_nonempty_text_at_the_scalar_midpoint() {
        let limits = Limits::new(10_000, 1_000, false).expect("limits");
        let text = "a".repeat(3_000);
        let mut zero = |_: &str| ExactCount::Known(0);
        let plan = pack(
            &[block(1, Role::Assistant, &text)],
            Meter::uncalibrated(),
            limits,
            Some(&mut zero),
        )
        .expect("packed");
        let sizes: Vec<_> = plan
            .chunks
            .iter()
            .map(|chunk| chunk.text.chars().count())
            .collect();
        assert!(
            sizes.iter().all(|size| *size <= 2_000),
            "a zero tokenizer count left over-budget chunks of {sizes:?}"
        );
    }
}
