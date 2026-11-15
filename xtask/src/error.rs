use std::fmt;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Finding(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("Rust parse: {0}")]
    Rust(#[from] syn::Error),
    #[error("toml serialization: {0}")]
    TomlWrite(#[from] toml::ser::Error),
    #[error("regex: {0}")]
    Regex(#[from] regex::Error),
    #[error("cargo metadata: {0}")]
    Metadata(#[from] cargo_metadata::Error),
    #[error("command {command} failed: {stderr}")]
    Command { command: String, stderr: String },
    #[error("{tool} is missing; install it with `{install}`")]
    MissingTool { tool: String, install: String },
    #[error("{0}")]
    Usage(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn finding<T>(message: impl fmt::Display) -> Result<T> {
    Err(Error::Finding(message.to_string()))
}
