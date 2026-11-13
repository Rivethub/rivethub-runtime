use std::path::Path;

use super::{TestResult, provider, start, stop, write_config};

const STAMP: &str = "2026-10-01T00:00:00.000Z";

#[tokio::test]
async fn local_folds_a_session_variant_topic_into_its_parent_and_redirects() -> TestResult {
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    let database = home.path().join(".rivethub").join("memory.sqlite");
    let wiki = home.path().join("wiki");
    std::fs::create_dir_all(&wiki)?;
    seed_topics(&database)?;
    write_config(
        home.path(),
        &provider(
            home.path(),
            &format!(
                "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    wiki_dir: {}\ngateway:\n  host: 127.0.0.1\n  port: {port}\n",
                database.display(),
                wiki.display()
            ),
        ),
    )?;
    let started = start(
        home.path(),
        &["local", "--no-lan"],
        &[
            "gateway listening on ",
            "wiki consolidate acmekit-deploys -> acmekit-deploys (+1)",
        ],
    )
    .await?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .default_headers(crate::owner::home_headers(home.path())?)
        .build()?;
    let response = client
        .get(format!(
            "http://127.0.0.1:{port}/api/wiki/acmekit-deploys-friday-rollout"
        ))
        .send()
        .await;
    let slugs = topic_slugs(&database);
    stop(started).await?;
    let response = response?;
    assert_eq!(response.status().as_u16(), 308);
    assert_eq!(
        response
            .headers()
            .get("location")
            .ok_or("redirect location")?
            .to_str()?,
        "/api/wiki/acmekit-deploys"
    );
    assert_eq!(slugs?, vec!["acmekit-deploys".to_owned()]);
    Ok(())
}

fn seed_topics(path: &Path) -> TestResult {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    migrate::sqlite(path)?;
    let conn = rusqlite::Connection::open(path)?;
    for (slug, title, state) in [
        (
            "acmekit-deploys",
            "AcmeKit deploys",
            "AcmeKit deploys on Fridays.",
        ),
        (
            "acmekit-deploys-friday-rollout",
            "Friday rollout",
            "The Friday rollout ran.",
        ),
    ] {
        conn.execute(
            "INSERT INTO ros_wiki_topics (slug, title, current_state, search_text, created_at,
                updated_at) VALUES (?1, ?2, ?3, ?3, ?4, ?4)",
            (slug, title, state, STAMP),
        )?;
    }
    Ok(())
}

fn topic_slugs(path: &Path) -> TestResult<Vec<String>> {
    let conn = rusqlite::Connection::open(path)?;
    let mut statement = conn.prepare("SELECT slug FROM ros_wiki_topics ORDER BY slug")?;
    let slugs = statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(slugs)
}
