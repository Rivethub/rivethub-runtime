use std::future::Future;
use std::pin::Pin;

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
use serde::Deserialize;
use url::Url;

use super::CLIENT_ID;
use super::Server;
use super::ServerError;
use super::checked;

const METADATA_PATH: &str = ".well-known/oauth-authorization-server";

pub(crate) type OauthClient =
    BasicClient<EndpointNotSet, EndpointSet, EndpointNotSet, EndpointSet, EndpointSet>;

#[derive(Debug, Clone)]
pub(crate) struct Endpoints {
    device: DeviceAuthorizationUrl,
    token: TokenUrl,
    revocation: RevocationUrl,
}

#[derive(Deserialize)]
struct Metadata {
    #[serde(rename = "token_endpoint")]
    token: Url,
    #[serde(rename = "device_authorization_endpoint")]
    device: Url,
    #[serde(rename = "revocation_endpoint")]
    revocation: Url,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Transport {
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("the share server's OAuth endpoint answered {0}")]
    Status(u16),
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
        if response.status().is_client_error() {
            return Err(ServerError::NoDeviceFlow(self.host.clone()));
        }
        let body = checked(response).await?.bytes().await?;
        let metadata: Metadata = serde_json::from_slice(&body)
            .map_err(|error| ServerError::Protocol(format!("its OAuth metadata: {error}")))?;
        let own_origin = [&metadata.token, &metadata.device, &metadata.revocation]
            .iter()
            .all(|url| url.origin() == self.base.origin());
        if !own_origin {
            return Err(ServerError::Protocol(
                "its OAuth metadata sends sign-in to another origin".to_owned(),
            ));
        }
        Ok(Endpoints {
            device: DeviceAuthorizationUrl::from_url(metadata.device),
            token: TokenUrl::from_url(metadata.token),
            revocation: self.revocation_url_oauth2_accepts(metadata.revocation)?,
        })
    }

    pub(crate) fn oauth_client(endpoints: &Endpoints) -> OauthClient {
        BasicClient::new(ClientId::new(CLIENT_ID.to_owned()))
            .set_auth_type(AuthType::RequestBody)
            .set_device_authorization_url(endpoints.device.clone())
            .set_token_uri(endpoints.token.clone())
            .set_revocation_url(endpoints.revocation.clone())
    }

    fn revocation_url_oauth2_accepts(&self, url: Url) -> Result<RevocationUrl, ServerError> {
        self.loopback_scheme(url, "http", "https")
            .map(RevocationUrl::from_url)
            .map_err(|()| ServerError::Protocol("a loopback URL kept its scheme".to_owned()))
    }

    fn loopback_scheme(&self, mut url: Url, from: &str, to: &str) -> Result<Url, ()> {
        if self.host.is_loopback() && url.scheme() == from {
            url.set_scheme(to)?;
        }
        Ok(url)
    }

    fn wire_url(&self, uri: &str) -> Result<Url, Transport> {
        let url = Url::parse(uri).map_err(|error| Transport::Url(error.to_string()))?;
        self.loopback_scheme(url, "https", "http")
            .map_err(|()| Transport::Url(uri.to_owned()))
    }

    async fn exchange(&self, request: HttpRequest) -> Result<HttpResponse, Transport> {
        let (parts, body) = request.into_parts();
        let response = self
            .client
            .request(parts.method, self.wire_url(&parts.uri.to_string())?)
            .headers(parts.headers)
            .body(body)
            .send()
            .await?;
        let status = response.status();
        if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
            return Err(Transport::Status(status.as_u16()));
        }
        let headers = response.headers().clone();
        let mut answer = HttpResponse::new(response.bytes().await?.to_vec());
        *answer.status_mut() = status;
        *answer.headers_mut() = headers;
        Ok(answer)
    }
}

impl<'c> AsyncHttpClient<'c> for Server {
    type Error = Transport;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, Transport>> + Send + 'c>>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(self.exchange(request))
    }
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
        RequestTokenError::Request(Transport::Url(reason)) | RequestTokenError::Other(reason) => {
            ServerError::Protocol(reason)
        }
        RequestTokenError::Parse(error, _) => ServerError::Protocol(error.to_string()),
    }
}
