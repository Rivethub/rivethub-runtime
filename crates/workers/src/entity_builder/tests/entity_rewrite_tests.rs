use chrono::TimeZone;
use chrono::Utc;
use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::EntityPage;
use kernel::wiki::entity_pass::EntityRewrite;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::FactState;
use kernel::wiki::entity_pass::LiveFact;
use kernel::wiki::entity_pass::RewriteFact;
use uuid::Uuid;

use super::stored_rewrite;
use crate::WorkerError;

fn stamp() -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000, 0)
        .single()
        .unwrap_or_default()
}

fn leaf() -> EntityLeaf {
    EntityLeaf {
        id: Uuid::from_u128(4),
        conversation_id: Some(Uuid::from_u128(9)),
        content: "leaf".to_owned(),
        start: stamp(),
        end: stamp(),
        owner: Some("owner".to_owned()),
    }
}

fn view() -> EntityView {
    EntityView {
        id: Uuid::from_u128(3),
        name: "gpubox".to_owned(),
        slug: "gpubox".to_owned(),
        type_name: "Hosts".to_owned(),
        type_slug: "hosts".to_owned(),
        aliases: Vec::new(),
        facts: Vec::new(),
        history: Vec::new(),
        snapshot: String::new(),
        owner: Some("owner".to_owned()),
    }
}

fn page_with_ram() -> EntityPage {
    EntityPage {
        summary: "Host.".to_owned(),
        article: "Lead.".to_owned(),
        facts: vec![LiveFact {
            attribute: "ram".to_owned(),
            value: "87 GB available".to_owned(),
            as_of: stamp(),
            status: FactState::Current,
        }],
        observed_types: Vec::new(),
        fixed: Vec::new(),
    }
}

#[test]
fn an_older_session_is_told_the_page_reflects_later_sessions() {
    let prompt = super::rewrite_prompt(true);
    assert!(prompt.contains("later sessions"));
    assert!(prompt.contains("no newer counterpart"));
    assert!(prompt.contains("date order"));
    assert!(!super::rewrite_prompt(false).contains("no newer counterpart"));
}

#[test]
fn the_stored_rewrite_records_the_lead_session() {
    let rewrite = EntityRewrite {
        summary: "Host.".to_owned(),
        current_facts: vec![RewriteFact {
            attribute: "ram".to_owned(),
            value: "152 GB available".to_owned(),
            as_of: "2023-11-14T22:13:20Z".to_owned(),
        }],
        history_sentence: "RAM is 152 GB.".to_owned(),
        article: "Lead.".to_owned(),
        ..EntityRewrite::default()
    };
    let body = stored_rewrite(&EntityPage::default(), &rewrite, &view(), &[leaf()], "", "")
        .expect("rewrite");
    let lead = leaf();
    assert_eq!(body.session_id, lead.id);
    assert_eq!(body.session_at, lead.start);
}

#[test]
fn an_empty_summary_is_rejected() {
    let rewrite = EntityRewrite::default();
    let error = stored_rewrite(&EntityPage::default(), &rewrite, &view(), &[leaf()], "", "");
    assert!(matches!(error, Err(WorkerError::EmptySummary)));
}

#[test]
fn an_empty_snapshot_does_not_wipe_existing_facts() {
    let rewrite = EntityRewrite {
        summary: "Host.".to_owned(),
        current_facts: vec![RewriteFact {
            attribute: "other".to_owned(),
            value: "ignored".to_owned(),
            as_of: "2023-11-14T22:13:20Z".to_owned(),
        }],
        retired: vec![kernel::wiki::entity_pass::RetiredFact {
            attribute: "ram".to_owned(),
            kind: "contradicted".to_owned(),
            detail: "ram was removed".to_owned(),
        }],
        ..EntityRewrite::default()
    };
    let error = stored_rewrite(
        &page_with_ram(),
        &rewrite,
        &view(),
        &[leaf()],
        "ram was removed",
        "ram was removed",
    );
    assert!(matches!(error, Err(WorkerError::Llm { .. })));
}

#[test]
fn an_omitted_fact_is_carried_forward() {
    let page = EntityPage {
        facts: vec![
            LiveFact {
                attribute: "ram".to_owned(),
                value: "87 GB available".to_owned(),
                as_of: stamp(),
                status: FactState::Current,
            },
            LiveFact {
                attribute: "ip".to_owned(),
                value: "11.22.33.44".to_owned(),
                as_of: stamp(),
                status: FactState::Ended,
            },
        ],
        ..page_with_ram()
    };
    let rewrite = EntityRewrite {
        summary: "Host.".to_owned(),
        current_facts: vec![RewriteFact {
            attribute: "ram".to_owned(),
            value: "152 GB available".to_owned(),
            as_of: "2023-11-14T22:13:20Z".to_owned(),
        }],
        history_sentence: "RAM is 152 GB.".to_owned(),
        ..EntityRewrite::default()
    };
    let body = stored_rewrite(&page, &rewrite, &view(), &[leaf()], "", "").expect("rewrite");
    let ip = body
        .facts
        .iter()
        .find(|fact| fact.attribute == "ip")
        .expect("carried ip");
    assert_eq!(ip.value, "11.22.33.44");
    assert_eq!(ip.as_of, stamp());
    assert_eq!(ip.status, FactState::Ended);
    let ram = body
        .facts
        .iter()
        .find(|fact| fact.attribute == "ram")
        .expect("updated ram");
    assert_eq!(ram.value, "152 GB available");
    let sentence = body.history.map(|line| line.sentence).unwrap_or_default();
    assert_eq!(sentence, "RAM is 152 GB.");
}

#[test]
fn the_rewrite_prompt_keeps_facts_the_session_does_not_mention() {
    let prompt = super::super::ENTITY_REWRITE_SYSTEM_PROMPT;
    assert!(
        prompt.contains(
            "not mentioned, not re-verified, or not observed in this session stays current"
        )
    );
    assert!(
        prompt.contains(
            "Only a renamed attribute or a contradiction from this session can retire it"
        )
    );
    assert!(prompt.contains("kind is renamed or contradicted"));
    assert!(!prompt.contains("was wrong"));
    assert!(!prompt.contains("no longer true"));
    assert!(!prompt.contains("current_facts or in retired"));
    assert!(!prompt.contains("Current facts come from"));
    assert!(!prompt.contains("only include facts supported by the passages"));
    assert!(prompt.contains(
        "New facts come from what this session's tool output shows or what the user states as fact now"
    ));
    assert!(prompt.contains(
        "summary is one or two sentences in the present tense that say what the entity is"
    ));
    assert!(prompt.contains("summary does not report what this session did"));
    assert!(prompt.contains(
        "When this session's passages state a different value, current_facts records that observed value under the same attribute"
    ));
}

#[test]
fn excerpt_ratio_is_excerpt_chars_over_original_chars() {
    let report = super::EntityRunReport {
        excerpt_chars: 2_500,
        excerpt_source_chars: 10_000,
        ..super::EntityRunReport::default()
    };
    assert_eq!(report.excerpt_ratio(), "0.2500");
    assert_eq!(super::EntityRunReport::default().excerpt_ratio(), "0");
}
