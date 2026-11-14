use config::model::DropsSection;
use config::{type_document, validate_document};

fn document(text: &str) -> Result<serde_yaml_ng::Value, serde_yaml_ng::Error> {
    serde_yaml_ng::from_str(text)
}

fn drops_of(yaml: &serde_yaml_ng::Value) -> Result<DropsSection, Box<dyn std::error::Error>> {
    Ok(type_document(yaml)?
        .config
        .gateway
        .ok_or("gateway section")?
        .drops)
}

#[test]
fn the_drop_box_is_off_with_cloud_limits_by_default() -> Result<(), Box<dyn std::error::Error>> {
    let yaml = document("memory:\n  backend: sqlite\ngateway:\n  port: 5174\n")?;
    let result = validate_document(&yaml);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(
        drops_of(&yaml)?,
        DropsSection {
            enabled: false,
            root: None,
            max_bytes: 64 * 1024 * 1024,
            per_user_drops: 50,
            per_user_bytes: 512 * 1024 * 1024,
        }
    );
    Ok(())
}

#[test]
fn a_drop_size_the_cli_cannot_download_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    for (size, refused) in [
        ("0", true),
        ("67108865", true),
        ("67108864", false),
        ("1", false),
    ] {
        let yaml = document(&format!(
            "memory:\n  backend: sqlite\ngateway:\n  drops:\n    enabled: true\n    max_bytes: {size}\n"
        ))?;
        let result = validate_document(&yaml);
        let flagged = result
            .errors
            .iter()
            .any(|finding| finding.path == "gateway.drops.max_bytes");
        assert_eq!(flagged, refused, "max_bytes {size}: {:?}", result.errors);
    }
    Ok(())
}

#[test]
fn an_unknown_drops_key_is_a_type_error() -> Result<(), Box<dyn std::error::Error>> {
    let yaml = document("memory:\n  backend: sqlite\ngateway:\n  drops:\n    enable: true\n")?;
    assert!(type_document(&yaml).is_err());
    let result = validate_document(&yaml);
    assert!(
        !result.errors.is_empty(),
        "an unknown key passed validation"
    );
    Ok(())
}
