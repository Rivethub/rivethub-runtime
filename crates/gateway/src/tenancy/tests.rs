use sha2::{Digest, Sha256};

use super::{Tenancy, UserId};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn resolve(tenancy: &Tenancy, token: &str) -> TestResult<String> {
    Ok(tenancy.resolve_bearer(token)?.as_str().to_owned())
}

fn digest(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

fn registry() -> TestResult<Tenancy> {
    Ok(Tenancy::local(UserId::new("owner".into())?)
        .with_token_digest(&digest("alice-secret"), UserId::new("alice".into())?)?)
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
