use sqlx::Connection as _;
use sqlx::PgConnection;
use sqlx::PgPool;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SERVICE: &str = "rh_trust_service";
const READER: &str = "rh_trust_reader";
const PERSON: &str = "rh_trust_person";
const UPKEEP: &str = "rh_trust_upkeep";

async fn login(pool: &PgPool, role: &str, setup: &[String]) -> Result<PgConnection, sqlx::Error> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DO $$ BEGIN CREATE ROLE {role} LOGIN NOSUPERUSER NOBYPASSRLS PASSWORD '{role}'; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
    )))
    .execute(pool)
    .await?;
    for statement in setup {
        sqlx::query(sqlx::AssertSqlSafe(statement.clone()))
            .execute(pool)
            .await?;
    }
    let options = pool
        .connect_options()
        .as_ref()
        .clone()
        .username(role)
        .password(role);
    PgConnection::connect_with(&options).await
}

async fn seen(
    conn: &mut PgConnection,
    principal: &str,
    maintenance: &str,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query(
        "SELECT set_config('rh.principal', $1, false), set_config('rh.maintenance', $2, false)",
    )
    .bind(principal)
    .bind(maintenance)
    .execute(&mut *conn)
    .await?;
    sqlx::query_scalar("SELECT owner_user_id FROM rh_conversations ORDER BY owner_user_id")
        .fetch_all(&mut *conn)
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn only_trusted_logins_can_assert_a_person_or_turn_on_maintenance(
    pool: PgPool,
) -> TestResult {
    sqlx::query(
        "INSERT INTO rh_conversations (session_key, agent, owner_user_id)
         VALUES ('trust-alice', 'test', 'alice'), ('trust-bob', 'test', 'bob')",
    )
    .execute(&pool)
    .await?;
    let mut service = login(&pool, SERVICE, &[format!("GRANT rh_gateway TO {SERVICE}")]).await?;
    let mut reader = login(
        &pool,
        READER,
        &[
            format!("GRANT USAGE ON SCHEMA public TO {READER}"),
            format!("GRANT SELECT, UPDATE ON rh_conversations TO {READER}"),
        ],
    )
    .await?;
    let mut person = login(
        &pool,
        PERSON,
        &[
            format!("GRANT USAGE ON SCHEMA public TO {PERSON}"),
            format!("GRANT SELECT ON rh_conversations TO {PERSON}"),
            format!(
                "INSERT INTO rh_role_principals (role_name, user_id) VALUES ('{PERSON}', 'alice')"
            ),
        ],
    )
    .await?;
    let mut upkeep = login(
        &pool,
        UPKEEP,
        &[format!("GRANT rh_maintenance TO {UPKEEP}")],
    )
    .await?;
    let alice = vec!["alice".to_owned()];
    let both = vec!["alice".to_owned(), "bob".to_owned()];
    let none: Vec<String> = Vec::new();

    assert_eq!(
        seen(&mut reader, "", "on").await?,
        none,
        "an unpinned reader turned maintenance on"
    );
    assert_eq!(
        seen(&mut reader, "alice", "off").await?,
        none,
        "an unpinned reader asserted a person"
    );
    let updated = sqlx::query("UPDATE rh_conversations SET title = 'x'")
        .execute(&mut reader)
        .await?
        .rows_affected();
    assert_eq!(updated, 0, "an unpinned reader wrote rows");

    assert_eq!(
        seen(&mut service, "", "on").await?,
        none,
        "the serving login turned maintenance on"
    );
    assert_eq!(seen(&mut service, "alice", "off").await?, alice);

    assert_eq!(seen(&mut upkeep, "", "on").await?, both);
    assert_eq!(
        seen(&mut upkeep, "bob", "off").await?,
        none,
        "a maintenance login asserted a person"
    );

    for (principal, maintenance) in [("", "off"), ("bob", "off"), ("", "on"), ("bob", "on")] {
        assert_eq!(
            seen(&mut person, principal, maintenance).await?,
            alice,
            "the pinned login under principal {principal:?} maintenance {maintenance}"
        );
    }
    Ok(())
}
