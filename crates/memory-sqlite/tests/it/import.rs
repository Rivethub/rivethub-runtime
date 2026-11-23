use std::collections::BTreeMap;
use std::path::PathBuf;

use memory_sqlite::SqliteStore;
use memory_sqlite::import::{ImportError, ImportReport, ImportTable};
use strum::IntoEnumIterator;
use testkit::Json;
use tokio_util::sync::CancellationToken;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn fixture(name: &str) -> PathBuf {
    testkit::recording(&format!("memory-sqlite/import/{name}"))
}

struct Target {
    directory: tempfile::TempDir,
    store: SqliteStore,
}

impl Target {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("memory.sqlite");
        migrate::sqlite(&path)?;
        Ok(Self {
            directory,
            store: SqliteStore::open(path)?,
        })
    }

    async fn import(&self, text: &str) -> std::result::Result<ImportReport, ImportError> {
        let path = self.directory.path().join("dump.sql");
        std::fs::write(&path, text)?;
        self.store
            .import_pg_dump(path, CancellationToken::new())
            .await
    }

    async fn assert_empty(&self) -> Result {
        for table in ImportTable::iter() {
            let count: i64 = self
                .store
                .with_conn(move |conn| {
                    conn.query_row(&format!("SELECT count(*) FROM ros_{table}"), [], |row| {
                        row.get(0)
                    })
                    .map_err(|source| memory_sqlite::SqliteError::Query {
                        operation: "test count",
                        source,
                    })
                })
                .await?;
            assert_eq!(count, 0, "{table}");
        }
        assert!(self.store.pg_import_report().await?.is_none());
        Ok(())
    }
}

#[tokio::test]
async fn every_table_replays_hand_checked_rows_and_builds_search_indexes() -> Result {
    replay("copy-cases.txt", "expected.json", 2).await
}

#[tokio::test]
async fn real_pre_0020_pg_dump_replays_the_seed_rows_and_indexes() -> Result {
    replay("pre-0020.pg.txt", "pre-0020.expected.json", 1).await
}

async fn replay(dump: &str, expectation: &str, summaries: i64) -> Result {
    let target = Target::new()?;
    let report = target
        .store
        .import_pg_dump(fixture(dump), CancellationToken::new())
        .await?;
    let expected: BTreeMap<ImportTable, Vec<BTreeMap<String, Json>>> =
        serde_json::from_str(&std::fs::read_to_string(fixture(expectation))?)?;
    assert_eq!(report.rows.len(), ImportTable::iter().count());
    for (table, rows) in expected {
        assert_eq!(report.rows.get(&table), Some(&u64::try_from(rows.len())?));
        let query = if table == ImportTable::Meta {
            "SELECT * FROM ros_meta WHERE key = 'embed_model' ORDER BY rowid".to_owned()
        } else {
            format!("SELECT * FROM ros_{table} ORDER BY rowid")
        };
        let actual = target
            .store
            .with_conn(move |conn| Ok(testkit::query_json(conn, &query, &[])))
            .await??;
        assert_eq!(actual.len(), rows.len());
        for (actual, expected) in actual.iter().zip(rows) {
            let actual = actual.as_object().ok_or("expected row object")?;
            let defaults = actual
                .iter()
                .filter(|(column, _)| !expected.contains_key(*column));
            for (column, value) in defaults {
                assert_import_default(table, column, value)?;
            }
            for (column, expected) in expected {
                let actual = actual.get(&column).ok_or("missing column")?;
                if (expected.is_object() || expected.is_array()) && actual.is_string() {
                    let actual: Json = serde_json::from_str(actual.as_str().ok_or("JSON text")?)?;
                    assert_eq!(actual, expected, "{table}.{column}");
                } else {
                    assert_eq!(actual, &expected, "{table}.{column}");
                }
            }
        }
    }
    assert_eq!(target.store.pg_import_report().await?, Some(report));
    for (table, term, count) in [
        ("ros_messages_fts", "quartz", 1),
        ("ros_summaries_fts", "quartz", summaries),
        ("ros_wiki_topics_fts", "quartz", 1),
        ("rh_messages_trigram", "uart", 1),
        ("rh_summaries_trigram", "uart", summaries),
    ] {
        let found: i64 = target
            .store
            .with_conn(move |conn| {
                Ok(conn.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE {table} MATCH ?1"),
                    [term],
                    |row| row.get(0),
                ))
            })
            .await??;
        assert_eq!(found, count, "{table}");
    }
    Ok(())
}

fn assert_import_default(table: ImportTable, column: &str, value: &Json) -> Result {
    match (table, column) {
        (ImportTable::Summaries, "quality_flags" | "index_needs_repair")
        | (ImportTable::Conversations, "title_from_capture") => {
            assert_eq!(value, &Json::from(0), "{table}.{column}");
        }
        (ImportTable::WikiTopics, "history") => {
            assert_eq!(value, &Json::from("[]"), "{table}.{column}");
        }
        (ImportTable::WikiTopics, "visibility") => {
            assert_eq!(value.as_str(), Some("private"), "{table}.{column}");
        }
        (ImportTable::Summaries, _)
        | (ImportTable::SummarySources, "render_key")
        | (ImportTable::WikiTopics, "scope" | "local_slug")
        | (ImportTable::TagTaxonomy, "owner_user_id" | "globally_shared")
        | (ImportTable::WikiRedirects, "owner_user_id") => {
            assert!(value.is_null(), "{table}.{column}");
        }
        _ => return Err(format!("unrecorded {table}.{column}").into()),
    }
    Ok(())
}

#[tokio::test]
async fn a_second_import_refuses_without_changing_rows_or_receipt() -> Result {
    let target = Target::new()?;
    let dump = std::fs::read_to_string(fixture("copy-cases.txt"))?;
    let report = target.import(&dump).await?;
    assert!(matches!(
        target.import(&dump).await,
        Err(ImportError::NonEmpty { .. })
    ));
    assert_eq!(target.store.pg_import_report().await?, Some(report));
    Ok(())
}

#[tokio::test]
async fn malformed_values_and_late_errors_roll_back_every_table() -> Result {
    let original = std::fs::read_to_string(fixture("copy-cases.txt"))?;
    let cases = [
        original.replace("Café", "Ca\0fé"),
        original.replace("[0,5]", "[0,0]"),
        original.replace("[0,5]", "[0,5,1]"),
        original.replace("[0,5]", "[1e999,5]"),
        original.replace("2026-10-10 12:01:00+00", "not-a-time"),
        original.replace("{\"source\":\"import\"}", "[]"),
        original.replace("{old-acme}", "{NULL}"),
        original.replace("{old-acme}", "{\"unfinished}"),
        original.replace("{old-acme}", "{a,}"),
        original.replace("0.75", "NaN"),
        original.replace("\\nline", "\\qline"),
        original.replace("\\nline", "\\000line"),
        original.replace("m1\tc1\tmain", "m1\tmissing\tmain"),
        original.replace("old-acme\tacmekit\t2026", "old-acme\tmissing\t2026"),
        original.replace("\t7\t2026", "\tseven\t2026"),
        original.replace("\tf\t2026", "\ttrue\t2026"),
        original.replace(
            "-- PostgreSQL database dump complete",
            "DROP TABLE ros_messages;",
        ),
        original.replace(
            "COPY public.ros_meta (key, value)",
            "COPY public.ros_meta (key, key)",
        ),
        original.replace(
            "COPY public.ros_meta (key, value)",
            "COPY public.ros_meta (key, unknown)",
        ),
    ];
    for dump in cases {
        let target = Target::new()?;
        assert!(
            target.import(&dump).await.is_err(),
            "must refuse malformed dump"
        );
        target.assert_empty().await?;
    }
    Ok(())
}

#[tokio::test]
async fn truncated_copy_unknown_tables_and_duplicate_blocks_are_refused() -> Result {
    let original = std::fs::read_to_string(fixture("copy-cases.txt"))?;
    let duplicate = original.replace("-- PostgreSQL database dump complete", "COPY public.ros_meta (key, value) FROM stdin;\nother\tvalue\n\\.\n-- PostgreSQL database dump complete");
    for dump in [
        String::new(),
        "-- PostgreSQL database dump complete\n".to_owned(),
        original.replace("-- PostgreSQL database dump complete", ""),
        original.replace(
            "embed_model\tfixture-model\n\\.",
            "embed_model\tfixture-model",
        ),
        original.replace("public.ros_meta", "public.ros_tasks"),
        original.replace("public.ros_meta", "private.ros_meta"),
        original.replace(
            "embed_model\tfixture-model",
            "embed_model\tfixture-model\textra",
        ),
        duplicate,
    ] {
        let target = Target::new()?;
        assert!(target.import(&dump).await.is_err());
        target.assert_empty().await?;
    }
    Ok(())
}

#[tokio::test]
async fn postgres_16_envelope_and_copy_byte_escapes_are_accepted() -> Result {
    let target = Target::new()?;
    let dump = std::fs::read_to_string(fixture("copy-cases.txt"))?
        .replace("\\restrict ImportFixture\n", "")
        .replace("\\unrestrict ImportFixture\n", "")
        .replace("SET transaction_timeout = 0;\n", "")
        .replace(
            "Café 🚀\\nline",
            "Caf\\303\\251 \\xf0\\x9f\\x9a\\x80\\nline",
        );
    target.import(&dump).await?;
    let content: String = target
        .store
        .with_conn(|conn| {
            Ok(conn.query_row("SELECT content FROM ros_messages", [], |row| row.get(0)))
        })
        .await??;
    assert_eq!(content, "Café 🚀\nline\ttab\\slash");
    Ok(())
}

#[tokio::test]
async fn cancellation_and_missing_files_leave_the_target_empty() -> Result {
    let target = Target::new()?;
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        target
            .store
            .import_pg_dump(fixture("copy-cases.txt"), cancellation)
            .await,
        Err(ImportError::Cancelled)
    ));
    assert!(matches!(
        target
            .store
            .import_pg_dump(
                target.directory.path().join("absent.sql"),
                CancellationToken::new()
            )
            .await,
        Err(ImportError::File { .. })
    ));
    target.assert_empty().await
}

#[tokio::test]
async fn simultaneous_imports_commit_exactly_once() -> Result {
    let target = Target::new()?;
    let (first, second) = tokio::join!(
        target
            .store
            .import_pg_dump(fixture("copy-cases.txt"), CancellationToken::new()),
        target
            .store
            .import_pg_dump(fixture("copy-cases.txt"), CancellationToken::new()),
    );
    assert!(matches!(
        (&first, &second),
        (Ok(_), Err(ImportError::NonEmpty { .. })) | (Err(ImportError::NonEmpty { .. }), Ok(_))
    ));
    Ok(())
}

fn summaries(columns: &str, body: &str) -> String {
    format!(
        "COPY public.ros_summaries ({columns}) FROM stdin;\n{body}\n\\.\n-- PostgreSQL database dump complete\n"
    )
}

async fn summary_rows(target: &Target) -> Result<Vec<(String, String, i64)>> {
    let rows = target
        .store
        .with_conn(|conn| {
            Ok(testkit::query_json(
                conn,
                "SELECT id, kind, depth FROM ros_summaries ORDER BY id",
                &[],
            ))
        })
        .await??;
    let mut stored = Vec::new();
    for row in rows {
        let object = row.as_object().ok_or("expected row object")?;
        let id = object
            .get("id")
            .and_then(testkit::Json::as_str)
            .ok_or("id")?
            .to_owned();
        let kind = object
            .get("kind")
            .and_then(testkit::Json::as_str)
            .ok_or("kind")?
            .to_owned();
        let depth = object
            .get("depth")
            .and_then(testkit::Json::as_i64)
            .ok_or("depth")?;
        stored.push((id, kind, depth));
    }
    Ok(stored)
}

async fn refuses(dump: &str, detail: &str) -> Result {
    let target = Target::new()?;
    match target.import(dump).await {
        Err(ImportError::Format { detail: found, .. }) => assert_eq!(found, detail),
        other => return Err(format!("expected a format error, got {other:?}").into()),
    }
    target.assert_empty().await
}

#[tokio::test]
async fn imported_legacy_summary_kinds_follow_depth() -> Result {
    let target = Target::new()?;
    let dump = summaries(
        "id, depth, kind, content, created_at",
        "d1\t1\tcondensed\tcondensed one\t2026-10-10 12:00:00+00
d2\t2\tcondensed\tcondensed two\t2026-10-10 12:00:00+00
k0\t0\tbranch\tkept branch\t2026-10-10 12:00:00+00
k2\t2\tleaf\tkept leaf\t2026-10-10 12:00:00+00
kn\t-3\tleaf\tkept negative\t2026-10-10 12:00:00+00
kr\t1\troot\tkept root\t2026-10-10 12:00:00+00
u0\t0\trollup\tunknown zero\t2026-10-10 12:00:00+00
u4\t4\trollup\tunknown four\t2026-10-10 12:00:00+00",
    );
    target.import(&dump).await?;
    assert_eq!(
        summary_rows(&target).await?,
        vec![
            ("d1".to_owned(), "branch".to_owned(), 1),
            ("d2".to_owned(), "root".to_owned(), 2),
            ("k0".to_owned(), "branch".to_owned(), 0),
            ("k2".to_owned(), "leaf".to_owned(), 2),
            ("kn".to_owned(), "leaf".to_owned(), -3),
            ("kr".to_owned(), "root".to_owned(), 1),
            ("u0".to_owned(), "leaf".to_owned(), 0),
            ("u4".to_owned(), "root".to_owned(), 4),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_canonical_summary_kind_imports_without_a_depth_column() -> Result {
    let target = Target::new()?;
    let dump = summaries(
        "id, kind, content, created_at",
        "keep\tleaf\tkept leaf\t2026-10-10 12:00:00+00",
    );
    target.import(&dump).await?;
    assert_eq!(
        summary_rows(&target).await?,
        vec![("keep".to_owned(), "leaf".to_owned(), 0)]
    );
    Ok(())
}

#[tokio::test]
async fn a_legacy_summary_kind_without_depth_rolls_back() -> Result {
    refuses(
        &summaries(
            "id, kind, content, created_at",
            "d1\tcondensed\tcondensed one\t2026-10-10 12:00:00+00",
        ),
        "legacy summary kind needs its depth",
    )
    .await
}

#[tokio::test]
async fn a_legacy_summary_kind_with_null_depth_rolls_back() -> Result {
    refuses(
        &summaries(
            "id, depth, kind, content, created_at",
            "d1\t\\N\tcondensed\tcondensed one\t2026-10-10 12:00:00+00",
        ),
        "legacy summary kind needs its depth",
    )
    .await
}

#[tokio::test]
async fn a_legacy_summary_kind_below_zero_rolls_the_import_back() -> Result {
    refuses(
        &summaries(
            "id, depth, kind, content, created_at",
            "bad\t-1\tcondensed\tbelow zero\t2026-10-10 12:00:00+00",
        ),
        "legacy summary kind has no depth mapping",
    )
    .await
}
