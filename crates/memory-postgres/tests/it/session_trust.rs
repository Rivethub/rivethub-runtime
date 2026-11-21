use sqlx::ConnectOptions as _;
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

const GATEWAY_ONLY: &str = "rh_trust_gateway_only";
const PINNED_SERVICE: &str = "rh_trust_pinned_service";

fn denied<T>(result: &Result<T, sqlx::Error>) -> bool {
    matches!(
        result,
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("42501")
    )
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_gateway_only_login_writes_through_the_triggers_and_cannot_escalate(
    pool: PgPool,
) -> TestResult {
    let mut gateway = login(
        &pool,
        GATEWAY_ONLY,
        &[format!("GRANT rh_gateway TO {GATEWAY_ONLY}")],
    )
    .await?;
    sqlx::query("SELECT set_config('rh.principal', 'alice', false)")
        .execute(&mut gateway)
        .await?;
    let conversation: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO rh_conversations (session_key, agent, owner_user_id)
         VALUES ('gateway-alice', 'test', 'alice') RETURNING id",
    )
    .fetch_one(&mut gateway)
    .await?;
    sqlx::query(
        "INSERT INTO rh_messages (conversation_id, agent, channel, role, content, owner_user_id)
         VALUES ($1, 'test', 'test', 'user', 'hello', 'alice')",
    )
    .bind(conversation)
    .execute(&mut gateway)
    .await?;
    let counted: Option<i64> = sqlx::query_scalar(
        "SELECT sum(total)::bigint FROM rh_message_counts WHERE conversation_id = $1",
    )
    .bind(conversation)
    .fetch_one(&mut gateway)
    .await?;
    assert_eq!(
        counted,
        Some(1),
        "the count trigger did not run for the gateway"
    );

    for statement in [
        "SELECT rh_refresh_owner('alice')",
        "SELECT rh_backfill_message_counts(10)",
    ] {
        assert!(
            denied(&sqlx::query(statement).execute(&mut gateway).await),
            "the gateway ran {statement}"
        );
    }
    for statement in [
        "INSERT INTO rh_role_principals (role_name, user_id) VALUES ('rh_trust_gateway_only', 'bob')",
        "UPDATE rh_role_principals SET user_id = 'bob'",
        "DELETE FROM rh_role_principals",
    ] {
        assert!(
            denied(&sqlx::query(statement).execute(&mut gateway).await),
            "the gateway wrote rh_role_principals: {statement}"
        );
    }

    let owner: String = sqlx::query_scalar(
        "SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE oid = current_schema()::regnamespace",
    )
    .fetch_one(&pool)
    .await?;
    for role in ["rh_maintenance".to_owned(), owner] {
        assert!(
            sqlx::query(sqlx::AssertSqlSafe(format!("SET ROLE \"{role}\"")))
                .execute(&mut gateway)
                .await
                .is_err(),
            "the gateway became {role}"
        );
    }
    sqlx::query("SET ROLE rh_gateway")
        .execute(&mut gateway)
        .await?;
    assert_eq!(
        seen(&mut gateway, "", "on").await?,
        Vec::<String>::new(),
        "SET ROLE rh_gateway turned maintenance on"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_pinned_login_in_both_groups_stays_pinned(pool: PgPool) -> TestResult {
    sqlx::query(
        "INSERT INTO rh_conversations (session_key, agent, owner_user_id)
         VALUES ('pinned-alice', 'test', 'alice'), ('pinned-bob', 'test', 'bob')",
    )
    .execute(&pool)
    .await?;
    let mut pinned = login(
        &pool,
        PINNED_SERVICE,
        &[
            format!("GRANT rh_gateway, rh_maintenance TO {PINNED_SERVICE}"),
            format!(
                "INSERT INTO rh_role_principals (role_name, user_id) VALUES ('{PINNED_SERVICE}', 'alice')"
            ),
        ],
    )
    .await?;
    for (principal, maintenance) in [("", "on"), ("bob", "off"), ("bob", "on")] {
        assert_eq!(
            seen(&mut pinned, principal, maintenance).await?,
            vec!["alice".to_owned()],
            "the pinned login under principal {principal:?} maintenance {maintenance}"
        );
    }
    Ok(())
}

const OWNER: &str = "rh_trust_owner";

/// Hands the schema and everything in it to a login that is not a superuser,
/// the way a hub that served as its owner looks before it splits.
async fn hand_to_owner(pool: &PgPool) -> Result<PgConnection, sqlx::Error> {
    let owner = login(pool, OWNER, &[]).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "ALTER SCHEMA public OWNER TO {OWNER};
         DO $$ DECLARE item record; BEGIN
            FOR item IN SELECT c.oid::regclass AS name FROM pg_class c
                          JOIN pg_namespace n ON n.oid = c.relnamespace
                         WHERE n.nspname = 'public' AND c.relkind IN ('r', 'v', 'm', 'S', 'p')
                           AND NOT EXISTS (SELECT 1 FROM pg_depend d
                                            WHERE d.objid = c.oid AND d.deptype IN ('e', 'a', 'i')) LOOP
                EXECUTE format('ALTER TABLE %s OWNER TO {OWNER}', item.name);
            END LOOP;
            FOR item IN SELECT p.oid::regprocedure AS name FROM pg_proc p
                          JOIN pg_namespace n ON n.oid = p.pronamespace
                         WHERE n.nspname = 'public'
                           AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = p.oid AND d.deptype = 'e') LOOP
                EXECUTE format('ALTER ROUTINE %s OWNER TO {OWNER}', item.name);
            END LOOP;
         END $$"
    )))
    .execute(pool)
    .await?;
    Ok(owner)
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_owner_session_still_serves_on_the_new_schema(pool: PgPool) -> TestResult {
    sqlx::query(
        "INSERT INTO rh_conversations (session_key, agent, owner_user_id)
         VALUES ('owner-alice', 'test', 'alice'), ('owner-bob', 'test', 'bob')",
    )
    .execute(&pool)
    .await?;
    let mut owner = hand_to_owner(&pool).await?;
    assert_eq!(
        seen(&mut owner, "alice", "off").await?,
        vec!["alice".to_owned()]
    );
    assert_eq!(
        seen(&mut owner, "", "on").await?,
        vec!["alice".to_owned(), "bob".to_owned()]
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_serving_preflight_refuses_an_owner_login(pool: PgPool) -> TestResult {
    hand_to_owner(&pool).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("GRANT rh_gateway TO {OWNER}")))
        .execute(&pool)
        .await?;
    let url = pool
        .connect_options()
        .as_ref()
        .clone()
        .username(OWNER)
        .password(OWNER)
        .to_url_lossy()
        .to_string();
    let refused = memory_postgres::PostgresMemory::check_serving_url(&url).await;
    assert!(
        matches!(refused, Err(memory_postgres::Error::OwningLogin { .. })),
        "{refused:?}"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn tables_created_after_the_migration_get_the_serving_grants(pool: PgPool) -> TestResult {
    sqlx::query("CREATE TABLE rh_late_reports (id bigserial PRIMARY KEY, body text)")
        .execute(&pool)
        .await?;
    let granted: bool = sqlx::query_scalar(
        "SELECT has_table_privilege('rh_gateway', 'rh_late_reports', 'INSERT')
            AND has_sequence_privilege('rh_gateway', 'rh_late_reports_id_seq', 'USAGE')",
    )
    .fetch_one(&pool)
    .await?;
    assert!(granted, "a later table missed the serving grants");
    Ok(())
}
