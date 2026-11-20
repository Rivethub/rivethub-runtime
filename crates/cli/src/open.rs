use std::time::Duration;

use config::Environment;
use serde::Serialize;
use wire::gateway::auth::LoginCodeResponse;

use crate::status::{self, Endpoint};
use crate::{Error, Exit, Report, configuration, facts};

const TIMEOUT: Duration = Duration::from_secs(10);
const MINT_PATH: &str = "api/auth/login-codes";

const NO_APP_MARK: &str = "The web app is not installed";

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum Summary {
    Minted {
        origin: String,
        url: String,
        code: String,
        expires_at: jiff::Timestamp,
    },
    Failed {
        reason: FailureReason,
        error: String,
    },
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum FailureReason {
    NoWebApp,
    NotRunning,
    Error,
}

pub(crate) async fn run(env: &Environment, no_browser: bool, json: bool) -> Result<Report, Error> {
    let minted = mint(env).await;
    if !json {
        let mut report = Report::success(format!("{}\n", minted?.url));
        if no_browser || !has_display(env) {
            return Ok(report);
        }
        if let Err(error) = opener::open(report.stdout.trim_end()) {
            report.stderr = format!("Could not launch a browser ({error}). Open the URL above.\n");
        }
        return Ok(report);
    }
    let (summary, exit) = match minted {
        Ok(Minted {
            origin,
            url,
            code,
            expires_at,
        }) => (
            Summary::Minted {
                origin,
                url: url.into(),
                code,
                expires_at,
            },
            Exit::Ok,
        ),
        Err(error) => {
            let reason = if matches!(error, Error::NoWebApp) {
                FailureReason::NoWebApp
            } else if matches!(&error, Error::GatewayNotRunning { .. })
                || matches!(&error, Error::Http(source) if source.is_connect())
            {
                FailureReason::NotRunning
            } else {
                FailureReason::Error
            };
            let exit = error.exit();
            let mut report = facts::machine(&Summary::Failed {
                reason,
                error: error.to_string(),
            })?;
            report.stderr = format!("Error: {error}\n");
            report.exit = exit;
            return Ok(report);
        }
    };
    let mut report = facts::machine(&summary)?;
    report.exit = exit;
    Ok(report)
}

struct Minted {
    origin: String,
    url: url::Url,
    code: String,
    expires_at: jiff::Timestamp,
}

async fn mint(env: &Environment) -> Result<Minted, Error> {
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
    let code = minted.code.expose().to_owned();
    let mut login = url.join("login")?;
    login.set_fragment(Some(&format!("code={code}")));
    Ok(Minted {
        origin: facts::origin(&url),
        url: login,
        code,
        expires_at: jiff::Timestamp::now()
            .checked_add(Duration::from_secs(minted.expires_in_secs))
            .map_err(|_| Error::LoginCodeExpiry)?,
    })
}

async fn serves_no_web_app(client: &reqwest::Client, url: &url::Url) -> Result<bool, Error> {
    let page = client.get(url.clone()).send().await?;
    let status = page.status();
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
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
