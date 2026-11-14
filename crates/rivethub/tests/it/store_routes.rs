pub(crate) const STORE_ROUTES: &[&str] = &[
    "/api/sessions",
    "/api/sessions/trash",
    "/api/sessions/history",
    "/api/sessions/suggestions",
    "/api/today",
    "/api/topics",
    "/api/topics/filing",
    "/api/entities",
    "/api/review",
    "/api/tidy/proposals",
    "/api/changes",
    "/api/me",
    "/api/memory/models",
    "/api/memory/flagged",
    "/api/memory/stats",
];

pub(crate) async fn none_unavailable(
    client: &reqwest::Client,
    base: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut unavailable = Vec::new();
    for route in STORE_ROUTES {
        let reply = client.get(format!("{base}{route}")).send().await?;
        if reply.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            unavailable.push(format!("{route}: {}", reply.text().await?));
        }
    }
    assert!(
        unavailable.is_empty(),
        "every store-backed route has its port: {unavailable:?}"
    );
    Ok(())
}
