use std::os::unix::fs::PermissionsExt as _;

use super::*;

#[derive(Debug, Default)]
struct MemoryVault(parking_lot::Mutex<BTreeMap<String, String>>);

impl Vault for MemoryVault {
    fn read(&self, server: &str) -> Result<Option<String>, keyring::Error> {
        Ok(self.0.lock().get(server).cloned())
    }

    fn write(&self, server: &str, secret: &str) -> Result<(), keyring::Error> {
        self.0.lock().insert(server.to_owned(), secret.to_owned());
        Ok(())
    }

    fn erase(&self, server: &str) -> Result<(), keyring::Error> {
        self.0.lock().remove(server);
        Ok(())
    }
}

const OLD_FILE: &str = r#"{"logins":{"rivethub.cloud":{"access_token":"rhca_x","refresh_token":"rhcr_x","expires_at":"2099-01-01T00:00:00Z","scope":"drops:write"},"team.example.com":{"team_token":"rht_x","user":"phil"}}}"#;

fn server(host: &str) -> ShareServer {
    host.parse().expect("server")
}

fn old_file(dir: &Path) -> PathBuf {
    let file = dir.join("share-logins.json");
    std::fs::write(&file, OLD_FILE).expect("old logins");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    file
}

fn team(token: &str) -> Login {
    Login::Team(TeamLogin {
        team_token: token.to_owned(),
        user: "phil".to_owned(),
    })
}

fn token_of(login: &Login) -> &str {
    match login {
        Login::Device(tokens) => &tokens.refresh_token,
        Login::Team(team) => &team.team_token,
    }
}

#[tokio::test]
async fn the_file_store_reads_old_files_and_round_trips() {
    let dir = tempfile::tempdir().expect("dir");
    let store = LoginStore::with_vault(old_file(dir.path()), None);
    assert_eq!(store.kind(), TokenStore::File);

    let cloud = store.load(&server("rivethub.cloud")).await.expect("cloud");
    assert_eq!(token_of(&cloud), "rhcr_x");
    let listed = store.list().expect("list");
    assert_eq!(
        listed.get("team.example.com"),
        Some(&Listed::Team {
            user: "phil".to_owned()
        })
    );

    store
        .save(&server("other.example.com"), &team("rht_other"))
        .await
        .expect("save");
    let read = store
        .load(&server("other.example.com"))
        .await
        .expect("load");
    assert_eq!(token_of(&read), "rht_other");
    let mode = std::fs::metadata(store.file())
        .expect("file")
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "{mode:o}");

    store
        .forget(&server("other.example.com"))
        .await
        .expect("forget");
    assert!(matches!(
        store.load(&server("other.example.com")).await,
        Err(ServerError::LoggedOut(_))
    ));
    assert!(!format!("{cloud:?}").contains("rhcr_x"));
}

#[tokio::test]
async fn the_keyring_store_keeps_only_non_secret_fields_in_the_file() {
    let dir = tempfile::tempdir().expect("dir");
    let vault = Arc::new(MemoryVault::default());
    let store = LoginStore::with_vault(old_file(dir.path()), Some(vault.clone()));
    assert_eq!(store.kind(), TokenStore::Keyring);
    let cloud = server("rivethub.cloud");

    let old = store
        .load(&cloud)
        .await
        .expect("an old file entry still reads");
    store
        .save(&cloud, &old)
        .await
        .expect("moved into the keyring");

    let text = std::fs::read_to_string(store.file()).expect("file");
    assert!(!text.contains("rhcr_x"), "{text}");
    assert!(text.contains("drops:write"), "{text}");
    assert!(
        vault
            .0
            .lock()
            .get("rivethub.cloud")
            .is_some_and(|secret| secret.contains("rhcr_x"))
    );
    let read = store.load(&cloud).await.expect("from the keyring");
    assert_eq!(token_of(&read), "rhcr_x");

    store.forget(&cloud).await.expect("logout");
    assert!(
        vault.0.lock().get("rivethub.cloud").is_none(),
        "logout erases the keyring entry"
    );
    assert!(
        !std::fs::read_to_string(store.file())
            .expect("file")
            .contains("rivethub.cloud")
    );
}

#[tokio::test]
async fn a_keyring_login_in_a_file_only_process_says_so() {
    let dir = tempfile::tempdir().expect("dir");
    let file = old_file(dir.path());
    let keyring = LoginStore::with_vault(file.clone(), Some(Arc::new(MemoryVault::default())));
    keyring
        .save(&server("team.example.com"), &team("rht_y"))
        .await
        .expect("save");

    let refused = LoginStore::with_vault(file, None)
        .load(&server("team.example.com"))
        .await;

    assert!(
        matches!(&refused, Err(ServerError::Store(reason)) if reason.contains("RIVETHUB_TOKEN_STORE=file")),
        "{refused:?}"
    );
}

#[tokio::test]
async fn the_environment_chooses_the_file_store_without_touching_a_keyring() {
    let dir = tempfile::tempdir().expect("dir");
    let file = dir.path().join("share-logins.json");
    let forced = Environment::from_vars([(TOKEN_STORE_VARIABLE, "file")]);
    let chosen = LoginStore::choose(&forced, file.clone())
        .await
        .expect("file");
    assert_eq!(chosen.kind(), TokenStore::File);

    if KEYRING_IS_SECRET_SERVICE {
        let busless = Environment::from_vars([("HOME", "/nonexistent")]);
        let chosen = LoginStore::choose(&busless, file.clone())
            .await
            .expect("file");
        assert_eq!(chosen.kind(), TokenStore::File);
    }

    let wrong = Environment::from_vars([(TOKEN_STORE_VARIABLE, "vault")]);
    assert!(matches!(
        LoginStore::choose(&wrong, file).await,
        Err(ServerError::Store(_))
    ));
}

#[derive(Debug, Default)]
struct RefusingVault;

impl Vault for RefusingVault {
    fn read(&self, _: &str) -> Result<Option<String>, keyring::Error> {
        Ok(None)
    }

    fn write(&self, _: &str, _: &str) -> Result<(), keyring::Error> {
        Err(keyring::Error::NoEntry)
    }

    fn erase(&self, _: &str) -> Result<(), keyring::Error> {
        Ok(())
    }
}

#[tokio::test]
async fn an_unpinned_keyring_that_refuses_the_write_falls_back_to_the_file() {
    let dir = tempfile::tempdir().expect("dir");
    let file = dir.path().join("share-logins.json");
    let store = LoginStore::with_vault(file.clone(), Some(Arc::new(RefusingVault)));
    store
        .save(&server("rivethub.cloud"), &team("rht_headless"))
        .await
        .expect("falls back to the file");
    let read = store.load(&server("rivethub.cloud")).await.expect("load");
    assert_eq!(token_of(&read), "rht_headless");
    let mode = std::fs::metadata(&file).expect("file").permissions().mode();
    assert_eq!(mode & 0o077, 0, "{mode:o}");

    let pinned = LoginStore {
        pinned: true,
        ..LoginStore::with_vault(
            dir.path().join("pinned.json"),
            Some(Arc::new(RefusingVault)),
        )
    };
    let refused = pinned
        .save(&server("rivethub.cloud"), &team("rht_headless"))
        .await
        .expect_err("a pinned keyring is not second-guessed");
    assert!(matches!(refused, ServerError::Keyring(_)), "{refused}");
    assert!(!dir.path().join("pinned.json").exists());
}
