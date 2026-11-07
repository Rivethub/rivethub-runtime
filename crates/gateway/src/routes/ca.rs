use std::num::NonZeroU32;
use std::sync::Arc;

use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::Request;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::http::header;
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing;
use governor::DefaultDirectRateLimiter;
use governor::Quota;
use governor::RateLimiter;
use kernel::ca::CaError;
use kernel::ca::CertificateAuthority;
use kernel::ca::CrlPem;
use kernel::ca::CsrPem;
use kernel::ca::EnrollmentToken;
use kernel::ca::SignAuthority;
use wire::ca::SignRequest;
use wire::ca::SignResponse;
use wire::gateway::GatewayError;

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Body;
use crate::tenancy::PeerCertificate;

const SIGN_PER_MINUTE: NonZeroU32 = NonZeroU32::new(60).expect("non-zero sign quota");
const PEM_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

#[derive(Debug, thiserror::Error)]
enum CaRouteError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("certificate authority refused the request")]
    Forbidden,
    #[error("certificate signing request was rejected")]
    BadRequest,
    #[error("body too large")]
    BodyTooLarge,
    #[error("certificate authority is not usable")]
    Unavailable,
    #[error("method not allowed")]
    MethodNotAllowed,
}

impl From<ApiError> for CaRouteError {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::BadRequest(_)
            | ApiError::Code(_, _)
            | ApiError::Unprocessable(_)
            | ApiError::BodyMustBeObject
            | ApiError::Serialize(_) => Self::BadRequest,
            ApiError::BodyTooLarge => Self::BodyTooLarge,
            ApiError::Unauthorized
            | ApiError::ForbiddenOrigin
            | ApiError::SessionForbidden
            | ApiError::OwnerRequired => Self::Unauthorized,
            ApiError::MethodNotAllowed => Self::MethodNotAllowed,
            ApiError::Unavailable(_)
            | ApiError::RequestTimeout
            | ApiError::Io(_)
            | ApiError::Task(_)
            | ApiError::Internal
            | ApiError::Transcript(_)
            | ApiError::Terminal(_)
            | ApiError::Harness(_)
            | ApiError::Store(_) => Self::Unavailable,
            ApiError::UnknownCommand(_)
            | ApiError::Forbidden(_)
            | ApiError::Conflict(_)
            | ApiError::NotFound(_)
            | ApiError::Missing(_) => Self::Forbidden,
        }
    }
}

impl IntoResponse for CaRouteError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::BadRequest => StatusCode::BAD_REQUEST,
            Self::BodyTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
        };
        json_error(status, &self.to_string())
    }
}

#[derive(Clone, Debug)]
struct SignBudget {
    limiter: Arc<DefaultDirectRateLimiter>,
}

impl SignBudget {
    fn new() -> Self {
        Self {
            limiter: Arc::new(RateLimiter::direct(Quota::per_minute(SIGN_PER_MINUTE))),
        }
    }
}

pub fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/ca/v1/chain",
            routing::get(chain).fallback(method_not_allowed),
        )
        .route("/ca/v1/crl", routing::get(crl).fallback(method_not_allowed))
        .route(
            "/ca/v1/sign",
            routing::post(sign)
                .fallback(method_not_allowed)
                .layer(axum::extract::DefaultBodyLimit::max(super::BODY_LIMIT))
                .layer(axum::middleware::from_fn_with_state(
                    SignBudget::new(),
                    limit_sign,
                )),
        )
}

async fn limit_sign(State(budget): State<SignBudget>, request: Request, next: Next) -> Response {
    if budget.limiter.check().is_err() {
        return json_error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many certificate requests",
        );
    }
    next.run(request).await
}

async fn method_not_allowed() -> CaRouteError {
    CaRouteError::MethodNotAllowed
}

async fn chain(State(gateway): State<Arc<Gateway>>) -> Result<Response, CaRouteError> {
    let authority = authority_of(&gateway)?;
    let parts = authority.chain().await.map_err(map_ca)?;
    Ok(pem_response(certificate_chain(&parts)?))
}

async fn crl(State(gateway): State<Arc<Gateway>>) -> Result<Response, CaRouteError> {
    let authority = authority_of(&gateway)?;
    let CrlPem(pem) = authority.crl().await.map_err(map_ca)?;
    if !is_pem(&pem, "X509 CRL") {
        tracing::warn!("certificate authority returned a CRL that is not PEM");
        return Err(CaRouteError::Unavailable);
    }
    Ok(pem_response(pem))
}

async fn sign(
    State(gateway): State<Arc<Gateway>>,
    peer: Option<Extension<PeerCertificate>>,
    headers: HeaderMap,
    Body(SignRequest { csr }): Body<SignRequest>,
) -> Result<Json<SignResponse>, CaRouteError> {
    let principal = match peer.as_ref() {
        Some(Extension(certificate)) => certificate.verified_principal().map_err(map_ca)?,
        None => None,
    };
    let authority = authority_of(&gateway)?;
    let sign_authority = match principal {
        Some(principal) => SignAuthority::Renewal(principal),
        None => SignAuthority::Token(EnrollmentToken(bearer(headers.get(header::AUTHORIZATION))?)),
    };
    let signed = authority
        .sign(CsrPem(csr), sign_authority)
        .await
        .map_err(map_ca)?;
    Ok(Json(SignResponse {
        leaf: signed.leaf,
        chain: signed.chain,
    }))
}

fn authority_of(gateway: &Gateway) -> Result<Arc<dyn CertificateAuthority>, CaRouteError> {
    gateway
        .certificate_authority()
        .map(Arc::clone)
        .ok_or(CaRouteError::Unavailable)
}

fn bearer(header: Option<&HeaderValue>) -> Result<String, CaRouteError> {
    let Some(value) = header else {
        return Err(CaRouteError::Unauthorized);
    };
    let text = value.to_str().map_err(|_| CaRouteError::Unauthorized)?;
    let Some(token) = text.strip_prefix("Bearer ") else {
        return Err(CaRouteError::Unauthorized);
    };
    if token.is_empty() || token.as_bytes().iter().any(u8::is_ascii_whitespace) {
        return Err(CaRouteError::Unauthorized);
    }
    Ok(token.to_owned())
}

fn map_ca(error: CaError) -> CaRouteError {
    match error {
        CaError::Refused(reason) => {
            tracing::warn!(%reason, "certificate authority refused a request");
            CaRouteError::Forbidden
        }
        CaError::InvalidCsr(reason) => {
            tracing::warn!(%reason, "certificate authority rejected a csr");
            CaRouteError::BadRequest
        }
        CaError::Unavailable(source) => {
            tracing::warn!(%source, "certificate authority is unavailable");
            CaRouteError::Unavailable
        }
    }
}

fn certificate_chain(parts: &[String]) -> Result<String, CaRouteError> {
    if parts.is_empty() {
        tracing::warn!("certificate authority published an empty chain");
        return Err(CaRouteError::Unavailable);
    }
    let mut body = String::new();
    for part in parts {
        if !is_pem(part, "CERTIFICATE") {
            tracing::warn!("certificate authority published a chain that is not PEM");
            return Err(CaRouteError::Unavailable);
        }
        body.push_str(part);
    }
    Ok(body)
}

fn is_pem(pem: &str, label: &str) -> bool {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let Some(rest) = pem.strip_prefix(&begin) else {
        return false;
    };
    let rest = match rest.strip_prefix('\r') {
        Some(rest) => rest,
        None => rest,
    };
    let Some(rest) = rest.strip_prefix('\n') else {
        return false;
    };
    let Some((encoded, trailer)) = rest.rsplit_once(&end) else {
        return false;
    };
    !encoded.trim().is_empty() && trailer.chars().all(char::is_whitespace)
}

fn pem_response(body: String) -> Response {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static(PEM_CONTENT_TYPE),
        )],
        body,
    )
        .into_response()
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(GatewayError::new(message))).into_response()
}
