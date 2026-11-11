use crate::Exit;
use crate::service::StartupError;

impl StartupError {
    #[must_use]
    pub fn exit(&self) -> Exit {
        match self {
            Self::Config(_)
            | Self::Node(_)
            | Self::Composition(_)
            | Self::SqliteUnavailable
            | Self::Settings(_)
            | Self::Url
            | Self::State(_)
            | Self::StateTask(_)
            | Self::EmbedModel
            | Self::SqliteEmbedModel
            | Self::EmbedProvider(_) => Exit::Failure,
            Self::Memory(_) | Self::Close(_) => Exit::Memory,
            Self::Bind(_) => Exit::Bind,
            Self::Serve(_) | Self::Io(_) => Exit::Serve,
        }
    }
}
