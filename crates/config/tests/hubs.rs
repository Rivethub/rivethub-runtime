use config::{Profile, type_document, validate_document};
use wire::gateway::account::HubKind;

fn document(text: &str) -> Result<serde_yaml_ng::Value, serde_yaml_ng::Error> {
    serde_yaml_ng::from_str(text)
}

#[test]
fn hubs_telemetry_and_cloud_are_typed_sections() -> Result<(), Box<dyn std::error::Error>> {
    let yaml = document(
        "memory:\n  backend: sqlite\nhubs:\n  - name: household\n    url: https://team.example.com/\n  - name: desk\n    url: http://127.0.0.1:5174\n    kind: local\ntelemetry:\n  disabled: true\ncloud:\n  url: http://127.0.0.1:8080\n  update_key: AAAA\n",
    )?;
    let result = validate_document(&yaml, Profile::Standalone);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    let typed = type_document(&yaml)?.config;
    let hubs = typed.hubs.ok_or("hubs")?;
    assert_eq!(
        hubs.iter()
            .map(|hub| (hub.name.as_str(), hub.kind))
            .collect::<Vec<_>>(),
        vec![("household", HubKind::Team), ("desk", HubKind::Local)]
    );
    assert!(typed.telemetry.is_some_and(|section| section.disabled));
    assert_eq!(
        typed
            .cloud
            .and_then(|section| section.url)
            .map(String::from)
            .as_deref(),
        Some("http://127.0.0.1:8080/")
    );
    Ok(())
}

#[test]
fn a_hub_listed_twice_or_off_the_web_is_an_error() -> Result<(), Box<dyn std::error::Error>> {
    let yaml = document(
        "memory:\n  backend: sqlite\nhubs:\n  - name: lab\n    url: https://lab.example.com\n  - name: Lab\n    url: ftp://lab.example.com\n",
    )?;
    let result = validate_document(&yaml, Profile::Standalone);
    let paths: Vec<_> = result
        .errors
        .iter()
        .map(|finding| finding.path.as_str())
        .collect();
    assert_eq!(paths, vec!["hubs.1.name", "hubs.1.url"]);
    Ok(())
}
