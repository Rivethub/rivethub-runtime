#![cfg(test)]
use crate::session_support;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use gateway::sessions::{SessionRegistry, owners_file};
use session_support::{Fixture, http, user};
use wire::gateway::GatewayError;

struct Row {
    id: &'static str,
    parent: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![Row {
        id: "c",
        parent: Some("p"),
    }]
}

fn ids(rows: &[Row]) -> Vec<&'static str> {
    rows.iter().map(|row| row.id).collect()
}

fn state() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("state directory");
    let path = owners_file(directory.path());
    std::fs::create_dir_all(path.parent().expect("owners directory")).expect("owners directory");
    (directory, path)
}

fn registry(path: &Path) -> SessionRegistry {
    SessionRegistry::default().with_owners_file(path.to_path_buf())
}

fn phil_node(path: &Path) -> SessionRegistry {
    registry(path).with_node_owner(user("phil"))
}

fn stored(path: &Path) -> BTreeMap<String, String> {
    serde_json::from_slice(&std::fs::read(path).expect("owners file")).expect("owners json")
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

fn error_text(body: &[u8]) -> String {
    serde_json::from_slice::<GatewayError>(body)
        .expect("error body")
        .error
}

#[test]
fn the_owners_file_lives_in_the_state_directory() {
    assert_eq!(
        owners_file(Path::new("/state")),
        PathBuf::from("/state/den/session-owners.json")
    );
}

#[test]
fn a_registry_built_from_paths_reads_the_installed_owners_file() {
    let home = tempfile::tempdir().expect("home");
    let installed = home.path().join(".rivethub").join("den");
    std::fs::create_dir_all(&installed).expect("state directory");
    std::fs::write(
        installed.join("session-owners.json"),
        r#"{"claude-code:aaa":"phil","bbb":"coco"}"#,
    )
    .expect("seed");
    let env =
        config::Environment::from_vars([(std::ffi::OsStr::new("HOME"), home.path().as_os_str())]);
    let paths = config::Paths::new(&env).expect("paths");
    let registry = registry(&paths.session_owners_file()).with_node_owner(user("phil"));
    assert_eq!(registry.owner("aaa").expect("key"), Some(user("phil")));
    assert_eq!(registry.owner("bbb").expect("key"), Some(user("coco")));
    assert!(!registry.visible(&user("phil"), "bbb", None).expect("key"));
    assert!(registry.visible(&user("coco"), "bbb", None).expect("key"));
}

#[test]
fn a1_the_canonical_key_wins_over_the_bare_key() {
    let (_directory, path) = state();
    std::fs::write(&path, r#"{"claude-code:aaa":"phil","aaa":"other"}"#).expect("seed");
    let registry = registry(&path);
    assert_eq!(registry.owner("aaa").expect("key"), Some(user("phil")));
    assert_eq!(
        registry.owner("claude-code:aaa").expect("key"),
        Some(user("phil"))
    );
}

#[test]
fn keys_fold_to_native_ids_and_unfoldable_keys_round_trip() {
    let (_directory, path) = state();
    std::fs::write(
        &path,
        r#"{"claude-code:-home-x/3f2c9a10-1b2c-4d5e-8f90-a1b2c3d4e5f6":"phil","grok-build:g1":"other","odd/key":"ann","empty":"","number":7,"":"phil"}"#,
    )
    .expect("seed");
    let registry = registry(&path);
    assert_eq!(
        registry
            .owner("3f2c9a10-1b2c-4d5e-8f90-a1b2c3d4e5f6")
            .expect("key"),
        Some(user("phil"))
    );
    assert_eq!(registry.owner("g1").expect("key"), Some(user("other")));
    assert_eq!(registry.owner("empty").expect("key"), None);
    assert_eq!(registry.owner("number").expect("key"), None);
    registry.set("n", &user("ann")).expect("set");
    assert_eq!(
        stored(&path),
        BTreeMap::from([
            (
                "3f2c9a10-1b2c-4d5e-8f90-a1b2c3d4e5f6".to_owned(),
                "phil".to_owned()
            ),
            ("g1".to_owned(), "other".to_owned()),
            ("n".to_owned(), "ann".to_owned()),
            ("odd/key".to_owned(), "ann".to_owned()),
        ])
    );
}

#[tokio::test]
async fn a2_untagged_sessions_are_visible_to_the_node_owner_only() {
    let (_directory, path) = state();
    let registry = phil_node(&path);
    assert!(registry.visible(&user("phil"), "aaa", None).expect("key"));
    assert!(!registry.visible(&user("other"), "aaa", None).expect("key"));
    let fixture = Fixture::with_registry(registry_for_fixture(&path));
    let (status, body) = http(
        &fixture.gateway,
        "GET",
        &format!(
            "/api/harness-sessions/{}/transcript",
            wire::session_id::encode_segment(&"claude-code:aaa".parse().expect("session id"))
        ),
        Vec::new(),
        Some("alice-test-token"),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(error_text(&body), "session is owned by another user");
    let (status, body) = http(
        &fixture.gateway,
        "GET",
        &format!(
            "/api/harness-sessions/{}/transcript",
            wire::session_id::encode_segment(&"claude-code:aaa".parse().expect("session id"))
        ),
        Vec::new(),
        None,
    )
    .await;
    assert_eq!(status, 501);
    assert_eq!(error_text(&body), "claude-code has no transcript source");
    assert!(!path.exists());
}

fn registry_for_fixture(path: &Path) -> SessionRegistry {
    registry(path).with_node_owner(user("owner"))
}

#[test]
fn a3_a_child_row_inherits_its_tagged_parent() {
    let (_directory, path) = state();
    std::fs::write(&path, r#"{"p":"other"}"#).expect("seed");
    let registry = phil_node(&path);
    let listed = rows();
    registry.stamp_nested(&listed, |row| row.id, |row| row.parent);
    assert_eq!(stored(&path).get("c").map(String::as_str), Some("other"));
    let other = registry.filter(&user("other"), rows(), |row| row.id, |row| row.parent);
    assert_eq!(ids(&other), ["c"]);
    let phil = registry.filter(&user("phil"), rows(), |row| row.id, |row| row.parent);
    assert!(phil.is_empty());
}

#[test]
fn a4_a_child_of_an_untagged_parent_stays_with_the_node_owner() {
    let (_directory, path) = state();
    let registry = phil_node(&path);
    let listed = rows();
    registry.stamp_nested(&listed, |row| row.id, |row| row.parent);
    assert!(!path.exists());
    assert_eq!(registry.owner("c").expect("key"), None);
    let phil = registry.filter(&user("phil"), rows(), |row| row.id, |row| row.parent);
    assert_eq!(ids(&phil), ["c"]);
    let other = registry.filter(&user("other"), rows(), |row| row.id, |row| row.parent);
    assert!(other.is_empty());
    assert!(
        registry
            .visible(&user("phil"), "c", Some("p"))
            .expect("key")
    );
    assert!(
        !registry
            .visible(&user("other"), "c", Some("p"))
            .expect("key")
    );
}

#[test]
fn a5_inherit_never_overwrites() {
    let (_directory, path) = state();
    std::fs::write(&path, r#"{"c":"phil","p":"other"}"#).expect("seed");
    let registry = phil_node(&path);
    assert!(!registry.inherit("c", "p").expect("keys"));
    assert_eq!(registry.owner("c").expect("key"), Some(user("phil")));
    assert!(!registry.inherit("p", "p").expect("keys"));
    assert!(!registry.inherit("d", "untagged").expect("keys"));
    assert_eq!(registry.owner("d").expect("key"), None);
    assert!(registry.inherit("d", "claude-code:p").expect("keys"));
    assert_eq!(stored(&path).get("d").map(String::as_str), Some("other"));
}

#[test]
fn a6_the_first_resumer_claims_an_untagged_session() {
    let (_directory, path) = state();
    let registry = phil_node(&path);
    assert_eq!(registry.claim(&user("other"), "aaa").expect("claim"), "aaa");
    assert_eq!(stored(&path).get("aaa").map(String::as_str), Some("other"));
    assert!(!registry.visible(&user("phil"), "aaa", None).expect("key"));
    assert_eq!(
        registry
            .authorize_visible(&user("phil"), "aaa")
            .expect_err("forbidden")
            .to_string(),
        "session is owned by another user"
    );
    assert_eq!(
        registry
            .authorize_visible(&user("other"), "claude-code:aaa")
            .expect("visible"),
        "aaa"
    );
}

#[test]
fn a8_a_corrupt_file_starts_empty_and_is_rewritten_private() {
    let (_directory, path) = state();
    std::fs::write(&path, "{not json").expect("seed");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("mode");
    let registry = phil_node(&path);
    assert_eq!(registry.owner("aaa").expect("key"), None);
    registry.set("aaa", &user("phil")).expect("set");
    assert_eq!(
        std::fs::read_to_string(&path).expect("owners file"),
        "{\n  \"aaa\": \"phil\"\n}\n"
    );
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn a_non_object_file_starts_empty() {
    let (_directory, path) = state();
    std::fs::write(&path, r#"["aaa","phil"]"#).expect("seed");
    assert_eq!(registry(&path).owner("aaa").expect("key"), None);
}

#[test]
fn set_is_a_no_op_for_the_same_owner_and_creates_the_parent_directory() {
    let directory = tempfile::tempdir().expect("state directory");
    let path = owners_file(&directory.path().join("nested").join("state"));
    let registry = registry(&path);
    registry.set("aaa", &user("phil")).expect("set");
    assert_eq!(mode(&path), 0o600);
    std::fs::remove_file(&path).expect("remove");
    registry.set("aaa", &user("phil")).expect("set");
    assert!(!path.exists());
    registry.set("aaa", &user("other")).expect("set");
    assert_eq!(stored(&path).get("aaa").map(String::as_str), Some("other"));
    assert!(registry.tag_if_absent("bbb", &user("phil")).expect("tag"));
    assert!(!registry.tag_if_absent("bbb", &user("other")).expect("tag"));
    assert_eq!(stored(&path).get("bbb").map(String::as_str), Some("phil"));
}

#[test]
fn a9_owners_survive_a_new_registry_over_the_same_file() {
    let (_directory, path) = state();
    let first = phil_node(&path);
    first
        .claim(&user("other"), "claude-code:aaa")
        .expect("claim");
    first
        .seed_owner(&user("phil"), "grok-build:ggg")
        .expect("seed");
    first.set("sss", &user("ann")).expect("set");
    drop(first);
    let second = phil_node(&path);
    assert_eq!(second.owner("aaa").expect("key"), Some(user("other")));
    assert_eq!(second.owner("ggg").expect("key"), Some(user("phil")));
    assert_eq!(second.owner("sss").expect("key"), Some(user("ann")));
    assert_eq!(
        second
            .claim(&user("phil"), "aaa")
            .expect_err("forbidden")
            .to_string(),
        "session is owned by another user"
    );
}

#[test]
fn without_a_node_owner_untagged_sessions_are_visible_to_nobody_but_claimable() {
    let (_directory, path) = state();
    let registry = registry(&path);
    assert!(!registry.visible(&user("phil"), "aaa", None).expect("key"));
    assert!(
        !registry
            .visible(&user("phil"), "c", Some("p"))
            .expect("key")
    );
    let listed = registry.filter(&user("phil"), rows(), |row| row.id, |row| row.parent);
    assert!(listed.is_empty());
    registry.claim(&user("phil"), "aaa").expect("claim");
    assert!(registry.visible(&user("phil"), "aaa", None).expect("key"));
}
