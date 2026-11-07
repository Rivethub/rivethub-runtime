#[path = "db_resilience/logs.rs"]
mod logs;
#[path = "db_resilience/proxy.rs"]
mod proxy;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use kernel::tool::Tool;
use memory_postgres::PostgresMemory;
use sqlx::ConnectOptions as _;
use sqlx::PgPool;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::Command;
use wire::JsonObject;
use wire::gateway::HealthzResponse;

use logs::Logs;
use proxy::Proxy;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const TASK_DOWN: &str = "summary watch: database unreachable; reconnecting";
const TASK_UP: &str = "summary watch: database connection restored";
const JOB_DOWN: &str = "job notifications: database unreachable; reconnecting";
const JOB_UP: &str = "job notifications: database connection restored";

struct Running {
    child: Child,
    logs: Logs,
}

#[derive(Debug, sqlx::FromRow)]
struct Tally {
    n: i64,
    attempts: Option<i64>,
}

fn provider_config(home: &Path, postgres: &str, extra: &str) -> String {
    format!(
        "runtime:\n  workspace: {}\n  default_agent: main\nagents:\n  main:\n    provider: p\n    model: m\nproviders:\n  p:\n    type: openai-compatible\n    base_url: http://127.0.0.1:1\n    model: m\nmemory:\n  postgres:\n    connection_string: {postgres}\n{extra}",
        home.display()
    )
}

fn write_state(home: &Path, body: &str) -> TestResult {
    let state = home.join(".rivethub");
    std::fs::create_dir_all(&state)?;
    std::fs::write(state.join("config.yaml"), body)?;
    Ok(())
}

fn terminate(pid: u32) -> TestResult {
    let pid = i32::try_from(pid)?;
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    )?;
    Ok(())
}

async fn install_queue(pool: &PgPool) -> Result<(), sqlx::Error> {
    let present: bool =
        sqlx::query_scalar("SELECT to_regclass('graphile_worker._private_jobs') IS NOT NULL")
            .fetch_one(pool)
            .await?;
    if present {
        return Ok(());
    }
    sqlx::raw_sql(include_str!("../fixtures/graphile_worker.txt"))
        .execute(pool)
        .await?;
    Ok(())
}

async fn front(pool: &PgPool) -> TestResult<(Proxy, String)> {
    let direct = pool.connect_options().to_url_lossy().to_string();
    let upstream = proxy::upstream(&direct).await?;
    let proxy = Proxy::start(upstream).await?;
    let rewritten = proxy::point_at(&direct, proxy.addr())?;
    Ok((proxy, rewritten))
}

async fn spawn_rh(home: &Path, args: &[&str], ready: &str) -> TestResult<Running> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rh"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .current_dir(home)
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stderr = child.stderr.take().ok_or("missing stderr")?;
    let logs = Logs::capture(BufReader::new(stderr));
    if let Err(error) = logs.wait_for(ready).await {
        if child.try_wait()?.is_none()
            && let Some(pid) = child.id()
        {
            terminate(pid)?;
        }
        return Err(error.into());
    }
    Ok(Running { child, logs })
}

fn alive(child: &mut Child) -> TestResult {
    match child.try_wait()? {
        None => Ok(()),
        Some(status) => Err(format!("process exited during the outage: {status}").into()),
    }
}

async fn stop(running: Running, stopped: &str) -> TestResult {
    let Running { mut child, logs } = running;
    let pid = child.id().ok_or("process already stopped")?;
    terminate(pid)?;
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait()).await??;
    let text = logs.text().await;
    assert!(status.success(), "exit {status}\n{text}");
    assert!(text.contains("SIGTERM received — shutting down"), "{text}");
    assert!(text.contains(stopped), "{text}");
    Ok(())
}

async fn get_text(client: &reqwest::Client, url: &str) -> TestResult<(u16, String)> {
    let response = tokio::time::timeout(Duration::from_secs(20), client.get(url).send()).await??;
    let status = response.status().as_u16();
    let body = response.text().await?;
    Ok((status, body))
}

async fn wait_http(client: &reqwest::Client, url: &str, want: u16) -> TestResult<String> {
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (status, body) = get_text(client, url).await?;
            if status == want {
                return Ok(body);
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    match result {
        Ok(body) => body,
        Err(_elapsed) => Err(format!("timed out waiting for HTTP {want} from {url}").into()),
    }
}

fn client() -> TestResult<reqwest::Client> {
    Ok(reqwest::Client::builder().no_proxy().build()?)
}

async fn tally(pool: &PgPool, name: &str) -> Result<Tally, sqlx::Error> {
    sqlx::query_as(
        "SELECT count(*)::bigint AS n, max(j.attempts)::bigint AS attempts \
           FROM graphile_worker._private_jobs j \
           JOIN graphile_worker._private_tasks t ON t.id = j.task_id \
          WHERE t.identifier = $1",
    )
    .bind(name)
    .fetch_one(pool)
    .await
}

fn job(name: JobName) -> Job {
    Job {
        name,
        payload: JsonObject::default(),
        key: None,
        key_mode: JobKeyMode::KeepRunAt,
        priority: 0,
        run_at: None,
        max_attempts: None,
    }
}

async fn enqueue(pool: &PgPool, name: JobName) -> TestResult {
    PostgresMemory::new(pool.clone())
        .queue()
        .await?
        .enqueue(job(name))
        .await?;
    Ok(())
}

async fn wait_rows(pool: &PgPool, child: &mut Child, name: &str, picked: bool) -> TestResult {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            alive(child)?;
            let row = tally(pool, name).await?;
            let claimed = matches!(row.attempts, Some(attempts) if attempts > 0);
            if row.n == 0 || (picked && claimed) {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    match result {
        Ok(inner) => inner,
        Err(_elapsed) => match tally(pool, name).await {
            Ok(row) => Err(format!("job {name} was not picked up: {row:?}").into()),
            Err(error) => Err(error.into()),
        },
    }
}

async fn once(logs: &Logs, down: &str, up: &str) -> TestResult {
    let down_count = logs.count(down).await;
    let up_count = logs.count(up).await;
    let text = logs.text().await;
    assert_eq!(down_count, 1, "{text}");
    assert_eq!(up_count, 1, "{text}");
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn gateway_stays_up_across_a_database_outage(pool: PgPool) -> TestResult {
    migrate::postgres_pool(&pool).await?;
    let (proxy, url) = front(&pool).await?;
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    write_state(
        home.path(),
        &provider_config(
            home.path(),
            &url,
            &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
        ),
    )?;
    let mut running = spawn_rh(home.path(), &["gateway"], "gateway listening on ").await?;
    let client = client()?;
    let base = format!("http://127.0.0.1:{port}");
    let health = wait_http(&client, &format!("{base}/healthz"), 200).await?;
    let parsed: HealthzResponse = serde_json::from_str(&health)?;
    assert!(parsed.ok);
    assert_eq!(parsed.degraded, None);
    wait_http(&client, &format!("{base}/api/memory/stats"), 200).await?;
    proxy.pause().await;
    running.logs.wait_for(TASK_DOWN).await?;
    alive(&mut running.child)?;
    let degraded = wait_http(&client, &format!("{base}/healthz"), 503).await?;
    let parsed: HealthzResponse = serde_json::from_str(&degraded)?;
    assert!(!parsed.ok);
    let Some(reason) = parsed.degraded else {
        return Err("healthz omitted the degraded reason".into());
    };
    assert!(reason.contains("database unreachable"), "{reason}");
    let stats = wait_http(&client, &format!("{base}/api/memory/stats"), 503).await?;
    assert!(stats.contains("memory store unavailable"), "{stats}");
    proxy.resume();
    running.logs.wait_for(TASK_UP).await?;
    let restored = wait_http(&client, &format!("{base}/healthz"), 200).await?;
    let parsed: HealthzResponse = serde_json::from_str(&restored)?;
    assert!(parsed.ok);
    assert_eq!(parsed.degraded, None);
    wait_http(&client, &format!("{base}/api/memory/stats"), 200).await?;
    once(&running.logs, TASK_DOWN, TASK_UP).await?;
    alive(&mut running.child)?;
    stop(running, "gateway stopped").await?;
    drop(proxy);
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn gateway_starts_while_the_database_is_down(pool: PgPool) -> TestResult {
    migrate::postgres_pool(&pool).await?;
    let (proxy, url) = front(&pool).await?;
    proxy.pause().await;
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    write_state(
        home.path(),
        &provider_config(
            home.path(),
            &url,
            &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
        ),
    )?;
    let mut running = spawn_rh(home.path(), &["gateway"], "gateway listening on ").await?;
    let client = client()?;
    let base = format!("http://127.0.0.1:{port}");
    let degraded = wait_http(&client, &format!("{base}/healthz"), 503).await?;
    let parsed: HealthzResponse = serde_json::from_str(&degraded)?;
    assert!(!parsed.ok);
    wait_http(&client, &format!("{base}/api/memory/stats"), 503).await?;
    wait_http(&client, &format!("{base}/api/events/sessions"), 200).await?;
    proxy.resume();
    wait_http(&client, &format!("{base}/healthz"), 200).await?;
    wait_http(&client, &format!("{base}/api/memory/stats"), 200).await?;
    running
        .logs
        .wait_for("the memory database answered; memory is available")
        .await?;
    alive(&mut running.child)?;
    stop(running, "gateway stopped").await?;
    drop(proxy);
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn worker_stays_up_and_claims_after_the_database_returns(pool: PgPool) -> TestResult {
    migrate::postgres_pool(&pool).await?;
    install_queue(&pool).await?;
    let (proxy, url) = front(&pool).await?;
    let home = tempfile::tempdir()?;
    write_state(home.path(), &provider_config(home.path(), &url, ""))?;
    let mut running = spawn_rh(
        home.path(),
        &["worker", "--role", "summarizer"],
        "worker running",
    )
    .await?;
    proxy.pause().await;
    running.logs.wait_for(JOB_DOWN).await?;
    alive(&mut running.child)?;
    enqueue(&pool, JobName::Fixed(FixedJob::ReapDeadJobs)).await?;
    proxy.resume();
    running.logs.wait_for(JOB_UP).await?;
    wait_rows(&pool, &mut running.child, "reap-dead-jobs", false).await?;
    once(&running.logs, JOB_DOWN, JOB_UP).await?;
    stop(running, "worker stopped").await?;
    drop(proxy);
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn mcp_stats_fail_while_postgres_is_down_and_succeed_after(pool: PgPool) -> TestResult {
    migrate::postgres_pool(&pool).await?;
    let (proxy, url) = front(&pool).await?;
    let home = tempfile::tempdir()?;
    write_state(home.path(), &provider_config(home.path(), &url, ""))?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_rh"))
        .args(["mcp"])
        .env_clear()
        .env("HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stderr = child.stderr.take().ok_or("missing stderr")?;
    let logs = Logs::capture(BufReader::new(stderr));
    let stdout = child.stdout.take().ok_or("missing stdout")?;
    let stdin = child.stdin.take().ok_or("missing stdin")?;
    let client = tokio::time::timeout(
        Duration::from_secs(30),
        mcp::client::Client::connect((stdout, stdin), tokio_util::sync::CancellationToken::new()),
    )
    .await?
    .map_err(|error| format!("mcp connect failed: {error}"))?;
    let tools = client.tools().await.map_err(|error| format!("{error}"))?;
    let stats = tool(&tools, "memory_stats")?;
    let context = kernel::ToolContext {
        agent_id: None,
        working_dir: None,
        session: None,
    };
    let healthy = stats
        .call(Some(rmcp::model::JsonObject::new()), context.clone())
        .await?;
    assert_ne!(healthy.is_error, Some(true), "{healthy:?}");
    proxy.pause().await;
    alive(&mut child)?;
    let failed = tokio::time::timeout(
        Duration::from_secs(20),
        stats.call(Some(rmcp::model::JsonObject::new()), context.clone()),
    )
    .await?
    .map_err(|error| format!("{error}"))?;
    assert_eq!(failed.is_error, Some(true), "{failed:?}");
    proxy.resume();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let restored = stats
                .call(Some(rmcp::model::JsonObject::new()), context.clone())
                .await
                .map_err(|error| format!("{error}"))?;
            if restored.is_error != Some(true) {
                return Ok::<(), String>(());
            }
        }
    })
    .await??;
    let pid = child.id().ok_or("mcp already stopped")?;
    terminate(pid)?;
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait()).await??;
    let text = logs.text().await;
    assert!(status.success(), "mcp exit: {status}\n{text}");
    assert!(text.contains("SIGTERM received — shutting down"), "{text}");
    assert!(text.contains("mcp stopped"), "{text}");
    drop(client);
    drop(proxy);
    Ok(())
}

fn tool<'a>(
    tools: &'a [std::sync::Arc<dyn Tool>],
    name: &str,
) -> TestResult<&'a std::sync::Arc<dyn Tool>> {
    tools
        .iter()
        .find(|tool| tool.definition().name.as_ref() == name)
        .ok_or_else(|| format!("missing tool {name}").into())
}

#[sqlx::test(migrations = false)]
async fn local_sqlite_stays_healthy_when_postgres_backends_die(pool: PgPool) -> TestResult {
    migrate::postgres_pool(&pool).await?;
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    let port_text = port.to_string();
    let mut running = spawn_rh(
        home.path(),
        &["local", "--no-lan", "--port", &port_text],
        "gateway listening on ",
    )
    .await?;
    let client = client()?;
    let base = format!("http://127.0.0.1:{port}");
    wait_http(&client, &format!("{base}/healthz"), 200).await?;
    sqlx::query(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
         WHERE datname = current_database() AND pid <> pg_backend_pid() \
           AND backend_type = 'client backend'",
    )
    .execute(&pool)
    .await?;
    let health = wait_http(&client, &format!("{base}/healthz"), 200).await?;
    let parsed: HealthzResponse = serde_json::from_str(&health)?;
    assert!(parsed.ok);
    assert_eq!(parsed.degraded, None);
    wait_http(&client, &format!("{base}/api/memory/browse"), 200).await?;
    alive(&mut running.child)?;
    assert_eq!(running.logs.count(TASK_DOWN).await, 0);
    assert_eq!(running.logs.count(JOB_DOWN).await, 0);
    stop(running, "gateway stopped").await?;
    Ok(())
}
