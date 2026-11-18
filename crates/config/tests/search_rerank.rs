type TestResult = Result<(), Box<dyn std::error::Error>>;

fn document(section: &str) -> Result<serde_yaml_ng::Value, serde_yaml_ng::Error> {
    serde_yaml_ng::from_str(&format!("memory:\n  recall:\n    rerank:\n{section}"))
}

#[test]
fn reranking_is_opt_in_and_uses_the_measured_defaults() -> TestResult {
    let empty = config::type_document(&serde_yaml_ng::from_str("memory: {}")?)?;
    assert!(empty.config.memory.ok_or("memory")?.recall.rerank.is_none());

    for path in ["", "/", "/v1/decide"] {
        let yaml = document(&format!("      url: http://example.test{path}\n"))?;
        assert!(config::validate_document(&yaml).valid());
        let typed = config::type_document(&yaml)?;
        let settings = typed
            .config
            .memory
            .ok_or("memory")?
            .recall
            .rerank
            .ok_or("rerank")?;
        assert_eq!(
            (settings.top, settings.chars, settings.timeout_ms),
            (24, 300, 1500)
        );
        assert_eq!(
            settings.endpoint()?.as_str(),
            "http://example.test/v1/decide"
        );
    }
    Ok(())
}

#[test]
fn settings_validate_endpoints_and_bounds() -> TestResult {
    for (section, path) in [
        ("      url: file:///decide\n", "url"),
        ("      url: ' '\n", "url"),
        ("      url: http://example.test/?foo=bar\n", "url"),
        ("      top: 0\n", "top"),
        ("      top: 51\n", "top"),
        ("      chars: 0\n", "chars"),
        ("      chars: 4001\n", "chars"),
        ("      timeout_ms: 0\n", "timeout_ms"),
        ("      timeout_ms: 5001\n", "timeout_ms"),
    ] {
        let yaml = if path == "url" {
            document(section)?
        } else {
            document(&format!("      url: http://example.test\n{section}"))?
        };
        let report = config::validate_document(&yaml);
        let expected = format!("memory.recall.rerank.{path}");
        assert!(
            report.errors.iter().any(|error| error.path == expected),
            "{report}"
        );
    }
    let missing_url = document("      top: 24\n")?;
    assert!(!config::validate_document(&missing_url).valid());
    Ok(())
}
