use std::path::Path;
use std::sync::Arc;

use gateway::local_owner::LocalOwner;
use gateway::local_owner::read_token;

pub(crate) fn holder() -> Arc<LocalOwner> {
    static HOLDER: std::sync::LazyLock<Arc<LocalOwner>> =
        std::sync::LazyLock::new(|| match LocalOwner::unpersisted() {
            Ok(holder) => Arc::new(holder),
            Err(error) => unreachable!("random bytes for the test owner token: {error}"),
        });
    Arc::clone(&HOLDER)
}

pub(crate) fn authorize<B>(request: &mut axum::http::Request<B>) {
    if let Ok(value) =
        axum::http::HeaderValue::from_str(&format!("Bearer {}", holder().current().expose()))
    {
        request
            .headers_mut()
            .entry(axum::http::header::AUTHORIZATION)
            .or_insert(value);
    }
}

pub(crate) fn headers(state_dir: &Path) -> Result<reqwest::header::HeaderMap, String> {
    let token = read_token(state_dir)
        .map_err(|error| error.to_string())?
        .ok_or("the node wrote no local owner token")?;
    let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token.expose()))
        .map_err(|error| error.to_string())?;
    value.set_sensitive(true);
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::AUTHORIZATION, value);
    Ok(headers)
}

pub(crate) fn home_headers(home: &Path) -> Result<reqwest::header::HeaderMap, String> {
    headers(&home.join(".rivethub"))
}

pub(crate) fn holder_headers() -> Result<reqwest::header::HeaderMap, String> {
    let mut value =
        reqwest::header::HeaderValue::from_str(&format!("Bearer {}", holder().current().expose()))
            .map_err(|error| error.to_string())?;
    value.set_sensitive(true);
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::AUTHORIZATION, value);
    Ok(headers)
}
