use super::*;

fn candidate(
    id: u128,
    name: &str,
    type_name: &str,
    trigram: f32,
    embedding: f32,
) -> MatchCandidate {
    MatchCandidate {
        id: Uuid::from_u128(id),
        name: name.to_owned(),
        type_name: type_name.to_owned(),
        aliases: Vec::new(),
        summary: String::new(),
        trigram,
        embedding,
    }
}

#[test]
fn matching_never_crosses_host_project_model_repo_person_or_org() {
    assert!(types_compatible("Hosts", "hosts"));
    assert!(types_compatible("Software › Apps", "Apps"));
    assert!(types_compatible("Software", "CLIs and tools"));
    assert!(!types_compatible("Hosts", "Projects"));
    assert!(!types_compatible("Hosts", "Models"));
    assert!(!types_compatible("Hosts", "Hardware"));
    assert!(!types_compatible("People", "Organizations"));
    assert!(!types_compatible("Repos", "Software"));
    assert!(!types_compatible("Services", "Projects"));
    assert!(!types_compatible("Places", "Hosts"));
    assert!(merge_types_allowed("Hosts", "hosts"));
    assert!(!merge_types_allowed("Hosts", "Projects"));
    assert!(!merge_types_allowed("People", "Hosts"));
    assert!(!merge_types_allowed("Organizations", "Hosts"));
    assert!(host_party_mismatch("People", "Hosts"));
    assert!(host_party_mismatch("Hosts", "Organizations"));
    assert!(!host_party_mismatch("Hosts", "Hosts"));
}

#[test]
fn below_the_threshold_holds_and_a_sure_choice_binds() {
    assert_eq!(gate("0", 0.4, MATCH_THRESHOLD, 2), MatchAct::Hold);
    assert_eq!(gate("new", 0.5, MATCH_THRESHOLD, 2), MatchAct::Hold);
    assert_eq!(gate("new", 0.91, MATCH_THRESHOLD, 2), MatchAct::New);
    assert_eq!(gate("1", 0.8, MATCH_THRESHOLD, 2), MatchAct::Existing(1));
    assert_eq!(gate("9", 0.99, MATCH_THRESHOLD, 2), MatchAct::Hold);
    assert_eq!(normalize_confidence(91.0), 0.91);
}

#[test]
fn an_exact_name_binds_across_types_and_a_person_stays_apart() {
    let host = candidate(1, "gpubox", "Hosts", 1.0, 0.0);
    let project = candidate(2, "gpubox", "Projects", 1.0, 0.0);
    let mixed = [host.clone(), project];
    assert_eq!(
        exact_id("Hosts", "GPUBox", &[], &mixed),
        Some(Uuid::from_u128(1))
    );
    assert_eq!(
        exact_id("Projects", "gpubox", &[], &mixed),
        Some(Uuid::from_u128(1)),
        "a project with the host's name is the host"
    );
    let person = candidate(3, "Phil", "People", 1.0, 0.0);
    let product = candidate(4, "Phil", "Software", 1.0, 0.0);
    assert_eq!(
        exact_id("Software", "phil", &[], &[person, product.clone()]),
        Some(product.id)
    );
    assert_eq!(
        chosen_id("Hosts", &mixed, MatchAct::Existing(1)),
        None,
        "a decider index still has to be the same type"
    );
    assert_eq!(
        chosen_id("Hosts", &mixed, MatchAct::Existing(0)),
        Some(host.id)
    );
}

#[test]
fn the_same_normalized_name_is_one_entity_except_a_person() {
    let pairs = [
        (
            "Organizations",
            "Hugging Face",
            "Software",
            "Hugging Face",
            true,
        ),
        ("Hosts", "datahub", "Software", "datahub", true),
        ("Agents", "rivet-glm", "Software", "rivet-glm", true),
        ("Agents", "rivet-claude", "Hosts", "rivet-claude", true),
        ("Software", "T3 Code", "Projects", "T3 Code", true),
        ("Agents", "Grok", "Models", "Grok", true),
        (
            "Libraries and frameworks",
            "transformers",
            "Software",
            "transformers",
            true,
        ),
        ("Hosts", "omarchy", "Software", "Omarchy", true),
        (
            "People",
            "windowsxp811203",
            "Organizations",
            "windowsxp811203",
            false,
        ),
        ("Services", "postgresql", "Databases", "Postgres", false),
        ("Services", "datahub", "Hosts", "acme/datahub", true),
        ("Services", "datahub", "Hosts", "datahub-extra", false),
        ("Services", "web", "Repos", "acme/web", false),
    ];
    for (left_type, left_name, right_type, right_name, same) in pairs {
        assert_eq!(
            names_same_entity(left_type, right_type) && names_bind(left_name, right_name),
            same,
            "{left_name} [{left_type}] vs {right_name} [{right_type}]"
        );
    }
    let repo = "bottlecapai/ThinkingCap-Qwen3.8-Flash-Next";
    assert!(!names_bind("ThinkingCap", repo));
    assert!(identity_keys("acme/datahub").contains(&"datahub".to_owned()));
    assert!(!identity_keys("acme/web").contains(&"web".to_owned()));
    assert_eq!(canonical_key("PostgreSQL"), "postgresql");
    assert_eq!(dominant_type(&[]), None);
    let tied = ["Hosts".to_owned(), "Software".to_owned()];
    assert_eq!(dominant_type(&tied), None);
    let software = [
        "Software".to_owned(),
        "Hosts".to_owned(),
        "Software".to_owned(),
    ];
    assert_eq!(dominant_type(&software), Some("Software"));
}

#[test]
fn a_ref_is_verified_only_when_the_name_is_already_that_entity() {
    let aliases = vec!["g9".to_owned(), "srv9".to_owned()];
    assert!(ref_verified("G9", "gpubox", &aliases));
    assert!(ref_verified("GPU Box", "gpubox", &[]));
    assert!(!ref_verified("gerty", "gpubox", &aliases));
    assert!(!ref_verified("gerty", "gpubox", &[]));
    assert!(!ref_verified("", "gpubox", &aliases));
}

#[test]
fn weak_scores_are_not_candidates() {
    let weak = candidate(1, "gerty", "Hosts", 0.1, 0.2);
    let strong = candidate(2, "g9", "Hosts", 0.1, 0.7);
    let ranked = rank_candidates(vec![weak, strong.clone()]);
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked.first().map(|item| item.id), Some(strong.id));
}

#[test]
fn a_type_spelling_matches_only_the_same_word() {
    let existing = vec![
        "Hosts".to_owned(),
        "Services".to_owned(),
        "People".to_owned(),
    ];
    assert_eq!(same_type_name("host", &existing), Some("Hosts"));
    assert_eq!(same_type_name("Hosts", &existing), Some("Hosts"));
    assert_eq!(same_type_name("machine", &existing), None);
    assert_eq!(same_type_name("person", &existing), None);
    assert_eq!(same_type_name("people", &existing), Some("People"));
}

#[test]
fn markers_keep_identifiers_and_skip_plain_words() {
    let texts = vec![
        "box9".to_owned(),
        "G9".to_owned(),
        "gerty".to_owned(),
        "at 11.22.33.44".to_owned(),
        "https://gpubox.local/status".to_owned(),
        "acme/datahub".to_owned(),
        "web".to_owned(),
    ];
    let markers = surface_markers(&texts);
    assert!(markers.iter().any(|item| item == "box9"));
    assert!(markers.iter().any(|item| item == "11.22.33.44"));
    assert!(markers.iter().any(|item| item == "gpubox.local"));
    assert!(markers.iter().any(|item| item == "acme/datahub"));
    assert!(
        markers
            .iter()
            .all(|item| item != "G9" && item != "gerty" && item != "web")
    );
}
