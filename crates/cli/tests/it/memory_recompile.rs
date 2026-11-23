use cli::Exit;
use kernel::queue::{ClaimSelector, FixedJob, JobName, JobQueue, WorkerRole};
use kernel::wiki::recompile::RecompileWiki;
use memory_sqlite::SqliteStore;

use crate::support;

#[tokio::test]
async fn sqlite_recompile_dry_run_and_worker_queue() -> Result<(), Box<dyn std::error::Error>> {
    let home = support::home();
    let database = home.path().join("memory.sqlite");
    let wiki = home.path().join("custom-wiki");
    migrate::sqlite(&database)?;
    std::fs::create_dir_all(wiki.join("topics"))?;
    std::fs::write(wiki.join("topics/acmekit.md"), "test page")?;
    support::config(
        &home,
        &format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    wiki_dir: {}\n",
            database.display(),
            wiki.display()
        ),
    );
    let environment = support::environment(&home);
    let since = "2026-10-01T00:00:00Z";
    let mut args = vec![
        "memory",
        "recompile-wiki",
        "--since",
        since,
        "--slug",
        "acmekit",
        "--json",
    ];
    let queue = SqliteStore::open(&database)?;
    let selector = ClaimSelector::matching(queue.task_names(WorkerRole::WikiBuilder));

    args.push("--dry-run");
    let plan = support::run(&args, &environment).await;
    assert_eq!(plan.exit, Exit::Ok, "{}", plan.stderr);
    assert_eq!(
        plan.stdout,
        "{\"queued\":false,\"pages\":[{\"slug\":\"acmekit\",\"bytes\":9}]}\n"
    );
    assert!(queue.claim(&selector).await?.is_none());

    args.pop();
    let queued = support::run(&args, &environment).await;
    assert_eq!(queued.exit, Exit::Ok, "{}", queued.stderr);
    assert_eq!(
        queued.stdout,
        "{\"queued\":true,\"pages\":[{\"slug\":\"acmekit\",\"bytes\":9}]}\n"
    );
    let claimed = queue.claim(&selector).await?.ok_or("recompile job")?;
    assert_eq!(claimed.name, JobName::Fixed(FixedJob::RecompileWiki));
    let payload = claimed.payload.deserialize::<RecompileWiki>()?;
    assert_eq!(payload.slug.as_ref(), "acmekit");
    assert_eq!(payload.since, since.parse()?);
    Ok(())
}

#[tokio::test]
async fn recompile_takes_a_sqlite_file_without_any_node_config()
-> Result<(), Box<dyn std::error::Error>> {
    let home = support::home();
    let database = home.path().join("memory.sqlite");
    let wiki = home.path().join("wiki");
    migrate::sqlite(&database)?;
    std::fs::create_dir_all(wiki.join("topics"))?;
    std::fs::write(wiki.join("topics/acmekit.md"), "test page")?;
    let environment = support::environment(&home)
        .with_override(
            "RIVETHUB_CONFIG",
            home.path().join("missing.yaml").display().to_string(),
        )
        .with_override("WIKI_DIR", wiki.display().to_string())
        .with_override("RIVETHUB_PG_URL", "postgres://127.0.0.1:1/closed");
    let file = database.display().to_string();
    let queued = support::run(
        &[
            "memory",
            "recompile-wiki",
            "--since",
            "2026-10-01T00:00:00Z",
            "--slug",
            "acmekit",
            "--sqlite",
            &file,
            "--json",
        ],
        &environment,
    )
    .await;
    assert_eq!(queued.exit, Exit::Ok, "{}", queued.stderr);
    assert_eq!(
        queued.stdout,
        "{\"queued\":true,\"pages\":[{\"slug\":\"acmekit\",\"bytes\":9}]}\n"
    );
    let queue = SqliteStore::open(&database)?;
    let selector = ClaimSelector::matching(queue.task_names(WorkerRole::WikiBuilder));
    let claimed = queue.claim(&selector).await?.ok_or("recompile job")?;
    assert_eq!(claimed.name, JobName::Fixed(FixedJob::RecompileWiki));
    Ok(())
}
