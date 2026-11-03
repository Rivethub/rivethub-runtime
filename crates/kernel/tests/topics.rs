use chrono::DateTime;
use chrono::TimeZone;
use chrono::Utc;
use kernel::topics::Alias;
use kernel::topics::Change;
use kernel::topics::MAX_DEPTH;
use kernel::topics::MergePlan;
use kernel::topics::NewTopic;
use kernel::topics::Placement;
use kernel::topics::PlacementOutcome;
use kernel::topics::PlacementSource;
use kernel::topics::Redirect;
use kernel::topics::Root;
use kernel::topics::RootKind;
use kernel::topics::Slug;
use kernel::topics::Topic;
use kernel::topics::TopicChanges;
use kernel::topics::TopicError;
use kernel::topics::TopicId;
use kernel::topics::TopicKey;
use kernel::topics::TopicKind;
use kernel::topics::TopicListing;
use kernel::topics::TopicName;
use kernel::topics::TopicQuery;
use kernel::topics::TopicTree;
use kernel::topics::check_parent;
use kernel::topics::rank;
use std::str::FromStr;

use values::SessionId;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn at(minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 12, minute, 0)
        .single()
        .unwrap_or_default()
}

fn topic(name: &str, aliases: &[&str]) -> Result<Topic, TopicError> {
    Topic::create(
        TopicId::random(),
        NewTopic {
            kind: TopicKind::Work,
            name: TopicName::new(name)?,
            aliases: aliases
                .iter()
                .map(|alias| Alias::new(alias))
                .collect::<Result<_, _>>()?,
            parent: None,
            directory: None,
            owner: testkit::owner().ok(),
        },
        at(0),
    )
}

fn placement(
    by: PlacementSource,
    topic: TopicId,
    minute: u32,
) -> Result<Placement, Box<dyn std::error::Error>> {
    Ok(Placement {
        session: SessionId::from_str("claude-code:00000000-0000-4000-8000-000000000001")?,
        topic,
        by,
        at: at(minute),
        owner: testkit::owner().ok(),
    })
}

#[test]
fn slugs_fold_names_to_lowercase_dashed_text() -> TestResult {
    assert_eq!(Slug::fold("RivetHub Desktop")?.as_str(), "rivethub-desktop");
    assert_eq!(
        Slug::fold("  --Rust / Runtime!!  ")?.as_str(),
        "rust-runtime"
    );
    assert_eq!(Slug::fold("Café 2")?.as_str(), "café-2");
    assert!(matches!(
        Slug::fold("!!!"),
        Err(TopicError::EmptySlug { .. })
    ));
    Ok(())
}

#[test]
fn names_and_aliases_are_trimmed_and_never_empty() -> TestResult {
    assert_eq!(TopicName::new("  Deploy ")?.as_str(), "Deploy");
    assert!(matches!(TopicName::new("   "), Err(TopicError::EmptyName)));
    assert!(matches!(Alias::new(""), Err(TopicError::EmptyAlias)));
    let created = topic("Deploy", &["Ship", "ship", "CI"])?;
    let aliases: Vec<&str> = created.aliases.iter().map(Alias::as_str).collect();
    assert_eq!(aliases, ["CI", "Ship"]);
    assert_eq!(created.slug.as_str(), "deploy");
    Ok(())
}

#[test]
fn renaming_refolds_the_slug_and_changes_clear_or_keep_fields() -> TestResult {
    let created = topic("Deploy", &[])?;
    let parent = TopicId::random();
    let directory = Root::Local {
        node: "node-a".into(),
        path: "/home/user/acmekit".into(),
    };
    let renamed = created.clone().apply(
        TopicChanges {
            name: Some(TopicName::new("Release Train")?),
            aliases: None,
            parent: Change::Set(parent),
            directory: Change::Set(directory.clone()),
        },
        at(5),
    )?;
    assert_eq!(renamed.slug.as_str(), "release-train");
    assert_eq!(renamed.parent, Some(parent));
    assert_eq!(renamed.directory, Some(directory));
    assert_eq!(renamed.updated_at, at(5));
    assert_eq!(renamed.created_at, created.created_at);
    let cleared = renamed.apply(
        TopicChanges {
            parent: Change::Clear,
            ..TopicChanges::default()
        },
        at(6),
    )?;
    assert_eq!(cleared.parent, None);
    assert!(cleared.directory.is_some());
    Ok(())
}

#[test]
fn roots_round_trip_through_their_stored_parts() {
    let local = Root::Local {
        node: "node-a".into(),
        path: "/home/user/acmekit".into(),
    };
    assert_eq!(
        Root::from_parts(
            local.kind(),
            local.node().map(str::to_owned),
            local.path().into()
        ),
        Some(local)
    );
    assert_eq!(RootKind::Shared.to_string(), "shared");
    assert_eq!(Root::from_parts(RootKind::Local, None, "/srv".into()), None);
    assert_eq!(
        Root::from_parts(RootKind::Shared, Some("node-a".into()), "/srv".into()),
        None
    );
}

#[test]
fn the_categorizer_never_overrides_your_placement() -> TestResult {
    let mine = TopicId::random();
    let suggested = TopicId::random();
    let yours = placement(PlacementSource::You, mine, 1)?;
    let outcome = placement(PlacementSource::Categorizer, suggested, 2)?.admit(Some(yours.clone()));
    assert_eq!(outcome, PlacementOutcome::Kept(yours));
    Ok(())
}

#[test]
fn placements_replace_one_another_otherwise() -> TestResult {
    let first = TopicId::random();
    let second = TopicId::random();
    let fresh = placement(PlacementSource::Categorizer, first, 1)?;
    assert_eq!(
        fresh.clone().admit(None),
        PlacementOutcome::Placed(fresh.clone())
    );
    let recategorized = placement(PlacementSource::Categorizer, second, 2)?;
    assert_eq!(
        recategorized.clone().admit(Some(fresh)),
        PlacementOutcome::Placed(recategorized)
    );
    let moved = placement(PlacementSource::You, second, 3)?;
    let over_yours = placement(PlacementSource::You, first, 4)?;
    assert_eq!(
        over_yours.clone().admit(Some(moved)),
        PlacementOutcome::Placed(over_yours.clone())
    );
    assert_eq!(
        PlacementOutcome::Placed(over_yours.clone()).into_placement(),
        over_yours
    );
    assert_eq!(PlacementSource::Categorizer.to_string(), "categorizer");
    assert_eq!("you".parse::<PlacementSource>()?, PlacementSource::You);
    Ok(())
}

fn refusal(result: &Result<(), TopicError>) -> &'static str {
    if result.is_ok() {
        "ok"
    } else if matches!(result, Err(TopicError::TooDeep { max: MAX_DEPTH })) {
        "too deep"
    } else if matches!(result, Err(TopicError::IntoOwnSubtree)) {
        "own subtree"
    } else if matches!(result, Err(TopicError::OwnParent)) {
        "own parent"
    } else {
        "other"
    }
}

fn under(name: &str, parent: Option<&Topic>) -> Result<Topic, Box<dyn std::error::Error>> {
    let mut made = topic(name, &[])?;
    made.parent = parent.map(|parent| parent.id);
    Ok(made)
}

#[test]
fn topics_nest_four_levels_deep_and_never_under_themselves() -> TestResult {
    let runtime = under("Runtime", None)?;
    let topics = under("Topics", Some(&runtime))?;
    let merge = under("Merge", Some(&topics))?;
    let desktop = under("Desktop", None)?;
    let memory = under("Memory", Some(&desktop))?;
    let fresh = under("Fresh", None)?;
    let tree = TopicTree::of([&runtime, &topics, &merge, &desktop, &memory]);
    for (child, parent, want) in [
        (&fresh, &merge, "ok"),
        (&desktop, &topics, "ok"),
        (&desktop, &merge, "too deep"),
        (&runtime, &merge, "own subtree"),
        (&runtime, &runtime, "own parent"),
    ] {
        assert_eq!(
            refusal(&check_parent(child, parent, &tree)),
            want,
            "under {}",
            parent.name.as_str()
        );
    }
    assert_eq!((tree.depth(merge.id), tree.height(runtime.id)), (3, 3));
    assert_eq!(tree.root(merge.id), runtime.id);
    Ok(())
}

#[test]
fn merging_redirects_the_slug_and_moves_aliases_that_do_not_clash() -> TestResult {
    let source = topic("Desktop App", &["electron", "Tauri"])?;
    let into = topic("RivetHub Desktop", &["tauri"])?;
    let tree = TopicTree::of([&source, &into]);
    let plan = MergePlan::new(&source, &into, &tree)?;
    assert_eq!(
        plan.redirect,
        Redirect {
            slug: Slug::fold("desktop-app")?,
            topic: into.id,
        }
    );
    let moved: Vec<&str> = plan.aliases.iter().map(Alias::as_str).collect();
    assert_eq!(moved, ["Desktop App", "electron"]);
    let merged = plan.merged(into, at(9));
    let aliases: Vec<&str> = merged.aliases.iter().map(Alias::as_str).collect();
    assert_eq!(aliases, ["Desktop App", "electron", "tauri"]);
    assert_eq!(merged.updated_at, at(9));
    assert!(matches!(
        MergePlan::new(&source, &source, &tree),
        Err(TopicError::SelfMerge)
    ));
    Ok(())
}

#[test]
fn merging_a_parent_into_its_child_lifts_the_child_and_adopts_its_siblings() -> TestResult {
    let parent = under("Runtime", None)?;
    let child = under("Topics", Some(&parent))?;
    let sibling = under("Gateway", Some(&parent))?;
    let plan = MergePlan::new(&parent, &child, &TopicTree::of([&parent, &child, &sibling]))?;
    assert_eq!(plan.into_parent, None);
    assert_eq!(plan.subtopics, [sibling.id]);
    Ok(())
}

#[test]
fn merging_keeps_to_four_levels_and_never_into_a_deeper_descendant() -> TestResult {
    let runtime = under("Runtime", None)?;
    let topics = under("Topics", Some(&runtime))?;
    let merge = under("Merge", Some(&topics))?;
    let shallow = under("Shallow", None)?;
    let leaf = under("Leaf", Some(&shallow))?;
    let deep = under("Deep", None)?;
    let middle = under("Middle", Some(&deep))?;
    let bottom = under("Bottom", Some(&middle))?;
    let tree = TopicTree::of([
        &runtime, &topics, &merge, &shallow, &leaf, &deep, &middle, &bottom,
    ]);
    assert!(MergePlan::new(&shallow, &merge, &tree).is_ok());
    assert!(matches!(
        MergePlan::new(&deep, &merge, &tree),
        Err(TopicError::TooDeep { max: MAX_DEPTH })
    ));
    assert!(matches!(
        MergePlan::new(&runtime, &merge, &tree),
        Err(TopicError::IntoOwnSubtree)
    ));
    Ok(())
}

#[test]
fn keys_are_ids_or_folded_slugs() -> TestResult {
    let id = TopicId::random();
    assert_eq!(TopicKey::parse(&id.to_string())?, TopicKey::Id(id));
    assert_eq!(
        TopicKey::parse("RivetHub Desktop")?,
        TopicKey::Slug(TopicKind::Work, Slug::fold("rivethub-desktop")?)
    );
    assert!(TopicKey::parse("--").is_err());
    Ok(())
}

fn listed(topic: Topic, last_active: Option<u32>) -> TopicListing {
    TopicListing {
        topic,
        session_count: 0,
        last_active: last_active.map(at),
    }
}

#[test]
fn search_matches_prefixes_folded_slugs_aliases_and_redirects_by_recent_activity() -> TestResult {
    let desktop = topic("RivetHub Desktop", &["Tauri"])?;
    let runtime = topic("Rust Runtime", &["rh"])?;
    let deploy = topic("Deploy", &[])?;
    let redirects = vec![Redirect {
        slug: Slug::fold("desktop-app")?,
        topic: desktop.id,
    }];
    let listings = vec![
        listed(desktop, Some(1)),
        listed(runtime, Some(5)),
        listed(deploy, None),
    ];
    let names = |query: &str| -> Vec<String> {
        rank(listings.clone(), &redirects, &TopicQuery::new(query), 10)
            .into_iter()
            .map(|listing| listing.topic.name.as_str().to_owned())
            .collect()
    };
    assert_eq!(names("riv"), ["RivetHub Desktop"]);
    assert_eq!(names("desk"), ["RivetHub Desktop"]);
    assert_eq!(names("runtime"), ["Rust Runtime"]);
    assert_eq!(names("RH"), ["Rust Runtime"]);
    assert_eq!(names("de"), ["RivetHub Desktop", "Deploy"]);
    assert_eq!(names("tau"), ["RivetHub Desktop"]);
    assert_eq!(names("Desktop App"), ["RivetHub Desktop"]);
    assert_eq!(names(""), ["Rust Runtime", "RivetHub Desktop", "Deploy"]);
    assert_eq!(names("zzz"), Vec::<String>::new());
    assert_eq!(
        rank(listings.clone(), &redirects, &TopicQuery::new(""), 1).len(),
        1
    );
    Ok(())
}

fn kinded(kind: TopicKind, name: &str, parent: Option<&Topic>) -> Result<Topic, TopicError> {
    Topic::create(
        TopicId::random(),
        NewTopic {
            kind,
            name: TopicName::new(name)?,
            aliases: Vec::new(),
            parent: parent.map(|parent| parent.id),
            directory: None,
            owner: testkit::owner().ok(),
        },
        at(0),
    )
}

#[test]
fn each_kind_nests_only_where_it_belongs() -> TestResult {
    let software = kinded(TopicKind::Type, "Software", None)?;
    let apps = kinded(TopicKind::Type, "Apps", Some(&software))?;
    let rust = kinded(TopicKind::Type, "Rust", Some(&apps))?;
    let crates = kinded(TopicKind::Type, "Crates", Some(&rust))?;
    let tokio = kinded(TopicKind::Entity, "tokio", Some(&crates))?;
    let project = kinded(TopicKind::Work, "RivetOS", None)?;
    let tree = TopicTree::of([&software, &apps, &rust, &crates, &tokio, &project]);
    let refused = |child: &Topic, parent: &Topic| match check_parent(child, parent, &tree) {
        Ok(()) => None,
        Err(error) => Some(error.to_string()),
    };
    assert_eq!(refused(&tokio, &crates), None);
    assert_eq!(
        refused(&project, &software).as_deref(),
        Some("Work topics nest only under work topics")
    );
    assert_eq!(
        refused(&apps, &project).as_deref(),
        Some("Types nest only under types")
    );
    assert_eq!(
        refused(&tokio, &tokio).as_deref(),
        Some("A topic can't be its own parent.")
    );
    let other = kinded(TopicKind::Entity, "serde", Some(&crates))?;
    assert_eq!(
        refused(&other, &tokio).as_deref(),
        Some("An entity sits only under a type")
    );
    let depth = TopicTree::of([&apps, &rust, &crates, &tokio]);
    assert_eq!(depth.height(apps.id), 3);
    assert!(matches!(
        kinded(TopicKind::Entity, "loose", None),
        Err(TopicError::EntityWithoutType)
    ));
    assert!(matches!(
        MergePlan::new(&tokio, &crates, &tree),
        Err(TopicError::MergeAcrossKinds)
    ));
    assert!(matches!(
        software.accepts_sessions(),
        Err(TopicError::NotWork { .. })
    ));
    assert!(project.accepts_sessions().is_ok());
    Ok(())
}
