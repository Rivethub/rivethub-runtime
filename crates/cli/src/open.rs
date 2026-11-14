use std::time::Duration;

use config::Environment;
use wire::gateway::auth::LoginCodeResponse;

use crate::status::{self, Endpoint};
use crate::{Error, Report, configuration};

const TIMEOUT: Duration = Duration::from_secs(10);
const MINT_PATH: &str = "api/auth/login-codes";

const NO_APP_MARK: &str = "The web app is not installed";

pub(crate) async fn run(env: &Environment, no_browser: bool) -> Result<Report, Error> {
    let (_, loaded) = configuration::load(env).await?;
    let Endpoint { url, client } = status::gateway(env, &loaded.config, TIMEOUT).await?;
    if serves_no_web_app(&client, &url).await? {
        return Err(Error::NoWebApp);
    }
    let response = client.post(url.join(MINT_PATH)?).send().await?;
    if !response.status().is_success() {
        return Err(Error::AuthOpen {
            status: response.status().as_u16(),
        });
    }
    let minted: LoginCodeResponse = response.json().await?;
    let mut login = url.join("login")?;
    login.set_fragment(Some(&format!("code={}", minted.code.expose())));
    let mut report = Report::success(format!("{login}\n"));
    if no_browser || !has_display(env) {
        return Ok(report);
    }
    if let Err(error) = opener::open(login.as_str()) {
        report.stderr = format!("Could not launch a browser ({error}). Open the URL above.\n");
    }
    Ok(report)
}

async fn serves_no_web_app(client: &reqwest::Client, url: &url::Url) -> Result<bool, Error> {
    let page = client.get(url.clone()).send().await?;
    let status = page.status();
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a non-UTF-8 header is no header"
        )
    )]
    let html = page
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"));
    let body = page.text().await?;
    Ok((html && body.contains(NO_APP_MARK))
        || (status.as_u16() == 404 && body.contains("not found")))
}

fn has_display(env: &Environment) -> bool {
    !cfg!(target_os = "linux")
        || ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|name| env.get(name).is_some_and(|value| !value.is_empty()))
}
