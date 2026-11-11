pub mod adapter;
pub mod client;
pub mod connection;
mod context;
mod discovery;
pub mod error;
pub mod events;
mod headless;
pub mod identity;
pub mod launch;
mod options;
pub mod permissions;
pub mod process;
mod prompt;
mod sheets;
mod supervised;

pub use adapter::AcpHarnessAdapter;
pub use connection::initialize_handshake;
pub use discovery::DEFAULT_DISCOVERY_TIMEOUT;
pub use error::AcpError;
pub use error::ChatUnsupportedReason;
pub use error::UnverifiedReason;
pub use identity::IdentityError;
pub use identity::NativeSession;
pub use launch::AcpCommand;
pub use launch::AcpLaunch;
pub use launch::LaunchFact;
pub use launch::LaunchTable;
pub use supervised::HeadlessHarness;
pub use supervised::HeadlessLaunch;
pub use supervised::Retry;
pub use supervised::Supervisor;

#[cfg(test)]
mod event_tests;
