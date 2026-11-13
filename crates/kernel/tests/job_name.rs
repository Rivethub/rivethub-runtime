use std::str::FromStr;

use kernel::queue::FixedJob;
use kernel::queue::JobName;
use kernel::queue::JobNameError;
use strum::IntoEnumIterator;

const FIXED: [(&str, JobName); 21] = [
    ("embed-target", JobName::Fixed(FixedJob::EmbedTarget)),
    (
        "enqueue-unembedded",
        JobName::Fixed(FixedJob::EnqueueUnembedded),
    ),
    (
        "enqueue-unchunked",
        JobName::Fixed(FixedJob::EnqueueUnchunked),
    ),
    (
        "compact-conversation",
        JobName::Fixed(FixedJob::SummarizeConversation),
    ),
    (
        "synthesize-tool-call",
        JobName::Fixed(FixedJob::SynthesizeToolCall),
    ),
    (
        "file-conversation",
        JobName::Fixed(FixedJob::FileConversation),
    ),
    ("propose-tidy", JobName::Fixed(FixedJob::ProposeTidy)),
    ("enqueue-idle", JobName::Fixed(FixedJob::EnqueueIdle)),
    ("extract-wiki", JobName::Fixed(FixedJob::ExtractWiki)),
    (
        "enqueue-wiki-backfill",
        JobName::Fixed(FixedJob::EnqueueWikiBackfill),
    ),
    (
        "consolidate-wiki",
        JobName::Fixed(FixedJob::ConsolidateWiki),
    ),
    ("recompile-wiki", JobName::Fixed(FixedJob::RecompileWiki)),
    (
        "enqueue-stale-wiki",
        JobName::Fixed(FixedJob::EnqueueStaleWiki),
    ),
    (
        "extract-entities",
        JobName::Fixed(FixedJob::ExtractEntities),
    ),
    (
        "entity-backfill",
        JobName::Fixed(FixedJob::EnqueueEntityBackfill),
    ),
    (
        "enqueue-stale-entities",
        JobName::Fixed(FixedJob::EnqueueStaleEntities),
    ),
    (
        "enqueue-stale-compaction",
        JobName::Fixed(FixedJob::EnqueueStaleSummaries),
    ),
    ("reap-dead-jobs", JobName::Fixed(FixedJob::ReapDeadJobs)),
    ("sync-topic-page", JobName::Fixed(FixedJob::SyncTopicPage)),
    (
        "purge-deleted-sessions",
        JobName::Fixed(FixedJob::PurgeDeletedSessions),
    ),
    ("digest-day", JobName::Fixed(FixedJob::DigestDay)),
];

#[test]
fn fixed_names_round_trip_through_the_wire_string() {
    for (wire, name) in FIXED {
        assert_eq!(name.to_string(), wire);
        assert_eq!(JobName::from_str(wire), Ok(name));
    }
}

#[test]
fn every_fixed_variant_round_trips_through_job_name() {
    for fixed in FixedJob::iter() {
        let name = JobName::Fixed(fixed);
        assert_eq!(name.to_string(), fixed.to_string());
        assert_eq!(JobName::from_str(&name.to_string()), Ok(name.clone()));
        let parsed: JobName = serde_json::from_str(&serde_json::to_string(&name).unwrap()).unwrap();
        assert_eq!(parsed, name);
        assert_eq!(
            serde_json::to_string(&parsed).unwrap(),
            format!("\"{fixed}\"")
        );
    }
}

#[test]
fn heartbeat_with_a_dashed_agent_round_trips() {
    let name = JobName::Heartbeat {
        agent: "rivet-edge-agent".to_string(),
        index: 7,
    };
    assert_eq!(name.to_string(), "heartbeat-rivet-edge-agent-7");
    assert_eq!(
        JobName::from_str("heartbeat-rivet-edge-agent-7"),
        Ok(name.clone())
    );
    assert_eq!(String::from(name), "heartbeat-rivet-edge-agent-7");
}

#[test]
fn heartbeat_splits_at_the_last_dash() {
    assert_eq!(
        JobName::from_str("heartbeat-agent-10"),
        Ok(JobName::Heartbeat {
            agent: "agent".to_string(),
            index: 10
        })
    );
    assert_eq!(
        JobName::from_str("heartbeat-a-b-c-0"),
        Ok(JobName::Heartbeat {
            agent: "a-b-c".to_string(),
            index: 0
        })
    );
}

#[test]
fn rejects_an_unknown_name() {
    assert_eq!(
        JobName::from_str("no-such-job"),
        Err(JobNameError::Unknown("no-such-job".to_string()))
    );
    assert_eq!(
        JobName::from_str("heartbeat"),
        Err(JobNameError::Unknown("heartbeat".to_string()))
    );
}

#[test]
fn rejects_a_non_numeric_heartbeat_index() {
    assert_eq!(
        JobName::from_str("heartbeat-agent-x"),
        Err(JobNameError::HeartbeatIndex(
            "heartbeat-agent-x".to_string()
        ))
    );
    assert_eq!(
        JobName::from_str("heartbeat-agent"),
        Err(JobNameError::HeartbeatIndex("heartbeat-agent".to_string()))
    );
    assert_eq!(
        JobName::from_str("heartbeat-agent-1-x"),
        Err(JobNameError::HeartbeatIndex(
            "heartbeat-agent-1-x".to_string()
        ))
    );
}

#[test]
fn serde_uses_the_wire_string() {
    let heartbeat = JobName::Heartbeat {
        agent: "edge-agent".to_string(),
        index: 3,
    };
    assert_eq!(
        serde_json::to_string(&heartbeat).unwrap(),
        "\"heartbeat-edge-agent-3\""
    );
    for (wire, name) in FIXED {
        let parsed: JobName = serde_json::from_str(&serde_json::to_string(&name).unwrap()).unwrap();
        assert_eq!(parsed, name.clone());
        assert_eq!(
            serde_json::to_string(&parsed).unwrap(),
            format!("\"{wire}\"")
        );
    }
}
