#[expect(
    dead_code,
    reason = "the replay binary shares this harness, and only its fixture reads the roster recordings"
)]
mod common;
#[expect(
    clippy::expect_used,
    reason = "helpers sit outside test functions, the only place clippy.toml exempts expect"
)]
#[expect(
    clippy::panic,
    reason = "a helper that cannot build the fixture stops the test"
)]
#[expect(
    unreachable_pub,
    reason = "the harness is a private module of the test binary"
)]
mod device_support;
#[cfg(test)]
mod session_support;

mod agents;
mod auth;
mod ca;
mod capture;
mod capture_owner;
mod claude_trust;
mod devices;
mod extract;
mod files;
mod harness_registry;
mod harness_setup;
mod harnesses;
mod hub_state;
mod memory_api;
mod mesh_channel;
mod mesh_registry;
mod mesh_replay;
mod owner_only;
mod parity;
mod parity_play;
mod phone;
mod plugin_creator;
mod plugin_security;
mod plugin_support;
mod plugins;
mod remote_client_auth;
mod server;
mod serving;
mod session_catalog;
mod session_delete;
mod session_history;
mod session_owners;
mod session_replay;
mod sessions;
mod store_error;
mod surface;
mod task_runner;
mod tasks_http;
mod term_agents;
mod term_owner_device;
mod tls_resolve;
mod today;
mod updates;
mod upgrade;
mod voice;
mod wiki;
