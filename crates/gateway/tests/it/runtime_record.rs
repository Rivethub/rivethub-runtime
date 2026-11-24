use std::net::SocketAddr;

use gateway::runtime_record::{FILE_NAME, publish, read_live, record_path};

const DEAD_PID: u32 = 2_000_000_000;

fn bound() -> SocketAddr {
    SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), 49321)
}

#[test]
fn a_published_record_is_found_and_removed_on_drop() {
    let state = tempfile::tempdir().expect("state");
    let published = publish(state.path(), bound(), false).expect("publish");
    let record = read_live(state.path()).expect("live record");
    assert_eq!(record.port, 49321);
    assert_eq!(record.pid, std::process::id());
    assert!(!record.tls);
    drop(published);
    assert!(read_live(state.path()).is_none());
    assert!(!state.path().join(FILE_NAME).exists());
}

#[test]
fn a_record_of_a_dead_process_is_ignored() {
    let state = tempfile::tempdir().expect("state");
    let text = format!(
        r#"{{"pid":{DEAD_PID},"state_dir":{:?},"host":"127.0.0.1","port":49321,"tls":false}}"#,
        state.path()
    );
    std::fs::write(record_path(state.path()), text).expect("write");
    assert!(read_live(state.path()).is_none());
}

#[test]
fn a_record_copied_from_another_state_directory_is_ignored() {
    let origin = tempfile::tempdir().expect("origin");
    let other = tempfile::tempdir().expect("other");
    let _published = publish(origin.path(), bound(), false).expect("publish");
    std::fs::copy(record_path(origin.path()), record_path(other.path())).expect("copy");
    assert!(read_live(other.path()).is_none());
    assert!(read_live(origin.path()).is_some());
}

#[test]
fn dropping_never_removes_a_record_a_newer_gateway_wrote() {
    let state = tempfile::tempdir().expect("state");
    let published = publish(state.path(), bound(), false).expect("publish");
    let newer = format!(
        r#"{{"pid":{DEAD_PID},"state_dir":{:?},"host":"127.0.0.1","port":1,"tls":false}}"#,
        state.path()
    );
    std::fs::write(record_path(state.path()), newer).expect("write");
    drop(published);
    assert!(state.path().join(FILE_NAME).exists());
}
