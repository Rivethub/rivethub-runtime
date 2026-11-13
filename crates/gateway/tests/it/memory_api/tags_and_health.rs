use super::*;
use kernel::memory::EmbedModelMismatch;
use wire::gateway::memory::EmbedStatus;
use wire::gateway::memory::MemoryHealthResponse;
use wire::gateway::memory::MemoryHealthStatus;

#[test]
fn tags_read_routes_cover_the_port_surface() {
    let (_store, app) = seeded().expect("seed store");
    let call = |method: &str, path: &str| {
        block_on(answer(
            app.clone(),
            recorded(method, &format!("/api/memory/tags{path}"), Vec::new()),
        ))
    };
    let response = call("GET", "").expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: TagListResult = serde_json::from_slice(&response.body).expect("tags response");
    assert_eq!(parsed.tags, Vec::new());

    let response = call("GET", "/pending").expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: PendingTagListResult =
        serde_json::from_slice(&response.body).expect("pending response");
    assert_eq!(parsed.tags, Vec::new());

    let response = call("GET", "/counts").expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: TagCountsResult = serde_json::from_slice(&response.body).expect("counts response");
    assert_eq!(parsed.counts, Vec::new());

    let response = call("GET", "/taxonomy").expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: TagTaxonomyListResult =
        serde_json::from_slice(&response.body).expect("taxonomy response");
    assert_eq!(parsed.entries, Vec::new());

    let response = call("GET", "/decide").expect("in-process answer");
    assert_eq!(response.status, 405);

    let response = call("GET", "/nope").expect("in-process answer");
    assert_eq!(response.status, 404);
    assert_eq!(error_of(&response).error, "unknown tags resource");
}

#[test]
fn tags_read_routes_reject_bad_params() {
    let (_store, app) = seeded().expect("seed store");
    let call = |target: &str| block_on(answer(app.clone(), recorded("GET", target, Vec::new())));
    let response = call("/api/memory/tags?entity_type=bogus").expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "bad entity_type");

    let response = call("/api/memory/tags?entity_id=zz").expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "bad entity_id");

    let response = call("/api/memory/tags?state=bogus").expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "bad state");

    let response = call("/api/memory/tags?limit=abc").expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "invalid limit");
}

#[test]
fn tags_decide_validates() {
    let (_store, app) = seeded().expect("seed store");
    let decide = |request: &MemoryTagsDecideRequest| post(&app, "/api/memory/tags/decide", request);
    let response = decide(&MemoryTagsDecideRequest {
        ids: Some(Vec::new()),
        state: Some(TagState::Accepted),
        decided_by: None,
    })
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "ids required");

    let response = decide(&MemoryTagsDecideRequest {
        ids: Some(vec!["t1".to_owned()]),
        state: Some(TagState::Suggested),
        decided_by: None,
    })
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(
        error_of(&response).error,
        "state must be accepted or rejected"
    );

    let response = decide(&MemoryTagsDecideRequest {
        ids: Some(vec!["t1".to_owned()]),
        state: Some(TagState::Accepted),
        decided_by: None,
    })
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "ids must be uuids");

    let response = decide(&MemoryTagsDecideRequest {
        ids: Some(vec![
            "00000000-0000-4000-8000-000000000000".to_owned();
            1001
        ]),
        state: Some(TagState::Accepted),
        decided_by: None,
    })
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "at most 1000 ids");
}

#[test]
fn tags_decide_reports_changed() {
    let (_store, app) = seeded().expect("seed store");
    let response = post(
        &app,
        "/api/memory/tags/decide",
        &MemoryTagsDecideRequest {
            ids: Some(vec![
                "00000000-0000-4000-8000-000000000001".to_owned(),
                "00000000-0000-4000-8000-000000000002".to_owned(),
            ]),
            state: Some(TagState::Rejected),
            decided_by: Some("owner".to_owned()),
        },
    )
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: TagDecisionResult =
        serde_json::from_slice(&response.body).expect("decide response");
    assert_eq!(parsed.changed, 0);
}

#[test]
fn tags_add_validates() {
    let (_store, app) = seeded().expect("seed store");
    let add = |request: &MemoryTagsAddRequest| post(&app, "/api/memory/tags/add", request);
    let response = add(&MemoryTagsAddRequest {
        entity_type: None,
        entity_id: Some("00000000-0000-4000-8000-000000000003".to_owned()),
        session_key: None,
        agent: None,
        tag: Some("project:acmekit".to_owned()),
        key: None,
        value: None,
        display: None,
        reason: None,
        decided_by: None,
    })
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(
        error_of(&response).error,
        "entity_type and entity_id (or session_key) required"
    );

    let response = add(&MemoryTagsAddRequest {
        entity_type: Some("conversation".to_owned()),
        entity_id: Some("zz".to_owned()),
        session_key: None,
        agent: None,
        tag: Some("project:acmekit".to_owned()),
        key: None,
        value: None,
        display: None,
        reason: None,
        decided_by: None,
    })
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "bad entity_id");

    let response = add(&MemoryTagsAddRequest {
        entity_type: Some("widget".to_owned()),
        entity_id: Some("00000000-0000-4000-8000-000000000003".to_owned()),
        session_key: None,
        agent: None,
        tag: Some("project:acmekit".to_owned()),
        key: None,
        value: None,
        display: None,
        reason: None,
        decided_by: None,
    })
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "bad entity_type");

    let response = add(&MemoryTagsAddRequest {
        entity_type: Some("conversation".to_owned()),
        entity_id: None,
        session_key: Some(SESSION.to_owned()),
        agent: None,
        tag: Some("nokey".to_owned()),
        key: None,
        value: None,
        display: None,
        reason: None,
        decided_by: None,
    })
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(
        error_of(&response).error,
        "invalid tag literal \"nokey\" (want key:value)"
    );
}

#[test]
fn tags_add_returns_the_tag() {
    let (_store, app) = seeded().expect("seed store");
    let response = post(
        &app,
        "/api/memory/tags/add",
        &MemoryTagsAddRequest {
            entity_type: Some("conversation".to_owned()),
            entity_id: Some("00000000-0000-4000-8000-000000000003".to_owned()),
            session_key: None,
            agent: None,
            tag: Some("project:acmekit".to_owned()),
            key: None,
            value: None,
            display: Some("AcmeKit".to_owned()),
            reason: None,
            decided_by: None,
        },
    )
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: TagAddResult = serde_json::from_slice(&response.body).expect("add response");
    assert_eq!(parsed.tag.key, "project");
    assert_eq!(parsed.tag.value, "acmekit");
    assert_eq!(parsed.tag.state, TagState::Accepted);
}

#[test]
fn tags_lookup_validates() {
    let (_store, app) = seeded().expect("seed store");
    let lookup = |request: &MemoryTagsLookupRequest| post(&app, "/api/memory/tags/lookup", request);
    let response = lookup(&MemoryTagsLookupRequest {
        session_keys: Some(Vec::new()),
        states: None,
    })
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "session_keys required");

    let response = lookup(&MemoryTagsLookupRequest {
        session_keys: Some(vec!["k".to_owned(); 501]),
        states: None,
    })
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "at most 500 session_keys");

    let response = lookup(&MemoryTagsLookupRequest {
        session_keys: Some(vec![SESSION.to_owned()]),
        states: Some(vec!["bogus".to_owned()]),
    })
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "bad state");
}

#[test]
fn tags_taxonomy_upsert_validates_and_returns_the_entry() {
    let (_store, app) = seeded().expect("seed store");
    let response = post(
        &app,
        "/api/memory/tags/taxonomy",
        &TagTaxonomyUpsertRequest {
            key: None,
            value: Some("acmekit".to_owned()),
            display: None,
            parent_value: None,
            aliases: None,
            reason: None,
            decided_by: None,
        },
    )
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "key and value required");

    let response = post(
        &app,
        "/api/memory/tags/taxonomy",
        &TagTaxonomyUpsertRequest {
            key: Some("project".to_owned()),
            value: Some("acmekit".to_owned()),
            display: Some("AcmeKit".to_owned()),
            parent_value: None,
            aliases: Some(vec!["acme".to_owned()]),
            reason: None,
            decided_by: None,
        },
    )
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: TagTaxonomyUpsertResult =
        serde_json::from_slice(&response.body).expect("upsert response");
    assert_eq!(parsed.entry.key, "project");
    assert_eq!(parsed.entry.value, "acmekit");
}

#[test]
fn tags_taxonomy_decide_validates_and_reports_changed() {
    let (_store, app) = seeded().expect("seed store");
    let response = post(
        &app,
        "/api/memory/tags/taxonomy/decide",
        &TagTaxonomyDecideRequest {
            entries: None,
            state: Some(TagState::Accepted),
            decided_by: None,
        },
    )
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "entries required");

    let response = post(
        &app,
        "/api/memory/tags/taxonomy/decide",
        &TagTaxonomyDecideRequest {
            entries: Some(vec![wire::gateway::memory::TagEntryPair {
                key: "project".to_owned(),
                value: "acmekit".to_owned(),
            }]),
            state: Some(TagState::Suggested),
            decided_by: None,
        },
    )
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(
        error_of(&response).error,
        "state must be accepted or rejected"
    );

    let response = post(
        &app,
        "/api/memory/tags/taxonomy/decide",
        &TagTaxonomyDecideRequest {
            entries: Some(vec![wire::gateway::memory::TagEntryPair {
                key: "project".to_owned(),
                value: "acmekit".to_owned(),
            }]),
            state: Some(TagState::Rejected),
            decided_by: None,
        },
    )
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: TagDecisionResult =
        serde_json::from_slice(&response.body).expect("decide response");
    assert_eq!(parsed.changed, 0);
}

#[test]
fn tags_taxonomy_merge_validates_and_merges() {
    let (_store, app) = seeded().expect("seed store");
    let response = post(
        &app,
        "/api/memory/tags/taxonomy/merge",
        &TagTaxonomyMergeRequest {
            key: Some("project".to_owned()),
            from: Some("acme".to_owned()),
            into: Some(String::new()),
        },
    )
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "key, from and into required");

    let response = post(
        &app,
        "/api/memory/tags/taxonomy/merge",
        &TagTaxonomyMergeRequest {
            key: Some("project".to_owned()),
            from: Some("acme".to_owned()),
            into: Some("acmekit".to_owned()),
        },
    )
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: TagTaxonomyMergeResult =
        serde_json::from_slice(&response.body).expect("merge response");
    assert_eq!(parsed.moved, 0);
    assert_eq!(parsed.dropped, 0);
    assert_eq!(parsed.into, "acmekit");
}

#[test]
fn tags_non_object_and_invalid_bodies() {
    let (_store, app) = seeded().expect("seed store");
    let raw = |path: &str, raw: &[u8]| {
        block_on(answer(
            app.clone(),
            recorded("POST", &format!("/api/memory/tags{path}"), raw.to_vec()),
        ))
    };
    let response = raw("/decide", b"[]").expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "body must be a JSON object");

    let response = raw("/decide", b"").expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "invalid JSON");
}

#[test]
fn memory_fallback_and_health_shape() {
    let (store, app) = seeded().expect("seed store");
    let call = |method: &str, target: &str| {
        block_on(answer(app.clone(), recorded(method, target, Vec::new())))
    };
    let response = call("GET", "/api/memory/health").expect("in-process answer");
    assert_eq!(response.status, 200);
    let health: MemoryHealthResponse =
        serde_json::from_slice(&response.body).expect("health response");
    assert_eq!(health.embeddings.status, EmbedStatus::Unavailable);
    assert_eq!(health.status, MemoryHealthStatus::Degraded);
    let observed = health.observed_at.expect("observation time");
    assert!((chrono::Utc::now() - observed).num_seconds().abs() < 60);
    assert_eq!(health.embed_queue_depth, 0);

    store.arm_embeddings();
    let response = call("GET", "/api/memory/health").expect("in-process answer");
    assert_eq!(response.status, 200);
    let health: MemoryHealthResponse =
        serde_json::from_slice(&response.body).expect("health response");
    assert_eq!(health.embeddings.status, EmbedStatus::Ok);
    assert_eq!(health.status, MemoryHealthStatus::Ok);

    let response = call("POST", "/api/memory/health").expect("in-process answer");
    assert_eq!(response.status, 405);

    let response = call("GET", "/api/memory").expect("in-process answer");
    assert_eq!(response.status, 404);
    assert_eq!(error_of(&response).error, "unknown memory resource");

    let response = call("GET", "/api/memory/bogus").expect("in-process answer");
    assert_eq!(response.status, 404);
    assert_eq!(error_of(&response).error, "unknown memory resource");
}

#[test]
fn a_model_mismatch_degrades_embedding_health() {
    let (store, app) = seeded().expect("seed store");
    store.arm_embeddings();
    block_on(store.script_embed_mismatch(EmbedModelMismatch {
        stored: "embed-a".to_owned(),
        configured: "embed-b".to_owned(),
    }));
    let response = block_on(answer(
        app,
        recorded("GET", "/api/memory/health", Vec::new()),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let health: MemoryHealthResponse =
        serde_json::from_slice(&response.body).expect("health response");
    assert_eq!(health.status, MemoryHealthStatus::Degraded);
    assert_eq!(health.embeddings.status, EmbedStatus::Unavailable);
    assert_eq!(
        health.embeddings.error.as_deref(),
        Some(
            "stored vectors were written by embedding model embed-a, the configured model is embed-b"
        )
    );
    assert!(
        health
            .embeddings
            .impact
            .is_some_and(|impact| impact.contains("rh memory reembed"))
    );
}

#[test]
fn a_message_hit_carries_its_session() {
    let (_, app) = seeded().expect("seed store");
    let response = block_on(answer(
        app,
        recorded("GET", "/api/memory/search?q=hello", Vec::new()),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: MemorySearchResponse =
        serde_json::from_slice(&response.body).expect("search response");
    let hit = parsed.results.first().expect("hello hit");
    assert_eq!(hit.session_id.as_deref(), Some(SESSION));
}

#[test]
fn search_reports_store_degradation() {
    let (store, app) = seeded().expect("seed store");
    block_on(store.set_search_outcome(kernel::memory::SearchOutcome {
        degraded: Some(kernel::memory::MemoryDegraded {
            reason: "embedding endpoint not configured".to_owned(),
            effect: "Keyword / FTS ranking only — not meaning-based.".to_owned(),
        }),
        ..Default::default()
    }));
    let response = block_on(answer(
        app.clone(),
        recorded("GET", "/api/memory/search?q=hello", Vec::new()),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: MemorySearchResponse =
        serde_json::from_slice(&response.body).expect("search response");
    assert_eq!(
        parsed.degraded,
        Some(wire::gateway::memory::MemoryDegraded {
            reason: "embedding endpoint not configured".to_owned(),
            effect: "Keyword / FTS ranking only — not meaning-based.".to_owned(),
        })
    );

    let response = post(
        &app,
        "/api/memory/tool/memory_search",
        &MemorySearchToolArgs {
            since: None,
            before: None,
            window: None,
            expand: None,
            synthesize: None,
            query: "hello".to_owned(),
            mode: None,
            scope: None,
            limit: None,
            agent: None,
            role: None,
            tool_name: None,
            tag: None,
        },
    )
    .expect("in-process answer");
    let inner: MemorySearchResponse =
        serde_json::from_str(&tool_text(&response)).expect("tool result is json text");
    assert_eq!(
        inner.degraded,
        Some(wire::gateway::memory::MemoryDegraded {
            reason: "embedding endpoint not configured".to_owned(),
            effect: "Keyword / FTS ranking only — not meaning-based.".to_owned(),
        })
    );
}

#[test]
fn search_with_a_wired_provider_carries_no_degradation() {
    let store: Arc<dyn MemoryStore> = Arc::new(testkit::FakeMemoryStore::default());
    let gateway = Arc::new(common::owned(
        Gateway::new(store, common::settings()).with_embed_provider(),
    ));
    let app = gateway.router(false);
    let response = block_on(answer(
        app,
        recorded("GET", "/api/memory/search?q=hello", Vec::new()),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: MemorySearchResponse =
        serde_json::from_slice(&response.body).expect("search response");
    assert_eq!(parsed.degraded, None);
}

#[test]
fn store_failures_map_to_statuses() {
    let gateway = common::failing_gateway();
    let app = gateway.router(false);
    let response = block_on(answer(
        app,
        recorded("GET", "/api/memory/stats", Vec::new()),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 503);
    assert_eq!(error_of(&response).error, "memory store unavailable");
    let response = block_on(answer(
        gateway.router(false),
        recorded("GET", "/api/memory/health", Vec::new()),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 503);
}
