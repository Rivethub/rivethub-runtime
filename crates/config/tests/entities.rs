use config::{Profile, type_document, validate_document};

fn document(text: &str) -> Result<serde_yaml_ng::Value, serde_yaml_ng::Error> {
    serde_yaml_ng::from_str(text)
}

#[test]
fn entities_matcher_is_an_optional_section() -> Result<(), Box<dyn std::error::Error>> {
    let yaml = document(
        "memory:\n  backend: sqlite\n  sqlite:\n    path: ':memory:'\nentities:\n  matcher: jev\n  matcher_url: http://host:8015\n",
    )?;
    let result = validate_document(&yaml, Profile::Standalone);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    let typed = type_document(&yaml)?.config;
    let entities = typed.entities.ok_or("entities")?;
    assert_eq!(entities.matcher.as_deref(), Some("jev"));
    assert_eq!(entities.matcher_url.as_deref(), Some("http://host:8015"));
    let omitted = document("memory:\n  backend: sqlite\n")?;
    let omitted = validate_document(&omitted, Profile::Standalone);
    assert!(omitted.errors.is_empty(), "{:?}", omitted.errors);
    let typed = type_document(&document("memory:\n  backend: sqlite\n")?)?.config;
    assert!(typed.entities.is_none());
    Ok(())
}

#[test]
fn entities_matcher_rejects_an_unknown_backend_or_a_jev_without_a_url()
-> Result<(), Box<dyn std::error::Error>> {
    let unknown = document("memory:\n  backend: sqlite\nentities:\n  matcher: other\n")?;
    let result = validate_document(&unknown, Profile::Standalone);
    assert!(
        result.errors.iter().any(|finding| {
            finding.path == "entities.matcher"
                && finding.message == "entities.matcher must be thinkingcap or jev, not other"
        }),
        "{:?}",
        result.errors
    );
    let missing = document("memory:\n  backend: sqlite\nentities:\n  matcher: jev\n")?;
    let result = validate_document(&missing, Profile::Standalone);
    assert!(
        result
            .errors
            .iter()
            .any(|finding| finding.message == "entities.matcher jev needs matcher_url"),
        "{:?}",
        result.errors
    );
    let ftp = document(
        "memory:\n  backend: sqlite\nentities:\n  matcher: jev\n  matcher_url: ftp://host:8015\n",
    )?;
    let result = validate_document(&ftp, Profile::Standalone);
    assert!(
        result.errors.iter().any(|finding| {
            finding.path == "entities.matcher_url"
                && finding.message == "The entities matcher url must be http or https"
        }),
        "{:?}",
        result.errors
    );
    let unknown_key =
        document("memory:\n  backend: sqlite\nentities:\n  matcher: thinkingcap\n  bogus: 1\n")?;
    let result = validate_document(&unknown_key, Profile::Standalone);
    assert!(
        result
            .warnings
            .iter()
            .any(|finding| { finding.message == "Unknown entities key \"bogus\"" }),
        "{:?}",
        result.warnings
    );
    Ok(())
}
