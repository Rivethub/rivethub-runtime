use crate::common;

use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use kernel::filing::FilingStore as _;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::EmbedTable;
use kernel::memory::jobs::UnembeddedStore as _;
use kernel::owner::Owner;
use kernel::tenancy::Scope;
use kernel::tenancy::Space;
use kernel::tenancy::SpaceId;
use kernel::tenancy::SpaceKind;
use kernel::tenancy::TenancyStore as _;
use kernel::tenancy::Viewer;
use kernel::tenancy::Visibility;
use kernel::topics::HistoryMark;
use kernel::topics::NewTopic;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::Topic;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
use kernel::topics::TopicStore as _;
use memory_postgres::CaptureOptions;
use memory_postgres::PostgresMemory;
use memory_postgres::PostgresTenancy;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use wire::message::Role;
use wire::session_id::SessionId;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const FIRST: &str = "claude-code:00000000-0000-4000-8000-0000000000b1";
const SECOND: &str = "claude-code:00000000-0000-4000-8000-0000000000b2";
const LATER: &str = "claude-code:00000000-0000-4000-8000-0000000000b3";
const UNCAPTURED: &str = "claude-code:00000000-0000-4000-8000-0000000000b4";
const THIRD: &str = "claude-code:00000000-0000-4000-8000-0000000000b5";
const PROBE: &str = "rh_rls_probe";
const PERSON: &str = "rh_rls_person";
const OWNER: &str = "rh_rls_owner";
const GROUP: &str = "rh_rls_group";

fn batch(session: &str, text: &str) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: session.to_owned(),
        agent: "rivet".to_owned(),
        channel: Some("cli".to_owned()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: vec![CaptureMessage {
            event_id: format!("{session}-{text}"),
            role: Role::User,
            content: text.to_owned(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: None,
            created_at: None,
        }],
    }
}

fn household() -> Result<SpaceId, Box<dyn std::error::Error>> {
    Ok(SpaceId::new("household".to_owned())?)
}

fn viewer(name: &str) -> Result<Viewer, Box<dyn std::error::Error>> {
    Ok(Viewer::new(
        Owner::new(name.to_owned())?,
        vec![household()?],
    ))
}

async fn probe(pool: &PgPool) -> Result<PgPool, Box<dyn std::error::Error>> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DO $$ BEGIN CREATE ROLE {PROBE} NOLOGIN; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
    )))
    .execute(pool)
    .await?;
    for grant in [
        format!("GRANT USAGE ON SCHEMA public TO {PROBE}"),
        format!("GRANT ALL ON ALL TABLES IN SCHEMA public TO {PROBE}"),
        format!("GRANT ALL ON ALL SEQUENCES IN SCHEMA public TO {PROBE}"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(grant))
            .execute(pool)
            .await?;
    }
    let options = pool
        .connect_options()
        .as_ref()
        .clone()
        .options([("role", PROBE)]);
    Ok(
        memory_postgres::scoped(PgPoolOptions::new().max_connections(2))
            .connect_with(options)
            .await?,
    )
}

async fn seed(
    pool: &PgPool,
) -> Result<(PostgresMemory, PostgresTenancy), Box<dyn std::error::Error>> {
    common::install_graphile(pool).await?;
    let store = PostgresMemory::new(pool.clone());
    let alice = Owner::new("alice".to_owned())?;
    for (session, text) in [(FIRST, "the first session"), (SECOND, "the second session")] {
        store
            .capture_batch(&batch(session, text), &CaptureOptions::owned_by(&alice))
            .await?;
    }
    let tenancy = PostgresTenancy::new(pool.clone());
    tenancy
        .sync_spaces(&[Space {
            id: household()?,
            kind: SpaceKind::Household,
            name: "Household".to_owned(),
            members: vec![alice, Owner::new("bob".to_owned())?],
        }])
        .await?;
    Ok((store, tenancy))
}

async fn visible(pool: &PgPool, scope: Scope, table: &str) -> Result<i64, sqlx::Error> {
    let sql = format!("SELECT count(*) FROM {table}");
    scope
        .run(async {
            sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql))
                .fetch_one(pool)
                .await
        })
        .await
}

async fn unscoped(pool: &PgPool, table: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
        .fetch_one(pool)
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn without_a_scope_the_store_sees_nothing(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let probe = probe(&pool).await?;
    for table in ["rh_conversations", "rh_messages", "rh_spaces"] {
        assert_eq!(unscoped(&probe, table).await?, 0, "{table}");
        assert_eq!(visible(&probe, Scope::Closed, table).await?, 0, "{table}");
    }
    assert_eq!(
        visible(&probe, Scope::Maintenance, "rh_conversations").await?,
        2
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_shared_session_shows_to_the_space_and_a_private_one_does_not(
    pool: PgPool,
) -> TestResult {
    seed(&pool).await?;
    let probe = probe(&pool).await?;
    let alice = viewer("alice")?;
    let bob = viewer("bob")?;
    let owners = Scope::Viewer(alice.clone());
    let members = Scope::Viewer(bob.clone());
    assert_eq!(visible(&probe, owners.clone(), "rh_messages").await?, 2);
    assert_eq!(visible(&probe, members.clone(), "rh_messages").await?, 0);

    let tenancy = PostgresTenancy::new(probe.clone());
    let second = SessionId::try_from(SECOND.to_owned())?;
    let shared = Visibility::Space(household()?);
    let marked = owners
        .clone()
        .run(tenancy.mark_session(&alice, &second, Some(&shared), Utc::now()))
        .await?;
    assert_eq!(marked.effective, shared);
    assert_eq!(
        visible(&probe, members.clone(), "rh_conversations").await?,
        1
    );
    assert_eq!(visible(&probe, members.clone(), "rh_messages").await?, 1);

    let refused = members
        .clone()
        .run(tenancy.mark_session(&bob, &second, Some(&Visibility::Private), Utc::now()))
        .await;
    assert!(refused.is_err(), "only the owner marks a session");
    let renamed = members
        .clone()
        .run(async {
            sqlx::query("UPDATE rh_conversations SET title = 'taken' WHERE session_key = $1")
                .bind(SECOND)
                .execute(&probe)
                .await
        })
        .await?;
    assert_eq!(
        renamed.rows_affected(),
        0,
        "a member reads a shared row but cannot write it"
    );

    owners
        .clone()
        .run(tenancy.mark_session(&alice, &second, Some(&Visibility::Private), Utc::now()))
        .await?;
    assert_eq!(visible(&probe, members, "rh_messages").await?, 0);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_session_follows_its_topic_unless_it_is_marked(pool: PgPool) -> TestResult {
    let (store, _) = seed(&pool).await?;
    let probe = probe(&pool).await?;
    let alice = viewer("alice")?;
    let members = Scope::Viewer(viewer("bob")?);
    let now = Utc::now();
    let topic = store
        .create(Topic::create(
            TopicId::random(),
            NewTopic {
                kind: TopicKind::Work,
                name: TopicName::new("Kitchen")?,
                aliases: Vec::new(),
                parent: None,
                directory: None,
                owner: Some(alice.principal().clone()),
            },
            now,
        )?)
        .await?;
    for session in [FIRST, SECOND] {
        store
            .place(Placement {
                session: SessionId::try_from(session.to_owned())?,
                topic: topic.id,
                by: PlacementSource::You,
                at: now,
                owner: Some(alice.principal().clone()),
            })
            .await?;
    }
    let tenancy = PostgresTenancy::new(probe.clone());
    let shared = Visibility::Space(household()?);
    let owners = Scope::Viewer(alice.clone());
    let marked = owners
        .clone()
        .run(tenancy.mark_topic(&alice, topic.id, Some(&shared)))
        .await?;
    assert_eq!(marked.effective, shared);
    assert_eq!(
        visible(&probe, members.clone(), "rh_topics WHERE kind = 'work'").await?,
        1
    );
    assert_eq!(
        visible(&probe, members.clone(), "rh_conversations").await?,
        2
    );
    assert_eq!(
        visible(&probe, members.clone(), "rh_session_placements").await?,
        2
    );

    let first = SessionId::try_from(FIRST.to_owned())?;
    owners
        .clone()
        .run(tenancy.mark_session(&alice, &first, Some(&Visibility::Private), Utc::now()))
        .await?;
    assert_eq!(
        visible(&probe, members.clone(), "rh_conversations").await?,
        1
    );

    owners
        .clone()
        .run(tenancy.mark_topic(&alice, topic.id, None))
        .await?;
    let second = SessionId::try_from(SECOND.to_owned())?;
    owners
        .clone()
        .run(tenancy.mark_session(&alice, &second, Some(&shared), Utc::now()))
        .await?;
    assert_eq!(
        visible(&probe, members.clone(), "rh_topics WHERE kind = 'work'").await?,
        0
    );
    assert_eq!(
        visible(&probe, members.clone(), "rh_conversations").await?,
        1
    );
    assert_eq!(
        visible(&probe, members, "rh_session_placements").await?,
        0,
        "a shared session in a private topic shows without its topic"
    );

    let uncaptured = SessionId::try_from(UNCAPTURED.to_owned())?;
    store
        .place(Placement {
            session: uncaptured.clone(),
            topic: topic.id,
            by: PlacementSource::You,
            at: now,
            owner: Some(alice.principal().clone()),
        })
        .await?;
    owners
        .clone()
        .run(tenancy.mark_topic(&alice, topic.id, Some(&shared)))
        .await?;
    let found = owners
        .run(tenancy.session_visibility(&alice, std::slice::from_ref(&uncaptured)))
        .await?;
    assert_eq!(
        found.get(&uncaptured).map(|state| state.effective.clone()),
        Some(shared),
        "a session with no transcript yet follows its topic"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_shared_default_applies_to_sessions_started_after_it(pool: PgPool) -> TestResult {
    let (store, tenancy) = seed(&pool).await?;
    let started: DateTime<Utc> = sqlx::query_scalar("SELECT max(created_at) FROM rh_conversations")
        .fetch_one(&pool)
        .await?;
    let alice = Owner::new("alice".to_owned())?;
    let shared = Visibility::Space(household()?);
    tenancy
        .set_default_visibility(&alice, &shared, started + Duration::microseconds(1))
        .await?;
    store
        .capture_batch(
            &batch(LATER, "a later session"),
            &CaptureOptions::owned_by(&alice),
        )
        .await?;
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT session_key, visibility FROM rh_conversations ORDER BY session_key")
            .fetch_all(&pool)
            .await?;
    assert_eq!(
        rows,
        vec![
            (FIRST.to_owned(), "private".to_owned()),
            (SECOND.to_owned(), "private".to_owned()),
            (LATER.to_owned(), "household".to_owned()),
        ]
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn text_search_keeps_its_index_under_row_security(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let probe = probe(&pool).await?;
    let plan: Vec<String> = Scope::Viewer(viewer("bob")?)
        .run(async {
            let mut tx = probe.begin().await?;
            sqlx::query("SET LOCAL enable_seqscan = off")
                .execute(&mut *tx)
                .await?;
            sqlx::query_scalar(
                "EXPLAIN (COSTS off) SELECT id FROM rh_messages \
                 WHERE content_tsv @@ plainto_tsquery('english', 'session')",
            )
            .fetch_all(&mut *tx)
            .await
        })
        .await?;
    assert!(
        plan.iter()
            .any(|line| line.contains("idx_ros_messages_fts")),
        "{plan:?}"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_filing_listing_reads_each_session_as_the_visibility_store_does(
    pool: PgPool,
) -> TestResult {
    let (store, _) = seed(&pool).await?;
    let probe = probe(&pool).await?;
    let alice = viewer("alice")?;
    let now = Utc::now();
    let topic = store
        .create(Topic::create(
            TopicId::random(),
            NewTopic {
                kind: TopicKind::Work,
                name: TopicName::new("Kitchen")?,
                aliases: Vec::new(),
                parent: None,
                directory: None,
                owner: Some(alice.principal().clone()),
            },
            now,
        )?)
        .await?;
    for session in [FIRST, SECOND, UNCAPTURED] {
        store
            .place(Placement {
                session: SessionId::try_from(session.to_owned())?,
                topic: topic.id,
                by: PlacementSource::You,
                at: now,
                owner: Some(alice.principal().clone()),
            })
            .await?;
    }
    let tenancy = PostgresTenancy::new(probe.clone());
    let owners = Scope::Viewer(alice.clone());
    let shared = Visibility::Space(household()?);
    owners
        .clone()
        .run(tenancy.mark_topic(&alice, topic.id, Some(&shared)))
        .await?;
    owners
        .clone()
        .run(tenancy.mark_session(
            &alice,
            &SessionId::try_from(FIRST.to_owned())?,
            Some(&Visibility::Private),
            now,
        ))
        .await?;
    let listing = PostgresMemory::new(probe);
    for viewer in [alice, viewer("bob")?] {
        let scope = Scope::Viewer(viewer.clone());
        let filed = scope.clone().run(listing.all_filed()).await?;
        let keys: Vec<SessionId> = filed
            .sessions
            .iter()
            .map(|session| session.placement.session.clone())
            .collect();
        let expected = scope
            .run(tenancy.session_visibility(&viewer, &keys))
            .await?;
        assert!(!keys.is_empty(), "{viewer:?} sees a filed session");
        for session in filed.sessions {
            assert_eq!(
                session.visibility.as_ref(),
                expected.get(&session.placement.session),
                "{:?} for {viewer:?}",
                session.placement.session
            );
        }
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_categorizer_files_a_session_captured_under_its_bare_key(pool: PgPool) -> TestResult {
    let (store, _) = seed(&pool).await?;
    let bare = "00000000-0000-4000-8000-0000000000b5";
    sqlx::query(
        "INSERT INTO rh_conversations (id, session_key, agent, channel, owner_user_id)
         VALUES ('00000000-0000-4000-8000-0000000000c5', $1, 'rivet', 'claude-code', 'alice')",
    )
    .bind(bare)
    .execute(&pool)
    .await?;
    let alice = viewer("alice")?;
    let topic = store
        .create(Topic::create(
            TopicId::random(),
            NewTopic {
                kind: TopicKind::Work,
                name: TopicName::new("Garden")?,
                aliases: Vec::new(),
                parent: None,
                directory: None,
                owner: Some(alice.principal().clone()),
            },
            Utc::now(),
        )?)
        .await?;
    let categorizer = PostgresMemory::new(probe(&pool).await?);
    Scope::Viewer(alice)
        .run(categorizer.place(Placement {
            session: SessionId::try_from(format!("claude-code:{bare}"))?,
            topic: topic.id,
            by: PlacementSource::Categorizer,
            at: Utc::now(),
            owner: None,
        }))
        .await?;
    let owner: Option<String> =
        sqlx::query_scalar("SELECT owner_user_id FROM rh_session_placements WHERE session_id = $1")
            .bind(format!("claude-code:{bare}"))
            .fetch_optional(&pool)
            .await?;
    assert_eq!(owner.as_deref(), Some("alice"));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn capture_checks_event_ids_through_their_index_under_row_security(
    pool: PgPool,
) -> TestResult {
    seed(&pool).await?;
    let probe = probe(&pool).await?;
    let conversation: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM rh_conversations WHERE session_key = $1")
            .bind(FIRST)
            .fetch_one(&pool)
            .await?;
    let plan: Vec<String> = Scope::Viewer(viewer("alice")?)
        .run(async {
            sqlx::query_scalar(
                "EXPLAIN SELECT seen.event_id FROM unnest($2::text[]) AS wanted(event_id)
                  CROSS JOIN LATERAL (
                      SELECT m.metadata->>'event_id' AS event_id FROM rh_messages m
                       WHERE m.conversation_id = $1 AND m.metadata->>'event_id' = wanted.event_id
                       LIMIT 1
                  ) seen",
            )
            .bind(conversation)
            .bind(vec!["a-new-event".to_owned()])
            .fetch_all(&probe)
            .await
        })
        .await?;
    assert!(
        plan.iter()
            .any(|line| line.contains("rh_messages_event_id")),
        "{plan:#?}"
    );
    Ok(())
}

async fn owned_by_an_ordinary_role(pool: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DO $$ BEGIN CREATE ROLE {OWNER} NOLOGIN; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
    )))
    .execute(pool)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DO $$ DECLARE item record; BEGIN
            FOR item IN SELECT c.oid::regclass AS name FROM pg_class c
                          JOIN pg_namespace n ON n.oid = c.relnamespace
                         WHERE n.nspname = 'public' AND c.relkind IN ('r', 'v') LOOP
                EXECUTE format('ALTER TABLE %s OWNER TO {OWNER}', item.name);
            END LOOP;
            FOR item IN SELECT p.oid::regprocedure AS name FROM pg_proc p
                          JOIN pg_namespace n ON n.oid = p.pronamespace
                         WHERE n.nspname = 'public' AND p.prokind = 'f'
                           AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = p.oid AND d.deptype = 'e') LOOP
                EXECUTE format('ALTER FUNCTION %s OWNER TO {OWNER}', item.name);
            END LOOP;
        END $$"
    )))
    .execute(pool)
    .await?;
    Ok(())
}

async fn queue_access(pool: &PgPool, role: &str) -> Result<(), sqlx::Error> {
    for grant in [
        format!("GRANT USAGE ON SCHEMA graphile_worker TO {role}"),
        format!("GRANT ALL ON ALL TABLES IN SCHEMA graphile_worker TO {role}"),
        format!("GRANT ALL ON ALL SEQUENCES IN SCHEMA graphile_worker TO {role}"),
        format!(
            "DO $$ DECLARE t text; BEGIN
                FOREACH t IN ARRAY ARRAY['_private_jobs', '_private_job_queues', '_private_tasks'] LOOP
                    EXECUTE format('CREATE POLICY {role} ON graphile_worker.%I TO {role} USING (true)', t);
                END LOOP;
             END $$"
        ),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(grant))
            .execute(pool)
            .await?;
    }
    Ok(())
}

async fn person(pool: &PgPool, principal: &str) -> Result<PgPool, Box<dyn std::error::Error>> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DO $$ BEGIN CREATE ROLE {PERSON} LOGIN PASSWORD '{PERSON}'; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
    )))
    .execute(pool)
    .await?;
    for grant in [
        format!("GRANT USAGE ON SCHEMA public TO {PERSON}"),
        format!("GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO {PERSON}"),
        format!("GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO {PERSON}"),
        format!(
            "DO $$ BEGIN CREATE ROLE {GROUP} NOLOGIN; \
             EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
        ),
        format!("GRANT SELECT ON ALL TABLES IN SCHEMA public TO {GROUP}"),
        format!("GRANT {GROUP} TO {PERSON}"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(grant))
            .execute(pool)
            .await?;
    }
    sqlx::query("INSERT INTO rh_role_principals (role_name, user_id) VALUES ($1, $2)")
        .bind(PERSON)
        .bind(principal)
        .execute(pool)
        .await?;
    let options = pool
        .connect_options()
        .as_ref()
        .clone()
        .username(PERSON)
        .password(PERSON);
    Ok(
        memory_postgres::scoped(PgPoolOptions::new().max_connections(2))
            .connect_with(options)
            .await?,
    )
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_pinned_login_keeps_its_person_whatever_it_sets(pool: PgPool) -> TestResult {
    let (store, tenancy) = seed(&pool).await?;
    let alice = viewer("alice")?;
    tenancy
        .mark_session(
            &alice,
            &SessionId::try_from(FIRST.to_owned())?,
            Some(&Visibility::Space(household()?)),
            Utc::now(),
        )
        .await?;
    store
        .capture_batch(
            &batch(THIRD, "a session outside the household"),
            &CaptureOptions::owned_by(&Owner::new("carol".to_owned())?),
        )
        .await?;
    owned_by_an_ordinary_role(&pool).await?;
    let pinned = person(&pool, "bob").await?;
    let forged = Scope::Viewer(alice.clone());
    for scope in [forged.clone(), Scope::Maintenance, Scope::Closed] {
        assert_eq!(
            visible(&pinned, scope.clone(), "rh_conversations").await?,
            1,
            "{scope:?}"
        );
        assert_eq!(visible(&pinned, scope, "rh_messages").await?, 1);
    }
    let mut elevated = pinned.begin().await?;
    sqlx::query("SELECT rh_elevate()")
        .execute(&mut *elevated)
        .await?;
    let seen: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_conversations")
        .fetch_one(&mut *elevated)
        .await?;
    assert_eq!(seen, 1, "rh_elevate() widened a pinned login");
    elevated.rollback().await?;
    let mut regrouped = pinned.begin().await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("SET ROLE {GROUP}")))
        .execute(&mut *regrouped)
        .await?;
    sqlx::query("SELECT set_config('rh.maintenance', 'on', true)")
        .execute(&mut *regrouped)
        .await?;
    let seen: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_conversations")
        .fetch_one(&mut *regrouped)
        .await?;
    assert_eq!(seen, 1, "SET ROLE into a group widened a pinned login");
    regrouped.rollback().await?;
    let as_person = PostgresMemory::new(pinned.clone());
    let stolen = forged
        .clone()
        .run(as_person.capture_batch(
            &batch(LATER, "claimed for alice"),
            &CaptureOptions::owned_by(&Owner::new("alice".to_owned())?),
        ))
        .await;
    assert!(
        stolen.is_err(),
        "a pinned login captured a session as someone else"
    );
    forged
        .run(as_person.capture_batch(
            &batch(LATER, "bob's own session"),
            &CaptureOptions::owned_by(&Owner::new("bob".to_owned())?),
        ))
        .await?;
    let owners: Vec<(String, String)> = sqlx::query_as(
        "SELECT session_key, owner_user_id FROM rh_conversations WHERE session_key = $1",
    )
    .bind(LATER)
    .fetch_all(&pool)
    .await?;
    assert_eq!(owners, vec![(LATER.to_owned(), "bob".to_owned())]);
    let counted: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rh_message_counts WHERE owner_user_id = $1")
            .bind("bob")
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        counted, 1,
        "the count trigger did not run for a pinned login"
    );
    let remapped =
        sqlx::query("INSERT INTO rh_role_principals (role_name, user_id) VALUES ($1, 'alice')")
            .bind(OWNER)
            .execute(&pinned)
            .await;
    assert!(remapped.is_err(), "a pinned login wrote the role mapping");
    Scope::Maintenance
        .run(PostgresTenancy::new(pinned).sync_spaces(&[]))
        .await?;
    assert_eq!(
        visible(&pool, Scope::Maintenance, "rh_space_members").await?,
        2
    );
    Ok(())
}

async fn work_topic(
    store: &PostgresMemory,
    owner: &Viewer,
    name: &str,
) -> Result<TopicId, Box<dyn std::error::Error>> {
    let topic = store
        .create(Topic::create(
            TopicId::random(),
            NewTopic {
                kind: TopicKind::Work,
                name: TopicName::new(name)?,
                aliases: Vec::new(),
                parent: None,
                directory: None,
                owner: Some(owner.principal().clone()),
            },
            Utc::now(),
        )?)
        .await?;
    Ok(topic.id)
}

async fn session_state_as(
    probe: &PgPool,
    owner: &Viewer,
    session: &str,
    conversation: Option<Uuid>,
    topic: TopicId,
) -> Result<(), sqlx::Error> {
    let who = owner.principal().as_str();
    let topic = topic.uuid();
    let correction = match conversation {
        Some(id) => format!("NULL, '{id}'"),
        None => format!("'{session}', NULL"),
    };
    let sql = format!(
        "INSERT INTO rh_session_startup (session_id, topic_id, tokens, sections, created_at)
             VALUES ('{session}', '{topic}', 1, '[]', now());
         INSERT INTO rh_session_suggestions (session_id, name, topic_id) VALUES ('{session}', 'Kitchen', '{topic}');
         INSERT INTO rh_session_history (session_id, marked_at) VALUES ('{session}', now());
         INSERT INTO rh_session_keeps (session_key, kept_at, kept_by) VALUES ('{session}', now(), '{who}');
         INSERT INTO rh_session_tombstones (session_key, deleted_at, deleted_by) VALUES ('{session}', now(), '{who}');
         INSERT INTO rh_filing_corrections (id, session_id, conversation_id, from_topic, corrected_at)
             VALUES (gen_random_uuid(), {correction}, '{topic}', now());
         INSERT INTO rh_hub_state (user_id, version, state) VALUES ('{who}', 1, '{{}}')"
    );
    Scope::Viewer(owner.clone())
        .run(async { sqlx::raw_sql(sqlx::AssertSqlSafe(sql)).execute(probe).await })
        .await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_pinned_login_sees_its_own_session_state_and_no_one_elses(pool: PgPool) -> TestResult {
    let (store, tenancy) = seed(&pool).await?;
    let alice = viewer("alice")?;
    let bob = viewer("bob")?;
    store
        .capture_batch(
            &batch(LATER, "bob's session"),
            &CaptureOptions::owned_by(bob.principal()),
        )
        .await?;
    let kitchen = work_topic(&store, &alice, "Kitchen").await?;
    let pantry = work_topic(&store, &alice, "Pantry").await?;
    Scope::Viewer(alice.clone())
        .run(tenancy.mark_topic(&alice, kitchen, Some(&Visibility::Space(household()?))))
        .await?;
    let hers: Uuid = sqlx::query_scalar("SELECT id FROM rh_conversations WHERE session_key = $1")
        .bind(FIRST)
        .fetch_one(&pool)
        .await?;
    let probe = probe(&pool).await?;
    queue_access(&pool, PROBE).await?;
    session_state_as(&probe, &alice, FIRST, Some(hers), kitchen).await?;
    session_state_as(&probe, &bob, LATER, None, kitchen).await?;
    Scope::Viewer(alice.clone())
        .run(
            sqlx::query(
                "INSERT INTO rh_agent_presets (id, name, node, directory, system_prompt)
                 VALUES ('helper', 'Helper', 'node-a', '/srv/helper', 'the household helper')",
            )
            .execute(&probe),
        )
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "INSERT INTO rh_flagged_jobs (task, job_key, payload, priority, max_attempts, failures, last_error,
             first_failed_at, last_failed_at, flagged_at)
             VALUES ('file-conversation', 'file-1', '{{}}', 0, 3, 3, 'failed', now(), now(), now());
         INSERT INTO rh_tasks_archive (goal, executor, agent_id, origin, conversation_id, session_key)
             VALUES ('hers', 'chat-loop', 'rivet', 'chat', '{hers}', NULL),
                    ('his', 'chat-loop', 'rivet', 'chat', NULL, '{LATER}'),
                    ('nobody''s', 'chat-loop', 'rivet', 'chat', NULL, NULL);
         INSERT INTO rh_delegation_runs_archive (from_agent, to_agent, task, status)
             VALUES ('rivet', 'helper', 'an old delegation', 'completed');
         INSERT INTO rh_topic_redirects (slug, topic_id, created_at)
             VALUES ('cooking', '{}', now()), ('stores', '{}', now());
         INSERT INTO rh_wiki_topics (slug, title, owner_user_id, visibility)
             VALUES ('her-notes', 'Her notes', 'alice', 'private'), ('his-notes', 'His notes', 'bob', 'private');
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at)
             VALUES ('her-old-notes', 'her-notes', now()), ('his-old-notes', 'his-notes', now())",
        kitchen.uuid(),
        pantry.uuid()
    )))
    .execute(&pool)
    .await?;
    owned_by_an_ordinary_role(&pool).await?;
    let pinned = person(&pool, "bob").await?;

    for (table, his, hers, all) in [
        ("rh_session_startup", 1, 1, 2),
        ("rh_session_suggestions", 1, 1, 2),
        ("rh_session_history", 1, 1, 2),
        ("rh_session_keeps", 1, 1, 2),
        ("rh_filing_corrections", 1, 1, 2),
        ("rh_hub_state", 1, 1, 2),
        ("rh_agent_presets", 1, 1, 1),
        ("rh_tasks_archive", 1, 1, 3),
        ("ros_tasks", 1, 1, 3),
        ("rh_delegation_runs_archive", 0, 0, 1),
        ("rh_flagged_jobs", 0, 1, 1),
        ("rh_topic_redirects", 1, 2, 2),
        ("rh_wiki_redirects", 1, 1, 2),
    ] {
        for scope in [
            Scope::Closed,
            Scope::Viewer(alice.clone()),
            Scope::Maintenance,
        ] {
            assert_eq!(
                visible(&pinned, scope.clone(), table).await?,
                his,
                "{table} for the pinned login under {scope:?}"
            );
        }
        assert_eq!(
            visible(&probe, Scope::Viewer(alice.clone()), table).await?,
            hers,
            "{table} for a service login acting for alice"
        );
        assert_eq!(
            visible(&probe, Scope::Maintenance, table).await?,
            all,
            "{table} under maintenance"
        );
        assert_eq!(
            visible(&probe, Scope::Closed, table).await?,
            0,
            "{table} outside any scope"
        );
    }
    for table in [
        "rh_session_tombstones",
        "rh_backfills",
        "_rivetos_migrations",
    ] {
        assert_eq!(
            visible(&pinned, Scope::Closed, table).await?,
            unscoped(&pool, table).await?,
            "{table} stays readable to every login"
        );
    }

    let forged = sqlx::query(
        "INSERT INTO rh_session_tombstones (session_key, deleted_at, deleted_by) VALUES ($1, now(), 'bob')",
    )
    .bind(SECOND)
    .execute(&pinned)
    .await;
    assert!(
        forged.is_err(),
        "a pinned login tombstoned someone else's session"
    );
    let rewritten = sqlx::query("UPDATE rh_agent_presets SET system_prompt = 'changed'")
        .execute(&pinned)
        .await?;
    assert_eq!(
        rewritten.rows_affected(),
        0,
        "a pinned login rewrote someone else's preset"
    );
    let as_person = PostgresMemory::new(pinned.clone());
    Scope::Closed
        .run(as_person.mark_history(HistoryMark {
            session: SessionId::try_from(UNCAPTURED.to_owned())?,
            at: Utc::now(),
        }))
        .await?;
    assert_eq!(
        visible(&pinned, Scope::Closed, "rh_session_history").await?,
        2,
        "a done mark on a session with no transcript and no placement"
    );
    let as_service = PostgresMemory::new(probe.clone());
    Scope::Viewer(alice)
        .run(kernel::topics::TopicStore::delete(&as_service, kitchen))
        .await?;
    assert_eq!(
        visible(&probe, Scope::Maintenance, "rh_session_suggestions").await?,
        0,
        "removing a topic removes every suggestion that pointed at it"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn every_elevating_function_runs_as_its_owner_on_a_pinned_path(pool: PgPool) -> TestResult {
    let loose: Vec<String> = sqlx::query_scalar(
        "SELECT p.oid::regprocedure::text FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
          WHERE n.nspname = current_schema() AND p.proname <> 'rh_elevate'
            AND (p.prosrc LIKE '%rh_elevate()%' OR p.prosrc LIKE '%rh_refresh_%')
            AND NOT (p.prosecdef
                     AND format('search_path=pg_catalog, %s, pg_temp', current_schema()) = ANY (p.proconfig))
          ORDER BY 1",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(loose, Vec::<String>::new());
    Ok(())
}

fn refused(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .as_deref()
        == Some("42501")
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_pinned_login_cannot_call_or_attach_an_elevating_function(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    owned_by_an_ordinary_role(&pool).await?;
    let pinned = person(&pool, "bob").await?;
    let callable: Vec<String> = sqlx::query_scalar(
        "SELECT p.oid::regprocedure::text FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
          WHERE n.nspname = current_schema() AND p.prosecdef AND has_function_privilege(p.oid, 'EXECUTE')
          ORDER BY 1",
    )
    .fetch_all(&pinned)
    .await?;
    assert_eq!(
        callable,
        Vec::<String>::new(),
        "a pinned login may call these elevating functions"
    );
    for call in [
        "SELECT rh_backfill_message_counts(10)",
        "SELECT rh_backfill_chunk_said(gen_random_uuid(), 10)",
    ] {
        let outcome = sqlx::query(call).execute(&pinned).await;
        assert!(
            outcome.as_ref().is_err_and(refused),
            "{call} as a pinned login: {outcome:?}"
        );
    }
    let mut conn = pinned.acquire().await?;
    sqlx::query("CREATE TEMP TABLE forged (LIKE rh_messages INCLUDING DEFAULTS)")
        .execute(&mut *conn)
        .await?;
    let attached = sqlx::query(
        "CREATE TRIGGER forged AFTER INSERT ON forged
             FOR EACH ROW EXECUTE FUNCTION rh_message_counts_inserted()",
    )
    .execute(&mut *conn)
    .await;
    assert!(
        attached.as_ref().is_err_and(refused),
        "a pinned login attached an elevating trigger function to its own table: {attached:?}"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_pinned_login_captures_without_reaching_the_job_queue(pool: PgPool) -> TestResult {
    let (store, _) = seed(&pool).await?;
    owned_by_an_ordinary_role(&pool).await?;
    let pinned = person(&pool, "bob").await?;
    for sql in [
        "SELECT count(*) FROM graphile_worker._private_jobs",
        "SELECT graphile_worker.add_job('embed-target', '{}')",
    ] {
        let outcome = sqlx::query(sql).execute(&pinned).await;
        assert!(
            outcome.as_ref().is_err_and(refused),
            "{sql} as a pinned login: {outcome:?}"
        );
    }
    let queued_before = unscoped(&pool, "graphile_worker._private_jobs").await?;
    let bob = viewer("bob")?;
    let as_person = PostgresMemory::new(pinned.clone());
    assert!(
        matches!(
            as_person.queue().await,
            Err(memory_postgres::Error::QueueAbsent)
        ),
        "a pinned login reached the job queue"
    );
    Scope::Viewer(bob.clone())
        .run(as_person.capture_batch(
            &batch(LATER, "bob's own session, long enough to embed"),
            &CaptureOptions::owned_by(bob.principal()),
        ))
        .await?;
    Scope::Viewer(bob.clone())
        .run(work_topic(&as_person, &bob, "Kitchen"))
        .await?;
    assert_eq!(
        unscoped(&pool, "graphile_worker._private_jobs").await?,
        queued_before,
        "a pinned login's capture queued a job"
    );
    let captured: String = sqlx::query_scalar(
        "SELECT m.id::text FROM rh_messages m JOIN rh_conversations c ON c.id = m.conversation_id
          WHERE c.session_key = $1",
    )
    .bind(LATER)
    .fetch_one(&pool)
    .await?;
    let swept = Scope::Maintenance
        .run(store.unembedded(EmbedTable::Messages, 100))
        .await?;
    assert!(
        swept.iter().any(|target| target.id == captured),
        "the store's embedding sweep does not see the pinned login's message: {swept:?}"
    );
    Ok(())
}
