mod jwt;
mod mint;

use config::provider::OpenaiProvider;
use thiserror::Error;

use crate::error::BuildError;
use crate::error::TokenError;
use crate::token::mint::TokenSource;

#[derive(Debug, Error)]
pub(crate) enum CallError {
    #[error(transparent)]
    Token(#[from] TokenError),
    #[error(transparent)]
    Status(#[from] crate::error::StatusError),
    #[error("the model server request failed: {source}")]
    Transport {
        #[source]
        source: reqwest::Error,
    },
}

impl From<CallError> for kernel::model::ModelProviderError {
    fn from(error: CallError) -> Self {
        match error {
            CallError::Token(error) => error.into(),
            CallError::Status(error) => error.into(),
            CallError::Transport { source } => crate::request_error(source),
        }
    }
}

pub(crate) struct Auth {
    static_key: Option<String>,
    command: Option<TokenSource>,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "derive(Debug) would print the static API key; the impl redacts it and defers the command to TokenSource"
    )
)]
impl std::fmt::Debug for Auth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Auth")
            .field(
                "static_key",
                &self.static_key.as_ref().map(|_| "<redacted>"),
            )
            .field("command", &self.command)
            .finish()
    }
}

impl Auth {
    pub(crate) fn static_key(api_key: Option<String>) -> Self {
        Self {
            static_key: api_key,
            command: None,
        }
    }

    pub(crate) fn from_provider(provider: &OpenaiProvider) -> Result<Self, BuildError> {
        let command = match &provider.token_command {
            Some(argv) => Some(TokenSource::new(
                argv,
                provider.token_ttl_ms,
                provider.token_command_timeout_ms,
            )?),
            None => None,
        };
        let static_key = match &command {
            Some(_) => None,
            None => configured_key(provider)?,
        };
        Ok(Self {
            static_key,
            command,
        })
    }

    async fn bearer(&self) -> Result<Option<String>, TokenError> {
        match &self.command {
            Some(command) => command.bearer().await.map(Some),
            None => Ok(self.static_key.clone()),
        }
    }

    async fn remint(&self) -> Result<Option<String>, TokenError> {
        let Some(command) = &self.command else {
            return Ok(None);
        };
        command.invalidate();
        command.bearer().await.map(Some)
    }

    fn invalidate(&self) {
        if let Some(command) = &self.command {
            command.invalidate();
        }
    }
}

fn configured_key(provider: &OpenaiProvider) -> Result<Option<String>, BuildError> {
    if let Some(key) = provider.api_key.as_ref().filter(|key| !key.is_empty()) {
        return Ok(Some(key.clone()));
    }
    let Some(path) = provider.api_key_file.as_ref() else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(path).map_err(|source| BuildError::ApiKeyFile {
        path: path.display().to_string(),
        source,
    })?;
    let key = text.trim();
    if key.is_empty() {
        Ok(None)
    } else {
        Ok(Some(key.to_owned()))
    }
}

pub(crate) fn dispatch(
    auth: &Auth,
    build: impl Fn() -> reqwest::RequestBuilder + Send + Sync,
) -> impl std::future::Future<Output = Result<reqwest::Response, CallError>> + Send {
    Box::pin(dispatch_call(auth, build))
}

async fn dispatch_call(
    auth: &Auth,
    build: impl Fn() -> reqwest::RequestBuilder + Send + Sync,
) -> Result<reqwest::Response, CallError> {
    let token = auth.bearer().await?;
    let response = send(&build, token.as_deref()).await?;
    if response.status() != reqwest::StatusCode::UNAUTHORIZED || token.is_none() {
        return crate::check_status(response)
            .await
            .map_err(CallError::Status);
    }
    match auth.remint().await {
        Ok(Some(fresh)) => {
            drop(response);
            let retried = send(&build, Some(fresh.as_str())).await?;
            if retried.status() == reqwest::StatusCode::UNAUTHORIZED {
                auth.invalidate();
            }
            crate::check_status(retried)
                .await
                .map_err(CallError::Status)
        }
        Ok(None) => crate::check_status(response)
            .await
            .map_err(CallError::Status),
        Err(error) => Err(CallError::Token(error)),
    }
}

async fn send(
    build: &(impl Fn() -> reqwest::RequestBuilder + Send + Sync),
    token: Option<&str>,
) -> Result<reqwest::Response, CallError> {
    let mut request = build();
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    request
        .send()
        .await
        .map_err(|source| CallError::Transport { source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_the_key_and_the_argv() {
        let static_only = Auth::static_key(Some("static-secret".to_owned()));
        let static_text = format!("{static_only:?}");
        assert!(static_text.contains("<redacted>"));
        assert!(!static_text.contains("static-secret"));
        let provider = OpenaiProvider {
            api_key: Some("static-secret".to_owned()),
            token_command: Some(vec!["/bin/echo".to_owned(), "static-secret".to_owned()]),
            ..OpenaiProvider::default()
        };
        let auth = Auth::from_provider(&provider).expect("auth");
        let text = format!("{auth:?}");
        assert!(!text.contains("static-secret"));
        assert!(!text.contains("/bin/echo"));
        assert!(text.contains("argv"));
    }
}
