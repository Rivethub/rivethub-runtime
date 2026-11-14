use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use chrono::Utc;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::mpsc;
use wire::plugin::PluginId;
use wire::plugin::host::{PluginLogLine, PluginLogStream};

const ROTATE_AT: u64 = 5 * 1024 * 1024;
const LOG_FILE: &str = "plugin.log";
const PREVIOUS_LOG_FILE: &str = "plugin.log.1";
pub(super) const DEFAULT_TAIL: usize = 200;
pub(super) const MAX_TAIL: usize = 2000;
const QUEUED_LINES: usize = 1024;
const REPORT_EVERY: u32 = 256;

#[derive(Debug, Clone)]
pub(super) struct LogSink {
    lines: mpsc::Sender<(PathBuf, PluginLogLine)>,
    dropped: Dropped,
}

type Dropped = Arc<Mutex<BTreeMap<PathBuf, u64>>>;

impl LogSink {
    pub(super) fn open(
        tasks: &tokio_util::task::TaskTracker,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self::with_capacity(tasks, shutdown, QUEUED_LINES)
    }

    fn with_capacity(
        tasks: &tokio_util::task::TaskTracker,
        shutdown: tokio_util::sync::CancellationToken,
        capacity: usize,
    ) -> Self {
        let (lines, received) = mpsc::channel(capacity);
        let dropped = Dropped::default();
        tasks.spawn(write_lines(received, Arc::clone(&dropped), shutdown));
        Self { lines, dropped }
    }

    pub(super) fn write(
        &self,
        folder: &Path,
        plugin: &PluginId,
        stream: PluginLogStream,
        text: String,
    ) {
        if stream == PluginLogStream::Host {
            tracing::info!(plugin = plugin.as_ref(), "{text}");
        }
        let line = PluginLogLine {
            at: Utc::now(),
            stream,
            text,
        };
        match self.lines.try_send((folder.to_path_buf(), line)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                *self.dropped.lock().entry(folder.to_path_buf()).or_default() += 1;
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::debug!(plugin = plugin.as_ref(), "plug-in log writer has stopped");
            }
        }
    }
}

struct OpenLog {
    file: tokio::fs::File,
    size: u64,
}

async fn write_lines(
    mut received: mpsc::Receiver<(PathBuf, PluginLogLine)>,
    dropped: Dropped,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let mut open: BTreeMap<PathBuf, OpenLog> = BTreeMap::new();
    let mut since_report = 0_u32;
    loop {
        let next = tokio::select! {
            biased;
            next = received.recv() => next,
            () = shutdown.cancelled() => {
                received.close();
                received.recv().await
            }
        };
        let Some((folder, line)) = next else {
            return;
        };
        written(&mut open, &folder, &line).await;
        since_report += 1;
        if received.is_empty() || since_report >= REPORT_EVERY {
            since_report = 0;
            report_dropped(&mut open, &dropped).await;
        }
    }
}

async fn report_dropped(open: &mut BTreeMap<PathBuf, OpenLog>, dropped: &Dropped) {
    let lost = std::mem::take(&mut *dropped.lock());
    for (folder, count) in lost {
        tracing::warn!(folder = %folder.display(), "{count} plug-in log lines dropped");
        let notice = PluginLogLine {
            at: Utc::now(),
            stream: PluginLogStream::Host,
            text: format!(
                "{count} lines dropped: the plug-in wrote faster than its log could be saved"
            ),
        };
        written(open, &folder, &notice).await;
    }
}

async fn written(open: &mut BTreeMap<PathBuf, OpenLog>, folder: &Path, line: &PluginLogLine) {
    if let Err(error) = append(open, folder, line).await {
        tracing::warn!(%error, folder = %folder.display(), "plug-in log line not written");
        open.remove(folder);
    }
}

async fn append(
    open: &mut BTreeMap<PathBuf, OpenLog>,
    folder: &Path,
    line: &PluginLogLine,
) -> Result<(), std::io::Error> {
    let mut bytes = serde_json::to_vec(line)?;
    bytes.push(b'\n');
    let rotate = open
        .get(folder)
        .is_some_and(|log| log.size + bytes.len() as u64 > ROTATE_AT);
    if rotate {
        open.remove(folder);
        tokio::fs::rename(folder.join(LOG_FILE), folder.join(PREVIOUS_LOG_FILE)).await?;
    }
    let log = match open.entry(folder.to_path_buf()) {
        std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::btree_map::Entry::Vacant(entry) => {
            tokio::fs::create_dir_all(folder).await?;
            let file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(folder.join(LOG_FILE))
                .await?;
            let size = file.metadata().await?.len();
            entry.insert(OpenLog { file, size })
        }
    };
    log.file.write_all(&bytes).await?;
    log.file.flush().await?;
    log.size += bytes.len() as u64;
    Ok(())
}

pub(super) async fn tail(
    folder: &Path,
    count: usize,
) -> Result<Vec<PluginLogLine>, std::io::Error> {
    let mut lines = Vec::new();
    for name in [PREVIOUS_LOG_FILE, LOG_FILE] {
        match tokio::fs::read_to_string(folder.join(name)).await {
            Ok(text) => {
                for raw in text.lines() {
                    match serde_json::from_str::<PluginLogLine>(raw) {
                        Ok(line) => lines.push(line),
                        Err(error) => {
                            tracing::debug!(%error, "unreadable plug-in log line skipped");
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let skip = lines.len().saturating_sub(count);
    Ok(lines.split_off(skip))
}

#[cfg(test)]
mod tests {
    use wire::plugin::PluginId;
    use wire::plugin::host::PluginLogStream;

    use super::{LogSink, tail};

    #[tokio::test]
    async fn lines_beyond_the_queue_are_counted_and_reported()
    -> Result<(), Box<dyn std::error::Error>> {
        let folder = tempfile::tempdir()?;
        let tasks = tokio_util::task::TaskTracker::new();
        let shutdown = tokio_util::sync::CancellationToken::new();
        let sink = LogSink::with_capacity(&tasks, shutdown.clone(), 8);
        let plugin = PluginId::try_from("noisy".to_owned())?;
        for index in 0..100 {
            sink.write(
                folder.path(),
                &plugin,
                PluginLogStream::Stdout,
                format!("line {index}"),
            );
        }
        sink.write(
            folder.path(),
            &plugin,
            PluginLogStream::Stdout,
            "after".to_owned(),
        );
        tokio::task::yield_now().await;
        sink.write(
            folder.path(),
            &plugin,
            PluginLogStream::Stdout,
            "last".to_owned(),
        );
        shutdown.cancel();
        tasks.close();
        tasks.wait().await;
        let lines: Vec<String> = tail(folder.path(), 1000)
            .await?
            .into_iter()
            .map(|line| line.text)
            .collect();
        assert_eq!(lines.first().map(String::as_str), Some("line 0"));
        assert!(
            lines.len() <= 10,
            "the queue held at most its capacity: {lines:?}"
        );
        assert!(lines.iter().any(|text| text == "last"), "{lines:?}");
        assert_eq!(
            lines.last().map(String::as_str),
            Some("93 lines dropped: the plug-in wrote faster than its log could be saved"),
            "{lines:?}"
        );
        Ok(())
    }
}
