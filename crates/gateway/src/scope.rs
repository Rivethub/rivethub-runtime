use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use axum::body::Body;
use axum::extract::Request;
use axum::extract::State;
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use bytes::Bytes;
use http_body::Frame;
use http_body::SizeHint;
use kernel::deadline::Deadline;
use kernel::tenancy::Scope;

use crate::Gateway;

struct ScopedBody {
    scope: Scope,
    inner: Body,
}

impl http_body::Body for ScopedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let inner = &mut this.inner;
        this.scope.clone().enter(|| Pin::new(inner).poll_frame(cx))
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

pub(crate) async fn scope_request(
    State(gateway): State<Arc<Gateway>>,
    request: Request,
    next: Next,
) -> Response {
    let (parts, body) = request.into_parts();
    let viewer = crate::tenancy::caller(&gateway, &parts).map(|user| gateway.tenancy.viewer(&user));
    let request = Request::from_parts(parts, body);
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a stated rule, as note 89 of the contract runs a request whose caller is not identified in the closed scope, and its route refuses it"
        )
    )]
    let scope = match viewer {
        Ok(viewer) => Scope::Viewer(viewer),
        Err(_) => Scope::Closed,
    };
    let deadline = if matches!(*request.method(), Method::GET | Method::HEAD) {
        Deadline::after(gateway.settings().request_deadline)
    } else {
        Deadline::unbounded()
    };
    let response = scope.clone().run(deadline.run(next.run(request))).await;
    response.map(|inner| Body::new(ScopedBody { scope, inner }))
}
