use jiff::Timestamp;
use std::path::PathBuf;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::AsyncWriteExt as _;

use crate::Error;

#[derive(Debug, Clone)]
pub(super) struct Book {
    dir: PathBuf,
}

impl Book {
    pub(super) async fn open(dir: PathBuf) -> Result<Self, Error> {
        tokio::fs::create_dir_all(&dir).await?;
        Ok(Self { dir })
    }

    pub(super) fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub(super) async fn read<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>, Error> {
        match tokio::fs::read(self.path(name)).await {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) async fn write<T: Serialize>(&self, name: &str, value: &T) -> Result<(), Error> {
        self.write_text(name, &serde_json::to_string_pretty(value)?)
            .await
    }

    pub(super) async fn write_text(&self, name: &str, text: &str) -> Result<(), Error> {
        let partial = self.path(&format!("{name}.partial"));
        tokio::fs::write(&partial, text).await?;
        tokio::fs::rename(partial, self.path(name)).await?;
        Ok(())
    }

    pub(super) async fn lines<T: DeserializeOwned>(&self, name: &str) -> Result<Vec<T>, Error> {
        let text = match tokio::fs::read_to_string(self.path(name)).await {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(Error::from))
            .collect()
    }

    pub(super) async fn append<T: Serialize>(&self, name: &str, values: &[T]) -> Result<(), Error> {
        let mut text = String::new();
        for value in values {
            text.push_str(&serde_json::to_string(value)?);
            text.push('\n');
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path(name))
            .await?;
        file.write_all(text.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }

    pub(super) async fn forget(&self, names: &[&str]) -> Result<(), Error> {
        for name in names {
            match tokio::fs::remove_file(self.path(name)).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub(super) async fn note(&self, line: &str) -> Result<(), Error> {
        let stamped = format!(
            "{} {line}\n",
            Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ")
        );
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path("progress.log"))
            .await?;
        file.write_all(stamped.as_bytes()).await?;
        Ok(())
    }
}
