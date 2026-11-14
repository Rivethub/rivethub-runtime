use std::fs;
use std::path::{Path, PathBuf};

use config::Environment;

fn temp_home(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rivethub-state-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn env_for(home: &Path) -> Result<Environment, Box<dyn std::error::Error>> {
    let text = home.to_str().ok_or("temp home is not utf-8")?;
    Ok(Environment::from_vars([(
        "HOME".to_owned(),
        text.to_owned(),
    )]))
}

#[test]
fn legacy_state_moves_once_and_later_reads_use_the_new_home()
-> Result<(), Box<dyn std::error::Error>> {
    let home = temp_home("move")?;
    let outcome = legacy_state_moves(&home);
    fs::remove_dir_all(&home)?;
    outcome
}

fn legacy_state_moves(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let legacy = home.join(".rivetos");
    fs::create_dir_all(legacy.join("nested"))?;
    fs::write(legacy.join("config.yaml"), "runtime: {}\n")?;
    fs::write(legacy.join("nested").join("marker"), "kept")?;

    let moved = config::adopt_legacy_state(&env_for(home)?)?;
    let notice = moved.ok_or("the first start moves the directory")?;
    assert!(notice.contains(".rivetos"), "{notice}");
    assert!(notice.contains(".rivethub"), "{notice}");
    assert!(home.join(".rivethub/config.yaml").is_file());
    assert_eq!(
        fs::read_to_string(home.join(".rivethub/nested/marker"))?,
        "kept"
    );
    assert!(!legacy.exists());

    let again = config::adopt_legacy_state(&env_for(home)?)?;
    assert!(again.is_none(), "the move happens once: {again:?}");

    let location = config::resolve_config_path(&env_for(home)?)?.ok_or("config")?;
    assert!(location.path().ends_with(".rivethub/config.yaml"));
    assert!(!location.is_legacy());
    Ok(())
}

#[test]
fn an_existing_rivethub_directory_is_left_in_place() -> Result<(), Box<dyn std::error::Error>> {
    let home = temp_home("both")?;
    let outcome = both_directories_stay(&home);
    fs::remove_dir_all(&home)?;
    outcome
}

fn both_directories_stay(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let legacy = home.join(".rivetos");
    let hub = home.join(".rivethub");
    fs::create_dir_all(&legacy)?;
    fs::create_dir_all(&hub)?;
    fs::write(legacy.join("old"), "old")?;
    fs::write(hub.join("new"), "new")?;

    let moved = config::adopt_legacy_state(&env_for(home)?)?;
    assert!(moved.is_none());
    assert_eq!(fs::read_to_string(legacy.join("old"))?, "old");
    assert_eq!(fs::read_to_string(hub.join("new"))?, "new");
    Ok(())
}

#[test]
fn a_legacy_file_is_not_a_state_directory() -> Result<(), Box<dyn std::error::Error>> {
    let home = temp_home("file")?;
    let outcome = legacy_file_stays(&home);
    fs::remove_dir_all(&home)?;
    outcome
}

fn legacy_file_stays(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(home.join(".rivetos"), "not a directory")?;
    let moved = config::adopt_legacy_state(&env_for(home)?)?;
    assert!(moved.is_none());
    assert!(home.join(".rivetos").is_file());
    assert!(!home.join(".rivethub").exists());
    Ok(())
}

#[test]
fn a_missing_legacy_directory_stays_missing() -> Result<(), Box<dyn std::error::Error>> {
    let home = temp_home("absent")?;
    let outcome = missing_legacy_stays(&home);
    fs::remove_dir_all(&home)?;
    outcome
}

fn missing_legacy_stays(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let moved = config::adopt_legacy_state(&env_for(home)?)?;
    assert!(moved.is_none());
    assert!(!home.join(".rivethub").exists());
    assert!(!home.join(".rivetos").exists());
    Ok(())
}

#[test]
fn a_live_legacy_owner_refuses_cutover_without_renaming() -> Result<(), Box<dyn std::error::Error>>
{
    let home = temp_home("live")?;
    let legacy = home.join(".rivetos");
    fs::create_dir_all(&legacy)?;
    let pid = std::process::id();
    fs::write(legacy.join("rivetos.pid"), pid.to_string())?;
    fs::write(legacy.join("marker"), "kept")?;
    let result = config::adopt_legacy_state(&env_for(&home)?);
    assert!(
        matches!(result, Err(config::StateError::LiveOwner { pid: found, .. }) if found == pid)
    );
    assert!(!home.join(".rivethub").exists());
    assert_eq!(fs::read_to_string(legacy.join("marker"))?, "kept");
    fs::remove_dir_all(home)?;
    Ok(())
}

#[test]
fn a_held_owner_lock_refuses_cutover() -> Result<(), Box<dyn std::error::Error>> {
    let home = temp_home("locked")?;
    let legacy = home.join(".rivetos");
    fs::create_dir_all(&legacy)?;
    let lock = fs::File::create(legacy.join("rivetos-owner.lock"))?;
    lock.lock()?;
    let result = config::adopt_legacy_state(&env_for(&home)?);
    assert!(matches!(result, Err(config::StateError::Locked { .. })));
    assert!(legacy.is_dir());
    assert!(!home.join(".rivethub").exists());
    drop(lock);
    config::adopt_legacy_state(&env_for(&home)?)?;
    assert!(home.join(".rivethub").is_dir());
    fs::remove_dir_all(home)?;
    Ok(())
}

#[test]
fn malformed_owner_state_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
    let home = temp_home("malformed")?;
    let legacy = home.join(".rivetos");
    fs::create_dir_all(&legacy)?;
    for pid in ["", "0", "-1", "not a PID", "4294967295"] {
        fs::write(legacy.join("gateway.pid"), pid)?;
        let result = config::adopt_legacy_state(&env_for(&home)?);
        assert!(matches!(
            result,
            Err(config::StateError::InvalidOwner { .. })
        ));
        assert!(legacy.is_dir());
        assert!(!home.join(".rivethub").exists());
    }
    fs::write(legacy.join("gateway.pid"), format!("1{}", " ".repeat(1024)))?;
    assert!(matches!(
        config::adopt_legacy_state(&env_for(&home)?),
        Err(config::StateError::InvalidOwner { .. })
    ));
    fs::remove_dir_all(home)?;
    Ok(())
}

#[test]
fn owner_file_symlinks_cannot_escape_the_state_directory() -> Result<(), Box<dyn std::error::Error>>
{
    let home = temp_home("symlink")?;
    let legacy = home.join(".rivetos");
    fs::create_dir(&legacy)?;
    let sibling = home.join("sibling.pid");
    fs::write(&sibling, "1")?;
    std::os::unix::fs::symlink(&sibling, legacy.join("gateway.pid"))?;
    assert!(matches!(
        config::adopt_legacy_state(&env_for(&home)?),
        Err(config::StateError::Inspect { .. })
    ));
    assert!(legacy.is_dir());
    assert!(!home.join(".rivethub").exists());
    assert_eq!(fs::read_to_string(sibling)?, "1");
    fs::remove_dir_all(home)?;
    Ok(())
}

#[test]
fn legacy_state_symlinks_are_refused() -> Result<(), Box<dyn std::error::Error>> {
    let home = temp_home("state-link")?;
    let sibling = home.join("outside");
    fs::create_dir(&sibling)?;
    fs::write(sibling.join("marker"), "untouched")?;
    std::os::unix::fs::symlink(&sibling, home.join(".rivetos"))?;
    let result = config::adopt_legacy_state(&env_for(&home)?);
    assert!(matches!(result, Err(config::StateError::Symlink { .. })));
    assert!(!home.join(".rivethub").exists());
    assert_eq!(fs::read_to_string(sibling.join("marker"))?, "untouched");
    fs::remove_dir_all(home)?;
    Ok(())
}

#[test]
fn claim_node_writes_the_pid_and_a_second_claim_is_refused()
-> Result<(), Box<dyn std::error::Error>> {
    let home = temp_home("claim")?;
    let outcome = claim_round_trip(&home);
    fs::remove_dir_all(&home)?;
    outcome
}

fn claim_round_trip(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let directory = home.join("state");
    let first = config::claim_node(&directory, std::process::id())?;
    let pid_path = directory.join("rivethub.pid");
    assert_eq!(
        fs::read_to_string(&pid_path)?,
        format!("{}\n", std::process::id())
    );
    assert!(matches!(
        config::claim_node(&directory, std::process::id()),
        Err(config::StateError::AlreadyRunning { pid }) if pid == std::process::id()
    ));
    first.release()?;
    assert!(!pid_path.exists());
    Ok(())
}

#[test]
fn a_live_pid_file_refuses_the_claim() -> Result<(), Box<dyn std::error::Error>> {
    let home = temp_home("claim-live")?;
    let directory = home.join("state");
    fs::create_dir_all(&directory)?;
    let pid = std::process::id();
    fs::write(directory.join("rivethub.pid"), format!("{pid}\n"))?;
    let result = config::claim_node(&directory, pid.wrapping_add(1).max(2));
    assert!(matches!(
        result,
        Err(config::StateError::AlreadyRunning { pid: found }) if found == pid
    ));
    fs::remove_dir_all(home)?;
    Ok(())
}

#[test]
fn a_stale_pid_is_replaced_and_a_symlink_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let home = temp_home("claim-stale")?;
    let outcome = stale_and_linked(&home);
    fs::remove_dir_all(&home)?;
    outcome
}

fn stale_and_linked(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let directory = home.join("state");
    fs::create_dir_all(&directory)?;
    let mut child = std::process::Command::new("/bin/sleep").arg("60").spawn()?;
    let stale = child.id();
    child.kill()?;
    child.wait()?;
    fs::write(directory.join("rivethub.pid"), format!("{stale}\n"))?;
    let claim = config::claim_node(&directory, std::process::id())?;
    assert_eq!(
        fs::read_to_string(directory.join("rivethub.pid"))?,
        format!("{}\n", std::process::id())
    );
    claim.release()?;

    let linked = home.join("linked");
    std::os::unix::fs::symlink(&directory, &linked)?;
    assert!(matches!(
        config::claim_node(&linked, std::process::id()),
        Err(config::StateError::Symlink { .. })
    ));
    let outside = home.join("outside");
    fs::write(&outside, "kept")?;
    std::os::unix::fs::symlink(&outside, directory.join("rivethub.pid"))?;
    assert!(matches!(
        config::claim_node(&directory, std::process::id()),
        Err(config::StateError::Inspect { .. })
    ));
    assert_eq!(fs::read_to_string(outside)?, "kept");
    Ok(())
}

#[test]
fn a_stale_lock_directory_does_not_block_the_move() -> Result<(), Box<dyn std::error::Error>> {
    let home = temp_home("stale-lock-dir")?;
    let outcome = stale_lock_directory(&home);
    fs::remove_dir_all(&home)?;
    outcome
}

fn stale_lock_directory(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let legacy = home.join(".rivetos");
    let lock = legacy.join("pi-capture-state.json.lock");
    fs::create_dir_all(&lock)?;
    let stale = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    fs::File::open(&lock)?.set_modified(stale)?;
    let moved = config::adopt_legacy_state(&env_for(home)?)?;
    assert!(
        moved.is_some(),
        "a stale proper-lockfile directory is not an owner"
    );
    assert!(home.join(".rivethub/pi-capture-state.json.lock").is_dir());
    Ok(())
}

#[test]
fn a_fresh_lock_directory_blocks_the_move() -> Result<(), Box<dyn std::error::Error>> {
    let home = temp_home("fresh-lock-dir")?;
    let outcome = fresh_lock_directory(&home);
    fs::remove_dir_all(&home)?;
    outcome
}

fn fresh_lock_directory(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let legacy = home.join(".rivetos");
    fs::create_dir_all(legacy.join("capture-state.json.lock"))?;
    let refused = config::adopt_legacy_state(&env_for(home)?);
    assert!(
        matches!(refused, Err(config::StateError::Locked { .. })),
        "{refused:?}"
    );
    assert!(legacy.is_dir());
    assert!(!home.join(".rivethub").exists());
    Ok(())
}

#[test]
fn rivethub_home_is_the_state_directory_and_locks_stay_with_it()
-> Result<(), Box<dyn std::error::Error>> {
    let home = temp_home("hub-home")?;
    let first = temp_home("hub-a")?;
    let second = temp_home("hub-b")?;
    let outcome = separate_homes(&home, &first, &second);
    fs::remove_dir_all(&home)?;
    fs::remove_dir_all(&first)?;
    fs::remove_dir_all(&second)?;
    outcome
}

fn separate_homes(
    home: &Path,
    first: &Path,
    second: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let text = |path: &Path| {
        path.to_str()
            .ok_or("temp path is not utf-8")
            .map(str::to_owned)
    };
    let env = Environment::from_vars([
        ("HOME".to_owned(), text(home)?),
        ("RIVETHUB_HOME".to_owned(), text(first)?),
    ]);
    let paths = config::Paths::new(&env)?;
    assert_eq!(paths.state_dir(), first);
    assert_ne!(paths.state_dir(), home.join(".rivethub"));
    fs::create_dir_all(home.join(".rivetos"))?;
    assert!(config::adopt_legacy_state(&env)?.is_none());
    assert!(home.join(".rivetos").is_dir());
    let held = config::claim_node(first, std::process::id())?;
    let other = config::claim_node(second, std::process::id())?;
    assert!(config::claim_node(first, std::process::id()).is_err());
    other.release()?;
    held.release()?;
    let relative = Environment::from_vars([
        ("HOME".to_owned(), text(home)?),
        ("RIVETHUB_HOME".to_owned(), "relative".to_owned()),
    ]);
    assert!(config::Paths::new(&relative).is_err());
    Ok(())
}
