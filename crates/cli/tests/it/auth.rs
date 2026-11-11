use std::sync::Arc;

use cli::Exit;
use gateway::local_owner::{OwnerToken, token_path};

use crate::support::{config, environment, home, install_owner_token, run};

type Seen = Arc<parking_lot::Mutex<Vec<(String, String, Option<String>)>>>;

async fn stub(
    status: axum::http::StatusCode,
) -> Result<(u16, Seen, tokio::task::JoinHandle<()>), std::io::Error> {
    let seen = Seen::default();
    let recorder = Arc::clone(&seen);
    let app = axum::Router::new().fallback(
        move |method: axum::http::Method, uri: axum::http::Uri, headers: axum::http::HeaderMap| {
            let recorder = Arc::clone(&recorder);
            async move {
                recorder.lock().push((
                    method.to_string(),
                    uri.path().to_owned(),
                    headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned),
                ));
                status
            }
        },
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        drop(axum::serve(listener, app).await);
    });
    Ok((port, seen, server))
}

#[tokio::test]
async fn token_path_prints_the_path_as_text_and_json() -> Result<(), Box<dyn std::error::Error>> {
    let home = home();
    let env = environment(&home);
    let expected = token_path(&gateway::state_dir(&env)?);
    let text = run(&["auth", "token-path"], &env).await;
    assert_eq!(text.exit, Exit::Ok);
    assert_eq!(text.stdout, format!("{}\n", expected.display()));
    let json = run(&["auth", "token-path", "--json"], &env).await;
    let value: serde_json::Value = serde_json::from_str(&json.stdout)?;
    assert_eq!(
        value,
        serde_json::json!({ "path": expected.display().to_string() })
    );
    Ok(())
}

#[tokio::test]
async fn rotate_posts_to_the_gateway_with_the_bearer() -> Result<(), Box<dyn std::error::Error>> {
    let (port, seen, server) = stub(axum::http::StatusCode::OK).await?;
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    let token = OwnerToken::generate()?;
    install_owner_token(&env, &token);
    let report = run(&["auth", "rotate"], &env).await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(
        seen.lock().as_slice(),
        [(
            "POST".to_owned(),
            "/api/auth/rotate".to_owned(),
            Some(format!("Bearer {}", token.expose()))
        )]
    );
    server.abort();
    Ok(())
}

#[tokio::test]
async fn a_refused_rotation_keeps_the_file_and_never_prints_the_token()
-> Result<(), Box<dyn std::error::Error>> {
    let (port, _, server) = stub(axum::http::StatusCode::UNAUTHORIZED).await?;
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    let token = OwnerToken::generate()?;
    install_owner_token(&env, &token);
    let report = run(&["auth", "rotate"], &env).await;
    assert_ne!(report.exit, Exit::Ok);
    assert!(report.stderr.contains("401"), "{}", report.stderr);
    assert!(!report.stderr.contains(token.expose()));
    assert!(!report.stdout.contains(token.expose()));
    assert!(token_path(&gateway::state_dir(&env)?).exists());
    server.abort();
    Ok(())
}

#[tokio::test]
async fn rotate_with_the_gateway_down_deletes_the_token_file()
-> Result<(), Box<dyn std::error::Error>> {
    let port = testkit::ports::claim()?;
    let home = home();
    let env = environment(&home);
    config(
        &home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
    let token = OwnerToken::generate()?;
    install_owner_token(&env, &token);
    let report = run(&["auth", "rotate"], &env).await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(!token_path(&gateway::state_dir(&env)?).exists());
    Ok(())
}
