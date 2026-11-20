use std::time::Duration;

use config::Config;
use config::model::PostgresSection;

use crate::error::WorkerError;

const EMBEDDING_WIDTH: u32 = 1024;
const CHARS_PER_CHUNK: u32 = 6_000;
const TOOL_RESULT_CAP: u32 = 32_768;
const MAX_NULL_FAILURES: u32 = 3;
const EMBED_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbedSettings {
    pub width: u32,
    pub chars_per_chunk: u32,
    pub tool_result_cap: u32,
    pub max_failures: u32,
    pub chunks_enabled: bool,
    pub timeout: Duration,
}

impl EmbedSettings {
    #[must_use]
    pub fn standard() -> Self {
        Self {
            width: EMBEDDING_WIDTH,
            chars_per_chunk: CHARS_PER_CHUNK,
            tool_result_cap: TOOL_RESULT_CAP,
            max_failures: MAX_NULL_FAILURES,
            chunks_enabled: true,
            timeout: EMBED_TIMEOUT,
        }
    }

    pub fn from_config(config: &Config) -> Result<Self, WorkerError> {
        let mut settings = Self::standard();
        if let Some(width) = postgres_width(config) {
            if width == 0 {
                return Err(WorkerError::Width);
            }
            let expected = kernel::memory::summary_backend::SUMMARY_EMBEDDING_WIDTH;
            if !matches!(usize::try_from(width), Ok(got) if got == expected) {
                return Err(WorkerError::EmbedWidth {
                    got: width,
                    expected,
                });
            }
        }
        if let Some(chars) = postgres_section(config).and_then(|postgres| postgres.chars_per_chunk)
        {
            if chars == 0 {
                return Err(WorkerError::ChunkWindow);
            }
            settings.chars_per_chunk = chars;
        }
        if config.sqlite_selected() {
            settings.chunks_enabled = false;
        }
        Ok(settings)
    }

    #[must_use]
    pub fn with_width(mut self, width: u32) -> Self {
        self.width = width;
        self
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub fn with_chunks_enabled(mut self, enabled: bool) -> Self {
        self.chunks_enabled = enabled;
        self
    }
}

fn postgres_width(config: &Config) -> Option<u32> {
    postgres_section(config).and_then(|postgres| postgres.embed_expected_dims)
}

fn postgres_section(config: &Config) -> Option<&PostgresSection> {
    config
        .memory
        .as_ref()
        .and_then(|memory| memory.postgres.as_ref())
}
