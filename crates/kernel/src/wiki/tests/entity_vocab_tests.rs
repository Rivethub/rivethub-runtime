use super::*;

#[test]
fn near_duplicates_fold_onto_the_vocabulary_and_other_needs_a_key() {
    assert_eq!(
        canonical_attribute("Hosts", "Memory").as_deref(),
        Some("ram")
    );
    assert_eq!(canonical_attribute("Hosts", "mem").as_deref(), Some("ram"));
    assert_eq!(canonical_attribute("host", "GPU").as_deref(), Some("gpus"));
    assert_eq!(
        canonical_attribute("Hosts", "filesystem").as_deref(),
        Some("filesystems")
    );
    assert_eq!(
        canonical_attribute("Hosts", "deploy").as_deref(),
        Some("role")
    );
    assert_eq!(
        canonical_attribute("Services", "drop-in").as_deref(),
        Some("config")
    );
    assert_eq!(
        canonical_attribute("Services", "dropin").as_deref(),
        Some("config")
    );
    assert_eq!(
        canonical_attribute("Services", "Drop_In").as_deref(),
        Some("config")
    );
    assert_eq!(canonical_attribute("Hosts", "other"), None);
    assert_eq!(canonical_attribute("Hosts", "other-"), None);
    assert_eq!(
        canonical_attribute("Hosts", "other-warranty").as_deref(),
        Some("other-warranty")
    );
    assert_eq!(
        canonical_attribute("Hosts", "warranty").as_deref(),
        Some("other-warranty")
    );
    assert_eq!(
        canonical_attribute("People", "preferences").as_deref(),
        Some("preferences")
    );
    assert_eq!(vocabulary_key("Hosts", "warranty"), None);
    assert!(
        vocabulary()
            .iter()
            .any(|entry| entry.root == "service" && entry.attributes.contains(&"rollback"))
    );
}
