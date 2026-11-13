use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use oauth2::AsyncHttpClient;
use oauth2::AuthType;
use oauth2::ClientId;
use oauth2::DeviceAuthorizationUrl;
use oauth2::EndpointNotSet;
use oauth2::EndpointSet;
use oauth2::ErrorResponseType;
use oauth2::HttpRequest;
use oauth2::HttpResponse;
use oauth2::RequestTokenError;
use oauth2::StandardErrorResponse;
use oauth2::TokenUrl;
use oauth2::basic::BasicClient;
use reqwest::StatusCode;
use reqwest::header::CONTENT_TYPE;
use reqwest::header::HeaderMap;
use reqwest::header::RETRY_AFTER;
use serde::Deserialize;
use url::Url;

use super::CLIENT_ID;
use super::OauthErrorCode;
use super::Server;
use super::ServerError;

const METADATA_PATH: &str = ".well-known/oauth-authorization-server";

pub(crate) type OauthClient =
    BasicClient<EndpointNotSet, EndpointSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

#[derive(Debug, Clone)]
pub(crate) struct Endpoints {
    device: DeviceAuthorizationUrl,
    token: TokenUrl,
    revocation: Url,
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

#[derive(Deserialize)]
struct Refusal {
    error: OauthErrorCode,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Transport {
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("the share server is busy or down")]
    Unavailable {
        status: u16,
        retry_after: Option<Duration>,
    },
    #[error("the OAuth request names a URL outside the share server's origin")]
    Origin,
}

impl From<Transport> for ServerError {
    fn from(error: Transport) -> Self {
        match error {
            Transport::Http(error) => Self::Http(error.without_url()),
            Transport::Unavailable {
                status,
                retry_after,
            } => Self::Unavailable {
                status,
                retry_after,
            },
            Transport::Origin => Self::Protocol("an OAuth request left the share server's origin"),
        }
    }
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let digits = headers.get(RETRY_AFTER)?.as_bytes();
    if digits.is_empty() {
        return None;
    }
    digits
        .iter()
        .try_fold(0_u64, |seconds, byte| {
            let digit = char::from(*byte).to_digit(10)?;
            seconds.checked_mul(10)?.checked_add(u64::from(digit))
        })
        .map(Duration::from_secs)
}

pub(crate) fn code(wire: &str) -> Result<OauthErrorCode, ServerError> {
    OauthErrorCode::deserialize(
        serde::de::value::StrDeserializer::<serde::de::value::Error>::new(wire),
    )
    .map_err(|_| ServerError::Protocol("its OAuth error code was unreadable"))
}

pub(crate) fn refused(wire: &str) -> ServerError {
    match code(wire) {
        Ok(code) => ServerError::Refused(code),
        Err(error) => error,
    }
}

impl Server {
    pub(crate) async fn endpoints(&self) -> Result<Endpoints, ServerError> {
        let response = self
            .client
            .get(self.endpoint(METADATA_PATH)?)
            .send()
            .await?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
            return Err(ServerError::NoDeviceFlow(self.host.clone()));
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(ServerError::Unavailable {
                status: status.as_u16(),
                retry_after: retry_after(response.headers()),
            });
        }
        if !status.is_success() {
            return Err(ServerError::Answered(status.as_u16()));
        }
        let body = response.bytes().await?;
        let metadata: Metadata = serde_json::from_slice(&body)
            .map_err(|_| ServerError::Protocol("its OAuth metadata is unreadable"))?;
        let own_origin = [&metadata.token, &metadata.device, &metadata.revocation]
            .iter()
            .all(|url| url.origin() == self.base.origin());
        if !own_origin {
            return Err(ServerError::Protocol(
                "its OAuth metadata sends sign-in to another origin",
            ));
        }
        Ok(Endpoints {
            device: DeviceAuthorizationUrl::from_url(metadata.device),
            token: TokenUrl::from_url(metadata.token),
            revocation: metadata.revocation,
        })
    }

    pub(crate) fn oauth_client(endpoints: &Endpoints) -> OauthClient {
        BasicClient::new(ClientId::new(CLIENT_ID.to_owned()))
            .set_auth_type(AuthType::RequestBody)
            .set_device_authorization_url(endpoints.device.clone())
            .set_token_uri(endpoints.token.clone())
    }

    pub(crate) async fn revoke(
        &self,
        endpoints: &Endpoints,
        refresh_token: &str,
    ) -> Result<(), ServerError> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("token", refresh_token)
            .append_pair("token_type_hint", "refresh_token")
            .append_pair("client_id", CLIENT_ID)
            .finish();
        let request = oauth2::http::Request::post(endpoints.revocation.as_str())
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body.into_bytes())
            .map_err(|_| ServerError::Protocol("the revocation request could not be built"))?;
        let response = self.exchange(request).await?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        if status == StatusCode::BAD_REQUEST
            && let Ok(refusal) = serde_json::from_slice::<Refusal>(response.body())
        {
            return Err(ServerError::Refused(refusal.error));
        }
        Err(ServerError::Answered(status.as_u16()))
    }

    fn same_origin(&self, uri: &str) -> Result<Url, Transport> {
        let url = Url::parse(uri).map_err(|_| Transport::Origin)?;
        if url.origin() == self.base.origin() {
            Ok(url)
        } else {
            Err(Transport::Origin)
        }
    }

    async fn exchange(&self, request: HttpRequest) -> Result<HttpResponse, Transport> {
        let (parts, body) = request.into_parts();
        let url = self.same_origin(&parts.uri.to_string())?;
        let response = self
            .client
            .request(parts.method, url)
            .headers(parts.headers)
            .body(body)
            .send()
            .await?;
        let status = response.status();
        if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
            return Err(Transport::Unavailable {
                status: status.as_u16(),
                retry_after: retry_after(response.headers()),
            });
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

pub(crate) fn failed<T: ErrorResponseType + AsRef<str> + std::fmt::Display + 'static>(
    error: RequestTokenError<Transport, StandardErrorResponse<T>>,
) -> ServerError {
    match error {
        RequestTokenError::ServerResponse(refusal) => refused(refusal.error().as_ref()),
        RequestTokenError::Request(error) => error.into(),
        RequestTokenError::Parse(_, _) => {
            ServerError::Protocol("its OAuth response is not in the expected shape")
        }
        RequestTokenError::Other(_) => ServerError::Protocol("its OAuth response was unexpected"),
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use axum::Router;
    use axum::body::Bytes;
    use axum::http::Method;
    use axum::http::StatusCode;
    use axum::http::Uri;
    use axum::response::IntoResponse as _;
    use oauth2::StandardErrorResponse;
    use oauth2::basic::BasicErrorResponseType;
    use parking_lot::Mutex;

    use super::*;
    use crate::share_server::Login;
    use crate::share_server::Tokens;
    use crate::share_server::logins::LoginStore;

    const CANARY: &str = "canary-9d41e7";

    async fn wire_recorder() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        let app = Router::new().fallback(move |method: Method, uri: Uri, _body: Bytes| {
            let recorder = Arc::clone(&recorder);
            async move {
                recorder.lock().push(format!("{method} {uri}"));
                if uri.path().ends_with("oauth-authorization-server") {
                    let origin = format!(
                        "{}://{}",
                        uri.scheme_str().unwrap_or("http"),
                        uri.authority()
                            .map(axum::http::uri::Authority::as_str)
                            .unwrap_or_default()
                    );
                    let body = format!(
                        r#"{{"device_authorization_endpoint":"{origin}/api/auth/device/code","token_endpoint":"{origin}/api/auth/oauth2/token","revocation_endpoint":"{origin}/api/auth/oauth2/revoke"}}"#
                    );
                    return (
                        StatusCode::OK,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        body,
                    )
                        .into_response();
                }
                StatusCode::OK.into_response()
            }
        });
        tokio::spawn(async move { axum::serve(listener, app).await });
        (address, seen)
    }

    fn server_via(host: &str, proxy: SocketAddr, dir: &std::path::Path) -> Server {
        let host: capture::seal::ShareServer = host.parse().expect("host");
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://{proxy}")).expect("proxy"))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");
        Server {
            base: Url::parse(&host.base_url()).expect("base"),
            host,
            client,
            logins: LoginStore::with_vault(dir.join(crate::share_server::STORE_FILE), None),
            lock: dir.join(crate::share_server::LOCK_FILE),
        }
    }

    async fn signed_in(server: &Server) {
        let tokens = Tokens {
            access_token: "access".to_owned(),
            refresh_token: "refresh".to_owned(),
            expires_at: chrono::DateTime::<chrono::Utc>::MAX_UTC,
            scope: "drops:write".to_owned(),
        };
        server
            .logins
            .save(&server.host, &Login::Device(tokens))
            .await
            .expect("logins");
    }

    #[tokio::test]
    async fn logout_revokes_at_the_exact_origin_it_discovered() {
        for host in ["127.0.0.1:443", "127.0.0.1:80", "127.0.0.1:8080"] {
            let dir = tempfile::tempdir().expect("dir");
            let (proxy, seen) = wire_recorder().await;
            let server = server_via(host, proxy, dir.path());
            signed_in(&server).await;
            let report = crate::memory::desk::sign_out(&server).await;
            assert!(report.is_ok(), "{host}: {report:?}");
            let authority = Url::parse(&format!("http://{host}/"))
                .expect("url")
                .origin()
                .ascii_serialization();
            let sent = seen.lock().clone();
            assert!(
                sent.contains(&format!("POST {authority}/api/auth/oauth2/revoke")),
                "{host}: {sent:?}"
            );
            assert!(
                sent.iter()
                    .all(|line| line.contains(&format!("{authority}/"))),
                "{host}: every request keeps its origin: {sent:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_request_for_any_other_origin_is_refused_before_it_is_sent() {
        let dir = tempfile::tempdir().expect("dir");
        let (proxy, seen) = wire_recorder().await;
        for (host, target) in [
            (
                "team.example.com",
                "http://team.example.com/api/auth/oauth2/revoke",
            ),
            (
                "team.example.com",
                "https://other.example.com/api/auth/oauth2/revoke",
            ),
            (
                "127.0.0.1:443",
                "http://127.0.0.1:80/api/auth/oauth2/revoke",
            ),
            (
                "127.0.0.1:443",
                "https://127.0.0.1:443/api/auth/oauth2/revoke",
            ),
        ] {
            let server = server_via(host, proxy, dir.path());
            let request = oauth2::http::Request::post(target)
                .body(Vec::new())
                .expect("request");
            let outcome = server.exchange(request).await;
            assert!(outcome.is_err(), "{host} -> {target}");
        }
        assert!(seen.lock().is_empty(), "{:?}", seen.lock());
    }

    #[test]
    fn a_refusal_names_only_allowlisted_codes() {
        let hostile = StandardErrorResponse::new(
            BasicErrorResponseType::Extension(format!("{CANARY}-code")),
            Some(format!("{CANARY}-description")),
            Some(format!("https://evil.example/{CANARY}-uri")),
        );
        let known = StandardErrorResponse::new(BasicErrorResponseType::InvalidGrant, None, None);
        let shown: Vec<String> = [
            failed::<BasicErrorResponseType>(RequestTokenError::ServerResponse(hostile)),
            failed::<BasicErrorResponseType>(RequestTokenError::Other(format!("{CANARY}-other"))),
            failed::<BasicErrorResponseType>(RequestTokenError::ServerResponse(known)),
        ]
        .iter()
        .flat_map(|error| [error.to_string(), format!("{error:?}")])
        .collect();
        for text in &shown {
            assert!(!text.contains(CANARY), "{text}");
        }
        assert!(
            shown.iter().any(|text| text.contains("invalid_grant")),
            "{shown:?}"
        );
    }
}
