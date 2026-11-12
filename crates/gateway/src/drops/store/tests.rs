use std::os::unix::fs::PermissionsExt as _;
use std::time::{Duration, Instant, SystemTime};

use chrono::{TimeDelta, Utc};

use super::{Claim, Part, Record, Store};
use crate::drops::{DropId, DropsConfig, OpenError, Rates};
use crate::tenancy::UserId;

fn config(root: std::path::PathBuf) -> DropsConfig {
    DropsConfig {
        root,
        max_bytes: 64 << 20,
        per_user_drops: 50,
        per_user_bytes: 512 << 20,
        ttl: TimeDelta::days(7),
        rates: Rates::default(),
    }
}

fn plant(root: &std::path::Path, expires_in: TimeDelta) -> DropId {
    let id = DropId::generate().expect("random source");
    let body = b"age-encryption.org/v1\nsealed";
    let record = Record {
        owner: "alice".to_owned(),
        bytes: body.len() as u64,
        sha256: hex::encode(<sha2::Sha256 as sha2::Digest>::digest(body)),
        created_at: Utc::now(),
        expires_at: Utc::now() + expires_in,
    };
    let name = |part: Part| root.join(format!("{}{}", id.as_str(), part.suffix()));
    std::fs::write(name(Part::Body), body).expect("body");
    std::fs::write(
        name(Part::Record),
        serde_json::to_vec(&record).expect("record"),
    )
    .expect("record file");
    id
}

fn names(root: &std::path::Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(root)
        .expect("root")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    found.sort();
    found
}

#[test]
fn ten_thousand_expired_drops_are_swept_within_ten_seconds() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path().join("drops");
    std::fs::create_dir(&root).expect("root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
    for _ in 0..10_000 {
        plant(&root, TimeDelta::seconds(-1));
    }
    let kept = plant(&root, TimeDelta::days(1));
    let store = Store::open(&config(root.clone())).expect("open");
    let started = Instant::now();
    let swept = store.sweep(Duration::from_secs(3600)).expect("sweep");
    let took = started.elapsed();
    assert_eq!(swept, 10_000);
    assert!(took < Duration::from_secs(10), "sweep took {took:?}");
    assert_eq!(
        names(&root),
        vec![
            format!("{}.age", kept.as_str()),
            format!("{}.json", kept.as_str())
        ]
    );
}

#[test]
fn old_files_with_no_drop_are_swept_and_recent_ones_kept() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path().join("drops");
    let store = Store::open(&config(root.clone())).expect("open");
    let old = DropId::generate().expect("random source");
    let recent = DropId::generate().expect("random source");
    let hour_ago = SystemTime::now() - Duration::from_secs(3601);
    for part in [Part::Body, Part::Sending, Part::RecordDraft] {
        let path = root.join(format!("{}{}", old.as_str(), part.suffix()));
        let file = std::fs::File::create(&path).expect("orphan");
        file.set_modified(hour_ago).expect("mtime");
    }
    std::fs::write(root.join(format!("{}.age", recent.as_str())), b"x").expect("recent");
    std::fs::write(root.join("unrelated.txt"), b"x").expect("unrelated");
    assert_eq!(store.sweep(Duration::from_secs(3600)).expect("sweep"), 3);
    let mut kept = vec![
        format!("{}.age", recent.as_str()),
        "unrelated.txt".to_owned(),
    ];
    kept.sort();
    assert_eq!(names(&root), kept);
}

#[test]
fn a_claim_cut_by_a_crash_is_released_at_start() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path().join("drops");
    std::fs::create_dir(&root).expect("root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
    let id = plant(&root, TimeDelta::days(1));
    std::fs::rename(
        root.join(format!("{}.age", id.as_str())),
        root.join(format!("{}.sending", id.as_str())),
    )
    .expect("claimed");
    let store = Store::open(&config(root)).expect("open");
    assert!(matches!(
        store.claim(&id),
        super::Claim::Ready { bytes: 28, .. }
    ));
    assert!(matches!(store.claim(&id), super::Claim::Gone));
}

#[test]
fn an_unsafe_root_is_refused_at_start() {
    let dir = tempfile::tempdir().expect("temp");
    let open = dir.path().join("open");
    std::fs::create_dir(&open).expect("root");
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o750)).expect("mode");
    assert!(matches!(
        Store::open(&config(open)),
        Err(OpenError::Unsafe { rule, .. }) if rule.contains("0700")
    ));
    let real = dir.path().join("real");
    let link = dir.path().join("link");
    std::fs::create_dir(&real).expect("real");
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).expect("mode");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    assert!(matches!(
        Store::open(&config(link)),
        Err(OpenError::Unsafe { rule, .. }) if rule.contains("symlink")
    ));
    let made = dir.path().join("made");
    Store::open(&config(made.clone())).expect("created");
    let permissions = std::fs::metadata(&made).expect("made").permissions();
    assert_eq!(permissions.mode() & 0o777, 0o700);
}

fn private_root(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let root = dir.path().join("drops");
    std::fs::create_dir(&root).expect("root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
    root
}

fn alice() -> UserId {
    UserId::new("alice".to_owned()).expect("user id")
}

#[test]
fn a_revoke_during_a_download_that_fails_wins() {
    let dir = tempfile::tempdir().expect("temp");
    let root = private_root(&dir);
    let id = plant(&root, TimeDelta::days(1));
    let store = Store::open(&config(root.clone())).expect("open");
    assert!(
        store.take(&id).expect("claim"),
        "the download claims the drop"
    );
    assert!(
        store.revoke(&id, &alice()).expect("revoke"),
        "the owner's revoke of a claimed drop is answered 204"
    );
    store.put_back(&id);
    assert!(matches!(store.claim(&id), Claim::Gone));
    assert_eq!(names(&root), Vec::<String>::new());
    assert_eq!(store.index.lock().usage(&alice()), (0, 0));
}

#[test]
fn a_revoke_is_acknowledged_only_after_the_root_is_synced() {
    let dir = tempfile::tempdir().expect("temp");
    let root = private_root(&dir);
    let id = plant(&root, TimeDelta::days(1));
    let store = Store::open(&config(root.clone())).expect("open");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o300)).expect("mode");
    let revoked = store.revoke(&id, &alice());
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
    assert!(
        revoked.is_err(),
        "a root that cannot be opened for fsync fails the revoke"
    );
    assert_eq!(names(&root), Vec::<String>::new());
    assert!(matches!(store.claim(&id), Claim::Gone));
}

#[test]
fn an_expiry_that_loses_the_claim_leaves_the_download_alone() {
    let dir = tempfile::tempdir().expect("temp");
    let root = private_root(&dir);
    let id = plant(&root, TimeDelta::seconds(-1));
    let store = Store::open(&config(root.clone())).expect("open");
    assert!(
        store.take(&id).expect("claim"),
        "the download claims the drop"
    );
    assert_eq!(store.sweep(Duration::from_secs(3600)).expect("sweep"), 0);
    assert_eq!(
        names(&root),
        vec![
            format!("{}.json", id.as_str()),
            format!("{}.sending", id.as_str())
        ]
    );
    assert_eq!(store.index.lock().usage(&alice()), (1, 28));
}

#[test]
fn a_record_without_its_body_is_deleted_at_start() {
    let dir = tempfile::tempdir().expect("temp");
    let root = private_root(&dir);
    let id = plant(&root, TimeDelta::days(1));
    std::fs::remove_file(root.join(format!("{}.age", id.as_str()))).expect("burned body");
    let store = Store::open(&config(root.clone())).expect("open");
    assert_eq!(names(&root), Vec::<String>::new());
    assert_eq!(store.index.lock().usage(&alice()), (0, 0));
}
