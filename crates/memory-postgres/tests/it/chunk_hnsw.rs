use sqlx::PgPool;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CONVERSATION: &str = "00000000-0000-4000-8000-0000000000bb";
const MESSAGES: i32 = 2_000;
const LAST_CHUNK: i32 = 3;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_chunk_arm_can_take_its_order_from_the_hnsw_index(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let plan =
        memory_postgres::explain_chunk_arm_without_full_sorts(&pool, &probe_vector(), 10).await?;
    assert!(uses_hnsw(&plan), "{plan}");
    assert!(!scans_chunks_sequentially(&plan), "{plan}");
    assert_eq!(hnsw_sits_under_the_limit(&plan), Ok(()), "{plan}");
    Ok(())
}

fn hnsw_sits_under_the_limit(plan: &str) -> Result<(), String> {
    let nodes = plan_nodes(plan);
    let scan = nodes
        .iter()
        .rposition(|(_, text)| {
            text.contains("ros_message_chunks_embedding_hnsw")
                && text.to_ascii_lowercase().contains("index scan")
        })
        .ok_or_else(|| format!("no hnsw index scan\n{plan}"))?;
    let mut at = scan;
    loop {
        let Some(parent) = parent_of(&nodes, at) else {
            return Err(format!("hnsw scan has no limit ancestor\n{plan}"));
        };
        let Some((_, text)) = nodes.get(parent) else {
            return Err(format!("hnsw scan has no limit ancestor\n{plan}"));
        };
        if is_limit(text) {
            return Ok(());
        }
        if blocks_the_index_order(text) {
            return Err(format!("hnsw scan is under {text}\n{plan}"));
        }
        at = parent;
    }
}

fn plan_nodes(plan: &str) -> Vec<(usize, &str)> {
    plan.lines()
        .filter_map(|line| {
            let text = line.trim().trim_start_matches("->").trim();
            if text.is_empty() {
                None
            } else {
                let depth = line.chars().take_while(|ch| ch.is_whitespace()).count();
                Some((depth, text))
            }
        })
        .collect()
}

fn parent_of(nodes: &[(usize, &str)], index: usize) -> Option<usize> {
    let depth = nodes.get(index)?.0;
    nodes
        .iter()
        .enumerate()
        .take(index)
        .rev()
        .find(|(_, (parent_depth, _))| *parent_depth < depth)
        .map(|(at, _)| at)
}

fn is_limit(text: &str) -> bool {
    text.to_ascii_lowercase().starts_with("limit")
}

fn blocks_the_index_order(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.starts_with("sort")
        || lower.starts_with("incremental sort")
        || lower.starts_with("unique")
        || lower.starts_with("hashaggregate")
        || lower.starts_with("group")
        || lower.starts_with("windowagg")
}

fn probe_vector() -> Vec<f32> {
    let mut vector = vec![0.0; 1024];
    if let Some(slot) = vector.get_mut(0) {
        *slot = 1.0;
    }
    vector
}

fn uses_hnsw(plan: &str) -> bool {
    plan.lines().any(|line| {
        line.contains("ros_message_chunks_embedding_hnsw")
            && line.to_ascii_lowercase().contains("index scan")
    })
}

fn scans_chunks_sequentially(plan: &str) -> bool {
    plan.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("seq scan") && lower.contains("rh_message_chunks")
    })
}

async fn seed(pool: &PgPool) -> Result<(), sqlx::Error> {
    let mut conn = pool.acquire().await?;
    session(&mut conn).await?;
    conversation(&mut conn).await?;
    messages(&mut conn).await?;
    drop_chunk_indexes(&mut conn).await?;
    chunks(&mut conn).await?;
    chunk_index(&mut conn).await?;
    analyze(&mut conn).await?;
    Ok(())
}

async fn session(conn: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT set_config('rivet.defer_embed_enqueue', 'on', false)")
        .fetch_one(&mut *conn)
        .await?;
    Ok(())
}

async fn conversation(conn: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rh_conversations (id, session_key, agent, channel, owner_user_id)
         VALUES ($1::uuid, 'chunk-hnsw', 'acme', 'test', 'owner')",
    )
    .bind(CONVERSATION)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn messages(conn: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rh_messages
            (id, conversation_id, agent, channel, role, content, created_at, owner_user_id)
         SELECT gen_random_uuid(), $1::uuid, 'acme', 'test', 'user', 'chunk hnsw', now(), 'owner'
         FROM generate_series(1, $2)",
    )
    .bind(CONVERSATION)
    .bind(MESSAGES)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn drop_chunk_indexes(conn: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("DROP INDEX IF EXISTS ros_message_chunks_embedding_hnsw")
        .execute(&mut *conn)
        .await?;
    sqlx::query("DROP INDEX IF EXISTS rh_message_chunks_said_hnsw")
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn chunks(conn: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rh_message_chunks
            (message_id, idx, char_start, char_end, content, embedding, owner_user_id)
         SELECT m.id, g.idx, 0, 4, 'seed',
                array_fill(((g.idx + 1)::real / 20.0), ARRAY[1024])::halfvec(1024),
                'owner'
         FROM rh_messages m
         CROSS JOIN generate_series(0, $1) AS g(idx)",
    )
    .bind(LAST_CHUNK)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn chunk_index(conn: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE INDEX ros_message_chunks_embedding_hnsw
         ON rh_message_chunks USING hnsw (embedding halfvec_cosine_ops)
         WITH (m = 32, ef_construction = 120)",
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn analyze(conn: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    for statement in [
        "ANALYZE rh_message_chunks",
        "ANALYZE rh_messages",
        "ANALYZE rh_conversations",
        "ANALYZE rh_session_trash",
    ] {
        sqlx::query(statement).execute(&mut *conn).await?;
    }
    Ok(())
}
