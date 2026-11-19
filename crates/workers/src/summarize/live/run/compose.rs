use super::*;

type ComposeFuture<'a> =
    Pin<Box<dyn Future<Output = Result<([u8; 32], Reply), WorkerError>> + Send + 'a>>;

impl Run<'_> {
    pub(super) fn compose<'a>(
        &'a self,
        plan: ComposePlan,
        current: &'a [Source],
        parts: &'a [&'a PreparedSummaryRecord],
    ) -> ComposeFuture<'a> {
        Box::pin(async move {
            let members: BTreeSet<_> = parts
                .iter()
                .flat_map(|part| part.sources.iter().map(|entry| entry.message))
                .collect();
            let sources: Vec<_> = current
                .iter()
                .filter(|source| members.contains(&source.id))
                .cloned()
                .collect();
            match plan {
                ComposePlan::Call(prompt) => self.call(prompt, &sources).await,
                ComposePlan::Reduce { groups, parent } => {
                    let [left, right] = *groups;
                    let (before, after) = parts.split_at(parts.len() / 2);
                    let (_, left) = self.compose(left, current, before).await?;
                    let (_, right) = self.compose(right, current, after).await?;
                    let (key, mut reply) = self
                        .call(
                            plan::after_reduction(parent, [&left.text, &right.text]),
                            &sources,
                        )
                        .await?;
                    if left.quality_flags != 0 || right.quality_flags != 0 {
                        reply.quality_flags |= INPUT_WARNING;
                    }
                    Ok((key, reply))
                }
            }
        })
    }
}

pub(super) fn compose_input<'a>(
    records: &[String],
    parts: &'a [PreparedSummaryRecord],
    total_tokens: usize,
    meter: Meter,
    limits: Limits,
) -> Result<(ComposePlan, Vec<&'a PreparedSummaryRecord>), WorkerError> {
    match plan::compose(records, total_tokens, meter, limits) {
        Ok(plan) => return Ok((plan, parts.iter().collect())),
        Err(PlanError::ComposeContext) => {}
        Err(error) => return Err(settings_error(error)),
    }
    let budget = limits
        .context_tokens()
        .saturating_sub(2 * settings::CARRY_CEILING as usize + 2_032);
    let mut split = Vec::new();
    let mut owners = Vec::new();
    for (text, part) in records.iter().zip(parts) {
        let mut remaining = vec![text.as_str()];
        while let Some(piece) = remaining.pop() {
            if meter.estimate(piece) < budget {
                split.push(piece);
                owners.push(part);
                continue;
            }
            let cut =
                composition_cut(piece).ok_or_else(|| settings_error(PlanError::ComposeContext))?;
            let (left, right) = piece.split_at(cut);
            remaining.push(right);
            remaining.push(left);
        }
    }
    let plan = plan::compose(&split, total_tokens, meter, limits).map_err(settings_error)?;
    Ok((plan, owners))
}

fn composition_cut(piece: &str) -> Option<usize> {
    let chars = piece.chars().count();
    let byte_at = |index| piece.char_indices().nth(index).map(|(byte, _)| byte);
    let middle = byte_at(chars / 2).filter(|&byte| byte > 0)?;
    let lower = byte_at(chars / 4)?;
    let upper = byte_at(chars - chars / 4).unwrap_or(piece.len());
    let nearest = piece
        .match_indices("\n\n")
        .map(|(byte, _)| byte + 2)
        .chain(piece.match_indices('\n').filter_map(|(byte, _)| {
            let start = byte + 1;
            is_heading(&piece[start..]).then_some(start)
        }))
        .filter(|&byte| byte >= lower && byte <= upper && byte > 0 && byte < piece.len())
        .min_by_key(|&byte| byte.abs_diff(middle));
    if nearest.is_some() {
        return nearest;
    }
    let start = piece[..middle].rfind('\n').map_or(0, |byte| byte + 1);
    if is_heading(&piece[start..]) {
        return piece[middle..]
            .find('\n')
            .map(|byte| byte + middle + 1)
            .filter(|&byte| byte < piece.len())
            .or_else(|| (start > 0).then_some(start));
    }
    Some(middle)
}

fn is_heading(line: &str) -> bool {
    let trimmed = line.trim_start_matches(' ');
    let title = trimmed.trim_start_matches('#');
    (1..=6).contains(&(trimmed.len() - title.len())) && title.starts_with(' ')
}

pub(super) fn rebase(
    part: &PreparedSummaryRecord,
    current: &BTreeMap<Uuid, &Source>,
    retained: bool,
) -> Result<String, WorkerError> {
    let times = current
        .iter()
        .map(|(&id, source)| (id, source.created_at))
        .collect();
    let sources = owner_sources(part, &times)?;
    let sections = llm::sections(&part.content, &sources);
    let mut result = String::new();
    for section in sections {
        use std::fmt::Write as _;
        let text: String = part
            .content
            .chars()
            .skip(section.chars.start)
            .take(section.chars.len())
            .collect();
        let Some((heading, body)) = text
            .split_once('\n')
            .or_else(|| text.starts_with("## ").then_some((text.as_str(), "")))
        else {
            result.push_str(&text);
            continue;
        };
        if !heading.starts_with("## ") {
            result.push_str(&text);
            continue;
        }
        result.push_str(rebased_title(heading, retained));
        if section.range_verified {
            let mapped = |ordinal| {
                sources
                    .iter()
                    .find(|source| source.ordinal == ordinal)
                    .and_then(|source| current.get(&source.id))
                    .map(|source| source.ordinal)
            };
            if let (Some(first), Some(last)) =
                (mapped(section.source_first), mapped(section.source_last))
                && first <= last
            {
                write!(result, " [{}-{}]", plan::tag(first), plan::tag(last))
                    .map_err(settings_error)?;
            }
        }
        if text.contains('\n') {
            result.push('\n');
            result.push_str(body);
        }
    }
    Ok(result)
}

fn rebased_title(heading: &str, retained: bool) -> &str {
    let Some(raw) = heading.strip_prefix("## ").filter(|_| retained) else {
        return heading
            .rsplit_once(" [m")
            .map_or(heading, |(title, _)| title);
    };
    let (title, range) = llm::heading_range(raw);
    if range.is_some() {
        &heading[..3 + title.len()]
    } else {
        heading
    }
}
