use super::*;

fn argv(program: &str) -> Vec<String> {
    vec![program.to_owned()]
}

#[test]
fn stdout_keeps_one_trailing_newline_only() {
    assert_eq!(decode_token(b"ab c\n").expect("space kept"), "ab c");
    assert_eq!(decode_token(b"secret").expect("no newline"), "secret");
    assert_eq!(decode_token(b"secret\r\n").expect("crlf"), "secret");
    assert!(matches!(
        decode_token(b"secret\n\n"),
        Err(TokenError::NotOneLine)
    ));
    assert!(matches!(decode_token(b"\n"), Err(TokenError::EmptyStdout)));
    assert!(matches!(decode_token(b""), Err(TokenError::EmptyStdout)));
    assert!(matches!(
        decode_token(&[0xff]),
        Err(TokenError::NotUtf8 { .. })
    ));
    let shown = decode_token(b"secret\n\n")
        .expect_err("two lines")
        .to_string();
    assert!(!shown.contains("secret"));
}

#[test]
fn defaults_and_zero_refuse_to_build() {
    let source = TokenSource::new(&argv("/bin/true"), None, None).expect("defaults");
    assert_eq!(source.ttl, Duration::from_millis(DEFAULT_TTL_MS));
    assert_eq!(source.timeout, Duration::from_millis(DEFAULT_TIMEOUT_MS));
    assert!(matches!(
        TokenSource::new(&[] as &[String], None, None),
        Err(BuildError::TokenCommand)
    ));
    for hole in [
        vec![String::new()],
        vec!["/bin/true".to_owned(), String::new()],
    ] {
        assert!(matches!(
            TokenSource::new(&hole, None, None),
            Err(BuildError::TokenArg)
        ));
    }
    assert!(matches!(
        TokenSource::new(&argv("/bin/true"), Some(0), None),
        Err(BuildError::TokenTtl)
    ));
    assert!(matches!(
        TokenSource::new(&argv("/bin/true"), None, Some(0)),
        Err(BuildError::TokenTimeout)
    ));
}

#[test]
fn invalidate_drops_the_cache_and_bumps_the_generation() {
    let source = TokenSource::new(&argv("/bin/true"), None, None).expect("source");
    {
        let mut inner = source.inner.lock();
        inner.cache = Some(Cached {
            token: "held".to_owned(),
            expires: tokio::time::Instant::now() + Duration::from_secs(60),
        });
    }
    source.invalidate();
    let inner = source.inner.lock();
    assert!(inner.cache.is_none());
    assert_eq!(inner.generation, 1);
}

#[tokio::test(start_paused = true)]
async fn the_cached_token_expires_at_the_ttl() {
    let directory = scratch_dir();
    let log = directory.path().join("ttl");
    let script = format!(
        "printf 'x\\n' >> '{}'; n=$(grep -c x '{}'); printf 'tok-%s\\n' \"$n\"",
        log.display(),
        log.display()
    );
    let source = TokenSource::new(
        &["/bin/sh".to_owned(), "-c".to_owned(), script],
        Some(1_000),
        None,
    )
    .expect("source");
    let first = mint_while_time_is_paused(&source)
        .await
        .expect("first mint");
    let second = source.bearer().await.expect("cached");
    assert_eq!(first, "tok-1");
    assert_eq!(second, first);
    tokio::time::advance(Duration::from_millis(999)).await;
    assert_eq!(source.bearer().await.expect("still cached"), "tok-1");
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(
        mint_while_time_is_paused(&source).await.expect("refreshed"),
        "tok-2"
    );
}

fn jwt(exp: u64) -> String {
    use base64::Engine;
    let encode = |text: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text);
    format!(
        "{}.{}.{}",
        encode("{\"alg\":\"HS256\",\"typ\":\"JWT\"}"),
        encode(&format!("{{\"sub\":\"svc\",\"exp\":{exp}}}")),
        encode("signature")
    )
}

fn epoch_secs(at: SystemTime) -> u64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs()
}

#[test]
fn a_jwt_lives_until_thirty_seconds_before_its_exp() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    assert_eq!(
        jwt::lifetime(&jwt(1_003_600), now).expect("a JWT"),
        Duration::from_secs(3_570)
    );
    assert_eq!(
        jwt::lifetime(&jwt(1_000_020), now).expect("a JWT"),
        Duration::ZERO
    );
    assert_eq!(
        jwt::lifetime(&jwt(999_000), now).expect("a JWT"),
        Duration::ZERO
    );
    let ttl = Duration::from_millis(1_000);
    for opaque in ["opaque-secret", "a.b", "a.!!!.c", "a.e30.c", "a.b.c.d"] {
        assert!(jwt::lifetime(opaque, now).is_err(), "{opaque}");
        assert_eq!(cache_lifetime(opaque, ttl), ttl, "{opaque}");
    }
}

fn counting_source(directory: &tempfile::TempDir, token: &str) -> TokenSource {
    let log = directory.path().join("mints");
    let script = format!(
        "printf 'x\\n' >> '{log}'; n=$(grep -c x '{log}'); printf '%s\\n' \"{token}$n\"",
        log = log.display(),
    );
    TokenSource::new(
        &["/bin/sh".to_owned(), "-c".to_owned(), script],
        Some(1_000),
        None,
    )
    .expect("source")
}

#[tokio::test(start_paused = true)]
async fn a_jwt_bearer_is_cached_until_its_exp_not_the_ttl() {
    let directory = scratch_dir();
    let token = jwt(epoch_secs(SystemTime::now()) + 3_600);
    let source = counting_source(&directory, &token);
    let first = mint_while_time_is_paused(&source)
        .await
        .expect("first mint");
    assert_eq!(first, format!("{token}1"));
    tokio::time::advance(Duration::from_secs(60)).await;
    assert_eq!(source.bearer().await.expect("still cached"), first);
    tokio::time::advance(Duration::from_secs(3_600)).await;
    assert_eq!(
        mint_while_time_is_paused(&source).await.expect("refreshed"),
        format!("{token}2")
    );
}

#[tokio::test(start_paused = true)]
async fn a_jwt_inside_the_expiry_margin_is_not_cached() {
    let directory = scratch_dir();
    let token = jwt(epoch_secs(SystemTime::now()) + 10);
    let source = counting_source(&directory, &token);
    assert_eq!(
        mint_while_time_is_paused(&source).await.expect("first"),
        format!("{token}1")
    );
    assert_eq!(
        mint_while_time_is_paused(&source).await.expect("second"),
        format!("{token}2")
    );
}

#[tokio::test]
async fn a_jwt_exp_beyond_the_clock_range_is_not_cached() {
    let directory = scratch_dir();
    let token = jwt(u64::MAX);
    let source = counting_source(&directory, &token);
    assert_eq!(source.bearer().await.expect("first"), format!("{token}1"));
    assert_eq!(source.bearer().await.expect("second"), format!("{token}2"));
}

#[tokio::test]
async fn overlapping_callers_share_one_mint() {
    let directory = scratch_dir();
    let log = directory.path().join("log");
    let release = directory.path().join("release");
    let script = format!(
        "printf 'x\\n' >> '{log}'; while [ ! -f '{release}' ]; do sleep 0.01; done; printf 'winner\\n'",
        log = log.display(),
        release = release.display()
    );
    let source = Arc::new(
        TokenSource::new(
            &["/bin/sh".to_owned(), "-c".to_owned(), script],
            None,
            Some(5_000),
        )
        .expect("source"),
    );
    let first = tokio::spawn({
        let source = Arc::clone(&source);
        async move { source.bearer().await }
    });
    wait_for_line(&log).await;
    let second = tokio::spawn({
        let source = Arc::clone(&source);
        async move { source.bearer().await }
    });
    wait_until(|| source.waiting.load(Ordering::SeqCst) >= 1).await;
    assert_eq!(
        line_count(&log),
        1,
        "a second caller starts another command"
    );
    std::fs::write(&release, b"go").expect("release");
    let lead = tokio::time::timeout(Duration::from_secs(5), first)
        .await
        .expect("the lead finishes")
        .expect("the lead joins")
        .expect("the lead mints");
    let waited = tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .expect("the waiter finishes")
        .expect("the waiter joins")
        .expect("the waiter mints");
    assert_eq!(lead, "winner");
    assert_eq!(waited, lead);
    assert_eq!(line_count(&log), 1);
}

#[tokio::test]
async fn a_mint_started_before_invalidate_is_not_the_retry() {
    let directory = scratch_dir();
    let log = directory.path().join("log");
    let release = directory.path().join("release");
    let script = format!(
        "printf 'x\\n' >> '{log}'; n=$(grep -c x '{log}'); if [ \"$n\" -eq 1 ]; then while [ ! -f '{release}' ]; do sleep 0.01; done; printf 'stale\\n'; else printf 'fresh\\n'; fi",
        log = log.display(),
        release = release.display()
    );
    let source = Arc::new(
        TokenSource::new(
            &["/bin/sh".to_owned(), "-c".to_owned(), script],
            None,
            Some(5_000),
        )
        .expect("source"),
    );
    let lead = tokio::spawn({
        let source = Arc::clone(&source);
        async move { source.bearer().await }
    });
    wait_for_line(&log).await;
    source.invalidate();
    let retry = tokio::spawn({
        let source = Arc::clone(&source);
        async move { source.bearer().await }
    });
    wait_until(|| source.waiting.load(Ordering::SeqCst) >= 1).await;
    assert_eq!(
        line_count(&log),
        1,
        "the retry starts a command before the first mint ends"
    );
    std::fs::write(&release, b"go").expect("release");
    let stale = tokio::time::timeout(Duration::from_secs(5), lead)
        .await
        .expect("the first mint finishes")
        .expect("the first mint joins")
        .expect("the first mint returns");
    let fresh = tokio::time::timeout(Duration::from_secs(5), retry)
        .await
        .expect("the retry finishes")
        .expect("the retry joins")
        .expect("the retry mints");
    assert_eq!(stale, "stale");
    assert_eq!(fresh, "fresh");
    assert_eq!(line_count(&log), 2);
}

async fn mint_while_time_is_paused(source: &TokenSource) -> Result<String, TokenError> {
    let (send, recv) = std::sync::mpsc::channel();
    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let started = std::sync::Arc::clone(&running);
    let blocker = tokio::task::spawn_blocking(move || {
        started.store(true, std::sync::atomic::Ordering::SeqCst);
        let received = recv.recv();
        assert!(received.is_ok(), "the mint finished before the blocker");
    });
    while !running.load(std::sync::atomic::Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
    let result = source.bearer().await;
    assert!(send.send(()).is_ok(), "the blocker is gone");
    let joined = blocker.await;
    assert!(joined.is_ok(), "the blocker joins");
    result
}

fn scratch_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap_or_else(|error| panic!("a scratch directory: {error}"))
}

async fn wait_until(mut ready: impl FnMut() -> bool) {
    let waited = tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(waited.is_ok(), "the waiter did not join the mint");
}

async fn wait_for_line(path: &std::path::Path) {
    let waited = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match std::fs::read_to_string(path) {
                Ok(text) if !text.is_empty() => return,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("read {}: {error}", path.display()),
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(waited.is_ok(), "the command did not start");
}

fn line_count(path: &std::path::Path) -> usize {
    match std::fs::read_to_string(path) {
        Ok(text) => text.lines().count(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("read {}: {error}", path.display()),
    }
}
