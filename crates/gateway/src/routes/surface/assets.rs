use std::io;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use axum::body::Body;
use axum::http::HeaderValue;
use axum::http::Method;
use axum::http::Request;
use axum::http::StatusCode;
use axum::http::header;
use axum::response::Response;
use tower::ServiceExt as _;
use tower_http::services::ServeDir;

use crate::error::ApiError;

const CACHE_IMMUTABLE: &str = "public, max-age=31536000, immutable";
const CACHE_VOLATILE: &str = "no-cache";
const OCTET_STREAM: HeaderValue = HeaderValue::from_static("application/octet-stream");

enum Decode {
    Relative(PathBuf),
    Refused,
}

enum Target {
    File,
    Directory,
    Missing,
    Refused,
}

enum Decision {
    File,
    Index,
    Spa,
    Reject,
}

pub(super) async fn serve(root: &Path, method: &Method, path: &str) -> Result<Response, ApiError> {
    let owned = path.to_owned();
    let root_for_class = root.to_owned();
    let joined = tokio::task::spawn_blocking(move || decide(&root_for_class, &owned)).await;
    match super::super::join_task(joined)?? {
        Decision::Reject => Err(ApiError::NotFound("not found")),
        Decision::File => read_served(root, method, path, path).await,
        Decision::Index => read_served(root, method, &index_path(path), path).await,
        Decision::Spa => read_served(root, method, "/index.html", path).await,
    }
}

pub(super) fn request_refused(path: &str) -> bool {
    matches!(decode(path), Decode::Refused)
}

fn decide(root: &Path, path: &str) -> Result<Decision, ApiError> {
    if request_refused(path) {
        return Ok(Decision::Reject);
    }
    match locate(root, path)? {
        Target::Refused => Ok(Decision::Reject),
        Target::File => Ok(Decision::File),
        Target::Directory => match locate(root, &index_path(path))? {
            Target::File => Ok(Decision::Index),
            Target::Refused => Ok(Decision::Reject),
            Target::Directory | Target::Missing => extensionless_spa(root, path),
        },
        Target::Missing => extensionless_spa(root, path),
    }
}

fn extensionless_spa(root: &Path, path: &str) -> Result<Decision, ApiError> {
    if has_extension(path) {
        return Ok(Decision::Reject);
    }
    match locate(root, "/index.html")? {
        Target::File => Ok(Decision::Spa),
        Target::Directory | Target::Missing | Target::Refused => Ok(Decision::Reject),
    }
}

fn locate(root: &Path, request_path: &str) -> Result<Target, ApiError> {
    let root = match root.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Target::Missing),
        Err(error) => return Err(io_error(error)),
    };
    let Decode::Relative(relative) = decode(request_path) else {
        return Ok(Target::Refused);
    };
    let candidate = root.join(relative);
    match candidate.canonicalize() {
        Ok(path) if path.starts_with(&root) && path.is_file() => Ok(Target::File),
        Ok(path) if path.starts_with(&root) && path.is_dir() => Ok(Target::Directory),
        Ok(path) if path.starts_with(&root) => Ok(Target::Missing),
        Ok(_) => Ok(Target::Refused),
        Err(error) if error.kind() == io::ErrorKind::NotFound => ancestor(&root, &candidate),
        Err(error) => Err(io_error(error)),
    }
}

fn ancestor(root: &Path, candidate: &Path) -> Result<Target, ApiError> {
    let mut current = candidate.to_path_buf();
    loop {
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return match std::fs::canonicalize(&current) {
                    Ok(path) if path.starts_with(root) => Ok(Target::Missing),
                    Ok(_) => Ok(Target::Refused),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Target::Refused),
                    Err(error) => Err(io_error(error)),
                };
            }
            Ok(_) => {
                return match std::fs::canonicalize(&current) {
                    Ok(path) if path.starts_with(root) => Ok(Target::Missing),
                    Ok(_) => Ok(Target::Refused),
                    Err(error) => Err(io_error(error)),
                };
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !current.pop() {
                    return Ok(Target::Refused);
                }
            }
            Err(error) => return Err(io_error(error)),
        }
    }
}

fn decode(path: &str) -> Decode {
    let text = match percent_encoding::percent_decode_str(path).decode_utf8() {
        Ok(text) => text,
        Err(error) => {
            tracing::debug!("static request path is not utf-8: {error}");
            return Decode::Refused;
        }
    };
    let mut relative = PathBuf::new();
    for component in Path::new(text.as_ref()).components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(part) => relative.push(part),
            Component::ParentDir | Component::Prefix(_) => return Decode::Refused,
        }
    }
    Decode::Relative(relative)
}

fn index_path(path: &str) -> String {
    if path.ends_with('/') {
        format!("{path}index.html")
    } else {
        format!("{path}/index.html")
    }
}

fn has_extension(path: &str) -> bool {
    let Decode::Relative(relative) = decode(path) else {
        return true;
    };
    let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    match name.rfind('.') {
        Some(0) | None => false,
        Some(_) => true,
    }
}

fn io_error(error: io::Error) -> ApiError {
    tracing::error!("static file: {error}");
    ApiError::Io(error)
}

async fn read_served(
    root: &Path,
    method: &Method,
    serve_path: &str,
    cache_path: &str,
) -> Result<Response, ApiError> {
    let request = Request::builder()
        .method(method.clone())
        .uri(serve_path)
        .body(Body::empty())
        .map_err(|error| {
            tracing::error!("static request: {error}");
            ApiError::Internal
        })?;
    let response = match ServeDir::new(root).oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    if response.status() != StatusCode::OK {
        return Err(ApiError::NotFound("not found"));
    }
    let content_type = match response.headers().get(header::CONTENT_TYPE) {
        Some(value) => value.clone(),
        None => OCTET_STREAM,
    };
    let bytes = axum::body::to_bytes(Body::new(response.into_body()), usize::MAX)
        .await
        .map_err(|error| {
            tracing::error!("static body: {error}");
            ApiError::Internal
        })?;
    file_response(method, cache_path, content_type, bytes)
}

fn file_response(
    method: &Method,
    path: &str,
    content_type: HeaderValue,
    bytes: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let body = if *method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, cache_control(path))
        .body(body)
        .map_err(|error| {
            tracing::error!("static response: {error}");
            ApiError::Internal
        })
}

fn cache_control(path: &str) -> &'static str {
    if immutable(path) {
        CACHE_IMMUTABLE
    } else {
        CACHE_VOLATILE
    }
}

fn immutable(path: &str) -> bool {
    let Some((_, tail)) = path.rsplit_once('-') else {
        return false;
    };
    if tail.contains('/') {
        return false;
    }
    let Some((hash, ext)) = tail.split_once('.') else {
        return false;
    };
    hash.len() == 8 && word(hash) && word(ext) && !ext.contains('.')
}

fn word(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

#[cfg(test)]
mod tests {
    use super::request_refused;

    #[test]
    fn parent_segments_are_refused_after_percent_decoding() {
        assert!(request_refused("/../../etc/passwd"));
        assert!(request_refused("/foo/../bar"));
        assert!(request_refused("/..%2f..%2fetc%2fpasswd"));
        assert!(request_refused("/%2e%2e/%2E%2E/etc/passwd"));
        assert!(request_refused("/%2e%2e%2fetc"));
        assert!(!request_refused("/assets/app.js"));
        assert!(!request_refused("/foo/%2e/bar"));
        assert!(!request_refused("/sub/"));
    }
}
