#[expect(
    dead_code,
    reason = "integration binaries share fixtures and use different subsets"
)]
mod common;
mod search_support;

mod bundle;
mod capture_rules;
mod close;
mod embed_store;
mod import;
mod local_ports;
mod open;
mod queue_port;
mod reembed;
mod replay_capture;
mod replay_context;
mod replay_hybrid;
mod replay_jobs;
mod replay_read;
mod replay_search;
mod replay_suggest;
mod replay_summary;
mod replay_tags;
mod replay_wiki;
mod search_filters;
mod search_modes;
mod search_parity;
mod session_delete;
mod summarizer_health;
mod summary_dag;
mod summary_title;
mod topics;
mod unembedded_sweep;
mod wiki_consolidate;
mod wiki_routes;
