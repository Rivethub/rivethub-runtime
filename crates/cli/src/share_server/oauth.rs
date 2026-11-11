use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use oauth2::AsyncHttpClient;
use oauth2::AuthType;
use oauth2::ClientId;
use oauth2::DeviceAuthorizationUrl;
use oauth2::EndpointNotSet;
use oauth2::EndpointSet;
use oauth2::ErrorResponse;
use oauth2::HttpRequest;
use oauth2::HttpResponse;
use oauth2::RequestTokenError;
use oauth2::RevocationUrl;
use oauth2::TokenUrl;
use oauth2::basic::BasicClient;
use reqwest::StatusCode;
use reqwest::header::HeaderValue;
use reqwest::header::RETRY_AFTER;
use serde::Deserialize;
use url::Url;

use super::CLIENT_ID;
use super::DEVICE_PATH;
use super::REVOKE_PATH;
use super::Server;
use super::ServerError;
use super::TOKEN_PATH;
use super::checked;

const METADATA_PATH: &str = ".well-known/oauth-authorization-server";
const RATE_RETRIES: u32 = 2;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

pub(crate) type TokenClient =
    BasicClient<EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

#[derive(Debug, Clone)]
pub(crate) struct Endpoints {
    pub(crate) device: Option<DeviceAuthorizationUrl>,
    pub(crate) token: TokenUrl,
    pub(crate) revocation: Option<RevocationUrl>,
}

#[derive(Deserialize)]
struct Metadata {
    issuer: String,
    token_endpoint: Url,
    device_authorization_endpoint: Option<Url>,
    revocation_endpoint: Option<Url>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Transport {
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("the share server's OAuth endpoint answered {0}")]
    Status(u16),
    #[error("the share server is rate limiting this machine")]
    RateLimited { retry_after: Option<Duration> },
    #[error("the OAuth request names an invalid URL: {0}")]
    Url(String),
}

impl Server {
    pub(crate) async fn endpoints(&self) -> Result<Endpoints, ServerError> {
        let response = self
            .client
            .get(self.endpoint(METADATA_PATH)?)
            .send()
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            tracing::debug!(
                server = %self.host,
                "no RFC 8414 metadata; using the /api/cli endpoints of rivethub.cloud's first device-flow server"
            );
            return Ok(Endpoints {
                device: Some(DeviceAuthorizationUrl::from_url(
                    self.endpoint(DEVICE_PATH)?,
                )),
                token: TokenUrl::from_url(self.endpoint(TOKEN_PATH)?),
                revocation: Some(self.revocation_url_oauth2_accepts(self.endpoint(REVOKE_PATH)?)?),
            });
        }
        let metadata: Metadata = checked(response).await?.json().await?;
        if metadata.issuer.trim_end_matches('/') != self.base.as_str().trim_end_matches('/') {
            return Err(ServerError::Protocol(format!(
                "its OAuth metadata names the issuer {}, not {}",
                metadata.issuer, self.base
            )));
        }
        Ok(Endpoints {
            device: metadata
                .device_authorization_endpoint
                .map(DeviceAuthorizationUrl::from_url),
            token: TokenUrl::from_url(metadata.token_endpoint),
            revocation: metadata
                .revocation_endpoint
                .map(|url| self.revocation_url_oauth2_accepts(url))
                .transpose()?,
        })
    }

    pub(crate) fn token_client(endpoints: &Endpoints) -> TokenClient {
        BasicClient::new(ClientId::new(CLIENT_ID.to_owned()))
            .set_auth_type(AuthType::RequestBody)
            .set_token_uri(endpoints.token.clone())
    }

    fn revocation_url_oauth2_accepts(&self, url: Url) -> Result<RevocationUrl, ServerError> {
        self.loopback_scheme(url, "http", "https")
            .map(RevocationUrl::from_url)
            .map_err(|()| ServerError::Protocol("a loopback URL kept its scheme".to_owned()))
    }

    fn loopback_scheme(&self, mut url: Url, from: &str, to: &str) -> Result<Url, ()> {
        if self.host.is_loopback() && self.same_authority(&url) && url.scheme() == from {
            url.set_scheme(to)?;
        }
        Ok(url)
    }

    fn same_authority(&self, url: &Url) -> bool {
        url.host_str() == self.base.host_str() && url.port() == self.base.port()
    }

    fn wire_url(&self, uri: &str) -> Result<Url, Transport> {
        let url = Url::parse(uri).map_err(|error| Transport::Url(error.to_string()))?;
        self.loopback_scheme(url, "https", "http")
            .map_err(|()| Transport::Url(uri.to_owned()))
    }

    async fn exchange(&self, request: HttpRequest) -> Result<HttpResponse, Transport> {
        let (parts, body) = request.into_parts();
        let url = self.wire_url(&parts.uri.to_string())?;
        let mut retries = 0;
        loop {
            let response = self
                .client
                .request(parts.method.clone(), url.clone())
                .headers(parts.headers.clone())
                .body(body.clone())
                .send()
                .await?;
            let status = response.status();
            if status == StatusCode::TOO_MANY_REQUESTS {
                match response
                    .headers()
                    .get(RETRY_AFTER)
                    .map(retry_after)
                    .transpose()
                {
                    Ok(Some(wait)) if retries < RATE_RETRIES && wait <= MAX_RETRY_AFTER => {
                        retries += 1;
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    Ok(retry_after) => return Err(Transport::RateLimited { retry_after }),
                    Err(UnreadableRetryAfter) => {
                        return Err(Transport::RateLimited { retry_after: None });
                    }
                }
            }
            if status.is_server_error() {
                return Err(Transport::Status(status.as_u16()));
            }
            let headers = response.headers().clone();
            let mut answer = HttpResponse::new(response.bytes().await?.to_vec());
            *answer.status_mut() = status;
            *answer.headers_mut() = headers;
            return Ok(answer);
        }
    }
}

impl<'c> AsyncHttpClient<'c> for Server {
    type Error = Transport;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, Transport>> + Send + 'c>>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(self.exchange(request))
    }
}

#[derive(Debug)]
struct UnreadableRetryAfter;

fn retry_after(value: &HeaderValue) -> Result<Duration, UnreadableRetryAfter> {
    let text = value.to_str().map_err(|_| UnreadableRetryAfter)?.trim();
    if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
        return text
            .parse()
            .map(Duration::from_secs)
            .map_err(|_| UnreadableRetryAfter);
    }
    let at = chrono::DateTime::parse_from_rfc2822(text).map_err(|_| UnreadableRetryAfter)?;
    let wait = at.with_timezone(&chrono::Utc) - chrono::Utc::now();
    if wait <= chrono::TimeDelta::zero() {
        return Ok(Duration::ZERO);
    }
    wait.to_std().map_err(|_| UnreadableRetryAfter)
}

pub(crate) fn failed<T: ErrorResponse + 'static>(
    error: RequestTokenError<Transport, T>,
) -> ServerError {
    match error {
        RequestTokenError::ServerResponse(refusal) => ServerError::Refused(refusal.to_string()),
        RequestTokenError::Request(Transport::Http(error)) => ServerError::Http(error),
        RequestTokenError::Request(error @ Transport::Status(status)) => ServerError::Status {
            status,
            body: error.to_string(),
        },
        RequestTokenError::Request(Transport::RateLimited { retry_after }) => {
            ServerError::RateLimited(retry_after.map_or(0, |wait| wait.as_secs()))
        }
        RequestTokenError::Request(Transport::Url(reason)) | RequestTokenError::Other(reason) => {
            ServerError::Protocol(reason)
        }
        RequestTokenError::Parse(error, _) => ServerError::Protocol(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_reads_seconds_and_dates() {
        let header = |text: &str| HeaderValue::from_str(text).expect("header");
        assert_eq!(retry_after(&header("7")).ok(), Some(Duration::from_secs(7)));
        assert_eq!(
            retry_after(&header("Sun, 06 Nov 1994 08:49:37 GMT")).ok(),
            Some(Duration::ZERO),
            "a date in the past waits for nothing"
        );
        let later = (chrono::Utc::now() + chrono::TimeDelta::seconds(90)).to_rfc2822();
        let wait = retry_after(&header(&later)).expect("a date");
        assert!(wait > Duration::from_secs(80), "{wait:?}");
        assert!(retry_after(&header("soon")).is_err());
        assert!(retry_after(&header("-3")).is_err());
    }
}
