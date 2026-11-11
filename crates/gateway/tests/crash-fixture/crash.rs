use std::path::PathBuf;

use gateway::account::{CrashJob, install_crash_hook};

#[expect(
    clippy::panic,
    reason = "the fixture is a process that panics under the crash hook"
)]
fn main() {
    let mut args = std::env::args_os().skip(1);
    let state = args.next().map(PathBuf::from);
    let message = args.next().map(|text| text.to_string_lossy().into_owned());
    if let (Some(state), Some(message)) = (state, message) {
        install_crash_hook(&state, CrashJob::Cli);
        std::panic::panic_any(message);
    }
}
