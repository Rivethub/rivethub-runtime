#![cfg(test)]

#[expect(unreachable_pub)]
#[expect(dead_code)]
mod common;

use std::sync::Arc;
use std::time::Duration;

use common::HangEmbed;
use common::Store;
use common::embed_target;
use common::same_bits;
use common::settings;
use common::target_payload;
use common::wait_bool;
use common::width;
use kernel::model::Embedding;
use testkit::FakeProvider;
use testkit::Script;
use tokio_util::sync::CancellationToken;
use wire::JsonObject;
use workers::EmbedSettings;
use workers::EmbedTable;
use workers::EmbedTarget;
use workers::ParsedTarget;
use workers::Unembeddable;
use workers::WorkerError;
use workers::classify_unembeddable;
use workers::compose_message;
use workers::content_hash;
use workers::mean_pool;
use workers::read_target;
use workers::split_embed_text;

const ABC_SHA: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

#[test]
fn classify_rejects_media_and_keeps_ordinary_text() {
    assert_eq!(
        classify_unembeddable("  [media attached: photo]"),
        Some(Unembeddable::MediaMarker)
    );
    assert_eq!(classify_unembeddable("note [media attached: photo]"), None);
    assert_eq!(
        classify_unembeddable("MEDIA:clip"),
        Some(Unembeddable::MediaPrefix)
    );
    assert_eq!(classify_unembeddable("see MEDIA:clip"), None);
    assert_eq!(classify_unembeddable("[media attached]"), None);
    assert_eq!(classify_unembeddable("a normal sentence"), None);
}

#[test]
fn classify_base64_uses_the_recorded_thresholds() {
    let png = format!("iVBORw0KGgo{}", "A".repeat(200));
    assert_eq!(classify_unembeddable(&png), Some(Unembeddable::Base64Png));
    assert_eq!(
        classify_unembeddable(&format!("iVBORw0KGgo{}", "A".repeat(199))),
        None
    );
    assert_eq!(
        classify_unembeddable(&format!("/9j/{}", "A".repeat(500))),
        Some(Unembeddable::Base64Jpeg)
    );
    assert_eq!(
        classify_unembeddable(&format!("/9j/{}", "A".repeat(499))),
        None
    );
    let data = format!("data:image/png;base64,{}", "A".repeat(200));
    assert_eq!(
        classify_unembeddable(&data),
        Some(Unembeddable::Base64DataUrl)
    );
    assert_eq!(
        classify_unembeddable("data:image/png;base64,aGVsbG8="),
        None
    );
    assert_eq!(
        classify_unembeddable(&"A".repeat(4_000)),
        Some(Unembeddable::Base64Blob)
    );
    assert_eq!(
        classify_unembeddable(&format!("prompt{}", "p".repeat(4_000))),
        Some(Unembeddable::Base64Blob)
    );
    assert_eq!(classify_unembeddable(&mixed_base64(1_499)), None);
    assert_eq!(
        classify_unembeddable(&mixed_base64(1_500)),
        Some(Unembeddable::Base64Blob)
    );
    assert_eq!(
        classify_unembeddable(&format!("{}{}", mixed_base64(3_800), " ".repeat(200))),
        None
    );
    let dense = format!("{}{}", mixed_base64(3_801), " ".repeat(199));
    assert_eq!(
        classify_unembeddable(&dense),
        Some(Unembeddable::Base64Blob)
    );
    assert_eq!(classify_unembeddable("the word base64 is ordinary"), None);
}

fn mixed_base64(count: usize) -> String {
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
        .chars()
        .cycle()
        .take(count)
        .collect()
}

#[test]
fn compose_trims_joins_and_caps_by_scalar() {
    assert_eq!(compose_message("  hi  ", None, 10), "hi");
    assert_eq!(compose_message("hi", Some("  there "), 10), "hi\nthere");
    assert_eq!(compose_message("  ", Some(" tool "), 10), "tool");
    assert_eq!(compose_message("  ", Some("   "), 10), "");
    assert_eq!(compose_message("hi", Some(&"x".repeat(20)), 4), "hi\nxxxx");
    assert_eq!(compose_message("", Some("😀😀😀"), 2), "😀😀");
}

#[test]
fn split_prefers_a_break_inside_the_last_window() {
    assert_split("01234567\n\nXYZ", 10, &["01234567\n\n", "XYZ"]);
    assert_split("01234567\nXYZW", 10, &["01234567\n", "XYZW"]);
    assert_split("01234567. XY", 10, &["01234567. ", "XY"]);
    assert_split("0123456789ABCDEF", 10, &["0123456789", "ABCDEF"]);
    assert_split("\n\n0123456789MORE", 10, &["\n\n01234567", "89MORE"]);
    let emoji = "aaaa😀bbbbb";
    let chunks = split_embed_text(emoji, 6).expect("split");
    let first = chunks.first().expect("chunk");
    assert!(first.text.contains('😀'));
    assert_eq!(
        chunks
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect::<String>(),
        emoji
    );
    assert!(matches!(
        split_embed_text("abc", 0),
        Err(WorkerError::ChunkWindow)
    ));
}

#[test]
fn mean_pool_fits_vectors_and_keeps_zeros() {
    let averaged = mean_pool(&[vec![1.0, 3.0], vec![3.0, 5.0]], 2).expect("pool");
    assert!(same_bits(averaged.as_deref().expect("vector"), &[2.0, 4.0]));
    let skipped = mean_pool(&[vec![], vec![1.0, 2.0]], 2).expect("pool");
    assert!(same_bits(skipped.as_deref().expect("vector"), &[1.0, 2.0]));
    let truncated = mean_pool(&[vec![9.0, 8.0, 7.0]], 2).expect("pool");
    assert!(same_bits(
        truncated.as_deref().expect("vector"),
        &[9.0, 8.0]
    ));
    let short = mean_pool(&[vec![1.0]], 2).expect("pool");
    assert!(same_bits(short.as_deref().expect("vector"), &[1.0]));
    let mixed = mean_pool(&[vec![1.0, 3.0], vec![9.0], vec![3.0, 5.0]], 2).expect("pool");
    assert!(same_bits(mixed.as_deref().expect("vector"), &[2.0, 4.0]));
    let skipped_long = mean_pool(&[vec![1.0, 3.0], vec![9.0, 9.0, 9.0]], 8).expect("pool");
    assert!(same_bits(
        skipped_long.as_deref().expect("vector"),
        &[1.0, 3.0]
    ));
    assert!(mean_pool(&[Vec::new()], 2).expect("pool").is_none());
    assert!(mean_pool(&[], 2).expect("pool").is_none());
    let zeros = mean_pool(&[vec![0.0, 0.0]], 2).expect("pool");
    assert!(same_bits(zeros.as_deref().expect("vector"), &[0.0, 0.0]));
    assert!(matches!(
        mean_pool(&[vec![1.0]], 0),
        Err(WorkerError::Width)
    ));
}

#[test]
fn content_hash_is_sha256_hex() {
    assert_eq!(content_hash("abc"), ABC_SHA);
}

#[test]
fn width_comes_from_postgres_config() {
    let absent =
        workers::EmbedSettings::from_config(&config_with(None, Some(false))).expect("dims");
    assert_eq!(absent.width, 1024);
    assert_eq!(
        workers::EmbedSettings::from_config(&config::Config::default())
            .expect("default")
            .width,
        workers::EmbedSettings::standard().width
    );
    assert_eq!(
        workers::EmbedSettings::from_config(&config_with(Some(768), None))
            .expect("width")
            .width,
        768
    );
    assert!(matches!(
        workers::EmbedSettings::from_config(&config_with(Some(0), None)),
        Err(WorkerError::Width)
    ));
    assert_eq!(
        workers::EmbedSettings::standard().timeout,
        Duration::from_secs(30)
    );
}

#[test]
fn payload_parse_accepts_postgres_spacing_and_rejects_wrong_types() {
    let spaced: JsonObject =
        serde_json::from_str(r#"{"targetTable" : "ros_messages", "targetId" : "msg-1"}"#)
            .expect("json");
    assert_eq!(
        read_target(&spaced).expect("target"),
        ParsedTarget::Ready(EmbedTarget {
            table: EmbedTable::Messages,
            id: "msg-1".to_owned(),
        })
    );
    for table in [
        EmbedTable::Messages,
        EmbedTable::Summaries,
        EmbedTable::WikiTopics,
        EmbedTable::MessageChunks,
    ] {
        assert_eq!(
            read_target(&target_payload(&table.to_string(), "row-1")).expect("target"),
            ParsedTarget::Ready(EmbedTarget {
                table,
                id: "row-1".to_owned(),
            })
        );
    }
    assert_eq!(
        read_target(&target_payload("ros_nope", "msg-1")).expect("drop"),
        ParsedTarget::UnknownTable
    );
    assert_eq!(
        read_target(&target_payload("ros_nope", "")).expect("drop"),
        ParsedTarget::UnknownTable
    );
    assert_eq!(
        read_target(&target_payload("ros_messages", "")).expect("drop"),
        ParsedTarget::EmptyId
    );
    let wrong: JsonObject =
        serde_json::from_str(r#"{"targetTable": 1, "targetId": "msg-1"}"#).expect("json");
    assert!(matches!(
        read_target(&wrong),
        Err(WorkerError::Payload { .. })
    ));
}

#[tokio::test]
async fn drops_do_not_call_the_provider() {
    let provider = fnv();
    let store = Store::new();
    assert!(
        embed_id(&store, &provider, settings(), "ros_nope", "m")
            .await
            .is_ok()
    );
    assert!(
        embed_id(&store, &provider, settings(), "ros_messages", "")
            .await
            .is_ok()
    );
    assert!(
        embed_id(&store, &provider, settings(), "ros_messages", "missing")
            .await
            .is_ok()
    );
    store
        .insert(EmbedTable::Messages, "blank", "  ", None)
        .await
        .expect("seed");
    assert!(
        embed_id(&store, &provider, settings(), "ros_messages", "blank")
            .await
            .is_ok()
    );
    let requests = provider.embed_requests().await;
    assert_eq!(requests, Vec::<Vec<String>>::new());
    assert_eq!(store.writes().await, 0);
}

#[tokio::test]
async fn unembeddable_text_is_marked_and_ordinary_text_is_embedded() {
    let marked = Store::new();
    marked
        .insert(EmbedTable::Messages, "m", "[media attached: x]", None)
        .await
        .expect("seed");
    let provider = fnv();
    embed_id(&marked, &provider, settings(), "ros_messages", "m")
        .await
        .expect("drop");
    assert_eq!(
        marked.unembeddable(EmbedTable::Messages, "m").await,
        Some(Unembeddable::MediaMarker)
    );
    let requests = provider.embed_requests().await;
    assert_eq!(requests, Vec::<Vec<String>>::new());

    let ordinary = Store::new();
    ordinary
        .insert(EmbedTable::Messages, "m", "see the media note", None)
        .await
        .expect("seed");
    let provider = fnv();
    embed_id(&ordinary, &provider, settings(), "ros_messages", "m")
        .await
        .expect("embed");
    let stored = ordinary
        .written(EmbedTable::Messages, "m")
        .await
        .expect("write");
    assert_eq!(stored.vector.len(), width());
    assert_eq!(stored.content_hash, content_hash("see the media note"));
    assert_eq!(
        provider.embed_requests().await,
        vec![vec!["see the media note".to_owned()]]
    );
}

#[tokio::test]
async fn null_embeddings_retry_then_mark_permanent() {
    let store = message_store("hello").await;
    let provider = FakeProvider::new(vec![
        Script::Embed(vec![Embedding { vector: Vec::new() }]),
        Script::Embed(vec![Embedding { vector: Vec::new() }]),
        Script::Embed(vec![Embedding { vector: Vec::new() }]),
    ]);
    let first = embed_message(&store, &provider, settings())
        .await
        .expect_err("null");
    assert_eq!(first.to_string(), "Embedding returned null");
    assert_eq!(store.failures(EmbedTable::Messages, "m").await, 1);
    assert_eq!(
        store
            .embed_error(EmbedTable::Messages, "m")
            .await
            .as_deref(),
        Some("Embedding returned null")
    );
    assert!(
        store
            .embed_status(EmbedTable::Messages, "m")
            .await
            .is_none()
    );
    assert!(!store.permanent(EmbedTable::Messages, "m").await);
    embed_message(&store, &provider, settings())
        .await
        .expect_err("null");
    assert_eq!(store.failures(EmbedTable::Messages, "m").await, 2);
    embed_message(&store, &provider, settings())
        .await
        .expect("permanent");
    assert_eq!(store.failures(EmbedTable::Messages, "m").await, 3);
    assert!(store.permanent(EmbedTable::Messages, "m").await);
    assert_eq!(
        store
            .embed_error(EmbedTable::Messages, "m")
            .await
            .as_deref(),
        Some("Embedding returned null (permanent)")
    );
    assert_eq!(
        store
            .embed_status(EmbedTable::Messages, "m")
            .await
            .as_deref(),
        Some("failed")
    );
    assert!(store.written(EmbedTable::Messages, "m").await.is_none());
}

#[tokio::test]
async fn a_later_embedding_clears_the_null_mark() {
    let store = message_store("hello").await;
    let provider = FakeProvider::new(vec![
        Script::Embed(vec![Embedding { vector: Vec::new() }]),
        Script::Embed(vec![Embedding {
            vector: vec![1.0; 8],
        }]),
    ]);
    embed_message(&store, &provider, settings())
        .await
        .expect_err("null");
    embed_message(&store, &provider, settings())
        .await
        .expect("write");
    assert_eq!(store.failures(EmbedTable::Messages, "m").await, 0);
    assert!(!store.permanent(EmbedTable::Messages, "m").await);
    assert!(store.embed_error(EmbedTable::Messages, "m").await.is_none());
    assert!(
        store
            .embed_status(EmbedTable::Messages, "m")
            .await
            .is_none()
    );
    let stored = store
        .written(EmbedTable::Messages, "m")
        .await
        .expect("stored");
    assert_eq!(stored.vector.len(), width());
}

#[tokio::test]
async fn provider_failures_do_not_poison_the_row() {
    let store = message_store("hello").await;
    let provider = FakeProvider::new(vec![Script::EmbedHttpStatus(503)]);
    let error = embed_message(&store, &provider, settings())
        .await
        .expect_err("http");
    assert!(matches!(error, WorkerError::Embed(_)));
    assert!(error.to_string().contains("503"));
    assert_eq!(store.failures(EmbedTable::Messages, "m").await, 0);
    assert!(!store.permanent(EmbedTable::Messages, "m").await);
}

#[tokio::test]
async fn a_consistent_short_pool_is_stored_and_a_mixed_pool_keeps_the_shared_length() {
    let mut embed = settings();
    embed.chars_per_chunk = 4;
    let store = message_store("abcdefghij").await;
    let short = FakeProvider::new(vec![Script::Embed(vec![
        Embedding { vector: vec![1.0] },
        Embedding { vector: vec![3.0] },
        Embedding { vector: vec![5.0] },
    ])]);
    embed_message(&store, &short, embed)
        .await
        .expect("short pool");
    let stored = store
        .written(EmbedTable::Messages, "m")
        .await
        .expect("stored");
    assert!(same_bits(&stored.vector, &[3.0]));
    assert_eq!(stored.chunks.len(), 3);
    assert!(
        stored
            .chunks
            .iter()
            .all(|chunk| chunk.vector.as_ref().map(Vec::len) == Some(1))
    );
    assert_eq!(store.failures(EmbedTable::Messages, "m").await, 0);
    assert!(
        store
            .embed_status(EmbedTable::Messages, "m")
            .await
            .is_none()
    );

    let mixed_store = message_store("abcdefghij").await;
    let mixed = FakeProvider::new(vec![Script::Embed(vec![
        Embedding {
            vector: vec![1.0; 8],
        },
        Embedding { vector: vec![9.0] },
        Embedding {
            vector: vec![3.0; 8],
        },
    ])]);
    embed_message(&mixed_store, &mixed, embed)
        .await
        .expect("mixed pool");
    let stored = mixed_store
        .written(EmbedTable::Messages, "m")
        .await
        .expect("stored");
    assert!(same_bits(&stored.vector, &[2.0; 8]));
    assert_eq!(stored.chunks.len(), 2);
    assert!(
        stored
            .chunks
            .iter()
            .all(|chunk| chunk.vector.as_ref().map(Vec::len) == Some(8))
    );
    assert_eq!(mixed_store.failures(EmbedTable::Messages, "m").await, 0);
    assert!(
        mixed_store
            .embed_error(EmbedTable::Messages, "m")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn the_provider_must_return_one_vector_per_chunk() {
    let mut embed = settings();
    embed.chars_per_chunk = 4;
    let store = message_store("abcdefghij").await;
    let provider = FakeProvider::new(vec![Script::Embed(vec![Embedding {
        vector: vec![1.0; 8],
    }])]);
    let error = embed_message(&store, &provider, embed)
        .await
        .expect_err("count");
    assert!(error.to_string().contains("different number"));
    assert_eq!(store.failures(EmbedTable::Messages, "m").await, 0);
}

#[tokio::test]
async fn zeros_are_stored_and_longer_vectors_are_truncated() {
    let store = message_store("hello").await;
    let zeros = FakeProvider::new(vec![Script::Embed(vec![Embedding {
        vector: vec![0.0; 8],
    }])]);
    embed_message(&store, &zeros, settings())
        .await
        .expect("write");
    let stored = store
        .written(EmbedTable::Messages, "m")
        .await
        .expect("stored");
    assert!(same_bits(&stored.vector, &[0.0; 8]));

    let mut narrow = settings();
    narrow.width = 2;
    let longer = FakeProvider::new(vec![Script::Embed(vec![Embedding {
        vector: vec![1.0, 2.0, 3.0, 4.0],
    }])]);
    embed_message(&store, &longer, narrow).await.expect("write");
    let stored = store
        .written(EmbedTable::Messages, "m")
        .await
        .expect("stored");
    assert!(same_bits(&stored.vector, &[1.0, 2.0]));
}

#[tokio::test]
async fn long_messages_embed_as_one_batch_and_store_chunks() {
    let mut embed = settings();
    embed.chars_per_chunk = 10;
    let text = "0123456789ABCDEFGHIJ";
    let store = message_store(text).await;
    let provider = fnv();
    embed_message(&store, &provider, embed)
        .await
        .expect("write");
    let requests = provider.embed_requests().await;
    let batch = requests.first().expect("one batch");
    assert_eq!(requests.len(), 1);
    assert_eq!(batch.join(""), text);
    let stored = store
        .written(EmbedTable::Messages, "m")
        .await
        .expect("stored");
    assert_eq!(stored.vector.len(), width());
    assert!(stored.chunks.len() > 1);
    assert_eq!(stored.content_hash, content_hash(text));
    assert_eq!(
        stored
            .chunks
            .iter()
            .map(|chunk| chunk.content.as_str())
            .collect::<String>(),
        text
    );
}

#[tokio::test]
async fn a_short_body_still_chunks_when_the_tool_result_is_long() {
    let mut embed = settings();
    embed.chars_per_chunk = 10;
    let store = Store::new();
    store
        .insert(EmbedTable::Messages, "m", "hi", Some(&"x".repeat(20)))
        .await
        .expect("seed");
    let provider = fnv();
    embed_message(&store, &provider, embed)
        .await
        .expect("write");
    let stored = store
        .written(EmbedTable::Messages, "m")
        .await
        .expect("stored");
    let composed = compose_message("hi", Some(&"x".repeat(20)), 32_768);
    assert!(stored.chunks.len() > 1);
    assert_eq!(
        stored
            .chunks
            .iter()
            .map(|chunk| chunk.content.as_str())
            .collect::<String>(),
        composed
    );
}

#[tokio::test]
async fn disabled_chunks_skip_chunk_rows_and_chunk_jobs() {
    let mut embed = settings();
    embed.chunks_enabled = false;
    embed.chars_per_chunk = 10;
    let store = message_store("0123456789ABCDEFGHIJ").await;
    let provider = fnv();
    embed_message(&store, &provider, embed)
        .await
        .expect("write");
    let stored = store
        .written(EmbedTable::Messages, "m")
        .await
        .expect("stored");
    assert_eq!(stored.chunks, Vec::new());
    assert_eq!(provider.embed_requests().await.len(), 1);

    let store = Store::new();
    store
        .insert(EmbedTable::MessageChunks, "c", "chunk body", None)
        .await
        .expect("seed");
    let provider = fnv();
    embed_id(&store, &provider, embed, "ros_message_chunks", "c")
        .await
        .expect("drop");
    let requests = provider.embed_requests().await;
    assert_eq!(requests, Vec::<Vec<String>>::new());
    assert!(
        store
            .written(EmbedTable::MessageChunks, "c")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn summaries_embed_trimmed_content_without_the_tool_result() {
    let store = Store::new();
    store
        .insert(EmbedTable::Summaries, "s", "  hello  ", Some("tool"))
        .await
        .expect("seed");
    let provider = fnv();
    embed_id(&store, &provider, settings(), "ros_summaries", "s")
        .await
        .expect("embed");
    assert_eq!(
        provider.embed_requests().await,
        vec![vec!["hello".to_owned()]]
    );
    let stored = store
        .written(EmbedTable::Summaries, "s")
        .await
        .expect("stored");
    assert_eq!(stored.content_hash, content_hash("hello"));
    assert_eq!(stored.chunks, Vec::new());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn embed_timeout_does_not_poison_the_row() {
    let store = Arc::new(message_store("hello").await);
    let provider = Arc::new(HangEmbed::new());
    let payload = target_payload("ros_messages", "m");
    let cancel = CancellationToken::new();
    let embed = settings();
    let store_task = Arc::clone(&store);
    let provider_task = Arc::clone(&provider);
    let run = tokio::spawn(async move {
        embed_target(
            &store_task,
            provider_task.as_ref(),
            embed,
            &payload,
            &cancel,
        )
        .await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(29)).await;
    tokio::task::yield_now().await;
    assert!(!run.is_finished());
    tokio::time::advance(Duration::from_secs(1)).await;
    let error = run.await.expect("join").expect_err("timeout");
    assert!(matches!(error, WorkerError::EmbedTimeout));
    assert_eq!(store.failures(EmbedTable::Messages, "m").await, 0);
    assert!(!store.permanent(EmbedTable::Messages, "m").await);
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_during_embed_does_not_poison_the_row() {
    let store = Arc::new(message_store("hello").await);
    let provider = Arc::new(HangEmbed::new());
    let mut started = provider.started();
    let payload = target_payload("ros_messages", "m");
    let cancel = CancellationToken::new();
    let embed = settings();
    let store_task = Arc::clone(&store);
    let provider_task = Arc::clone(&provider);
    let token = cancel.clone();
    let run = tokio::spawn(async move {
        embed_target(&store_task, provider_task.as_ref(), embed, &payload, &token).await
    });
    wait_bool(&mut started, true).await;
    cancel.cancel();
    let error = run.await.expect("join").expect_err("cancel");
    assert!(matches!(error, WorkerError::Cancelled));
    assert_eq!(store.failures(EmbedTable::Messages, "m").await, 0);
    assert!(store.written(EmbedTable::Messages, "m").await.is_none());
}

fn assert_split(text: &str, max_chars: usize, expected: &[&str]) {
    let chunks = split_embed_text(text, max_chars).expect("split");
    let texts: Vec<&str> = chunks.iter().map(|chunk| chunk.text.as_str()).collect();
    assert_eq!(texts, expected);
    let mut joined = String::new();
    let mut cursor = 0_u32;
    for chunk in &chunks {
        assert_eq!(chunk.char_start, cursor);
        joined.push_str(&chunk.text);
        cursor = chunk.char_end;
    }
    assert_eq!(joined, text);
    assert_eq!(cursor, u32::try_from(text.chars().count()).expect("length"));
}

fn config_with(dims: Option<u32>, workers_flag: Option<bool>) -> config::Config {
    config::Config {
        memory: Some(config::model::MemorySection {
            postgres: Some(config::model::PostgresSection {
                embed_expected_dims: dims,
                ..config::model::PostgresSection::default()
            }),
            sqlite: Some(config::model::SqliteSection {
                workers: workers_flag,
                ..config::model::SqliteSection::default()
            }),
            ..config::model::MemorySection::default()
        }),
        ..config::Config::default()
    }
}

fn fnv() -> FakeProvider {
    FakeProvider::fnv_embeddings(width())
}

async fn message_store(content: &str) -> Store {
    let store = Store::new();
    store
        .insert(EmbedTable::Messages, "m", content, None)
        .await
        .expect("seed");
    store
}

async fn embed_message(
    store: &Store,
    provider: &FakeProvider,
    embed: EmbedSettings,
) -> Result<(), WorkerError> {
    embed_id(store, provider, embed, "ros_messages", "m").await
}

async fn embed_id(
    store: &Store,
    provider: &FakeProvider,
    embed: EmbedSettings,
    table: &str,
    id: &str,
) -> Result<(), WorkerError> {
    let payload = target_payload(table, id);
    embed_target(store, provider, embed, &payload, &CancellationToken::new()).await
}
