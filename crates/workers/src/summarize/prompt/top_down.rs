const SESSION: &str = r"You write the memory record of one working session between a person and an AI agent.

Other agents will read this record later, without the session, to find out what happened and to continue the work.

Write what was asked, what was done, what was found, what was decided and why, and what was left open.
Keep exact names and identifiers: hosts, addresses, ports, paths, commands, versions, model names, branch and pull request numbers, error messages, and numbers with their units.
Say who asked for or decided something when it matters.
Give the state each thing ended in. When something was tried and then reversed or corrected, say so.
Separate what was done from what was only proposed or planned.
Keep dates when the transcript gives them.
State only what the transcript shows.
Do not describe the transcript itself: no message counts, no tallies of tool calls, no remarks about the conversation.

Open with one paragraph that says what the session was about and how it ended. Then write the rest as sections, one section for each topic or thread of work the session actually contains. Use as few sections as the session needs, and keep material about one thing in one section. A reader will later look up one section without the rest of the record, so the section itself carries what that topic decided, what it found, and what it left open.

Each section starts with one heading line of this form: ## <title> [m00012-m00087]. The title is a short name of the topic, written in place of the placeholder. The two tags are the first and last message tags the section covers, written in place of the sample tags. Those tags are the labels on the transcript message headers. The range is how a reader finds the messages for that topic, and it belongs in the section. Threads that interleave may have overlapping ranges.
Write about {target_words} words. A session this size needs that much to keep its decisions, exact values and open work. Do not pad, and never go over {words} words.";

const PART: &str = r"You write the memory record of one working session between a person and an AI agent. The session is too long to read at once, so it is read in parts. You are given the transcript of part {index}{last}.

Write the record of this part only.

Other agents will read this record later, without the session, to find out what happened and to continue the work.

Write what was asked, what was done, what was found, what was decided and why, and what was left open.
Keep exact names and identifiers: hosts, addresses, ports, paths, commands, versions, model names, branch and pull request numbers, error messages, and numbers with their units.
Say who asked for or decided something when it matters.
Give the state each thing ended in. When something was tried and then reversed or corrected, say so.
Separate what was done from what was only proposed or planned.
Keep dates when the transcript gives them.
State only what the transcript shows.
Do not describe the transcript itself: no message counts, no tallies of tool calls, no remarks about the conversation.

Write this part as sections, one section for each topic or thread of work the part actually contains. Use as few sections as the part needs, and keep material about one thing in one section. A reader will later look up one section without the rest, so the section itself carries what that topic decided, what it found, and what it left open.

Each section starts with one heading line of this form: ## <title> [m00012-m00087]. The title is a short name of the topic, written in place of the placeholder. The two tags are the first and last message tags the section covers, written in place of the sample tags. Those tags are the labels on the transcript message headers in this part. The range is how a reader finds the messages for that topic, and it belongs in the section. Threads that interleave may have overlapping ranges. A heading that only introduces an earlier part is not a section heading.
Write about {target_words} words, and never more than {words}.";

const CARRY: &str = r"You are given the records of the first {count} parts of one long working session between a person and an AI agent. Write one condensed record that the reader of the next part will use as background.

Other agents will read this record later, without the session, to find out what happened and to continue the work.

Write what was asked, what was done, what was found, what was decided and why, and what was left open.
Keep exact names and identifiers: hosts, addresses, ports, paths, commands, versions, model names, branch and pull request numbers, error messages, and numbers with their units.
Say who asked for or decided something when it matters.
Give the state each thing ended in. When something was tried and then reversed or corrected, say so.
Separate what was done from what was only proposed or planned.
Keep dates when the transcript gives them.
State only what the transcript shows.
Do not describe the transcript itself: no message counts, no tallies of tool calls, no remarks about the conversation.

Keep the states things ended in, the decisions, the names and identifiers, and the open work. Drop step-by-step detail. Write at most {words} words.";

const FINAL: &str = r"You are given the records of all {count} parts of one working session between a person and an AI agent, in order. Write the single memory record of the whole session.

Other agents will read this record later, without the session, to find out what happened and to continue the work.

Write what was asked, what was done, what was found, what was decided and why, and what was left open.
Keep exact names and identifiers: hosts, addresses, ports, paths, commands, versions, model names, branch and pull request numbers, error messages, and numbers with their units.
Say who asked for or decided something when it matters.
Give the state each thing ended in. When something was tried and then reversed or corrected, say so.
Separate what was done from what was only proposed or planned.
Keep dates when the transcript gives them.
State only what the transcript shows.
Do not describe the transcript itself: no message counts, no tallies of tool calls, no remarks about the conversation.

Later parts override earlier ones where they disagree. Open with one paragraph that says what the session was about and how it ended. Then write the rest as sections, one section for each topic or thread of work the session actually contains. Use as few sections as the session needs, and keep material about one thing in one section. Where the same thread continues from one part into another, merge it into one section whose range runs from the earliest message tag in that thread to the latest. A reader will later look up one section without the rest of the record, so the section itself carries what that topic decided, what it found, and what it left open.

Each section starts with one heading line of this form: ## <title> [m00012-m00087]. The title is a short name of the topic, written in place of the placeholder. The two tags are the first and last message tags the section covers, written in place of the sample tags. Those tags are the labels on the transcript message headers. The range is how a reader finds the messages for that topic, and it belongs in the section. Threads that interleave may have overlapping ranges. A heading that only introduces a part is not a section heading.
Write about {target_words} words. A session this size needs that much to keep its decisions, exact values and open work. Do not pad, and never go over {words} words.";

use std::fmt::Write as _;

use super::settings::{
    CARRY_CEILING, Limits, PART_CEILING, SESSION_CEILING, TARGET_DIVISOR, request_max_tokens,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Prompt {
    pub(crate) system: String,
    pub(crate) user: String,
    pub(crate) editorial_ceiling: u32,
    pub(crate) target_words: u32,
    pub(crate) max_words: u32,
    pub(crate) max_tokens: u32,
    pub(crate) thinking: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Mode {
    Session { total_tokens: usize },
    Part { issued: usize, last: bool },
    Carry { count: usize },
    Final { count: usize, total_tokens: usize },
}

#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "the measured templates use Python placeholders"
)]
pub(crate) fn build(mode: Mode, input: String, limits: Limits) -> Prompt {
    let (system, user, ceiling, target) = match mode {
        Mode::Session { total_tokens } => (
            SESSION.to_owned(),
            input,
            session_ceiling(total_tokens),
            session_target(total_tokens),
        ),
        Mode::Part { issued, last } => (
            PART.replace("{index}", &issued.to_string())
                .replace("{last}", if last { ", the last part" } else { "" }),
            format!("# Transcript of part {issued}\n\n{input}"),
            PART_CEILING,
            words(PART_CEILING),
        ),
        Mode::Carry { count } => (
            CARRY.replace("{count}", &count.to_string()),
            input,
            CARRY_CEILING,
            words(CARRY_CEILING),
        ),
        Mode::Final {
            count,
            total_tokens,
        } => (
            FINAL.replace("{count}", &count.to_string()),
            input,
            session_ceiling(total_tokens),
            session_target(total_tokens),
        ),
    };
    let max_words = words(ceiling);
    Prompt {
        system: system
            .replace("{target_words}", &target.to_string())
            .replace("{words}", &max_words.to_string()),
        user,
        editorial_ceiling: ceiling,
        target_words: target,
        max_words,
        max_tokens: request_max_tokens(ceiling, limits.thinking()),
        thinking: limits.thinking(),
    }
}

pub(crate) fn join_records(records: &[impl AsRef<str>]) -> String {
    let mut joined = String::new();
    for (index, record) in records.iter().enumerate() {
        if index > 0 {
            joined.push_str("\n\n");
        }
        let _infallible = write!(
            joined,
            "## Record of part {}\n\n{}",
            index + 1,
            record.as_ref()
        );
    }
    joined
}

pub(crate) fn session_ceiling(total: usize) -> u32 {
    cap_session_tokens((total / 4).max(300))
}

fn session_target(total: usize) -> u32 {
    words(cap_session_tokens((total / TARGET_DIVISOR).max(400)))
}

fn cap_session_tokens(tokens: usize) -> u32 {
    if let Ok(tokens) = u32::try_from(tokens) {
        tokens.min(SESSION_CEILING)
    } else {
        SESSION_CEILING
    }
}

const fn words(tokens: u32) -> u32 {
    tokens * 3 / 4
}
