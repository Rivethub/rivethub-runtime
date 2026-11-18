use kernel::cleanup::JunkReason;

use super::report::FoldRow;
use super::report::ReportData;

const NEAR_TENTHS: usize = 6;

pub(super) fn headline(data: &ReportData) -> String {
    let filed: usize = data.filed_by_rule.values().sum();
    let rules: Vec<String> = data
        .filed_by_rule
        .iter()
        .map(|(rule, count)| format!("{rule} {count}"))
        .collect();
    let outcomes: Vec<String> = data
        .fold_outcomes
        .iter()
        .map(|(outcome, count)| format!("{outcome} {count}"))
        .collect();
    format!(
        "tree: {} topics; filed {filed} ({}); nothing fits {}; fold: {}; junk: {}; took {}",
        data.tree.len(),
        rules.join(", "),
        data.nothing_fits,
        outcomes.join(", "),
        data.junk.len(),
        duration(data.seconds.values().sum())
    )
}

pub(super) fn markdown(data: &ReportData) -> String {
    let mut out = String::new();
    let stages: Vec<String> = data
        .seconds
        .iter()
        .map(|(stage, seconds)| format!("{stage} {}", duration(*seconds)))
        .collect();
    push(&mut out, "# Topic cleanup dry run\n");
    push(
        &mut out,
        &format!(
            "Snapshot taken {}, report written {}. The run took {} ({}). Nothing in memory was changed.\n",
            data.snapshot_at.strftime("%Y-%m-%d %H:%M UTC"),
            data.written_at.strftime("%Y-%m-%d %H:%M UTC"),
            duration(data.seconds.values().sum()),
            stages.join(", ")
        ),
    );
    push(&mut out, &format!("**Headline:** {}\n", headline(data)));
    conversations(&mut out, data);
    fragmentation(&mut out, data);
    automated(&mut out, data);
    tree(&mut out, data);
    review(&mut out, data);
    filings(&mut out, data);
    folds(&mut out, data);
    junk(&mut out, data);
    out
}

fn conversations(out: &mut String, data: &ReportData) {
    let counts = &data.conversations;
    push(out, "## Conversations\n");
    push(out, "| | conversations |\n|---|---:|");
    push(out, &format!("| summarized | {} |", counts.summarized));
    push(
        out,
        &format!(
            "| never filed (sandbox runs, test channels, heartbeats) | {} |",
            counts.never
        ),
    );
    push(
        out,
        &format!(
            "| automated runs left out (scheduled or templated, single-turn, naming no project) | {} |",
            counts.automated
        ),
    );
    push(
        out,
        &format!(
            "| child sessions following their parent | {} |",
            counts.children
        ),
    );
    push(
        out,
        &format!(
            "| same session as another conversation | {} |",
            counts.same_session
        ),
    );
    push(
        out,
        &format!("| filed one by one | {} |\n", counts.filed_one_by_one),
    );
}

fn fragmentation(out: &mut String, data: &ReportData) {
    let shape = &data.fragmentation;
    push(out, "## Tree shape\n");
    push(out, "| | |\n|---|---:|");
    push(out, &format!("| topics | {} |", shape.topics));
    push(out, &format!("| top-level topics | {} |", shape.roots));
    push(
        out,
        &format!(
            "| topics sharing a distinctive word with an unrelated topic | {} |",
            shape.crowded
        ),
    );
    push(
        out,
        &format!(
            "| filings left at a top-level topic | {} of {} |\n",
            shape.root_direct, shape.filed
        ),
    );
}

fn review(out: &mut String, data: &ReportData) {
    push(
        out,
        &format!("## Tree review ({} changes)\n", data.review.len()),
    );
    for change in &data.review {
        let what = match &change.reason {
            JunkReason::Merged { into } => format!("merged into {into}"),
            JunkReason::Regrouped { under } => format!("moved under {under}"),
            JunkReason::Renamed { to } => format!("renamed to {to}"),
            JunkReason::Declined { change } => format!("declined: {change}"),
            JunkReason::Generic
            | JunkReason::Rejected
            | JunkReason::Title
            | JunkReason::Task
            | JunkReason::Activity
            | JunkReason::Vague
            | JunkReason::Refused { .. }
            | JunkReason::Duplicate { .. }
            | JunkReason::TooDeep
            | JunkReason::TooFew { .. } => continue,
        };
        push(out, &format!("- {}: {}", cell(&change.name), cell(&what)));
    }
    push(out, "");
}

fn automated(out: &mut String, data: &ReportData) {
    push(out, "## Automated runs left out\n");
    push(
        out,
        &format!(
            "{} task runs started by a heartbeat or an eval. Opening prompts shared by at least {} conversations, mostly single-turn and naming no project:\n",
            data.automated.by_task,
            kernel::cleanup::TEMPLATE_RUNS
        ),
    );
    push(out, "| opening prompt | runs | left out |\n|---|---:|---:|");
    for template in &data.automated.templates {
        push(
            out,
            &format!(
                "| {} | {} | {} |",
                cell(&template.opening),
                template.runs,
                template.left_out
            ),
        );
    }
    push(
        out,
        &format!(
            "\nNames and aliases too common to decide a filing by name (in more than {}% of conversations): {}\n",
            kernel::topics::GENERIC_SHARE_PERCENT,
            data.generic.join(", ")
        ),
    );
}

fn tree(out: &mut String, data: &ReportData) {
    push(
        out,
        &format!("## Proposed tree ({} topics)\n", data.tree.len()),
    );
    for row in &data.tree {
        let indent = "  ".repeat(row.depth);
        let origin = row.origin;
        let below = if row.with_subtopics == row.sessions {
            String::new()
        } else {
            format!(", {} with subtopics", row.with_subtopics)
        };
        let about = row
            .about
            .as_deref()
            .map(|about| format!(" — {}", cell(about)))
            .unwrap_or_default();
        push(
            out,
            &format!(
                "{indent}- **{}** ({origin}) — {} sessions{below}{about}",
                cell(&row.name),
                row.sessions
            ),
        );
        if !row.samples.is_empty() {
            let samples: Vec<String> = row
                .samples
                .iter()
                .map(|title| format!("“{}”", cell(title)))
                .collect();
            push(out, &format!("{indent}  - e.g. {}", samples.join("; ")));
        }
    }
    push(out, "");
    if !data.merges.is_empty() {
        push(out, "Existing topics merged:\n");
        for (from, into) in &data.merges {
            push(out, &format!("- {} → {}", cell(from), cell(into)));
        }
        push(out, "");
    }
    if !data.lifted.is_empty() {
        push(
            out,
            "Dropped after filing for fewer than 3 sessions (their filings moved up):\n",
        );
        for (name, to) in &data.lifted {
            push(out, &format!("- {} → {}", cell(name), cell(to)));
        }
        push(out, "");
    }
    let near = near_pairs(data);
    if !near.is_empty() {
        push(
            out,
            "Pairs of tree topics whose names overlap (check for duplicates):\n",
        );
        for (left, right) in near {
            push(out, &format!("- {} ~ {}", cell(&left), cell(&right)));
        }
        push(out, "");
    }
}

fn filings(out: &mut String, data: &ReportData) {
    push(out, "## Filings by rule\n");
    push(out, "| rule | filed |\n|---|---:|");
    for (rule, count) in &data.filed_by_rule {
        push(out, &format!("| {rule} | {count} |"));
    }
    push(out, &format!("\n**Nothing fits: {}**\n", data.nothing_fits));
    push(out, "| left unfiled | conversations |\n|---|---:|");
    for (rule, count) in &data.unfiled {
        push(out, &format!("| {rule} | {count} |"));
    }
    push(
        out,
        &format!("\n### Random sample of {} filings\n", data.sample.len()),
    );
    push(out, "| title | topic | rule |\n|---|---|---|");
    for row in &data.sample {
        push(
            out,
            &format!(
                "| {} | {} | {} |",
                cell(&row.title),
                cell(row.topic.as_deref().unwrap_or("—")),
                cell(&row.rule)
            ),
        );
    }
    push(out, "");
}

fn folds(out: &mut String, data: &ReportData) {
    let total: usize = data.fold_outcomes.values().sum();
    push(out, &format!("## Fold of {total} knowledge pages\n"));
    push(out, "| outcome | pages |\n|---|---:|");
    for (outcome, count) in &data.fold_outcomes {
        push(out, &format!("| {outcome} | {count} |"));
    }
    push(out, "\n| decided by | pages |\n|---|---:|");
    for (rule, count) in &data.fold_rules {
        push(out, &format!("| {rule} | {count} |"));
    }
    push(out, "");
    for (outcome, rows) in &data.fold_samples {
        push(out, &format!("### Sample: {outcome}\n"));
        fold_table(out, rows);
    }
    push(
        out,
        &format!("## Old entity sections ({})\n", data.sections.len()),
    );
    fold_table(out, &data.sections);
}

fn fold_table(out: &mut String, rows: &[FoldRow]) {
    push(
        out,
        "| page | outcome | target | decided by |\n|---|---|---|---|",
    );
    for row in rows {
        push(
            out,
            &format!(
                "| {} | {} | {} | {} |",
                cell(&row.title),
                row.outcome,
                cell(row.target.as_deref().unwrap_or("—")),
                row.rule
            ),
        );
    }
    push(out, "");
}

fn junk(out: &mut String, data: &ReportData) {
    push(out, &format!("## Junk names ({})\n", data.junk.len()));
    push(
        out,
        "Every proposed name that the guardrails refused or that looks like a title, path, task or duplicate.\n",
    );
    push(out, "| name | stage | why |\n|---|---|---|");
    for junk in &data.junk {
        let why = match &junk.reason {
            JunkReason::Generic => "catch-all name".to_owned(),
            JunkReason::Rejected => "rejected or deleted before".to_owned(),
            JunkReason::Title => "a conversation title".to_owned(),
            JunkReason::Task => "a one-off task".to_owned(),
            JunkReason::Activity => "an activity, not an area".to_owned(),
            JunkReason::Vague => "a vague name".to_owned(),
            JunkReason::Refused { refusal } => format!("looks like a {refusal}"),
            JunkReason::Duplicate { of } => format!("duplicate of {of}"),
            JunkReason::TooDeep => "deeper than 4 levels".to_owned(),
            JunkReason::TooFew { sessions } => format!("too few sessions ({sessions})"),
            JunkReason::Merged { into } => format!("merged into {into} by the review"),
            JunkReason::Regrouped { under } => format!("moved under {under} by the review"),
            JunkReason::Renamed { to } => format!("renamed to {to} by the review"),
            JunkReason::Declined { change } => format!("review change declined: {change}"),
        };
        push(
            out,
            &format!("| {} | {} | {} |", cell(&junk.name), junk.stage, cell(&why)),
        );
    }
    push(out, "");
}

fn near_pairs(data: &ReportData) -> Vec<(String, String)> {
    let words: Vec<(&str, Vec<String>)> = data
        .tree
        .iter()
        .map(|row| (row.path.as_str(), name_words(&row.name)))
        .collect();
    let mut pairs = Vec::new();
    for (index, (left, left_words)) in words.iter().enumerate() {
        for (right, right_words) in words.iter().skip(index + 1) {
            let shared = left_words
                .iter()
                .filter(|word| right_words.contains(word))
                .count();
            let longest = left_words.len().max(right_words.len());
            if shared > 0 && shared.saturating_mul(10) >= longest.saturating_mul(NEAR_TENTHS) {
                pairs.push(((*left).to_owned(), (*right).to_owned()));
            }
        }
    }
    pairs
}

fn name_words(name: &str) -> Vec<String> {
    name.split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| word.chars().count() > 2)
        .map(str::to_lowercase)
        .collect()
}

fn duration(seconds: f64) -> String {
    let whole = seconds.round();
    let minutes = (whole / 60.0).floor();
    let hours = (minutes / 60.0).floor();
    if hours >= 1.0 {
        format!("{hours:.0} h {:.0} min", hours.mul_add(-60.0, minutes))
    } else if minutes >= 1.0 {
        format!("{minutes:.0} min {:.0} s", minutes.mul_add(-60.0, whole))
    } else {
        format!("{whole:.0} s")
    }
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn push(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}
