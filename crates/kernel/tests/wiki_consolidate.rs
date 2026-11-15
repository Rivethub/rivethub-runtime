use chrono::{DateTime, Duration, Utc};
use kernel::wiki::consolidate::{
    ConsolidateWiki, WikiFold, WikiTopicStats, consolidation_plan, is_slug_variant,
};
use kernel::wiki::page::{FoldMark, topic_identity};

fn names(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

fn slugs(items: &[&str]) -> Vec<WikiTopicStats> {
    items.iter().map(|slug| topic(slug, slug, 0)).collect()
}

fn topic(slug: &str, title: &str, citations: u64) -> WikiTopicStats {
    WikiTopicStats {
        slug: slug.to_owned(),
        title: title.to_owned(),
        citations,
        history: 0,
        created_at: DateTime::<Utc>::UNIX_EPOCH,
        bound: false,
    }
}

fn fold(cluster: &str, parent: &str, children: &[&str]) -> WikiFold {
    WikiFold {
        cluster: cluster.to_owned(),
        parent: parent.to_owned(),
        children: names(children),
        mark: FoldMark::Merged,
    }
}

fn key(slug: &str) -> String {
    topic_identity(slug, "").key
}

#[test]
fn identity_drops_product_prefixes_plurals_and_dangling_words() {
    for (slug, expected) in [
        ("workbench-project", "workbench"),
        ("workbench-rivethub", "workbench"),
        ("workbench-and", "workbench"),
        ("rivetos-repository", "repository"),
        ("project-repositories", "repository"),
        ("the-mesh", "mesh"),
        ("rivethub-android", "android"),
        ("android-rivet", "android"),
        ("rivetos-training-pipeline", "pipeline-training"),
        ("training-pipelines", "pipeline-training"),
        ("rivetos-node-mesh", "mesh-node"),
        ("mesh-nodes", "mesh-node"),
        ("cursor-mcp-servers", "cursor-mcp-server"),
        ("mcp-cursor-server", "cursor-mcp-server"),
        ("infrastructure-and", "infrastructure"),
    ] {
        assert_eq!(key(slug), expected, "{slug}");
    }
    assert_eq!(topic_identity("rivetos-mesh", "Mesh").noise, 1);
    assert_eq!(topic_identity("mesh", "Mesh").noise, 0);
}

#[test]
fn identity_keeps_numbers_and_strips_only_untitled_dedupe_suffixes() {
    assert_eq!(key("web115-rivet"), "web115");
    assert_ne!(key("web114"), key("web115"));
    assert_eq!(topic_identity("grok-4", "Grok 4").key, "4-grok");
    assert_ne!(
        topic_identity("grok-4", "Grok 4").key,
        topic_identity("grok-3", "Grok 3").key
    );
    assert_ne!(topic_identity("grok-4", "Grok 4").key, key("rivet-grok"));
    assert_eq!(topic_identity("workbench-2", "Workbench").key, "workbench");
    assert_eq!(topic_identity("workbench-2", "Workbench").noise, 1);
    assert_eq!(key("rivethub"), "rivethub");
    assert_ne!(key("rivethub"), key("rivetos"));
}

#[test]
fn the_unprefixed_page_survives_its_identity_group() -> Result<(), serde_json::Error> {
    let plan = consolidation_plan(
        &[
            topic("rivet-mesh", "Rivet mesh", 9),
            topic("the-mesh", "The mesh", 3),
            topic("mesh", "Mesh", 0),
            topic("mesh-network", "Mesh network", 1),
        ],
        &defaults()?,
    );
    assert_eq!(
        plan,
        vec![fold("mesh", "mesh", &["rivet-mesh", "the-mesh"])]
    );
    let survivors = slugs(&["mesh", "mesh-network"]);
    assert_eq!(
        consolidation_plan(&survivors, &defaults()?),
        Vec::<WikiFold>::new()
    );
    Ok(())
}

#[test]
fn citations_then_history_then_age_pick_a_survivor_among_prefixed_pages()
-> Result<(), serde_json::Error> {
    let older = WikiTopicStats {
        history: 3,
        created_at: DateTime::<Utc>::UNIX_EPOCH - Duration::days(1),
        ..topic("rivethub-repository", "RivetHub repository", 5)
    };
    let plan = consolidation_plan(
        &[
            topic("rivet-repository", "Repository", 2),
            topic("rivetos-repository", "Repository", 5),
            WikiTopicStats {
                history: 3,
                ..topic("project-repository", "Repository", 5)
            },
            older,
        ],
        &defaults()?,
    );
    assert_eq!(
        plan,
        vec![fold(
            "repository",
            "rivethub-repository",
            &[
                "project-repository",
                "rivet-repository",
                "rivetos-repository"
            ],
        )]
    );
    Ok(())
}

#[test]
fn numbered_topics_stay_apart() -> Result<(), serde_json::Error> {
    let plan = consolidation_plan(
        &[
            topic("grok-3", "Grok 3", 1),
            topic("grok-4", "Grok 4", 1),
            topic("rivet-grok", "Grok", 1),
            topic("web114", "web114", 1),
            topic("web115", "web115", 1),
            topic("web115-project", "web115", 4),
            topic("workbench", "Workbench", 1),
            topic("workbench-2", "Workbench", 1),
        ],
        &defaults()?,
    );
    assert_eq!(
        plan,
        vec![
            fold("web115", "web115", &["web115-project"]),
            fold("workbench", "workbench", &["workbench-2"]),
        ]
    );
    Ok(())
}

#[test]
fn session_variants_fold_into_the_stem_page() -> Result<(), serde_json::Error> {
    let plan = consolidation_plan(
        &slugs(&[
            "acmekit-deploys",
            "acmekit-deploys-friday",
            "acmekit-deploys-friday-rollback",
            "staging-database",
        ]),
        &defaults()?,
    );
    assert_eq!(
        plan,
        vec![fold(
            "acmekit-deploys",
            "acmekit-deploys",
            &["acmekit-deploys-friday", "acmekit-deploys-friday-rollback"],
        )]
    );
    Ok(())
}

#[test]
fn siblings_sharing_a_stem_without_a_parent_page_stay_apart() -> Result<(), serde_json::Error> {
    let plan = consolidation_plan(
        &slugs(&["billing-service-refunds", "billing-service-fraud"]),
        &defaults()?,
    );
    assert_eq!(plan, Vec::<WikiFold>::new());
    Ok(())
}

#[test]
fn the_shortest_variant_parents_a_cluster_without_its_stem_page() -> Result<(), serde_json::Error> {
    let plan = consolidation_plan(
        &slugs(&[
            "billing-service-refunds-march",
            "billing-service-refunds",
            "billing-service-fraud",
        ]),
        &defaults()?,
    );
    assert_eq!(plan, Vec::<WikiFold>::new());
    let plan = consolidation_plan(
        &slugs(&["billing-service-refunds-march", "billing-service-refunds"]),
        &defaults()?,
    );
    assert_eq!(
        plan,
        vec![fold(
            "billing-service",
            "billing-service-refunds",
            &["billing-service-refunds-march"],
        )]
    );
    Ok(())
}

#[test]
fn larger_clusters_come_first_and_the_limit_caps_them() -> Result<(), serde_json::Error> {
    let corpus = slugs(&[
        "acmekit-deploys",
        "acmekit-deploys-friday",
        "staging-database",
        "staging-database-restore",
        "staging-database-snapshot",
    ]);
    let all = consolidation_plan(&corpus, &defaults()?);
    assert_eq!(
        all,
        vec![
            fold(
                "staging-database",
                "staging-database",
                &["staging-database-restore", "staging-database-snapshot"],
            ),
            fold(
                "acmekit-deploys",
                "acmekit-deploys",
                &["acmekit-deploys-friday"]
            ),
        ]
    );
    let limited = consolidation_plan(
        &corpus,
        &ConsolidateWiki {
            limit_clusters: 1,
            ..defaults()?
        },
    );
    assert_eq!(limited, all.iter().take(1).cloned().collect::<Vec<_>>());
    let strict = consolidation_plan(
        &corpus,
        &ConsolidateWiki {
            min_cluster_size: 3,
            ..defaults()?
        },
    );
    assert_eq!(strict, all.iter().take(1).cloned().collect::<Vec<_>>());
    Ok(())
}

#[test]
fn slugs_that_are_not_normalized_are_left_alone() -> Result<(), serde_json::Error> {
    let plan = consolidation_plan(
        &slugs(&[
            "acmekit-deploys",
            "acmekit-deploys-Friday",
            "acmekit-deploys--x",
        ]),
        &defaults()?,
    );
    assert_eq!(plan, Vec::<WikiFold>::new());
    Ok(())
}

#[test]
fn a_variant_is_a_hyphen_boundary_prefix() {
    assert!(is_slug_variant("acmekit", "acmekit-deploys"));
    assert!(is_slug_variant("acmekit-deploys", "acmekit"));
    assert!(is_slug_variant("acmekit", "acmekit"));
    assert!(!is_slug_variant("acme", "acmekit"));
    assert!(!is_slug_variant("", ""));
}

#[test]
fn a_page_bound_to_a_topic_is_never_folded_as_a_child() -> Result<(), serde_json::Error> {
    let bound = |slug: &str| WikiTopicStats {
        bound: true,
        ..topic(slug, slug, 0)
    };
    let plan = consolidation_plan(
        &[
            topic("rivet-billing", "Billing", 9),
            bound("billing"),
            topic("acmekit-deploys", "acmekit-deploys", 0),
            bound("acmekit-deploys-friday"),
        ],
        &defaults()?,
    );
    assert_eq!(plan, vec![fold("billing", "billing", &["rivet-billing"])]);
    Ok(())
}

#[test]
fn the_job_payload_takes_the_documented_defaults() -> Result<(), serde_json::Error> {
    let empty: ConsolidateWiki = serde_json::from_str("{}")?;
    assert_eq!(empty.limit_clusters, 50);
    assert_eq!(empty.min_cluster_size, 2);
    assert!(!empty.dry_run);
    let set: ConsolidateWiki =
        serde_json::from_str(r#"{"dryRun":true,"limitClusters":3,"minClusterSize":4}"#)?;
    assert_eq!(
        set,
        ConsolidateWiki {
            dry_run: true,
            limit_clusters: 3,
            min_cluster_size: 4,
        }
    );
    assert!(serde_json::from_str::<ConsolidateWiki>(r#"{"limitClusters":-1}"#).is_err());
    Ok(())
}

fn defaults() -> Result<ConsolidateWiki, serde_json::Error> {
    serde_json::from_str("{}")
}
