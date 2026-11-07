#![cfg(test)]

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use kernel::changes::{
    ChangeError, ChangeFeed, ChangeRecord, ChangeWindow, DailyDigest, DigestRequest, DigestStore,
    TopicRef,
};
use kernel::model::{LlmResponse, LlmResponseKind};
use kernel::owner::Owner;
use kernel::topics::{TopicId, TopicKind};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use wire::message::MessageContent;
use workers::digest::{DailyDigests, DigestEnv, handle_digest_day};

const BACKUPS: &str = "00000000-0000-4000-8000-0000000000a1";

struct Feed(Vec<ChangeRecord>);

#[async_trait]
impl ChangeFeed for Feed {
    async fn records(&self, _window: &ChangeWindow) -> Result<Vec<ChangeRecord>, ChangeError> {
        Ok(self.0.clone())
    }
}

#[derive(Default)]
struct Kept(Mutex<Vec<DailyDigest>>);

#[async_trait]
impl DigestStore for Kept {
    async fn digest(
        &self,
        _day: NaiveDate,
        _owner: &Owner,
    ) -> Result<Option<DailyDigest>, ChangeError> {
        Ok(None)
    }

    async fn keep_digest(&self, digest: &DailyDigest) -> Result<(), ChangeError> {
        self.0.lock().await.push(digest.clone());
        Ok(())
    }

    async fn request_digest(&self, _request: &DigestRequest) -> Result<(), ChangeError> {
        Ok(())
    }
}

fn at(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

fn backups() -> TopicRef {
    TopicRef {
        id: BACKUPS.parse::<TopicId>().unwrap(),
        title: "Backups".to_owned(),
        kind: TopicKind::Work,
    }
}

fn request() -> DigestRequest {
    DigestRequest::new(
        "2026-11-05".parse().unwrap(),
        at("2026-11-05T05:00:00Z"),
        at("2026-11-06T05:00:00Z"),
        &Owner::new("owner".to_owned()).unwrap(),
    )
}

fn answer(text: &str) -> testkit::FakeProvider {
    testkit::FakeProvider::new(vec![testkit::Script::Chat(LlmResponse {
        r#type: LlmResponseKind::Text,
        content: Some(text.to_owned()),
        tool_calls: None,
        usage: None,
        finish_reason: None,
    })])
}

#[tokio::test]
async fn the_summarizer_writes_the_days_digest_from_its_changes() {
    let feed = Feed(vec![
        ChangeRecord::Created {
            topic: backups(),
            at: at("2026-11-05T14:00:00Z"),
        },
        ChangeRecord::Decision {
            topic: backups(),
            text: "Keep 14 days of nightly dumps".to_owned(),
            session: None,
            at: at("2026-11-05T15:00:00Z"),
        },
    ]);
    let kept = Arc::new(Kept::default());
    let deps = DailyDigests {
        changes: Arc::new(feed),
        digests: kept.clone(),
    };
    let provider = answer(
        "  The day set up [Backups](/topics/00000000-0000-4000-8000-0000000000a1).\n\nDumps are kept 14 days. ",
    );
    let cancel = CancellationToken::new();
    let env = DigestEnv {
        deps: &deps,
        provider: &provider,
        model: "summarizer-model",
        cancel: &cancel,
    };
    handle_digest_day(&env, &request()).await.unwrap();
    let written = kept.0.lock().await.clone();
    let [digest] = written.as_slice() else {
        panic!("one digest is kept: {written:?}");
    };
    assert_eq!(digest.day, "2026-11-05".parse::<NaiveDate>().unwrap());
    assert_eq!(digest.owner.as_str(), "owner");
    assert_eq!(digest.changes, 2);
    assert_eq!(
        digest.summary,
        format!("The day set up [Backups](/topics/{BACKUPS}). Dumps are kept 14 days.")
    );
    let requests = provider.chat_requests().await;
    let [asked] = requests.as_slice() else {
        panic!("one model call: {requests:?}");
    };
    assert_eq!(asked.model, "summarizer-model");
    let prompt = match asked.messages.get(1).map(|message| &message.content) {
        Some(MessageContent::Text(text)) => text.clone(),
        other => panic!("the prompt is text: {other:?}"),
    };
    assert!(
        prompt.starts_with("Day: Thursday, November 5, 2026"),
        "{prompt}"
    );
    assert!(
        prompt.contains(&format!(
            "- decision on [Backups](/topics/{BACKUPS}): Keep 14 days of nightly dumps"
        )),
        "{prompt}"
    );
}

#[tokio::test]
async fn a_day_without_changes_asks_no_model_and_keeps_nothing() {
    let kept = Arc::new(Kept::default());
    let deps = DailyDigests {
        changes: Arc::new(Feed(Vec::new())),
        digests: kept.clone(),
    };
    let provider = answer("unused");
    let cancel = CancellationToken::new();
    let env = DigestEnv {
        deps: &deps,
        provider: &provider,
        model: "summarizer-model",
        cancel: &cancel,
    };
    handle_digest_day(&env, &request()).await.unwrap();
    assert_eq!(*kept.0.lock().await, []);
    assert_eq!(provider.chat_requests().await.len(), 0, "no model call");
}
