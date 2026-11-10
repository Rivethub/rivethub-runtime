use crate::{Error, Server};
use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::Response,
};
use rmcp::{
    ServiceExt,
    service::QuitReason,
    transport::{
        IntoTransport,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use sha2::{Digest, Sha256};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::net::{TcpListener, UnixListener};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Bearer([u8; 32]);

#[derive(Debug)]
pub struct UnixSocket {
    listener: UnixListener,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT B7 authentication material must be redacted; deriving Debug would expose the token digest"
    )
)]
impl std::fmt::Debug for Bearer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Bearer([redacted])")
    }
}

impl Bearer {
    pub fn new(secret: &str) -> Result<Self, Error> {
        if secret.is_empty() || secret.chars().any(char::is_whitespace) {
            return Err(Error::Refused(
                "bearer token must be nonempty and contain no whitespace",
            ));
        }
        Ok(Self(Sha256::digest(secret.as_bytes()).into()))
    }

    fn accepts(&self, supplied: &[u8]) -> bool {
        let digest: [u8; 32] = Sha256::digest(supplied).into();
        bool::from(self.0.ct_eq(&digest))
    }
}

async fn authorize(
    State(bearer): State<Option<Bearer>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if let Some(bearer) = bearer {
        let mut headers = request.headers().get_all(AUTHORIZATION).iter();
        let header = headers.next().ok_or(StatusCode::UNAUTHORIZED)?;
        if headers.next().is_some() {
            return Err(StatusCode::UNAUTHORIZED);
        }
        let header = header.to_str().map_err(|_| StatusCode::UNAUTHORIZED)?;
        let supplied = header
            .strip_prefix("Bearer ")
            .ok_or(StatusCode::UNAUTHORIZED)?;
        if !bearer.accepts(supplied.as_bytes()) {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }
    Ok(next.run(request).await)
}

fn router(
    server: Server,
    bearer: Option<Bearer>,
    config: StreamableHttpServerConfig,
    shutdown: CancellationToken,
) -> Router {
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        config
            .with_cancellation_token(shutdown)
            .enforce_origin_validation(),
    );
    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(bearer, authorize))
}

pub async fn serve_tcp(
    listener: TcpListener,
    server: Server,
    bearer: Bearer,
    shutdown: CancellationToken,
) -> Result<(), Error> {
    serve_tcp_with_config(
        listener,
        server,
        bearer,
        StreamableHttpServerConfig::default(),
        shutdown,
    )
    .await
}

pub async fn serve_tcp_with_config(
    listener: TcpListener,
    server: Server,
    bearer: Bearer,
    config: StreamableHttpServerConfig,
    shutdown: CancellationToken,
) -> Result<(), Error> {
    let result = axum::serve(
        listener,
        router(server.clone(), Some(bearer), config, shutdown.clone()),
    )
    .with_graceful_shutdown(shutdown.cancelled_owned())
    .await;
    server.shutdown();
    result?;
    Ok(())
}

pub async fn bind_unix(path: &Path) -> Result<UnixSocket, Error> {
    remove_stale_socket(path).await?;
    let path = path.to_owned();
    let listener = tokio::task::spawn_blocking(move || {
        let parent = path
            .parent()
            .ok_or(Error::Refused("socket path has no parent"))?;
        let private = tempfile::Builder::new()
            .prefix(".mcp-")
            .tempdir_in(parent)?;
        let temporary = private.path().join("s");
        let listener = std::os::unix::net::UnixListener::bind(&temporary)?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        std::fs::hard_link(&temporary, &path).map_err(|source| Error::SocketIo {
            path: path.clone(),
            source,
        })?;
        private.close()?;
        Ok::<_, Error>(listener)
    })
    .await??;
    Ok(UnixSocket {
        listener: UnixListener::from_std(listener)?,
    })
}

async fn remove_stale_socket(path: &Path) -> Result<(), Error> {
    let at_path = |source| Error::SocketIo {
        path: path.to_owned(),
        source,
    };
    let original = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(at_path(error)),
    };
    if !original.file_type().is_socket() {
        return Err(Error::SocketNotSocket {
            path: path.to_owned(),
        });
    }
    match tokio::time::timeout(
        std::time::Duration::from_secs(1),
        tokio::net::UnixStream::connect(path),
    )
    .await
    {
        Ok(Ok(_stream)) => {
            return Err(Error::SocketListening {
                path: path.to_owned(),
            });
        }
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            let current = tokio::fs::symlink_metadata(path).await.map_err(at_path)?;
            if current.dev() != original.dev() || current.ino() != original.ino() {
                return Err(Error::SocketChanged {
                    path: path.to_owned(),
                });
            }
            tokio::fs::remove_file(path).await.map_err(at_path)?;
        }
        Ok(Err(error)) => return Err(at_path(error)),
        Err(_) => {
            return Err(Error::SocketProbeTimeout {
                path: path.to_owned(),
            });
        }
    }
    Ok(())
}

pub async fn serve_unix(
    socket: UnixSocket,
    server: Server,
    bearer: Option<Bearer>,
    shutdown: CancellationToken,
) -> Result<(), Error> {
    let result = axum::serve(
        socket.listener,
        router(
            server.clone(),
            bearer,
            StreamableHttpServerConfig::default(),
            shutdown.clone(),
        ),
    )
    .with_graceful_shutdown(shutdown.cancelled_owned())
    .await;
    server.shutdown();
    result?;
    Ok(())
}

pub async fn serve_stdio(server: Server, shutdown: CancellationToken) -> Result<(), Error> {
    let reader = crate::nonblock::reader()?;
    Box::pin(serve_io(server, (reader, tokio::io::stdout()), shutdown)).await
}

pub async fn serve_io<T, E, A>(
    server: Server,
    transport: T,
    shutdown: CancellationToken,
) -> Result<(), Error>
where
    T: IntoTransport<rmcp::RoleServer, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let service = server
        .clone()
        .serve_with_ct(transport, shutdown)
        .await
        .map_err(|error| Error::Initialize(Box::new(error)))?;
    let result = service.waiting().await;
    server.shutdown();
    match result? {
        QuitReason::JoinError(error) => Err(Error::Task(error)),
        QuitReason::Closed | QuitReason::Cancelled => Ok(()),
        other => Err(Error::Invalid(format!(
            "unexpected MCP shutdown: {other:?}"
        ))),
    }
}
