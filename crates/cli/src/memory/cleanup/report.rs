use jiff::Timestamp;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;

use kernel::cleanup::Automated;
use kernel::cleanup::Fate;
use kernel::cleanup::FiledHow;
use kernel::cleanup::Filing;
use kernel::cleanup::FoldRule;
use kernel::cleanup::Folded;
use kernel::cleanup::Junk;
use kernel::cleanup::JunkReason;
use kernel::cleanup::JunkStage;
use kernel::cleanup::Origin;
use kernel::cleanup::ProposedTree;
use kernel::topics::PlacementSource;
use kernel::topics::TopicId;
use kernel::topics::TopicName;
use serde::Serialize;
use uuid::Uuid;

use super::Stage;
use super::Timings;
use super::filing::Filed;
use super::fold::Folds;
use crate::Error;

pub(super) const REPORT: &str = "report.md";
const REPORT_JSON: &str = "report.json";
const SAMPLE_FILINGS: usize = 60;
const SAMPLE_TITLES: usize = 3;
const SAMPLE_FOLDS: usize = 8;
const SHUFFLE: u128 = 0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835;

#[serde_with::serde_as]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReportData {
    #[serde_as(as = "wire::time::Rfc3339")]
    pub(super) snapshot_at: Timestamp,
    #[serde_as(as = "wire::time::Rfc3339")]
    pub(super) written_at: Timestamp,
    pub(super) seconds: BTreeMap<String, f64>,
    pub(super) conversations: Counts,
    pub(super) tree: Vec<TreeRow>,
    pub(super) merges: Vec<(String, String)>,
    pub(super) lifted: Vec<(String, String)>,
    pub(super) filed_by_rule: BTreeMap<String, usize>,
    pub(super) unfiled: BTreeMap<String, usize>,
    pub(super) nothing_fits: usize,
    pub(super) sample: Vec<SampleRow>,
    pub(super) fold_outcomes: BTreeMap<String, usize>,
    pub(super) fold_rules: BTreeMap<String, usize>,
    pub(super) fold_samples: BTreeMap<String, Vec<FoldRow>>,
    pub(super) sections: Vec<FoldRow>,
    pub(super) junk: Vec<Junk>,
    pub(super) automated: Automated,
    pub(super) generic: Vec<String>,
    pub(super) review: Vec<Junk>,
    pub(super) fragmentation: Fragmentation,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Fragmentation {
    pub(super) topics: usize,
    pub(super) roots: usize,
    pub(super) crowded: usize,
    pub(super) root_direct: usize,
    pub(super) filed: usize,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Counts {
    pub(super) summarized: usize,
    pub(super) never: usize,
    pub(super) automated: usize,
    pub(super) children: usize,
    pub(super) same_session: usize,
    pub(super) filed_one_by_one: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TreeRow {
    pub(super) path: String,
    pub(super) name: String,
    pub(super) depth: usize,
    pub(super) origin: Origin,
    pub(super) sessions: usize,
    pub(super) with_subtopics: usize,
    pub(super) about: Option<String>,
    pub(super) samples: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SampleRow {
    pub(super) title: String,
    pub(super) topic: Option<String>,
    pub(super) rule: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FoldRow {
    pub(super) key: String,
    pub(super) title: String,
    pub(super) outcome: String,
    pub(super) target: Option<String>,
    pub(super) rule: FoldRule,
}

pub(super) async fn write(
    stage: &Stage<'_>,
    filed: &Filed,
    folds: &Folds,
    timings: &Timings,
) -> Result<String, Error> {
    let data = build(stage, filed, folds, timings)?;
    stage.book.write(REPORT_JSON, &data).await?;
    stage
        .book
        .write_text(REPORT, &super::render::markdown(&data))
        .await?;
    Ok(super::render::headline(&data))
}

fn build(
    stage: &Stage<'_>,
    filed: &Filed,
    folds: &Folds,
    timings: &Timings,
) -> Result<ReportData, Error> {
    let titles: HashMap<Uuid, &str> = stage
        .snapshot
        .conversations
        .iter()
        .filter_map(|conversation| {
            conversation
                .title()
                .map(|title| (conversation.head.id, title))
        })
        .collect();
    let tree = &filed.tree;
    let one_by_one: Vec<&Filing> = filed
        .filings
        .iter()
        .filter(|filing| {
            !matches!(
                filing.how,
                FiledHow::Never | FiledHow::SameTarget | FiledHow::Automated
            )
        })
        .collect();
    let (filed_by_rule, unfiled, nothing_fits) = rule_counts(&one_by_one);
    let (fold_outcomes, fold_rules, fold_samples) = fold_summary(tree, &folds.pages);
    Ok(ReportData {
        snapshot_at: stage.snapshot.taken_at,
        written_at: Timestamp::now(),
        seconds: timings.seconds.clone(),
        conversations: Counts {
            summarized: stage.snapshot.conversations.len(),
            never: count_how(filed, |how| matches!(how, FiledHow::Never)),
            automated: count_how(filed, |how| matches!(how, FiledHow::Automated)),
            children: count_how(filed, |how| matches!(how, FiledHow::Inherits)),
            same_session: count_how(filed, |how| matches!(how, FiledHow::SameTarget)),
            filed_one_by_one: one_by_one.len(),
        },
        tree: tree_rows(tree, &one_by_one, &titles),
        merges: tree
            .merges
            .iter()
            .map(|merge| (merge.name.clone(), tree.path(merge.into)))
            .collect(),
        lifted: filed
            .lifts
            .iter()
            .map(|lift| {
                let to = lift
                    .to
                    .map_or_else(|| "(no topic)".to_owned(), |to| tree.path(to));
                (lift.name.clone(), to)
            })
            .collect(),
        filed_by_rule,
        unfiled,
        nothing_fits,
        sample: sample(tree, &one_by_one, &titles),
        fold_outcomes,
        fold_rules,
        fold_samples,
        sections: folds
            .sections
            .iter()
            .map(|folded| fold_row(tree, folded))
            .collect(),
        junk: junk(stage, filed, folds)?,
        automated: stage.automated.clone(),
        generic: filed.generic.iter().cloned().collect(),
        review: tree
            .junk
            .iter()
            .filter(|junk| junk.stage == JunkStage::Review)
            .cloned()
            .collect(),
        fragmentation: fragmentation(stage, tree, &one_by_one),
    })
}

type RuleCounts = (BTreeMap<String, usize>, BTreeMap<String, usize>, usize);

fn rule_counts(filings: &[&Filing]) -> RuleCounts {
    let mut filed_by_rule = BTreeMap::new();
    let mut unfiled = BTreeMap::new();
    for filing in filings {
        let bucket = if filing.topic.is_some() {
            &mut filed_by_rule
        } else {
            &mut unfiled
        };
        let count: &mut usize = bucket.entry(rule_name(filing)).or_default();
        *count = count.saturating_add(1);
    }
    let nothing_fits = filings
        .iter()
        .filter(|filing| filing.topic.is_none())
        .filter(|filing| {
            matches!(
                filing.how,
                FiledHow::Nothing
                    | FiledHow::Suggested { .. }
                    | FiledHow::Refused { .. }
                    | FiledHow::Rule { .. }
            )
        })
        .count();
    (filed_by_rule, unfiled, nothing_fits)
}

fn sample(
    tree: &ProposedTree,
    filings: &[&Filing],
    titles: &HashMap<Uuid, &str>,
) -> Vec<SampleRow> {
    let mut shuffled = filings.to_vec();
    shuffled.sort_by_key(|filing| filing.conversation.as_u128().wrapping_mul(SHUFFLE));
    shuffled
        .iter()
        .take(SAMPLE_FILINGS)
        .map(|filing| SampleRow {
            title: title_of(titles, filing.conversation),
            topic: filing.topic.map(|topic| tree.path(topic)),
            rule: rule_label(filing),
        })
        .collect()
}

fn tree_rows(
    tree: &ProposedTree,
    filings: &[&Filing],
    titles: &HashMap<Uuid, &str>,
) -> Vec<TreeRow> {
    let mut members: BTreeMap<TopicId, Vec<Uuid>> = BTreeMap::new();
    for filing in filings {
        if let Some(topic) = filing.topic {
            members.entry(topic).or_default().push(filing.conversation);
        }
    }
    let mut rows: Vec<TreeRow> = tree
        .topics
        .iter()
        .map(|topic| {
            let own = members.get(&topic.id).map_or(0, Vec::len);
            let with_subtopics = tree
                .topics
                .iter()
                .filter(|other| tree.within(other.id, topic.id))
                .map(|other| members.get(&other.id).map_or(0, Vec::len))
                .sum();
            let samples = members
                .get(&topic.id)
                .map(|ids| samples(ids, titles))
                .unwrap_or_default();
            TreeRow {
                path: tree.path(topic.id),
                name: topic.name.clone(),
                depth: tree.path(topic.id).matches(" › ").count(),
                origin: topic.origin,
                sessions: own,
                with_subtopics,
                about: topic.about.clone(),
                samples,
            }
        })
        .collect();
    rows.sort_by_key(|row| row.path.to_lowercase());
    rows
}

fn samples(ids: &[Uuid], titles: &HashMap<Uuid, &str>) -> Vec<String> {
    let step = (ids.len() / SAMPLE_TITLES).max(1);
    let spread = ids.iter().step_by(step).chain(ids.iter());
    let mut picked: Vec<String> = Vec::new();
    for title in spread.map(|id| title_of(titles, *id)) {
        if picked.len() == SAMPLE_TITLES {
            break;
        }
        if !picked.contains(&title) {
            picked.push(title);
        }
    }
    picked
}

type FoldSummary = (
    BTreeMap<String, usize>,
    BTreeMap<String, usize>,
    BTreeMap<String, Vec<FoldRow>>,
);

fn fold_summary(tree: &ProposedTree, pages: &[Folded]) -> FoldSummary {
    let mut outcomes: BTreeMap<String, usize> = BTreeMap::new();
    let mut rules: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples: BTreeMap<String, Vec<FoldRow>> = BTreeMap::new();
    let mut shuffled: Vec<&Folded> = pages.iter().collect();
    shuffled.sort_by_key(|folded| {
        folded.key.bytes().fold(0_u128, |hash, byte| {
            hash.wrapping_mul(SHUFFLE).wrapping_add(u128::from(byte))
        })
    });
    for folded in shuffled {
        let row = fold_row(tree, folded);
        let count: &mut usize = outcomes.entry(row.outcome.clone()).or_default();
        *count = count.saturating_add(1);
        let count: &mut usize = rules.entry(folded.rule.to_string()).or_default();
        *count = count.saturating_add(1);
        let kept = samples.entry(row.outcome.clone()).or_default();
        if kept.len() < SAMPLE_FOLDS {
            kept.push(row);
        }
    }
    (outcomes, rules, samples)
}

fn fold_row(tree: &ProposedTree, folded: &Folded) -> FoldRow {
    let (outcome, target) = match &folded.fate {
        Fate::Entity { kind } => ("entity", Some(kind.path())),
        Fate::Topic { name, under } => (
            "topic",
            Some(match under {
                Some(under) => format!("{} › {name}", tree.path(*under)),
                None => name.clone(),
            }),
        ),
        Fate::Section { topic } => ("section", Some(tree.path(*topic))),
        Fate::Merge { into } => ("merge", Some(into.clone())),
        Fate::Remove => ("remove", None),
        Fate::Keep => ("keep", None),
        Fate::Undecided => ("undecided", None),
    };
    FoldRow {
        key: folded.key.clone(),
        title: folded.title.clone(),
        outcome: outcome.to_owned(),
        target,
        rule: folded.rule,
    }
}

fn fragmentation(stage: &Stage<'_>, tree: &ProposedTree, filings: &[&Filing]) -> Fragmentation {
    let crowded = tree
        .clusters(stage.guard)
        .into_values()
        .flatten()
        .collect::<BTreeSet<TopicId>>()
        .len();
    let filed: Vec<TopicId> = filings.iter().filter_map(|filing| filing.topic).collect();
    Fragmentation {
        topics: tree.topics.len(),
        roots: tree
            .topics
            .iter()
            .filter(|topic| topic.parent.is_none())
            .count(),
        crowded,
        root_direct: filed
            .iter()
            .filter(|topic| {
                tree.topic(**topic)
                    .is_some_and(|topic| topic.parent.is_none())
            })
            .count(),
        filed: filed.len(),
    }
}

fn junk(stage: &Stage<'_>, filed: &Filed, folds: &Folds) -> Result<Vec<Junk>, Error> {
    let mut junk: Vec<Junk> = filed
        .tree
        .junk
        .iter()
        .filter(|junk| junk.stage != JunkStage::Review)
        .cloned()
        .collect();
    for filing in &filed.filings {
        match &filing.how {
            FiledHow::Suggested { name } => {
                if let Some(reason) = stage.guard.junk(&TopicName::new(name)?) {
                    junk.push(filing_junk(name, reason));
                }
            }
            FiledHow::Refused { name, refusal } => {
                junk.push(filing_junk(name, JunkReason::Refused { refusal: *refusal }));
            }
            FiledHow::Never
            | FiledHow::SameTarget
            | FiledHow::Placed { .. }
            | FiledHow::Corrected
            | FiledHow::Inherits
            | FiledHow::Rule { .. }
            | FiledHow::Nothing
            | FiledHow::Unanswered
            | FiledHow::Automated => {}
        }
    }
    junk.extend(folds.junk.iter().cloned());
    junk.sort_by_key(|junk| junk.name.to_lowercase());
    junk.dedup_by(|left, right| {
        left.name.eq_ignore_ascii_case(&right.name) && left.stage == right.stage
    });
    Ok(junk)
}

fn filing_junk(name: &str, reason: JunkReason) -> Junk {
    Junk {
        name: name.to_owned(),
        stage: JunkStage::Filing,
        reason,
    }
}

fn count_how(filed: &Filed, wanted: impl Fn(&FiledHow) -> bool) -> usize {
    filed
        .filings
        .iter()
        .filter(|filing| wanted(&filing.how))
        .count()
}

fn rule_name(filing: &Filing) -> String {
    let name = match &filing.how {
        FiledHow::Never => "never filed".to_owned(),
        FiledHow::SameTarget => "same session".to_owned(),
        FiledHow::Placed {
            by: PlacementSource::You,
        } => "your placement".to_owned(),
        FiledHow::Placed {
            by: PlacementSource::Categorizer,
        } => "categorizer placement".to_owned(),
        FiledHow::Corrected => "your correction".to_owned(),
        FiledHow::Inherits => "inherits its parent".to_owned(),
        FiledHow::Rule { rule } => rule.to_string(),
        FiledHow::Suggested { .. } => "suggested a new topic".to_owned(),
        FiledHow::Refused { .. } => "suggestion refused".to_owned(),
        FiledHow::Nothing => "nothing fits".to_owned(),
        FiledHow::Unanswered => "no usable answer".to_owned(),
        FiledHow::Automated => "automated run".to_owned(),
    };
    match &filing.lifted {
        Some(_) if filing.topic.is_some() => format!("{name}, lifted to parent"),
        Some(_) => format!("{name}, topic dropped"),
        None => name,
    }
}

fn rule_label(filing: &Filing) -> String {
    let detail = match &filing.how {
        FiledHow::Suggested { name } => Some(format!("\"{name}\"")),
        FiledHow::Refused { name, refusal } => Some(format!("\"{name}\" ({refusal})")),
        FiledHow::Never
        | FiledHow::SameTarget
        | FiledHow::Placed { .. }
        | FiledHow::Corrected
        | FiledHow::Inherits
        | FiledHow::Rule { .. }
        | FiledHow::Nothing
        | FiledHow::Unanswered
        | FiledHow::Automated => filing.lifted.as_ref().map(|from| format!("from {from}")),
    };
    match detail {
        Some(detail) => format!("{}: {detail}", rule_name(filing)),
        None => rule_name(filing),
    }
}

fn title_of(titles: &HashMap<Uuid, &str>, conversation: Uuid) -> String {
    titles
        .get(&conversation)
        .map_or_else(|| "(untitled)".to_owned(), |title| (*title).to_owned())
}
