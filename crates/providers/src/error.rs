use std::time::Duration;

use kernel::model::ModelProviderError;
use reqwest::StatusCode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("provider type is not an OpenAI-compatible model server")]
    NotOpenaiCompatible,
    #[error("provider is missing \"model\"")]
    MissingModel,
    #[error("provider is missing \"base_url\"")]
    MissingBaseUrl,
    #[error("provider \"base_url\" is not a valid URL: {source}")]
    BaseUrl {
        #[source]
        source: url::ParseError,
    },
    #[error("provider \"base_url\" must be an http(s) URL")]
    BaseUrlScheme,
    #[error("the HTTP client could not be built: {source}")]
    HttpClient {
        #[source]
        source: reqwest::Error,
    },
    #[error("embeddings are missing \"embed_endpoint\"")]
    MissingEmbedEndpoint,
    #[error("embeddings are missing \"embed_model\"")]
    MissingEmbedModel,
    #[error("\"embed_expected_dims\" does not fit a vector length on this platform: {source}")]
    EmbedDimensions {
        #[source]
        source: std::num::TryFromIntError,
    },
    #[error("token_command must be a non-empty argv array")]
    TokenCommand,
    #[error("token_command argv entries must be non-empty strings")]
    TokenArg,
    #[error("token_ttl_ms must be a positive number")]
    TokenTtl,
    #[error("token_command_timeout_ms must be a positive number")]
    TokenTimeout,
    #[error("provider \"api_key_file\" {path} could not be read: {source}")]
    ApiKeyFile {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Error)]
pub enum TokenError {
    #[error("token_command failed to start: {source}")]
    Spawn {
        #[source]
        source: std::sync::Arc<std::io::Error>,
    },
    #[error("token_command stdout was unreadable: {source}")]
    Read {
        #[source]
        source: std::sync::Arc<std::io::Error>,
    },
    #[error("token_command could not be stopped: {source}")]
    Stop {
        #[source]
        source: std::sync::Arc<std::io::Error>,
    },
    #[error("token_command kill failed: {kill}; wait failed: {source}")]
    StopAfterKill {
        kill: std::sync::Arc<std::io::Error>,
        #[source]
        source: std::sync::Arc<std::io::Error>,
    },
    #[error("token_command produced no stdout")]
    EmptyStdout,
    #[error("token_command stdout must be a single line")]
    NotOneLine,
    #[error("token_command exited {code}")]
    Exit { code: i32 },
    #[error("token_command failed")]
    Failed,
    #[error("token_command timed out")]
    TimedOut {
        #[source]
        source: std::sync::Arc<tokio::time::error::Elapsed>,
    },
    #[error("token_command stdout exceeded 64KiB")]
    TooLarge,
    #[error("token_command stdout is not UTF-8: {source}")]
    NotUtf8 {
        #[source]
        source: std::str::Utf8Error,
    },
}

#[derive(Debug, Error)]
pub enum StatusError {
    #[error("model server returned {status}: {body}")]
    Body {
        status: StatusCode,
        retry_after: Option<Duration>,
        body: String,
    },
    #[error("model server returned {status}, and the body was unreadable")]
    Unreadable {
        status: StatusCode,
        retry_after: Option<Duration>,
        #[source]
        source: reqwest::Error,
    },
}

impl StatusError {
    pub(crate) fn code(&self) -> u16 {
        match self {
            Self::Body { status, .. } | Self::Unreadable { status, .. } => status.as_u16(),
        }
    }

    pub(crate) fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Body { retry_after, .. } | Self::Unreadable { retry_after, .. } => *retry_after,
        }
    }

    pub(crate) fn into_failure(self) -> kernel::model::ModelCallFailure {
        kernel::model::ModelCallFailure::Http {
            status: self.code(),
            retry_after: self.retry_after(),
            source: Box::new(self),
        }
    }
}

#[derive(Debug, Error)]
pub enum StreamFault {
    #[error("the model server restarted tool call index {index}")]
    RestartedToolCall { index: u32 },
    #[error("the model server streamed arguments for unknown tool call {index}")]
    UnknownToolCall { index: u32 },
    #[error("the model server sent a tool call chunk without an index")]
    ToolCallWithoutIndex,
    #[error("the model server started a tool call without an id")]
    ToolCallWithoutId,
    #[error("the model stream ended before Done")]
    EndedBeforeDone,
}

#[derive(Debug, Error)]
pub enum Unsupported {
    #[error(
        "message content parts are not spoken to the model server: video URL splicing is out of \
         scope and no recording pins an image shape"
    )]
    ContentParts,
}

impl From<StatusError> for ModelProviderError {
    fn from(error: StatusError) -> Self {
        Self::Request {
            message: error.to_string(),
            source: Box::new(error.into_failure()),
        }
    }
}

impl From<TokenError> for ModelProviderError {
    fn from(error: TokenError) -> Self {
        Self::Request {
            message: error.to_string(),
            source: Box::new(kernel::model::ModelCallFailure::Credentials {
                source: Box::new(error),
            }),
        }
    }
}

impl From<StreamFault> for ModelProviderError {
    fn from(fault: StreamFault) -> Self {
        Self::Stream {
            message: fault.to_string(),
            source: Box::new(fault),
        }
    }
}

impl From<Unsupported> for ModelProviderError {
    fn from(error: Unsupported) -> Self {
        Self::Request {
            message: error.to_string(),
            source: Box::new(error),
        }
    }
}
