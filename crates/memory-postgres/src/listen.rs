use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio_util::sync::CancellationToken;

const FLOOR: Duration = Duration::from_millis(200);
const CEILING: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
#[error("listener stopped")]
pub(crate) struct Stopped;

pub(crate) struct Listener {
    pool: PgPool,
    channels: &'static [&'static str],
    name: &'static str,
    cancel: CancellationToken,
    inner: Option<PgListener>,
    delay: Duration,
    down: bool,
}

impl Listener {
    pub(crate) async fn connect_resilient(
        pool: PgPool,
        channels: &'static [&'static str],
        name: &'static str,
        cancel: CancellationToken,
    ) -> Result<Self, sqlx::Error> {
        let mut listener = Self::pending(pool, channels, name, cancel);
        match open(&listener.pool, listener.channels).await {
            Ok(inner) => {
                listener.inner = Some(inner);
                Ok(listener)
            }
            Err(error) if crate::loss::unreachable(&error) => {
                listener.mark_down(&error);
                Ok(listener)
            }
            Err(error) => Err(error),
        }
    }

    fn pending(
        pool: PgPool,
        channels: &'static [&'static str],
        name: &'static str,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            pool,
            channels,
            name,
            cancel,
            inner: None,
            delay: FLOOR,
            down: false,
        }
    }

    pub(crate) async fn recv(&mut self) -> Result<(), Stopped> {
        self.next().await?;
        Ok(())
    }

    pub(crate) async fn next(&mut self) -> Result<Option<String>, Stopped> {
        loop {
            if self.cancel.is_cancelled() {
                return Err(Stopped);
            }
            if self.inner.is_none() {
                match open(&self.pool, self.channels).await {
                    Ok(listener) => {
                        self.inner = Some(listener);
                        self.delay = FLOOR;
                        if self.down {
                            self.mark_up();
                            return Ok(None);
                        }
                    }
                    Err(error) => {
                        self.mark_down(&error);
                        if !self.pause().await {
                            return Err(Stopped);
                        }
                        self.delay = next_delay(self.delay);
                    }
                }
                continue;
            }
            let cancel = self.cancel.clone();
            let incoming = match self.inner.as_mut() {
                Some(listener) => {
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => return Err(Stopped),
                        incoming = listener.try_recv() => incoming,
                    }
                }
                None => continue,
            };
            match incoming {
                Ok(Some(notification)) => return Ok(Some(notification.payload().to_owned())),
                Ok(None) => {
                    self.inner = None;
                    self.mark_closed();
                }
                Err(error) => {
                    self.inner = None;
                    self.mark_down(&error);
                }
            }
        }
    }

    fn mark_down(&mut self, error: &sqlx::Error) {
        if self.down {
            return;
        }
        self.down = true;
        tracing::warn!("{}: database unreachable; reconnecting: {error}", self.name);
    }

    fn mark_closed(&mut self) {
        if self.down {
            return;
        }
        self.down = true;
        tracing::warn!(
            "{}: database unreachable; reconnecting: the connection closed",
            self.name
        );
    }

    fn mark_up(&mut self) {
        if !self.down {
            return;
        }
        self.down = false;
        tracing::info!("{}: database connection restored", self.name);
    }

    async fn pause(&self) -> bool {
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => false,
            () = tokio::time::sleep(self.delay) => true,
        }
    }
}

fn next_delay(current: Duration) -> Duration {
    match current.checked_mul(2) {
        Some(doubled) => doubled.min(CEILING),
        None => CEILING,
    }
}

async fn open(pool: &PgPool, channels: &[&str]) -> Result<PgListener, sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.eager_reconnect(false);
    for channel in channels {
        listener.listen(channel).await?;
    }
    Ok(listener)
}
