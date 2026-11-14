use serde_json::Value;
use wire::gateway::memory::{MemoryDegraded, MemorySearchResponse};

fn read(degraded: &str) -> Result<MemorySearchResponse, serde_json::Error> {
    serde_json::from_str(&format!(
        r#"{{"query":"q","scope":"both","degraded":{degraded},"results":[]}}"#
    ))
}

#[test]
fn the_string_an_older_node_sent_reads_as_the_object() -> Result<(), Box<dyn std::error::Error>> {
    let object = read(
        r#"{"reason":"embedding endpoint not configured","effect":"Keyword / FTS ranking only"}"#,
    )?;
    assert_eq!(
        object.degraded,
        Some(MemoryDegraded {
            reason: "embedding endpoint not configured".to_owned(),
            effect: "Keyword / FTS ranking only".to_owned(),
        })
    );
    let text = read(r#""query embedding failed: embed HTTP 500""#)?;
    assert_eq!(
        text.degraded
            .as_ref()
            .map(|degraded| degraded.reason.as_str()),
        Some("query embedding failed: embed HTTP 500")
    );
    assert_eq!(read("null")?.degraded, None);
    let written: Value = serde_json::to_value(&text)?;
    assert!(
        written.pointer("/degraded/reason").is_some()
            && written.pointer("/degraded/effect").is_some(),
        "always written as the object: {written}"
    );
    Ok(())
}
