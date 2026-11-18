#[test]
fn the_facts_section_lists_every_current_fact_after_the_prose() {
    let at = |d: &str| values::time::parse_rfc3339(d).unwrap();
    let facts = vec![
        LiveFact {
            attribute: "ram".into(),
            value: "372 GB total".into(),
            as_of: at("2026-11-07T15:41:00Z"),
            status: FactState::Current,
        },
        LiveFact {
            attribute: "ip".into(),
            value: "11.22.33.44".into(),
            as_of: at("2026-10-06T22:20:00Z"),
            status: FactState::Current,
        },
        LiveFact {
            attribute: "state".into(),
            value: "ds41-serve stopped".into(),
            as_of: at("2026-11-07T02:53:00Z"),
            status: FactState::Ended,
        },
    ];
    let article = article_with_facts("Lead sentence.", &facts);
    assert!(article.starts_with("Lead sentence.\n\n### Current facts\n"));
    assert!(article.contains("- **ip**: 11.22.33.44 (as of 2026-10-06)"));
    assert!(article.contains("- **ram**: 372 GB total (as of 2026-11-07)"));
    assert!(article.contains("- **state**: ds41-serve stopped (ended) (as of 2026-11-07)"));
    assert!(article.find("**ip**").unwrap() < article.find("**ram**").unwrap());
    assert_eq!(article_with_facts("Only prose.", &[]), "Only prose.");
}
use super::*;
use jiff::Timestamp;
use jiff::civil::Date;
use uuid::Uuid;

fn at(secs: i64) -> Timestamp {
    Timestamp::new(secs, 0).unwrap()
}

fn uid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn clock(id: u128, start: i64, end: i64) -> LeafClock {
    LeafClock {
        id: uid(id),
        conversation_id: Some(uid(9)),
        start: at(start),
        end: at(end),
    }
}

#[test]
fn slug_uses_the_type_and_never_a_numeric_suffix() {
    let lookup = |slug: &str| {
        if slug == "tinbox" {
            SlugOccupant::Other
        } else {
            SlugOccupant::Free
        }
    };
    assert_eq!(
        choose_slug("tinbox", "host", lookup).as_deref(),
        Some("tinbox-host")
    );
    let same = |slug: &str| {
        if slug == "tinbox" {
            SlugOccupant::Same
        } else {
            SlugOccupant::Other
        }
    };
    assert_eq!(
        choose_slug("tinbox", "host", same).as_deref(),
        Some("tinbox")
    );
    let taken = |_: &str| SlugOccupant::Other;
    assert_eq!(choose_slug("tinbox", "host", taken), None);
}

#[test]
fn a_valid_ref_binds_and_a_different_type_does_not() {
    let host = BindHit {
        id: uid(2),
        type_id: uid(7),
        created_at: at(1),
    };
    let project = BindHit {
        id: uid(3),
        type_id: uid(8),
        created_at: at(2),
    };
    let hits = [host, project];
    assert_eq!(
        pick_existing(
            &BindRequest {
                entity_ref: Some(uid(3)),
                ref_exists: true,
                type_id: Some(uid(7)),
            },
            &hits,
        ),
        Some(uid(3))
    );
    assert_eq!(
        pick_existing(
            &BindRequest {
                entity_ref: Some(uid(99)),
                ref_exists: false,
                type_id: Some(uid(8)),
            },
            &hits,
        ),
        Some(uid(3))
    );
    assert_eq!(
        pick_existing(
            &BindRequest {
                entity_ref: None,
                ref_exists: false,
                type_id: Some(uid(7)),
            },
            &hits,
        ),
        Some(uid(2))
    );
}

#[test]
fn as_of_stays_inside_the_leaf_and_never_uses_processing_time() {
    let leaves = [clock(1, 1_700_000_000, 1_700_003_600)];
    let inside = place_as_of("2023-11-14T22:13:20Z", &leaves);
    assert_eq!(inside, at(1_700_000_000));
    let snapped = place_as_of("2020-01-01T00:00:00Z", &leaves);
    let start = leaves.first().unwrap().start;
    assert_eq!(snapped, start);
    let dated = place_as_of("2023-11-14", &leaves);
    assert_eq!(dated, start);
}

#[test]
fn an_earlier_session_is_older_and_the_same_session_is_not() {
    let at = |text: &str| values::time::parse_rfc3339(text).unwrap();
    let mark = AppliedMark {
        at: at("2026-11-07T00:00:00Z"),
        summary_id: uuid::Uuid::from_u128(2),
    };
    assert!(older_than_mark(
        at("2026-11-01T00:00:00Z"),
        uuid::Uuid::from_u128(9),
        mark
    ));
    assert!(!older_than_mark(
        at("2026-11-07T00:00:00Z"),
        uuid::Uuid::from_u128(2),
        mark
    ));
    assert!(older_than_mark(
        at("2026-11-07T00:00:00Z"),
        uuid::Uuid::from_u128(1),
        mark
    ));
    assert!(!older_than_mark(
        at("2026-11-08T00:00:00Z"),
        uuid::Uuid::from_u128(1),
        mark
    ));
}

#[test]
fn history_is_one_entry_per_summary_newest_first() {
    let lines = [
        HistoryLine {
            date: Date::new(2024, 1, 2).unwrap(),
            sentence: "first".to_owned(),
            summary_id: uid(1),
            conversation_id: None,
        },
        HistoryLine {
            date: Date::new(2024, 3, 4).unwrap(),
            sentence: "later".to_owned(),
            summary_id: uid(2),
            conversation_id: None,
        },
        HistoryLine {
            date: Date::new(2024, 3, 4).unwrap(),
            sentence: "duplicate".to_owned(),
            summary_id: uid(2),
            conversation_id: None,
        },
    ];
    let rendered = render_history(&lines);
    assert_eq!(rendered.len(), 2);
    assert_eq!(rendered.first().unwrap().body, "later");
    assert_eq!(rendered.get(1).unwrap().body, "first");
    assert_eq!(rendered.first().unwrap().title, "");
}

#[test]
fn an_identification_keeps_the_verbatim_mention() {
    let parsed = parse_identifications(
        r#"{"observations":[{"name":"node_a","type":"Hosts","mention":"build VMs 213/216/217/218/212/207"}]}"#,
    )
    .unwrap();
    let mention = parsed
        .observations
        .first()
        .map(|item| item.mention.as_str());
    assert_eq!(mention, Some("build VMs 213/216/217/218/212/207"));
}

#[test]
fn synonyms_share_a_key_and_subitems_stay_in_one_value() {
    let observation = EntityObservation {
        name: "tinbox".to_owned(),
        type_name: "Hosts".to_owned(),
        facts: vec![
            ObservedFact {
                attribute: "memory".to_owned(),
                value: "372 GB total".to_owned(),
                as_of: "2023-11-14T22:13:20Z".to_owned(),
                status: FactState::Current,
            },
            ObservedFact {
                attribute: "RAM".to_owned(),
                value: "152 GB available".to_owned(),
                as_of: "2023-11-14T22:20:00Z".to_owned(),
                status: FactState::Current,
            },
            ObservedFact {
                attribute: "gpu".to_owned(),
                value: "GPU 0-3: ThinkingCap".to_owned(),
                as_of: "2023-11-14T22:13:20Z".to_owned(),
                ..ObservedFact::default()
            },
            ObservedFact {
                attribute: "gpus".to_owned(),
                value: "GPU 4-7: idle".to_owned(),
                as_of: "2023-11-14T22:20:00Z".to_owned(),
                ..ObservedFact::default()
            },
            ObservedFact {
                attribute: "role".to_owned(),
                value: "GPU 0-3: ThinkingCap; GPU 4-7: idle".to_owned(),
                as_of: "2023-11-14T22:13:20Z".to_owned(),
                ..ObservedFact::default()
            },
        ],
        ..EntityObservation::default()
    };
    let prepared = prepare(&observation, &[clock(1, 1_700_000_000, 1_700_003_600)]).unwrap();
    let ram: Vec<_> = prepared
        .facts
        .iter()
        .filter(|fact| fact.attribute == "ram")
        .collect();
    assert_eq!(ram.len(), 1);
    assert_eq!(
        ram.first().map(|fact| fact.value.as_str()),
        Some("152 GB available")
    );
    let gpus = prepared
        .facts
        .iter()
        .find(|fact| fact.attribute == "gpus")
        .unwrap();
    assert!(gpus.value.contains("GPU 0-3: ThinkingCap"));
    assert!(gpus.value.contains("GPU 4-7: idle"));
    assert!(prepared.facts.iter().all(|fact| fact.attribute != "role"));
    assert!(prepared.facts.iter().all(|fact| fact.attribute != "memory"));
}

#[test]
fn a_rewrite_parses_and_the_later_value_becomes_the_snapshot() {
    let text = "```json\n{\"summary\":\"Host.\",\"article\":\"Lead.\\n\\n### Current facts\\n\\n- **ram**: old\",\"current_facts\":[{\"attribute\":\"memory\",\"value\":\"87 GB available\",\"as_of\":\"2023-11-14T22:13:20Z\"},{\"attribute\":\"RAM\",\"value\":\"152 GB available\",\"as_of\":\"2023-11-14T22:20:00Z\"},{\"attribute\":\"other\",\"value\":\"nope\",\"as_of\":\"2023-11-14T22:20:00Z\"}],\"history_sentence\":\"RAM is 152 GB available.\"}\n```";
    let rewrite = parse_rewrite(text).unwrap();
    assert_eq!(rewrite.summary, "Host.");
    assert_eq!(rewrite.history_sentence, "RAM is 152 GB available.");
    assert_eq!(prose_article(&rewrite.article), "Lead.");
    let leaves = [clock(1, 1_700_000_000, 1_700_003_600)];
    let facts = snapshot_facts(
        "Hosts",
        &rewrite.current_facts,
        uid(4),
        Some(uid(9)),
        &leaves,
    );
    assert_eq!(facts.len(), 1);
    assert_eq!(
        facts.first().map(|fact| fact.attribute.as_str()),
        Some("ram")
    );
    assert_eq!(
        facts.first().map(|fact| fact.value.as_str()),
        Some("152 GB available")
    );
    assert_eq!(facts.first().map(|fact| fact.summary_id), Some(uid(4)));
    let line = session_history(
        uid(4),
        Some(uid(9)),
        Date::new(2023, 11, 14).unwrap(),
        &rewrite.history_sentence,
    );
    assert_eq!(
        line.map(|item| item.sentence),
        Some("RAM is 152 GB available.".to_owned())
    );
    assert!(session_history(uid(4), None, Date::new(2023, 11, 14).unwrap(), "  ").is_none());
}

#[test]
fn a_rewrite_accepts_a_retired_list_and_parses_without_one() {
    let text = r#"{"summary":"Host.","article":"Lead.","current_facts":[{"attribute":"port","value":"8080","as_of":"2023-11-14T22:13:20Z"}],"retired":[{"attribute":"other-port","kind":"renamed","detail":"port"}],"history_sentence":"Port is 8080."}"#;
    let rewrite = parse_rewrite(text).unwrap();
    let retired = rewrite.retired.first().unwrap();
    assert_eq!(retired.attribute, "other-port");
    assert_eq!(retired.kind, "renamed");
    assert_eq!(retired.detail, "port");
    let bare = parse_rewrite(r#"{"summary":"Host.","current_facts":[]}"#).unwrap();
    assert_eq!(bare.retired.len(), 0);
    let legacy = parse_rewrite(
        r#"{"summary":"Host.","retired":[{"attribute":"ip","reason":"not re-verified"}]}"#,
    )
    .unwrap();
    let legacy = legacy.retired.first().unwrap();
    assert_eq!(legacy.kind, "");
    assert_eq!(legacy.detail, "");
    let schema = serde_json::to_string(&rewrite_schema()).unwrap();
    assert!(schema.contains("retired"));
    assert!(schema.contains("\"kind\""));
    assert!(schema.contains("\"detail\""));
    assert!(!schema.contains("\"reason\""));
}

#[test]
fn an_omitted_fact_keeps_its_value_as_of_and_status() {
    let kept_at = at(1_600_000_000);
    let page = [LiveFact {
        attribute: "ip".to_owned(),
        value: "11.22.33.44".to_owned(),
        as_of: kept_at,
        status: FactState::Ended,
    }];
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
    let out = reconcile_facts(
        "Hosts",
        &page,
        &rewrite,
        uid(4),
        Some(uid(9)),
        &leaves(),
        "",
    );
    let ip = out
        .facts
        .iter()
        .find(|fact| fact.attribute == "ip")
        .unwrap();
    assert_eq!(ip.value, "11.22.33.44");
    assert_eq!(ip.as_of, kept_at);
    assert_eq!(ip.status, FactState::Ended);
    assert_eq!(out.carried.len(), 1);
    assert_eq!(out.retired.len(), 0);
}

#[test]
fn an_updated_attribute_replaces_the_value_and_keeps_the_sentence() {
    let page = [LiveFact {
        attribute: "ram".to_owned(),
        value: "87 GB available".to_owned(),
        as_of: at(1_600_000_000),
        status: FactState::Current,
    }];
    let rewrite = EntityRewrite {
        summary: "Host.".to_owned(),
        current_facts: vec![RewriteFact {
            attribute: "RAM".to_owned(),
            value: "152 GB available".to_owned(),
            as_of: "2023-11-14T22:13:20Z".to_owned(),
        }],
        history_sentence: "RAM is 152 GB.".to_owned(),
        ..EntityRewrite::default()
    };
    let out = reconcile_facts(
        "Hosts",
        &page,
        &rewrite,
        uid(4),
        Some(uid(9)),
        &leaves(),
        "",
    );
    assert_eq!(out.carried.len(), 0);
    assert_eq!(out.retired.len(), 0);
    assert_eq!(out.history_sentence, "RAM is 152 GB.");
    assert_eq!(out.facts.len(), 1);
    assert_eq!(
        out.facts.first().map(|fact| fact.value.as_str()),
        Some("152 GB available")
    );
}

#[test]
fn a_retired_fact_drops_out_of_the_snapshot_with_its_reason() {
    let page = [
        LiveFact {
            attribute: "ram".to_owned(),
            value: "152 GB available".to_owned(),
            as_of: at(1_700_000_000),
            status: FactState::Current,
        },
        LiveFact {
            attribute: "ip".to_owned(),
            value: "11.22.33.44".to_owned(),
            as_of: at(1_600_000_000),
            status: FactState::Current,
        },
    ];
    let rewrite = EntityRewrite {
        summary: "Host.".to_owned(),
        current_facts: vec![RewriteFact {
            attribute: "ram".to_owned(),
            value: "152 GB available".to_owned(),
            as_of: "2023-11-14T22:13:20Z".to_owned(),
        }],
        retired: vec![RetiredFact {
            attribute: "ip".to_owned(),
            kind: "contradicted".to_owned(),
            detail: "no longer true as of 2026-11-07".to_owned(),
        }],
        history_sentence: "RAM is 152 GB.".to_owned(),
        ..EntityRewrite::default()
    };
    let out = reconcile_facts(
        "Hosts",
        &page,
        &rewrite,
        uid(4),
        Some(uid(9)),
        &leaves(),
        "The address is no longer true as of 2026-11-07.",
    );
    assert!(out.facts.iter().all(|fact| fact.attribute != "ip"));
    assert_eq!(out.carried.len(), 0);
    assert_eq!(out.retired.len(), 1);
    assert_eq!(out.rejected.len(), 0);
    assert!(
        out.history_sentence
            .contains("ip retired: contradicted: no longer true as of 2026-11-07")
    );
}

#[test]
fn a_renamed_attribute_keeps_only_the_new_key() {
    let page = [LiveFact {
        attribute: "other-port".to_owned(),
        value: "8080".to_owned(),
        as_of: at(1_600_000_000),
        status: FactState::Current,
    }];
    let rewrite = EntityRewrite {
        summary: "Service.".to_owned(),
        current_facts: vec![RewriteFact {
            attribute: "port".to_owned(),
            value: "8080".to_owned(),
            as_of: "2023-11-14T22:13:20Z".to_owned(),
        }],
        retired: vec![RetiredFact {
            attribute: "other-port".to_owned(),
            kind: "renamed".to_owned(),
            detail: "port".to_owned(),
        }],
        history_sentence: "The listen port is 8080.".to_owned(),
        ..EntityRewrite::default()
    };
    let out = reconcile_facts(
        "Services",
        &page,
        &rewrite,
        uid(4),
        Some(uid(9)),
        &leaves(),
        "",
    );
    assert_eq!(out.facts.len(), 1);
    assert_eq!(
        out.facts.first().map(|fact| fact.attribute.as_str()),
        Some("port")
    );
    assert_eq!(out.carried.len(), 0);
    assert!(out.history_sentence.contains("renamed to port"));
}

#[test]
fn a_retirement_without_evidence_is_carried_forward() {
    let page = [
        live_fact("ip", "11.22.33.44"),
        live_fact("role", "migration target"),
        live_fact("hypervisor", "Incus 6.0.5"),
    ];
    let rewrite = EntityRewrite {
        summary: "Host.".to_owned(),
        current_facts: vec![emitted_fact("ram", "152 GB available")],
        retired: vec![
            RetiredFact {
                attribute: "ip".to_owned(),
                kind: String::new(),
                detail: "not re-verified".to_owned(),
            },
            RetiredFact {
                attribute: "role".to_owned(),
                kind: "unverified".to_owned(),
                detail: "Migration target status not verified".to_owned(),
            },
            RetiredFact {
                attribute: "hypervisor".to_owned(),
                kind: "contradicted".to_owned(),
                detail: "Incus version 6.0.5 not re-verified".to_owned(),
            },
        ],
        history_sentence: "RAM is 152 GB.".to_owned(),
        ..EntityRewrite::default()
    };
    let out = reconcile_page("Hosts", &page, &rewrite, "GPU 0 idle");
    for attribute in ["ip", "role", "hypervisor"] {
        assert!(
            out.facts.iter().any(|fact| fact.attribute == attribute),
            "{attribute} was dropped"
        );
    }
    assert_eq!(out.carried.len(), 3);
    assert_eq!(out.retired.len(), 0);
    assert_eq!(out.rejected.len(), 3);
    assert_eq!(out.history_sentence, "RAM is 152 GB.");
}

#[test]
fn a_rename_is_accepted_only_when_the_new_key_is_present() {
    let page = [
        live_fact("location", "rack 4"),
        live_fact("ip", "11.22.33.44"),
    ];
    let rewrite = EntityRewrite {
        summary: "Host.".to_owned(),
        current_facts: vec![emitted_fact("role", "compute")],
        retired: vec![
            RetiredFact {
                attribute: "location".to_owned(),
                kind: "renamed".to_owned(),
                detail: "role".to_owned(),
            },
            RetiredFact {
                attribute: "ip".to_owned(),
                kind: "renamed".to_owned(),
                detail: "absent-key".to_owned(),
            },
        ],
        history_sentence: "The role is compute.".to_owned(),
        ..EntityRewrite::default()
    };
    let out = reconcile_page("Hosts", &page, &rewrite, "");
    assert!(out.facts.iter().any(|fact| fact.attribute == "role"));
    assert!(out.facts.iter().all(|fact| fact.attribute != "location"));
    assert!(out.facts.iter().any(|fact| fact.attribute == "ip"));
    assert_eq!(out.carried.len(), 1);
    assert_eq!(out.retired.len(), 1);
    assert_eq!(
        out.retired.first().map(|item| item.attribute.as_str()),
        Some("location")
    );
    assert!(out.history_sentence.contains("renamed to role"));
    assert_eq!(out.rejected.len(), 1);
    let rejected = out.rejected.first().unwrap();
    assert_eq!(rejected.attribute, "ip");
    assert_eq!(rejected.kind, "renamed");
    assert_eq!(rejected.detail, "absent-key");
}

#[test]
fn a_contradiction_is_accepted_only_when_the_quote_is_in_the_passages() {
    let passages = "The address is now 11.22.33.47.";
    let page = [live_fact("ip", "11.22.33.44"), live_fact("ram", "87 GB")];
    let rewrite = EntityRewrite {
        summary: "Host.".to_owned(),
        current_facts: vec![emitted_fact("role", "compute")],
        retired: vec![
            RetiredFact {
                attribute: "ip".to_owned(),
                kind: "contradicted".to_owned(),
                detail: "address is now 11.22.33.47".to_owned(),
            },
            RetiredFact {
                attribute: "ram".to_owned(),
                kind: "contradicted".to_owned(),
                detail: "87 GB was a measurement error".to_owned(),
            },
        ],
        history_sentence: "The address changed.".to_owned(),
        ..EntityRewrite::default()
    };
    let out = reconcile_page("Hosts", &page, &rewrite, passages);
    assert!(out.facts.iter().all(|fact| fact.attribute != "ip"));
    assert!(out.facts.iter().any(|fact| fact.attribute == "ram"));
    assert_eq!(out.retired.len(), 1);
    assert_eq!(out.carried.len(), 1);
    assert!(
        out.history_sentence
            .contains("contradicted: address is now 11.22.33.47")
    );
    assert_eq!(out.rejected.len(), 1);
    assert_eq!(
        out.rejected.first().map(|item| item.attribute.as_str()),
        Some("ram")
    );
    let wrong_case = EntityRewrite {
        retired: vec![RetiredFact {
            attribute: "ip".to_owned(),
            kind: "contradicted".to_owned(),
            detail: "Address is now 11.22.33.47".to_owned(),
        }],
        ..rewrite
    };
    let missed = reconcile_page("Hosts", &page, &wrong_case, passages);
    assert!(missed.facts.iter().any(|fact| fact.attribute == "ip"));
    assert_eq!(missed.retired.len(), 0);
}

#[test]
fn a_contradiction_quote_matches_through_markdown_and_whitespace() {
    let passages = "**Bridging**  eno0\non   br0";
    let page = [live_fact("ip", "11.22.33.44")];
    let rewrite = EntityRewrite {
        summary: "Host.".to_owned(),
        current_facts: vec![emitted_fact("ram", "152 GB available")],
        retired: vec![RetiredFact {
            attribute: "ip".to_owned(),
            kind: "contradicted".to_owned(),
            detail: "Bridging eno0 on br0".to_owned(),
        }],
        ..EntityRewrite::default()
    };
    let out = reconcile_page("Hosts", &page, &rewrite, passages);
    assert!(out.facts.iter().all(|fact| fact.attribute != "ip"));
    assert_eq!(out.retired.len(), 1);
    assert_eq!(out.rejected.len(), 0);
    let marked = EntityRewrite {
        retired: vec![RetiredFact {
            attribute: "ip".to_owned(),
            kind: "contradicted".to_owned(),
            detail: "**Bridging**\neno0   on br0".to_owned(),
        }],
        ..rewrite.clone()
    };
    let from_marks = reconcile_page("Hosts", &page, &marked, "Bridging eno0 on br0");
    assert_eq!(from_marks.retired.len(), 1);
    let long = "v".repeat(201);
    let too_long = EntityRewrite {
        retired: vec![RetiredFact {
            attribute: "ip".to_owned(),
            kind: "contradicted".to_owned(),
            detail: long.clone(),
        }],
        ..rewrite.clone()
    };
    let rejected = reconcile_page("Hosts", &page, &too_long, &long);
    assert!(rejected.facts.iter().any(|fact| fact.attribute == "ip"));
    assert_eq!(rejected.retired.len(), 0);
    let capped = "v".repeat(200);
    let accepted = EntityRewrite {
        retired: vec![RetiredFact {
            attribute: "ip".to_owned(),
            kind: "contradicted".to_owned(),
            detail: capped.clone(),
        }],
        ..rewrite
    };
    let kept = reconcile_page("Hosts", &page, &accepted, &capped);
    assert_eq!(kept.retired.len(), 1);
    assert!(kept.facts.iter().all(|fact| fact.attribute != "ip"));
}

fn live_fact(attribute: &str, value: &str) -> LiveFact {
    LiveFact {
        attribute: attribute.to_owned(),
        value: value.to_owned(),
        as_of: at(1_600_000_000),
        status: FactState::Current,
    }
}

fn emitted_fact(attribute: &str, value: &str) -> RewriteFact {
    RewriteFact {
        attribute: attribute.to_owned(),
        value: value.to_owned(),
        as_of: "2023-11-14T22:13:20Z".to_owned(),
    }
}

fn reconcile_page(
    type_name: &str,
    page: &[LiveFact],
    rewrite: &EntityRewrite,
    passages: &str,
) -> FactReconciliation {
    reconcile_facts(
        type_name,
        page,
        rewrite,
        uid(4),
        Some(uid(9)),
        &leaves(),
        passages,
    )
}

fn leaves() -> [LeafClock; 1] {
    [clock(4, 1_700_000_000, 1_700_003_600)]
}

#[test]
fn an_identification_ignores_facts_and_accepts_a_bare_array() {
    let parsed = parse_identifications(
        r#"{"observations":[{"name":"tinbox","type":"Hosts","aliases":["11.22.33.44"],"facts":[{"attribute":"ram","value":"152 GB"}]}]}"#,
    )
    .unwrap();
    assert_eq!(parsed.observations.len(), 1);
    assert_eq!(
        parsed.observations.first().map(|item| item.name.as_str()),
        Some("tinbox")
    );
    let bare = parse_identifications(r#"[{"name":"dana","type":"People"}]"#).unwrap();
    assert_eq!(
        bare.observations
            .first()
            .map(|item| item.type_name.as_str()),
        Some("People")
    );
}

#[test]
fn an_invalid_identification_ref_reads_as_no_reference() {
    let parsed = parse_identifications(
        r#"{"observations":[{"entity_ref":"null","name":"tinbox","type":"Hosts"},{"entity_ref":"00000000-0000-0000-0000-000000000001","name":"hally","type":"Hosts"}]}"#,
    )
    .unwrap();
    let refs: Vec<(Option<Uuid>, &str)> = parsed
        .observations
        .iter()
        .map(|item| (item.entity_ref, item.name.as_str()))
        .collect();
    assert_eq!(
        refs,
        vec![(None, "tinbox"), (Some(Uuid::from_u128(1)), "hally"),]
    );
}
