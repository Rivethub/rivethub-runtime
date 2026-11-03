#![cfg(test)]

use chrono::{DateTime, NaiveDate, Utc};
use kernel::wiki::page::archive::{
    ArchivePart, SEGMENT_BUDGET, next_part, settle_archive, unarchived,
};
use kernel::wiki::page::bound::{
    ARCHIVED_TITLE, ARTICLE_BUDGET, Archived, HISTORY_LINE, SECTION_BUDGET, SECTIONS_BUDGET,
    bound_page, bound_parts, dropped, fit, history_line, rebase, tail,
};
use kernel::wiki::page::{ArticleSection, WikiHistoryEntry, WikiPage, WikiSlug};
use kernel::wiki::recompile::{rebase_recompiled, restore_history};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn at() -> Result<DateTime<Utc>, chrono::ParseError> {
    "2026-10-30T12:00:00Z".parse()
}

fn day(text: &str) -> Result<NaiveDate, chrono::ParseError> {
    text.parse()
}

fn entry(date: &str, title: &str, body: &str) -> Result<WikiHistoryEntry, chrono::ParseError> {
    Ok(WikiHistoryEntry {
        date: day(date)?,
        title: title.to_owned(),
        body: body.to_owned(),
    })
}

fn page() -> Result<WikiPage, Box<dyn std::error::Error>> {
    Ok(WikiPage::empty(
        WikiSlug::try_from("acmekit".to_owned())?,
        "AcmeKit".to_owned(),
    ))
}

fn paragraphs(count: usize, size: usize) -> String {
    (0..count)
        .map(|index| format!("{index:04} {}", "x".repeat(size)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[test]
fn fit_keeps_whole_leading_blocks_and_hands_back_the_rest() {
    let text = paragraphs(10, 100);
    let fitted = fit(&text, 330);
    assert_eq!(fitted.kept, paragraphs(3, 100));
    assert!(fitted.overflow.starts_with("0003 "));
    assert_eq!(fit("short", 10).overflow, "");
}

#[test]
fn fit_cuts_a_single_oversized_block_at_a_line() {
    let block = format!("{}\n{}", "a".repeat(60), "b".repeat(60));
    let fitted = fit(&block, 80);
    assert_eq!(fitted.kept, "a".repeat(60));
    assert_eq!(fitted.overflow, "b".repeat(60));
}

#[test]
fn tail_keeps_the_newest_blocks() {
    let text = paragraphs(10, 100);
    let kept = tail(&text, 330);
    assert!(kept.starts_with("0007 "));
    assert!(kept.ends_with(&"x".repeat(100)));
}

#[test]
fn dropped_lists_only_the_blocks_a_rewrite_left_out() {
    let before = "Ships on Fridays.\n\nRuns on the build box.\n\nOld port 8080.";
    let after = "Ships on Fridays.\n\nRuns on the build box.";
    assert_eq!(dropped(before, after), "Old port 8080.");
}

#[test]
fn rebase_takes_the_rewrite_when_the_page_did_not_move() {
    let base = "A.\n\nB.";
    assert_eq!(
        rebase(base, "A, merged.\n\nC.", "A.\n\n  B."),
        "A, merged.\n\nC."
    );
}

#[test]
fn rebase_keeps_a_concurrent_writers_text_and_ours() {
    let base = "Lead.\n\nOld fact.";
    let theirs = "Lead.\n\nOld fact.\n\nTheir new fact.";
    let ours = "Lead.\n\nOur new fact.";
    let merged = rebase(base, ours, theirs);
    assert!(merged.contains("Their new fact."));
    assert!(merged.contains("Our new fact."));
    assert!(!merged.contains("Old fact."));
    assert!(merged.starts_with("Lead."));
}

#[test]
fn rebase_keeps_one_lead_when_a_concurrent_writer_reworded_it() {
    let base = "Rivethub is the desktop app.\n\nRound 29 is deployed.";
    let theirs = "Rivethub is the desktop app. Currently, round 30 with its six fixes in commit \
        a65f94dc is deployed and QA is underway.\n\nRound 30 is deployed.";
    let ours = "Rivethub is the desktop app. Currently, round 30 is deployed with six fixes in \
        commit a65f94dc and QA is underway.\n\nRound 30 is deployed.\n\nHeadless QA started.";
    let merged = rebase(base, ours, theirs);
    assert_eq!(merged.matches("Rivethub is the desktop app").count(), 1);
    assert_eq!(merged, ours);
    let unrelated = format!("{theirs}\n\nThe Android build moved to the build box.");
    assert!(rebase(base, ours, &unrelated).ends_with("The Android build moved to the build box."));
}

#[test]
fn a_write_keeps_one_copy_of_a_repeated_paragraph() {
    let repeated = "The gateway cutover covers the mesh TLS client and the certificate authority.";
    let lead = "Rivethub is the desktop app spanning workspace, memory, chat and terminal \
        integration across the runtime and desktop repositories.";
    let newer = format!("{lead} Currently, round 30 is deployed with six docs fixes.");
    let older = format!("{lead} Currently, round 29 is deployed.");
    let listed = "- Rivethub spans workspace, memory, chat and terminal integration across the \
        runtime and desktop repositories.";
    let mut summary = String::new();
    let mut article = format!("{repeated}\n\n### Status\n\nShipped.\n\n{repeated}");
    let mut sections = vec![ArticleSection {
        heading: "Desktop App".to_owned(),
        body: format!("{newer}\n\n### Round 30\n\nDeployed.\n\n  {older}\n\n{listed}"),
    }];
    bound_parts(&mut summary, &mut article, &mut sections, &[]);
    assert_eq!(article, format!("{repeated}\n\n### Status\n\nShipped."));
    assert_eq!(
        sections.first().map(|section| section.body.clone()),
        Some(format!("{newer}\n\n### Round 30\n\nDeployed.\n\n{listed}"))
    );
}

#[test]
fn history_line_keeps_one_short_line() {
    let long = format!("{}. {}", "First sentence".repeat(10), "y".repeat(400));
    let line = history_line(&long);
    assert!(line.kept.chars().count() <= HISTORY_LINE);
    assert_eq!(line.overflow, long);
    assert_eq!(history_line("Short.").overflow, "");
}

#[test]
fn bound_page_archives_superseded_copies_and_holds_the_budget() -> TestResult {
    let mut previous = page()?;
    previous.history = vec![
        entry("2026-10-20", "Superseded article", "an old copy")?,
        entry("2026-10-19", "Deployed", "Shipped 1.2.")?,
    ];
    let mut next = previous.clone();
    next.article = paragraphs(200, 200);
    next.extra_sections = (0..6)
        .map(|index| ArticleSection {
            heading: format!("Part {index}"),
            body: paragraphs(40, 200),
        })
        .collect();
    let archived = bound_page(Some(&previous), &mut next);
    assert!(next.article.len() <= ARTICLE_BUDGET);
    assert!(
        next.extra_sections
            .iter()
            .all(|section| section.body.len() <= SECTION_BUDGET)
    );
    let total: usize = next.extra_sections.iter().map(|s| s.body.len()).sum();
    assert!(total <= SECTIONS_BUDGET);
    assert!(
        next.history
            .iter()
            .all(|kept| kept.title != "Superseded article")
    );
    assert!(archived.iter().any(|item| item.text == "an old copy"));
    assert!(archived.iter().any(|item| item.label == "Article"));
    Ok(())
}

#[test]
fn settle_archive_adds_one_dated_line_a_day_and_writes_every_block() -> TestResult {
    let mut previous = page()?;
    previous.article = paragraphs(3, 50);
    let mut next = previous.clone();
    next.article = paragraphs(1, 50);
    let mut archived = bound_page(Some(&previous), &mut next);
    let block = settle_archive(&mut next, at()?, Some("0123456789abcdef"), &mut archived)
        .ok_or("an archive block")?;
    assert!(block.contains("<a id=\"a-20261030T120000000Z\"></a>"));
    assert!(block.contains("last inline at 0123456789"));
    let line = next.history.first().ok_or("history line")?;
    assert_eq!(line.title, ARCHIVED_TITLE);
    assert!(
        line.body
            .contains("/wiki/acmekit/archive/2026-10#a-20261030T120000000Z")
    );
    let again = next.clone();
    next.article = String::new();
    let mut more = bound_page(Some(&again), &mut next);
    assert!(settle_archive(&mut next, at()?, None, &mut more).is_some());
    let lines = next
        .history
        .iter()
        .filter(|kept| kept.title == ARCHIVED_TITLE)
        .count();
    assert_eq!(lines, 1);
    Ok(())
}

#[test]
fn restore_history_puts_real_entries_back_newest_first_within_the_cap() -> TestResult {
    let mut current = page()?;
    current.history = vec![
        entry("2026-10-30", "Superseded article", "copy")?,
        entry("2026-10-28", "Kept", "Still here.")?,
    ];
    let mut recovered = vec![
        entry("2026-10-29", "Pushed out", "Was dropped by a copy.")?,
        entry("2026-10-28", "Kept", "Still  here.")?,
    ];
    recovered.extend(
        (0..60)
            .map(|index| entry("2026-09-01", &format!("Old {index}"), "Older."))
            .collect::<Result<Vec<_>, _>>()?,
    );
    let mut archived = Vec::new();
    let restored = restore_history(&mut current, recovered, &mut archived);
    assert_eq!(current.history.len(), 47);
    let first: Vec<&str> = current
        .history
        .iter()
        .take(2)
        .map(|kept| kept.title.as_str())
        .collect();
    assert_eq!(first, vec!["Pushed out", "Kept"]);
    assert_eq!(restored, 46);
    assert!(archived.iter().any(|item| item.text == "copy"));
    assert_eq!(
        archived
            .iter()
            .filter(|item| item.label.starts_with("History 2026-09-01"))
            .count(),
        15
    );
    Ok(())
}

#[test]
fn the_sections_budget_is_shared_and_uses_what_the_article_leaves() -> TestResult {
    let mut next = page()?;
    next.extra_sections = (0..7)
        .map(|index| ArticleSection {
            heading: format!("Part {index}"),
            body: paragraphs(40, 200),
        })
        .collect();
    next.extra_sections.push(ArticleSection {
        heading: "Small".to_owned(),
        body: "One line.".to_owned(),
    });
    let archived = bound_page(None, &mut next);
    assert!(
        next.extra_sections
            .iter()
            .all(|section| !section.body.is_empty())
    );
    let small = next.extra_sections.last().ok_or("small section")?;
    assert_eq!(small.body, "One line.");
    let total: usize = next.extra_sections.iter().map(|s| s.body.len()).sum();
    assert!(total <= SECTIONS_BUDGET + ARTICLE_BUDGET);
    assert!(total > SECTIONS_BUDGET);
    assert!(archived.iter().any(|item| item.label.starts_with("Part")));
    Ok(())
}

#[test]
fn many_note_sections_drop_the_oldest_whole_instead_of_shredding_all() -> TestResult {
    let mut next = page()?;
    next.extra_sections = (0..200)
        .map(|index| ArticleSection {
            heading: format!("Note {index}"),
            body: paragraphs(3, 200),
        })
        .collect();
    let archived = bound_page(None, &mut next);
    assert!(next.extra_sections.len() < 200);
    assert!(
        next.extra_sections
            .iter()
            .all(|section| section.body.len() >= 400)
    );
    let last = next.extra_sections.last().ok_or("a kept note")?;
    assert_eq!(last.heading, "Note 199");
    assert!(archived.iter().any(|item| item.label == "Note 0"));
    Ok(())
}

#[test]
fn the_entity_list_keeps_its_newest_names() -> TestResult {
    let mut next = page()?;
    next.meta.entities = (0..100).map(|index| format!("Entity {index}")).collect();
    let archived = bound_page(None, &mut next);
    assert_eq!(
        next.meta.entities.len(),
        kernel::wiki::page::bound::ENTITIES
    );
    assert_eq!(
        next.meta.entities.first().map(String::as_str),
        Some("Entity 36")
    );
    let names = archived
        .iter()
        .find(|item| item.label == "Entities")
        .ok_or("archived names")?;
    assert!(names.text.starts_with("Entity 0, Entity 1"));
    Ok(())
}

#[test]
fn restoring_twice_keeps_one_copy_of_a_shortened_entry() -> TestResult {
    let long = format!("Shipped 1.0. {}", "detail ".repeat(80));
    let mut current = page()?;
    current.history = vec![entry(
        "2026-10-28",
        "First release",
        "Shipped 1.0. detail…",
    )?];
    let mut archived = Vec::new();
    let restored = restore_history(
        &mut current,
        vec![entry("2026-10-28", "First release", &long)?],
        &mut archived,
    );
    assert_eq!(restored, 0);
    assert_eq!(current.history.len(), 1);
    Ok(())
}

#[test]
fn a_stable_lead_keeps_its_description_and_swaps_only_the_currently_sentence() {
    use kernel::wiki::page::lead::{split_lead, stable_summary};
    let existing = "RivetOS is a multi-agent runtime. Currently, the Rust port is in review.";
    let split = split_lead(existing);
    assert_eq!(split.lead, "RivetOS is a multi-agent runtime.");
    assert_eq!(
        split.currently.as_deref(),
        Some("Currently, the Rust port is in review.")
    );
    assert_eq!(
        stable_summary(
            existing,
            "Gateway routes landed today. Currently, v0.7.0 runs everywhere."
        ),
        "RivetOS is a multi-agent runtime. Currently, v0.7.0 runs everywhere."
    );
    assert_eq!(
        stable_summary(existing, "Gateway routes landed today."),
        existing
    );
    assert_eq!(
        stable_summary("", "RivetOS is a runtime. Currently, it is in beta."),
        "RivetOS is a runtime. Currently, it is in beta."
    );
    assert_eq!(
        split_lead("Recurrently Currently is a word.").lead,
        "Recurrently Currently is a word."
    );
}

#[test]
fn only_text_that_left_the_page_and_is_not_archived_yet_is_archived() -> TestResult {
    let mut next = page()?;
    next.article = "AcmeKit ships its releases every Friday from the build box.".to_owned();
    let candidates = || {
        vec![Archived {
            label: "Article".to_owned(),
            text: "Releases of AcmeKit ship every Friday from the build box.\n\nThe staging cluster listens on port 8080.".to_owned(),
            notice: true,
        }]
    };
    let kept = unarchived(candidates(), &next, "");
    assert_eq!(
        kept.iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>(),
        vec!["The staging cluster listens on port 8080."]
    );
    let archive =
        "## 2026-10-29 09:00 UTC\n\n### Article\n\nThe staging cluster listens on port 8080.\n";
    assert_eq!(
        unarchived(candidates(), &next, archive),
        Vec::<Archived>::new()
    );
    Ok(())
}

#[test]
fn a_currently_swap_archives_nothing() -> TestResult {
    use kernel::wiki::page::lead::stable_summary;
    let mut previous = page()?;
    previous.current_state =
        "AcmeKit is the deploy tool. Currently, releases ship on Fridays.".to_owned();
    previous.article = "AcmeKit deploys every service.".to_owned();
    let mut next = previous.clone();
    next.current_state = stable_summary(
        &previous.current_state,
        "Release 2.0 shipped. Currently, releases ship daily.",
    );
    assert_eq!(
        next.current_state,
        "AcmeKit is the deploy tool. Currently, releases ship daily."
    );
    let archived = bound_page(Some(&previous), &mut next);
    assert_eq!(unarchived(archived, &next, ""), Vec::<Archived>::new());
    Ok(())
}

#[test]
fn a_section_lead_keeps_its_wording_until_its_facts_change() {
    use kernel::wiki::page::lead::steady_lead;
    let section =
        "Rivethub is the desktop app. Currently, round 30 is deployed to every node.\n\nOld news.";
    let reworded =
        "RivetHub is the native desktop client. Currently, every node runs round 30.\n\nNew news.";
    assert_eq!(
        steady_lead(section, reworded),
        "Rivethub is the desktop app. Currently, round 30 is deployed to every node.\n\nNew news."
    );
    let moved = "RivetHub is the desktop client. Currently, round 31 is deployed to every node.";
    assert_eq!(
        steady_lead(section, moved),
        "Rivethub is the desktop app. Currently, round 31 is deployed to every node."
    );
    assert_eq!(steady_lead("### Note\n\nText.", reworded), reworded);
    let doubled = "Rivethub is the desktop app for workspace and memory.\n\nRivethub is the desktop \
        app for workspace, memory and chat. Currently, round 30 is out.";
    assert_eq!(
        kernel::wiki::page::lead::stable_summary(doubled, "Currently, round 31 is out."),
        "Rivethub is the desktop app for workspace and memory. Currently, round 31 is out."
    );
}

#[test]
fn a_currently_sentence_that_does_not_fit_is_left_out() {
    use kernel::wiki::page::lead::stable_summary;
    let lead = format!("AcmeKit is the deploy tool. {}", "It deploys. ".repeat(196));
    let summary = stable_summary(&lead, "Currently, releases ship daily.");
    assert_eq!(summary, lead.trim());
}

#[test]
fn a_recompile_keeps_what_a_concurrent_write_added() -> TestResult {
    let mut base = page()?;
    base.current_state = "AcmeKit news.".to_owned();
    base.article = "Ships on Fridays.\n\nBuilds on the old box.".to_owned();
    base.history = vec![entry("2026-10-28", "First release", "Shipped 1.0.")?];
    let mut ours = base.clone();
    ours.current_state = "AcmeKit is the deploy tool.".to_owned();
    ours.article = "AcmeKit ships every Friday.".to_owned();
    let mut current = base.clone();
    current.article =
        "Ships on Fridays.\n\nBuilds on the old box.\n\nPort 8080 opened for staging.".to_owned();
    current
        .history
        .insert(0, entry("2026-10-30", "Staging port", "Opened 8080.")?);
    let mut archived = Vec::new();
    let page = rebase_recompiled(&base, ours, &current, &mut archived);
    assert_eq!(page.current_state, "AcmeKit is the deploy tool.");
    assert_eq!(
        page.article,
        "Port 8080 opened for staging.\n\nAcmeKit ships every Friday."
    );
    let titles: Vec<&str> = page
        .history
        .iter()
        .map(|kept| kept.title.as_str())
        .collect();
    assert_eq!(titles, vec!["Staging port", "First release"]);
    assert_eq!(archived, Vec::<Archived>::new());
    Ok(())
}

#[test]
fn archive_writes_fill_numbered_parts_and_never_the_month_file() {
    let part = |month: &str, part: u32| ArchivePart {
        month: month.to_owned(),
        part,
    };
    assert_eq!(ArchivePart::parse("2026-10.md"), Some(part("2026-10", 0)));
    assert_eq!(ArchivePart::parse("2026-10.3.md"), Some(part("2026-10", 3)));
    assert_eq!(ArchivePart::parse("2026-10.03.md"), None);
    assert_eq!(ArchivePart::parse("2026-10.0.md"), None);
    assert_eq!(ArchivePart::parse("2026-13.md"), None);
    assert_eq!(part("2026-11", 2).name(), "2026-11.2.md");
    assert_eq!(next_part("2026-11", &[], 10), part("2026-11", 1));
    assert_eq!(
        next_part("2026-10", &[(part("2026-10", 0), 3_000_000)], 10),
        part("2026-10", 1)
    );
    let parts = [(part("2026-10", 0), 3_000_000), (part("2026-10", 1), 1024)];
    assert_eq!(next_part("2026-10", &parts, 10), part("2026-10", 1));
    assert_eq!(
        next_part("2026-10", &parts, SEGMENT_BUDGET),
        part("2026-10", 2)
    );
}
