use std::net::IpAddr;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    StepCa(#[from] stepca::Error),
    #[error("blocking command task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    #[error("certificate authority file is not a regular file")]
    CaFile,
    #[error("certificate authority path escapes its directory")]
    CaEscape,
    #[error("certificate authority path contains traversal")]
    CaTraversal,
    #[error("certificate authority file is missing")]
    Missing,
    #[error("the operator roster cannot be mesh.json, because that registry is self-registered")]
    MeshRegistry,
    #[error("operator roster is required")]
    RosterRequired,
    #[error("operator roster line {line} is malformed")]
    RosterLine { line: usize },
    #[error("operator roster line {line} repeats node `{name}`")]
    RosterNode { line: usize, name: String },
    #[error("operator roster line {line} repeats agent `{name}`")]
    RosterAgent { line: usize, name: String },
    #[error("operator roster line {line} repeats address {address}")]
    RosterAddress { line: usize, address: IpAddr },
    #[error("node `{0}` is not in the operator roster")]
    UnknownNode(String),
    #[error("agent `{0}` is not in the operator roster")]
    UnknownAgent(String),
    #[error("device user and device must each be a single unreserved name")]
    DevicePrincipal,
    #[error("certificate serial is not a canonical decimal integer")]
    Serial,
    #[error("step ca {action} failed: {detail}")]
    StepFailed {
        action: &'static str,
        detail: String,
    },
    #[error("step ca returned an empty or invalid one-time token")]
    StepToken,
    #[error("step ca output is not valid UTF-8")]
    StepEncoding,
    #[error("step ca output exceeds 1 MiB")]
    StepOutput,
    #[error("step ca did not provide pipes")]
    StepPipe,
    #[error("command cancelled")]
    Cancelled,
    #[error("certificate authority leaf is not an X.509 certificate")]
    Certificate,
    #[error("certificate authority root is not one PEM certificate")]
    Root,
}
