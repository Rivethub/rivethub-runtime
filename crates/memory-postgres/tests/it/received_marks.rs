use chrono::Utc;
use kernel::owner::Owner;
use kernel::tenancy::Scope;
use kernel::tenancy::TenancyStore as _;
use kernel::tenancy::Visibility;
use memory_postgres::CaptureOptions;
use sqlx::PgPool;
use wire::session_id::SessionKey;

use crate::visibility::{batch, household, seed, viewer};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_received_session_is_shared_with_a_space_by_its_import_key(pool: PgPool) -> TestResult {
    let (store, tenancy) = seed(&pool).await?;
    let received: SessionKey = format!("import:{}", "cd".repeat(32)).parse()?;
    let captured = "claude-code:00000000-0000-4000-8000-0000000000d1";
    store
        .capture_batch(
            &batch(captured, "a session alice received from a friend"),
            &CaptureOptions::owned_by(&Owner::new("alice".to_owned())?),
        )
        .await?;
    sqlx::query("UPDATE rh_conversations SET session_key = $1 WHERE session_key = $2")
        .bind(received.as_str())
        .bind(captured)
        .execute(&pool)
        .await?;
    let alice = viewer("alice")?;
    let shared = Visibility::Space(household()?);
    let marked = Scope::Viewer(alice.clone())
        .run(tenancy.mark_session(&alice, &received, Some(&shared), Utc::now()))
        .await?;
    assert_eq!(marked.effective, shared);
    let bob = viewer("bob")?;
    let seen = Scope::Viewer(bob.clone())
        .run(tenancy.session_visibility(&bob, std::slice::from_ref(&received)))
        .await?;
    assert_eq!(
        seen.get(&received).map(|state| state.effective.clone()),
        Some(shared),
        "the space reads the received session"
    );
    Ok(())
}
