use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicU64;
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::SystemTime;

use parking_lot::Mutex;
use tokio::io::AsyncReadExt;
use tokio::sync::watch;

use crate::error::BuildError;
use crate::error::TokenError;
use crate::token::jwt;

const DEFAULT_TTL_MS: u64 = 300_000;
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const STDOUT_CAP: usize = 64 * 1024;
const READ_CHUNK: usize = 8 * 1024;

pub(super) struct TokenSource {
    argv: Vec<String>,
    ttl: Duration,
    timeout: Duration,
    inner: Mutex<Inner>,
    #[cfg(test)]
    waiting: AtomicU64,
}

struct Inner {
    generation: u64,
    cache: Option<Cached>,
    flight: Option<Arc<Flight>>,
}

struct Cached {
    token: String,
    expires: tokio::time::Instant,
}

struct Flight {
    generation: u64,
    tx: watch::Sender<Option<Result<String, TokenError>>>,
    rx: watch::Receiver<Option<Result<String, TokenError>>>,
}

#[cfg_attr(dylint_lib = "rr_lints", expect(manualderive))]
impl std::fmt::Debug for TokenSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock();
        let generation = inner.generation;
        let cache = inner.cache.is_some();
        let flight = inner.flight.is_some();
        drop(inner);
        let mut debug = formatter.debug_struct("TokenSource");
        debug
            .field("argv", &self.argv.len())
            .field("ttl", &self.ttl)
            .field("timeout", &self.timeout)
            .field("generation", &generation)
            .field("cache", &cache)
            .field("flight", &flight);
        #[cfg(test)]
        debug.field("waiting", &self.waiting.load(Ordering::Relaxed));
        debug.finish()
    }
}

impl TokenSource {
    pub(super) fn new(
        argv: &[String],
        ttl_ms: Option<u64>,
        timeout_ms: Option<u64>,
    ) -> Result<Self, BuildError> {
        if argv.is_empty() {
            return Err(BuildError::TokenCommand);
        }
        if argv.iter().any(String::is_empty) {
            return Err(BuildError::TokenArg);
        }
        Ok(Self {
            argv: argv.to_vec(),
            ttl: millis(ttl_ms, DEFAULT_TTL_MS, BuildError::TokenTtl)?,
            timeout: millis(timeout_ms, DEFAULT_TIMEOUT_MS, BuildError::TokenTimeout)?,
            inner: Mutex::new(Inner {
                generation: 0,
                cache: None,
                flight: None,
            }),
            #[cfg(test)]
            waiting: AtomicU64::new(0),
        })
    }

    pub(super) async fn bearer(&self) -> Result<String, TokenError> {
        loop {
            match self.role() {
                Role::Ready(token) => return Ok(token),
                Role::Lead(flight) => return self.finish_flight(flight).await,
                Role::Wait(flight) => {
                    #[cfg(test)]
                    self.waiting.fetch_add(1, Ordering::SeqCst);
                    let result = wait_for(&flight).await;
                    let current = self.inner.lock().generation;
                    if flight.generation == current {
                        return result;
                    }
                }
            }
        }
    }

    pub(super) fn invalidate(&self) {
        let mut inner = self.inner.lock();
        inner.generation = inner.generation.wrapping_add(1);
        inner.cache = None;
    }

    fn role(&self) -> Role {
        let mut inner = self.inner.lock();
        if let Some(token) = fresh_token(&inner) {
            return Role::Ready(token);
        }
        if let Some(flight) = inner.flight.clone() {
            return Role::Wait(flight);
        }
        let (tx, rx) = watch::channel(None);
        let flight = Arc::new(Flight {
            generation: inner.generation,
            tx,
            rx,
        });
        inner.flight = Some(Arc::clone(&flight));
        drop(inner);
        Role::Lead(flight)
    }

    async fn finish_flight(&self, flight: Arc<Flight>) -> Result<String, TokenError> {
        let minted = mint(&self.argv, self.timeout).await;
        let mut inner = self.inner.lock();
        let published = match minted {
            Ok(token) => {
                if inner.generation == flight.generation
                    && let Some(expires) = cache_until(&token, self.ttl)
                {
                    inner.cache = Some(Cached {
                        token: token.clone(),
                        expires,
                    });
                }
                Ok(token)
            }
            Err(error) => Err(error),
        };
        flight
            .tx
            .send_modify(|slot| *slot = Some(published.clone()));
        if inner
            .flight
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &flight))
        {
            inner.flight = None;
        }
        drop(inner);
        published
    }
}

enum Role {
    Ready(String),
    Lead(Arc<Flight>),
    Wait(Arc<Flight>),
}

fn fresh_token(inner: &Inner) -> Option<String> {
    let cached = inner.cache.as_ref()?;
    if tokio::time::Instant::now() < cached.expires {
        Some(cached.token.clone())
    } else {
        None
    }
}

fn cache_until(token: &str, ttl: Duration) -> Option<tokio::time::Instant> {
    let lifetime = cache_lifetime(token, ttl);
    if lifetime.is_zero() {
        return None;
    }
    tokio::time::Instant::now().checked_add(lifetime)
}

fn cache_lifetime(token: &str, ttl: Duration) -> Duration {
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let lifetime = match jwt::lifetime(token, SystemTime::now()) {
        Ok(lifetime) => lifetime,
        Err(_) => ttl,
    };
    lifetime
}

async fn wait_for(flight: &Flight) -> Result<String, TokenError> {
    let mut rx = flight.rx.clone();
    loop {
        let ready = rx.borrow().clone();
        if let Some(result) = ready {
            return result;
        }
        if rx.changed().await.is_err() {
            return Err(TokenError::Failed);
        }
    }
}

fn millis(value: Option<u64>, default_ms: u64, zero: BuildError) -> Result<Duration, BuildError> {
    match value {
        None => Ok(Duration::from_millis(default_ms)),
        Some(0) => Err(zero),
        Some(ms) => Ok(Duration::from_millis(ms)),
    }
}

async fn mint(argv: &[String], timeout: Duration) -> Result<String, TokenError> {
    let mut child = spawn(argv)?;
    let Some(mut stdout) = child.stdout.take() else {
        return stopped(stop(&mut child).await, TokenError::Failed);
    };
    let deadline = tokio::time::Instant::now() + timeout;
    let read = read_with_deadline(&mut stdout, deadline).await;
    drop(stdout);
    let bytes = match read {
        Ok(bytes) => bytes,
        Err(error) => return stopped(stop(&mut child).await, error),
    };
    let status = match tokio::time::timeout_at(deadline, child.wait()).await {
        Ok(status) => status.map_err(|source| TokenError::Read {
            source: Arc::new(source),
        })?,
        Err(elapsed) => return stopped(stop(&mut child).await, timed_out(elapsed)),
    };
    if !status.success() {
        return Err(exit_error(status));
    }
    decode_token(&bytes)
}

fn spawn(argv: &[String]) -> Result<tokio::process::Child, TokenError> {
    let Some((program, rest)) = argv.split_first() else {
        return Err(TokenError::Failed);
    };
    let mut command = tokio::process::Command::new(program);
    command
        .args(rest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    command.spawn().map_err(|source| TokenError::Spawn {
        source: Arc::new(source),
    })
}

async fn read_with_deadline(
    stdout: &mut tokio::process::ChildStdout,
    deadline: tokio::time::Instant,
) -> Result<Vec<u8>, TokenError> {
    match tokio::time::timeout_at(deadline, read_capped(stdout, STDOUT_CAP)).await {
        Ok(result) => result,
        Err(elapsed) => Err(timed_out(elapsed)),
    }
}

async fn read_capped(
    stdout: &mut tokio::process::ChildStdout,
    cap: usize,
) -> Result<Vec<u8>, TokenError> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; READ_CHUNK];
    loop {
        let read = stdout
            .read(&mut chunk)
            .await
            .map_err(|source| TokenError::Read {
                source: Arc::new(source),
            })?;
        if read == 0 {
            return Ok(buf);
        }
        let Some(bytes) = chunk.get(..read) else {
            return Err(TokenError::Failed);
        };
        let Some(remaining) = cap.checked_sub(buf.len()) else {
            return Err(TokenError::TooLarge);
        };
        if bytes.len() > remaining {
            return Err(TokenError::TooLarge);
        }
        buf.extend_from_slice(bytes);
    }
}

async fn stop(child: &mut tokio::process::Child) -> Result<(), TokenError> {
    let kill = child.start_kill();
    let waited = child.wait().await;
    match (kill, waited) {
        (_, Ok(_)) => Ok(()),
        (Ok(()), Err(source)) => Err(TokenError::Stop {
            source: Arc::new(source),
        }),
        (Err(kill), Err(source)) => Err(TokenError::StopAfterKill {
            kill: Arc::new(kill),
            source: Arc::new(source),
        }),
    }
}

fn stopped(cleanup: Result<(), TokenError>, error: TokenError) -> Result<String, TokenError> {
    match cleanup {
        Ok(()) => Err(error),
        Err(error) => Err(error),
    }
}

fn timed_out(elapsed: tokio::time::error::Elapsed) -> TokenError {
    TokenError::TimedOut {
        source: Arc::new(elapsed),
    }
}

fn exit_error(status: std::process::ExitStatus) -> TokenError {
    match status.code() {
        Some(code) => TokenError::Exit { code },
        None => TokenError::Failed,
    }
}

fn decode_token(bytes: &[u8]) -> Result<String, TokenError> {
    let text = std::str::from_utf8(bytes).map_err(|source| TokenError::NotUtf8 { source })?;
    let stripped = strip_one_trailing_newline(text);
    if stripped.is_empty() {
        return Err(TokenError::EmptyStdout);
    }
    if stripped.contains('\n') || stripped.contains('\r') {
        return Err(TokenError::NotOneLine);
    }
    Ok(stripped.to_owned())
}

fn strip_one_trailing_newline(text: &str) -> &str {
    let Some(without_newline) = text.strip_suffix('\n') else {
        return text;
    };
    match without_newline.strip_suffix('\r') {
        Some(without_crlf) => without_crlf,
        None => without_newline,
    }
}

#[cfg(test)]
#[path = "flight_tests.rs"]
mod tests;
