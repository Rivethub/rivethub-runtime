use axum::extract::ConnectInfo;
use axum::http::Request;
use sha2::{Digest, Sha256};

use super::{Tenancy, UserId};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn resolve(tenancy: &Tenancy, token: Option<&str>, remote: &str) -> TestResult<String> {
    let mut request = Request::new(());
    if let Some(token) = token {
        request.headers_mut().insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse()?,
        );
    }
    let (mut parts, ()) = request.into_parts();
    parts
        .extensions
        .insert(ConnectInfo(remote.parse::<std::net::SocketAddr>()?));
    Ok(tenancy.resolve(&parts)?.as_str().to_owned())
}

fn digest(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

fn registry() -> TestResult<Tenancy> {
    Ok(Tenancy::local(UserId::new("owner".into())?)
        .with_token_digest(&digest("alice-secret"), UserId::new("alice".into())?)?)
}

#[test]
fn a_users_file_token_names_its_user_from_any_address() -> TestResult {
    let tenancy = registry()?;
    assert_eq!(
        resolve(&tenancy, Some("alice-secret"), "192.0.2.7:1234")?,
        "alice"
    );
    assert_eq!(
        resolve(&tenancy, Some("alice-secret"), "127.0.0.1:1234")?,
        "alice"
    );
    Ok(())
}

#[test]
fn loopback_without_a_token_is_the_owner_and_elsewhere_is_refused() -> TestResult {
    let tenancy = registry()?;
    assert_eq!(resolve(&tenancy, None, "127.0.0.1:1234")?, "owner");
    assert!(resolve(&tenancy, None, "192.0.2.7:1234").is_err());
    Ok(())
}

#[test]
fn an_unknown_token_is_refused_even_on_loopback() -> TestResult {
    let tenancy = registry()?;
    assert!(resolve(&tenancy, Some("not-a-token"), "127.0.0.1:1234").is_err());
    assert!(resolve(&tenancy, Some(""), "127.0.0.1:1234").is_err());
    Ok(())
}

#[test]
fn a_digest_must_be_hex_sha256_and_unique() -> TestResult {
    let owner = UserId::new("owner".into())?;
    let alice = UserId::new("alice".into())?;
    assert!(
        Tenancy::local(owner.clone())
            .with_token_digest("plaintext-token", alice.clone())
            .is_err()
    );
    assert!(
        registry()?
            .with_token_digest(&digest("alice-secret"), owner)
            .is_err()
    );
    assert!(
        registry()?
            .with_token_digest(&digest("other"), alice)
            .is_ok()
    );
    Ok(())
}
