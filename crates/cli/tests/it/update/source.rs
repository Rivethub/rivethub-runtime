use super::update_support;

use super::update_support::{Fixture, Published, Result, args};
use cli::Exit;
use cli::update::Channel;

fn prepare(fixture: &Fixture) -> Result<config::Environment> {
    let checkout = fixture.home.path().join("source checkout");
    std::fs::create_dir(&checkout)?;
    let migration = checkout.join("migration");
    std::fs::write(&migration, update_support::MIGRATION)?;
    fixture.script(
        "git",
        r#"#!/bin/sh
printf 'git' >> "$RIVETHUB_TEST_CALLS"
for arg do printf '|%s' "$arg" >> "$RIVETHUB_TEST_CALLS"; done
printf '\n' >> "$RIVETHUB_TEST_CALLS"
case "$1" in
    status) if [ "$RIVETHUB_TEST_FAIL" = dirty ]; then printf ' M Cargo.toml\n'; fi ;;
    branch) printf '%s\n' "$RIVETHUB_TEST_BRANCH" ;;
    pull) if [ "$RIVETHUB_TEST_FAIL" = pull ]; then exit 1; fi ;;
    rev-parse) printf 'abc123\n' ;;
esac
"#,
    )?;
    fixture.script(
        "cargo",
        r#"#!/bin/sh
printf 'cargo' >> "$RIVETHUB_TEST_CALLS"
for arg do printf '|%s' "$arg" >> "$RIVETHUB_TEST_CALLS"; last="$arg"; done
printf '\n' >> "$RIVETHUB_TEST_CALLS"
if [ "$RIVETHUB_TEST_FAIL" = build ]; then exit 1; fi
if [ "$RIVETHUB_TEST_FAIL" = escape ]; then
    /bin/mkdir -p "$RIVETHUB_INSTALL_ROOT/output"
    /bin/ln -s "$RIVETHUB_INSTALL_ROOT/output" "$last"
fi
/bin/mkdir -p "$last/release"
printf '#!/bin/sh\nprintf "RivetHub v0.8.3\\n"\n' > "$last/release/rh"
/bin/cp "$RIVETHUB_INSTALL_ROOT/migration" "$last/release/rivethub-migrate"
/bin/chmod 755 "$last/release/rh" "$last/release/rivethub-migrate"
"#,
    )?;
    Ok(fixture
        .env()
        .with_override("RIVETHUB_INSTALL_ROOT", checkout))
}

#[tokio::test]
async fn nightly_pulls_builds_locked_and_migrates_before_restart() -> Result {
    let fixture = Fixture::new()?;
    let env = prepare(&fixture)?.with_override("RIVETHUB_TEST_BRANCH", "main");
    let mut args = args();
    args.channel = Channel::Nightly;
    let report = fixture.run(&args, &env, Published::Good).await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    let calls = std::fs::read_to_string(&fixture.calls)?;
    assert!(calls.contains("git|pull|--ff-only|origin|main\n"));
    assert!(
        calls.contains(
            "cargo|build|--release|--locked|--bin|rh|--bin|rivethub-migrate|--target-dir|"
        )
    );
    let build = calls.find("cargo|build").ok_or("build")?;
    let migration = calls.find("migrate|sqlite").ok_or("migration")?;
    let restart = calls.find("systemctl|--user|restart").ok_or("restart")?;
    assert!(build < migration && migration < restart);
    assert_eq!(
        std::fs::read(fixture.binary.with_extension("previous"))?,
        b"old runtime"
    );
    Ok(())
}

#[tokio::test]
async fn source_failures_never_swap_or_restart() -> Result {
    for failure in ["dirty", "pull", "build", "escape"] {
        let fixture = Fixture::new()?;
        let env = prepare(&fixture)?
            .with_override("RIVETHUB_TEST_BRANCH", "main")
            .with_override("RIVETHUB_TEST_FAIL", failure);
        let mut args = args();
        args.channel = Channel::Nightly;
        let report = fixture.run(&args, &env, Published::Good).await?;
        assert_eq!(report.exit, Exit::Failure);
        assert_eq!(std::fs::read(&fixture.binary)?, b"old runtime");
        let calls = std::fs::read_to_string(&fixture.calls)?;
        assert!(!calls.lines().any(|line| line.starts_with("migrate|")));
        assert!(!calls.contains("systemctl"));
    }
    Ok(())
}
