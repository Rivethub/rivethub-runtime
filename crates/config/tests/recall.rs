use config::recall::RecallSection;

#[test]
fn recall_weight_is_bounded_and_defaults_to_a_mild_preference() {
    let default: RecallSection = serde_json::from_str("{}").expect("default");
    assert_eq!(default.weight(), 1.25);
    for value in [1.0, 1.25, 2.0] {
        let parsed: RecallSection =
            serde_json::from_str(&format!(r#"{{"own_weight":{value}}}"#)).expect("valid");
        assert_eq!(parsed.weight(), value);
    }
    for value in [0.0, 0.99, 2.01, -1.0] {
        let yaml = format!("memory:\n  recall: {{own_weight: {value}}}\n");
        let result = config::validate_document(
            &serde_yaml_ng::from_str(&yaml).expect("yaml"),
            config::Profile::Standalone,
        );
        assert!(
            result
                .errors
                .iter()
                .any(|finding| finding.path == "memory.recall.own_weight")
        );
    }
}
