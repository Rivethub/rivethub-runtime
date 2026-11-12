use axum::http::{Method, Request};
use sha2::{Digest, Sha256};

use wire::gateway::users::{UserToken, UsersRegistry};

use super::{Tenancy, UserId};
use crate::error::ApiError;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn resolve(tenancy: &Tenancy, token: &str) -> TestResult<String> {
    resolve_at(tenancy, token, Method::GET, "/api/memory/stats")
}

fn digest(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

fn registry() -> TestResult<Tenancy> {
    Ok(
        Tenancy::local(UserId::new("owner".into())?).with_users_file_token(
            &UserToken::Digest(digest("alice-secret")),
            UserId::new("alice".into())?,
        )?,
    )
}

#[test]
fn a_users_file_token_names_its_user() -> TestResult {
    let tenancy = registry()?;
    assert_eq!(resolve(&tenancy, "alice-secret")?, "alice");
    Ok(())
}

#[test]
fn an_unknown_or_empty_token_is_refused() -> TestResult {
    let tenancy = registry()?;
    assert!(resolve(&tenancy, "not-a-token").is_err());
    assert!(resolve(&tenancy, "").is_err());
    Ok(())
}

#[test]
fn a_digest_must_be_hex_sha256_and_unique() -> TestResult {
    let owner = UserId::new("owner".into())?;
    let alice = UserId::new("alice".into())?;
    assert!(
        Tenancy::local(owner.clone())
            .with_users_file_token(
                &UserToken::Digest("plaintext-token".to_owned()),
                alice.clone()
            )
            .is_err()
    );
    assert!(
        registry()?
            .with_users_file_token(&UserToken::Digest(digest("alice-secret")), owner)
            .is_err()
    );
    assert!(
        registry()?
            .with_users_file_token(&UserToken::Digest(digest("other")), alice)
            .is_ok()
    );
    Ok(())
}

fn tenancy_of(users: &str) -> TestResult<Tenancy> {
    let users: UsersRegistry = serde_json::from_str(users)?;
    let mut tenancy = Tenancy::local(UserId::new(users.owner_user_id)?);
    for (id, record) in users.users {
        for token in record.tokens.iter().flatten() {
            tenancy = tenancy.with_users_file_token(token, UserId::new(id.clone())?)?;
        }
    }
    Ok(tenancy)
}

fn resolve_at(tenancy: &Tenancy, token: &str, method: Method, path: &str) -> TestResult<String> {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
        .body(())?;
    let (parts, ()) = request.into_parts();
    Ok(tenancy.resolve_bearer(token, &parts)?.as_str().to_owned())
}

#[test]
fn a_bare_digest_is_full_access_and_a_drops_token_only_shares() -> TestResult {
    let users = format!(
        r#"{{"ownerUserId":"owner","unmappedIsOwner":false,"users":{{
            "alice":{{"tokens":["{alice}"]}},
            "erin":{{"tokens":[{{"digest":"{erin}","scopes":["drops:write"]}}]}}}}}}"#,
        alice = digest("alice-secret"),
        erin = digest("erin-secret"),
    );
    let tenancy = tenancy_of(&users)?;
    let drop = "/api/drops/AAAAAAAAAAAAAAAAAAAAAA";
    for (method, path) in [
        (Method::GET, "/api/memory/stats"),
        (Method::POST, "/api/drops"),
        (Method::DELETE, drop),
        (Method::GET, "/api/me"),
    ] {
        assert_eq!(
            resolve_at(&tenancy, "alice-secret", method.clone(), path)?,
            "alice",
            "{method} {path}"
        );
    }
    for (method, path) in [
        (Method::POST, "/api/drops"),
        (Method::DELETE, drop),
        (Method::GET, "/api/me"),
    ] {
        assert_eq!(
            resolve_at(&tenancy, "erin-secret", method.clone(), path)?,
            "erin",
            "{method} {path}"
        );
    }
    for (method, path) in [
        (Method::GET, "/api/memory/stats"),
        (Method::GET, drop),
        (Method::PATCH, "/api/me"),
        (Method::POST, "/api/drops/extra"),
    ] {
        let refused = resolve_at(&tenancy, "erin-secret", method.clone(), path);
        assert!(
            refused.is_err_and(|error| matches!(
                error.downcast_ref::<ApiError>(),
                Some(ApiError::InsufficientScope)
            )),
            "{method} {path}"
        );
    }
    Ok(())
}

#[test]
fn a_scoped_token_names_known_scopes() -> TestResult {
    let erin = digest("erin-secret");
    let users = |token: &str| {
        format!(
            r#"{{"ownerUserId":"owner","unmappedIsOwner":false,"users":{{"erin":{{"tokens":[{token}]}}}}}}"#
        )
    };
    let full = format!(r#"{{"digest":"{erin}","scopes":["drops:write","full"]}}"#);
    assert_eq!(
        resolve_at(
            &tenancy_of(&users(&full))?,
            "erin-secret",
            Method::GET,
            "/api/memory/stats"
        )?,
        "erin"
    );
    for refused in [
        format!(r#"{{"digest":"{erin}","scopes":[]}}"#),
        format!(r#"{{"digest":"{erin}","scopes":["memory:read"]}}"#),
        format!(r#"{{"digest":"{erin}","scopes":["drops:write"],"expires":1}}"#),
        r#"{"digest":"plaintext","scopes":["drops:write"]}"#.to_owned(),
    ] {
        assert!(tenancy_of(&users(&refused)).is_err(), "{refused}");
    }
    Ok(())
}
