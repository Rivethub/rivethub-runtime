use axum::http::StatusCode;
use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use kernel::model::LlmResponse;
use kernel::model::LlmResponseKind;
use kernel::topics::Suggestion;
use kernel::topics::TopicId;
use kernel::topics::TopicName;
use kernel::topics::TopicStore as _;
use testkit::FakeProvider;
use testkit::Script;
use tokio_util::sync::CancellationToken;
use wire::gateway::tidy::MemoryUpkeep;
use wire::gateway::tidy::ProposalState;
use wire::gateway::tidy::TidyBatchDecision;
use wire::gateway::tidy::TidyChange;
use wire::gateway::tidy::TidyPending;
use wire::gateway::tidy::TidyProposal;
use wire::gateway::topics::PlacedBy;
use wire::gateway::topics::PlacementReply;
use wire::gateway::topics::TopicRecord;
use wire::session_id::SessionId;
use workers::SessionTopics;
use workers::TidyEnv;
use workers::TidyRun;

use super::Api;
use super::TestResult;
use super::placement_path;

const WEEKLY: &str = r#"Here is this week's batch.
{"merge": "T2", "into": "T1", "reason": "One app under two names."}
{"create": "C1", "name": "Release Tooling", "under": "T3", "reason": "Three sessions built the release scripts."}
{"rename": "T3", "to": "Fix flaky CI for PR 1203", "reason": "Reads better."}
{"type": "Printers", "under": null, "reason": "The wiki tracks printers but has no type for them."}"#;

fn reply(text: &str) -> Script {
    Script::Chat(LlmResponse {
        r#type: LlmResponseKind::Text,
        content: Some(text.to_owned()),
        tool_calls: None,
        usage: None,
        finish_reason: None,
    })
}

async fn tidy(api: &Api, answer: &str, now: DateTime<Utc>) -> TestResult<TidyRun> {
    let provider = FakeProvider::new(vec![reply(answer)]);
    let topics = SessionTopics {
        store: api.store.clone(),
        filing: api.store.clone(),
        lineage: std::sync::Arc::new(capture::listing::StoreSessionLister::new(
            capture::listing::StoreRoots::default(),
        )),
    };
    let cancel = CancellationToken::new();
    let env = TidyEnv {
        owners: kernel::topics::SweepOwners::All,
        topics: &topics,
        tidy: api.store.as_ref(),
        provider: &provider,
        model: "tidier",
        now,
        cancel: &cancel,
    };
    Ok(workers::handle_tidy(&env).await?)
}

async fn post<T: serde::de::DeserializeOwned>(api: &Api, path: &str) -> TestResult<T> {
    api.json("POST", path, "", StatusCode::OK).await
}

async fn topic(api: &Api, name: &str, parent: Option<&str>) -> TestResult<TopicRecord> {
    let body = match parent {
        Some(parent) => format!(r#"{{"name":"{name}","parent":"{parent}"}}"#),
        None => format!(r#"{{"name":"{name}"}}"#),
    };
    api.json("POST", "/api/topics", &body, StatusCode::CREATED)
        .await
}

async fn seed(api: &Api) -> TestResult<Vec<SessionId>> {
    topic(api, "Desktop App", None).await?;
    topic(api, "Desktop Apps", None).await?;
    let runtime = topic(api, "Rivet Runtime", None).await?;
    let runtime_id: TopicId = runtime.id.parse()?;
    let members: Vec<SessionId> = (1..=3)
        .map(|n| format!("claude-code:00000000-0000-4000-8000-00000000008{n}").parse())
        .collect::<Result<_, _>>()?;
    for member in &members {
        api.store
            .suggest(Suggestion {
                session: member.clone(),
                name: TopicName::new("Release tooling")?,
                topic: None,
                parent: Some(runtime_id),
            })
            .await?;
    }
    Ok(members)
}

#[tokio::test]
async fn a_weekly_batch_is_proposed_decided_and_remembered() -> TestResult {
    let api = Api::new(true)?;
    let members = seed(&api).await?;
    let now = Utc::now();
    let TidyRun::Proposed(batch) = tidy(&api, WEEKLY, now).await? else {
        return Err("the first run proposes a batch".into());
    };
    assert_eq!(batch.proposals.len(), 3, "the title-like rename is refused");
    let pending: TidyPending = api
        .json("GET", "/api/tidy/proposals", "", StatusCode::OK)
        .await?;
    let listed: Vec<&TidyProposal> = pending
        .batches
        .iter()
        .flat_map(|batch| &batch.proposals)
        .collect();
    let [merge, create, added] = listed.as_slice() else {
        return Err(format!("three pending proposals: {listed:?}").into());
    };
    assert!(matches!(merge.change, TidyChange::Merge { .. }));
    assert!(matches!(added.change, TidyChange::AddType { .. }));
    assert_eq!(create.evidence.sessions, 3);
    let approved: TidyProposal =
        post(&api, &format!("/api/tidy/proposals/{}/approve", create.id)).await?;
    assert_eq!(approved.state, ProposalState::Approved);
    let placed: PlacementReply = api
        .json(
            "GET",
            &placement_path(members.first().ok_or("a member")?),
            "",
            StatusCode::OK,
        )
        .await?;
    assert_eq!(
        placed.placement.map(|placement| placement.by),
        Some(PlacedBy::Categorizer)
    );
    let (status, _) = api
        .error(
            "POST",
            &format!("/api/tidy/proposals/{}/approve", create.id),
            "",
        )
        .await?;
    assert_eq!(status, StatusCode::CONFLICT);
    post::<TidyProposal>(&api, &format!("/api/tidy/proposals/{}/reject", merge.id)).await?;
    let rest: TidyBatchDecision =
        post(&api, &format!("/api/tidy/batches/{}/reject", batch.id)).await?;
    assert_eq!(rest.decided.len(), 1);
    let upkeep: MemoryUpkeep = api
        .json("GET", "/api/memory/upkeep", "", StatusCode::OK)
        .await?;
    assert_eq!(upkeep.pending_proposals, 0);
    assert!(matches!(
        tidy(&api, WEEKLY, now + TimeDelta::days(1)).await?,
        TidyRun::NotDue { .. }
    ));
    let TidyRun::Proposed(later) = tidy(&api, WEEKLY, now + TimeDelta::days(8)).await? else {
        return Err("a week later the job runs again".into());
    };
    assert!(
        later.proposals.is_empty(),
        "rejected and applied changes are not proposed again: {later:?}"
    );
    Ok(())
}
