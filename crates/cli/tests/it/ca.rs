#[path = "ca_support/mod.rs"]
mod ca_support;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use ca_support::{Fixture, Replay, Result, assert_argv, canonical_url, invoke, utf8};
use cli::Exit;
use kernel::ca::Principal;
use tokio_util::sync::CancellationToken;

#[test]
fn principal_design_scenarios_are_typed_and_replayed() -> Result {
    #[derive(serde::Deserialize)]
    struct Case {
        uri: String,
        principal: Option<Principal>,
    }
    let cases: Vec<Case> = serde_json::from_str(&std::fs::read_to_string(testkit::golden_path(
        env!("CARGO_MANIFEST_DIR"),
        "tests/fixtures/pki/principals.json",
    ))?)?;
    for case in cases {
        match case.principal {
            Some(expected) => {
                assert_eq!(case.uri.parse::<Principal>()?, expected);
                assert_eq!(expected.spiffe_uri()?, case.uri);
            }
            None => assert!(case.uri.parse::<Principal>().is_err()),
        }
    }
    Ok(())
}

#[tokio::test]
async fn removed_init_and_import_are_refused() -> Result {
    let fixture = Fixture::new()?;
    for (args, needle) in [
        (vec!["ca".to_owned(), "init".to_owned()], "init"),
        (
            vec!["ca".to_owned(), "import".to_owned(), "legacy".to_owned()],
            "import",
        ),
    ] {
        let report = fixture.run(args).await?;
        assert_eq!(report.exit, Exit::Failure, "{}", report.stderr);
        assert!(report.stderr.contains(needle), "{}", report.stderr);
    }
    assert!(!fixture.argv.exists());
    Ok(())
}

#[tokio::test]
async fn help_lists_step_ca_commands() -> Result {
    let fixture = Fixture::new()?;
    let help = fixture.run(["ca", "--help"]).await?;
    assert_eq!(help.exit, Exit::Ok, "{}", help.stderr);
    for word in ["token", "revoke", "crl", "status"] {
        assert!(help.stdout.contains(word), "{}", help.stdout);
    }
    assert!(!help.stdout.contains("import"), "{}", help.stdout);
    let token_help = fixture.run(["ca", "token", "--help"]).await?;
    assert_eq!(token_help.exit, Exit::Ok, "{}", token_help.stderr);
    for word in ["node", "agent", "device"] {
        assert!(token_help.stdout.contains(word), "{}", token_help.stdout);
    }
    Ok(())
}

#[tokio::test]
async fn node_token_pins_roster_names() -> Result {
    let fixture = Fixture::new()?;
    let report = fixture
        .run(command(
            &fixture,
            true,
            &["token", "node", "rec-node.example.test"],
        )?)
        .await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(report.stdout, "one-time-token\n");
    assert_eq!(report.stderr, "");
    let mut expected = vec![
        "ca",
        "token",
        "rec-node.example.test",
        "--provisioner",
        "nodes",
        "--san",
        "192.0.2.15",
        "--san",
        "rec-node.example.test",
        "--san",
        "rec-node.example.test.rivet.mesh",
    ];
    let flags = endpoint(&fixture, "https://ca.example.test")?;
    expected.extend(flags.iter().map(String::as_str));
    assert_argv(&fixture.argv()?, &expected)?;
    std::fs::remove_file(&fixture.argv)?;
    let idle = fixture
        .run(command(
            &fixture,
            true,
            &["token", "node", "idle-node.example.test"],
        )?)
        .await?;
    assert_eq!(idle.exit, Exit::Ok, "{}", idle.stderr);
    assert_eq!(idle.stdout, "one-time-token\n");
    let mut idle_expected = vec![
        "ca",
        "token",
        "idle-node.example.test",
        "--provisioner",
        "nodes",
        "--san",
        "192.0.2.16",
        "--san",
        "idle-node.example.test",
        "--san",
        "idle-node.example.test.rivet.mesh",
    ];
    idle_expected.extend(flags.iter().map(String::as_str));
    assert_argv(&fixture.argv()?, &idle_expected)?;
    Ok(())
}

#[tokio::test]
async fn agent_token_pins_the_spiffe_uri() -> Result {
    let fixture = Fixture::new()?;
    let report = fixture
        .run(command(
            &fixture,
            true,
            &["token", "agent", "rec-agent.example.test"],
        )?)
        .await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(report.stdout, "one-time-token\n");
    let uri = "spiffe://rivet.mesh/agent/rec-node.example.test/rec-agent.example.test";
    let mut expected = vec!["ca", "token", uri, "--provisioner", "agents", "--san", uri];
    let flags = endpoint(&fixture, "https://ca.example.test")?;
    expected.extend(flags.iter().map(String::as_str));
    assert_argv(&fixture.argv()?, &expected)?;
    Ok(())
}

#[tokio::test]
async fn device_token_pins_the_user_device_spiffe_uri() -> Result {
    let fixture = Fixture::new()?;
    let report = fixture
        .run(command(
            &fixture,
            false,
            &["token", "device", "laptop", "--user", "alice"],
        )?)
        .await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(report.stdout, "one-time-token\n");
    let uri = "spiffe://rivet.mesh/device/alice/laptop";
    let mut expected = vec!["ca", "token", uri, "--provisioner", "devices", "--san", uri];
    let flags = endpoint(&fixture, "https://ca.example.test")?;
    expected.extend(flags.iter().map(String::as_str));
    assert_argv(&fixture.argv()?, &expected)?;
    assert!(!fixture.argv()?.iter().any(|arg| arg.contains("roster.txt")));
    std::fs::remove_file(&fixture.argv)?;
    let missing = fixture
        .run(command(&fixture, false, &["token", "device", "laptop"])?)
        .await?;
    assert_eq!(missing.exit, Exit::Failure, "{}", missing.stderr);
    assert!(missing.stderr.contains("--user"), "{}", missing.stderr);
    for (user, device) in [
        ("alice", ".."),
        ("bad/name", "laptop"),
        ("alice", "white space"),
    ] {
        let tail = ["token", "device", device, "--user", user];
        let refused = fixture.run(command(&fixture, false, &tail)?).await?;
        assert_eq!(refused.exit, Exit::Failure, "{tail:?}: {}", refused.stderr);
        assert!(
            refused.stderr.contains("unreserved"),
            "{tail:?}: {}",
            refused.stderr
        );
    }
    assert!(!fixture.argv.exists());
    Ok(())
}

#[tokio::test]
async fn missing_flags_unknown_names_and_mesh_json_do_not_call_step() -> Result {
    let fixture = Fixture::new()?;
    let no_password = strip_flag(
        &command(&fixture, true, &["token", "node", "rec-node.example.test"])?,
        "--provisioner-password-file",
    );
    let refused = fixture.run(no_password).await?;
    assert_eq!(refused.exit, Exit::Failure);
    assert!(
        refused.stderr.contains("provisioner-password-file"),
        "{}",
        refused.stderr
    );
    let no_roster = strip_flag(
        &command(
            &fixture,
            true,
            &["token", "agent", "rec-agent.example.test"],
        )?,
        "--roster",
    );
    let roster = fixture.run(no_roster).await?;
    assert!(roster.stderr.contains("--roster"), "{}", roster.stderr);
    let unknown = fixture
        .run(command(
            &fixture,
            true,
            &["token", "node", "absent.example.test"],
        )?)
        .await?;
    assert!(
        unknown.stderr.contains("not in the operator roster"),
        "{}",
        unknown.stderr
    );
    let unknown_agent = fixture
        .run(command(
            &fixture,
            true,
            &["token", "agent", "rec-node.example.test"],
        )?)
        .await?;
    assert!(
        unknown_agent.stderr.contains("not in the operator roster"),
        "{}",
        unknown_agent.stderr
    );
    let mesh = fixture.home.path().join("mesh.json");
    std::fs::write(
        &mesh,
        "only-mesh.example.test:192.0.2.20:only-agent.example.test\n",
    )?;
    let decoy = fixture
        .run(command(
            &fixture,
            true,
            &["token", "node", "only-mesh.example.test"],
        )?)
        .await?;
    assert!(
        decoy.stderr.contains("only-mesh.example.test"),
        "{}",
        decoy.stderr
    );
    std::fs::copy(&fixture.roster, &mesh)?;
    let mut as_roster = command(&fixture, false, &["token", "node", "rec-node.example.test"])?;
    as_roster.push("--roster".to_owned());
    as_roster.push(utf8(&mesh)?);
    let named = fixture.run(as_roster).await?;
    assert!(named.stderr.contains("mesh.json"), "{}", named.stderr);
    let link = fixture.home.path().join("linked-roster.txt");
    std::os::unix::fs::symlink(&mesh, &link)?;
    let mut linked = command(&fixture, false, &["token", "node", "rec-node.example.test"])?;
    linked.push("--roster".to_owned());
    linked.push(utf8(&link)?);
    let followed = fixture.run(linked).await?;
    assert!(followed.stderr.contains("mesh.json"), "{}", followed.stderr);
    assert!(!fixture.argv.exists());
    Ok(())
}

#[tokio::test]
async fn malformed_roster_lines_name_the_line() -> Result {
    let fixture = Fixture::new()?;
    for (text, needle) in [
        ("not a roster\n", "operator roster line 1 is malformed"),
        ("# comment\n", "operator roster line 1 is malformed"),
        (
            "rec-node.example.test:192.0.2.15:rec-agent.example.test\n\n",
            "operator roster line 2 is malformed",
        ),
        (
            "node.example.test:2001:db8::1:agent.example.test\n",
            "operator roster line 1 is malformed",
        ),
        (
            "node.example.test:not-an-ip:agent.example.test\n",
            "operator roster line 1 is malformed",
        ),
        (
            "a.example.test:192.0.2.15:b.example.test:extra\n",
            "operator roster line 1 is malformed",
        ),
    ] {
        roster_failure(&fixture, text, needle).await?;
    }
    Ok(())
}

#[tokio::test]
async fn duplicate_roster_rows_name_the_line() -> Result {
    let fixture = Fixture::new()?;
    roster_failure(
        &fixture,
        "rec-node.example.test:192.0.2.15:rec-agent.example.test\nrec-node.example.test:192.0.2.17:\n",
        "operator roster line 2 repeats node `rec-node.example.test`",
    )
    .await?;
    roster_failure(
        &fixture,
        "a.example.test:192.0.2.15:same.example.test\nb.example.test:192.0.2.16:same.example.test\n",
        "operator roster line 2 repeats agent `same.example.test`",
    )
    .await?;
    roster_failure(
        &fixture,
        "a.example.test:192.0.2.15:one.example.test\nb.example.test:192.0.2.15:\n",
        "operator roster line 2 repeats address 192.0.2.15",
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn step_failures_are_typed() -> Result {
    let fixture = Fixture::new()?;
    let env = fixture
        .environment()
        .with_override("RIVETHUB_TEST_FAIL", "1");
    let failed = invoke(
        &env,
        &CancellationToken::new(),
        command(&fixture, true, &["token", "node", "rec-node.example.test"])?,
    )
    .await?;
    assert_eq!(failed.exit, Exit::Failure);
    assert!(
        failed.stderr.contains("provisioner password rejected"),
        "{}",
        failed.stderr
    );
    assert!(
        failed.stderr.contains("step ca token failed"),
        "{}",
        failed.stderr
    );
    std::fs::remove_file(&fixture.argv)?;
    let blank_env = fixture
        .environment()
        .with_override("RIVETHUB_TEST_FAIL", "blank");
    let blank = invoke(
        &blank_env,
        &CancellationToken::new(),
        command(&fixture, true, &["token", "node", "rec-node.example.test"])?,
    )
    .await?;
    assert!(
        blank.stderr.contains("empty or invalid one-time token"),
        "{}",
        blank.stderr
    );
    let empty = fixture.home.path().join("empty-path");
    std::fs::create_dir(&empty)?;
    std::fs::remove_file(&fixture.argv)?;
    let missing = invoke(
        &fixture.environment_with_path(&empty),
        &CancellationToken::new(),
        command(&fixture, true, &["token", "node", "rec-node.example.test"])?,
    )
    .await?;
    assert_eq!(missing.exit, Exit::Failure);
    assert!(
        missing.stderr.contains("No such file") || missing.stderr.contains("os error 2"),
        "{}",
        missing.stderr
    );
    assert!(!fixture.argv.exists());
    Ok(())
}

#[tokio::test]
async fn revoke_uses_a_canonical_serial() -> Result {
    let fixture = Fixture::new()?;
    for serial in [
        "0",
        "01",
        "10:01",
        "spiffe://rivet.mesh/node/rec-node.example.test",
    ] {
        let report = fixture
            .run(command(&fixture, false, &["revoke", serial])?)
            .await?;
        assert_eq!(report.exit, Exit::Failure, "{serial}: {}", report.stderr);
        assert!(
            report.stderr.contains("canonical decimal"),
            "{serial}: {}",
            report.stderr
        );
        assert!(!fixture.argv.exists(), "{serial}");
    }
    let report = fixture
        .run(command(&fixture, false, &["revoke", "1001"])?)
        .await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(report.stdout, "Certificate revoked.\n");
    assert_eq!(report.stderr, "");
    let mut expected = vec!["ca", "revoke", "1001"];
    let flags = endpoint(&fixture, "https://ca.example.test")?;
    expected.extend(flags.iter().map(String::as_str));
    assert_argv(&fixture.argv()?, &expected)?;
    assert!(!fixture.argv()?.iter().any(|arg| arg == "--provisioner"));
    Ok(())
}

#[tokio::test]
async fn paths_refuse_traversal_symlinks_and_accept_spaces() -> Result {
    let fixture = Fixture::new()?;
    let escape = fixture.home.path().join("safe/../outside.pem");
    let mut traversed = command(&fixture, true, &["token", "node", "rec-node.example.test"])?;
    replace_flag(&mut traversed, "--root", &utf8(&escape)?);
    let report = tokio::time::timeout(Duration::from_secs(5), fixture.run(traversed)).await??;
    assert!(report.stderr.contains("traversal"), "{}", report.stderr);
    assert!(!escape.exists());
    assert!(!fixture.argv.exists());
    let linked_root = fixture.home.path().join("linked-root.pem");
    std::os::unix::fs::symlink(&fixture.root, &linked_root)?;
    let mut root_link = command(&fixture, true, &["token", "node", "rec-node.example.test"])?;
    replace_flag(&mut root_link, "--root", &utf8(&linked_root)?);
    let root_refused = fixture.run(root_link).await?;
    assert!(
        root_refused.stderr.contains("regular file"),
        "{}",
        root_refused.stderr
    );
    let linked_password = fixture.home.path().join("linked-password");
    std::os::unix::fs::symlink(&fixture.password, &linked_password)?;
    let mut password_link = command(&fixture, true, &["token", "node", "rec-node.example.test"])?;
    replace_flag(
        &mut password_link,
        "--provisioner-password-file",
        &utf8(&linked_password)?,
    );
    let password_refused = fixture.run(password_link).await?;
    assert!(
        password_refused.stderr.contains("regular file"),
        "{}",
        password_refused.stderr
    );
    let base = fixture.home.path().join("crl dir");
    let evil = fixture.home.path().join("crl dir-evil");
    std::fs::create_dir(&base)?;
    std::fs::create_dir(&evil)?;
    let secret = evil.join("secret.pem");
    std::fs::write(&secret, b"keep")?;
    let link = base.join("out.pem");
    std::os::unix::fs::symlink(&secret, &link)?;
    let mut crl_link = command(&fixture, false, &["crl"])?;
    crl_link.push("--crl-out".to_owned());
    crl_link.push(utf8(&link)?);
    let crl_refused = tokio::time::timeout(Duration::from_secs(5), fixture.run(crl_link)).await??;
    assert_eq!(crl_refused.exit, Exit::Failure, "{}", crl_refused.stderr);
    assert_eq!(std::fs::read(&secret)?, b"keep");
    let spaced = fixture.home.path().join("operator files");
    std::fs::create_dir(&spaced)?;
    let roster = spaced.join("roster.txt");
    let password = spaced.join("provisioner password");
    let root = spaced.join("root cert.pem");
    std::fs::copy(&fixture.roster, &roster)?;
    std::fs::copy(&fixture.password, &password)?;
    std::fs::copy(&fixture.root, &root)?;
    let mut spaced_args = command(&fixture, false, &["token", "node", "rec-node.example.test"])?;
    replace_flag(&mut spaced_args, "--root", &utf8(&root)?);
    replace_flag(
        &mut spaced_args,
        "--provisioner-password-file",
        &utf8(&password)?,
    );
    spaced_args.push("--roster".to_owned());
    spaced_args.push(utf8(&roster)?);
    let spaced_report = fixture.run(spaced_args).await?;
    assert_eq!(spaced_report.exit, Exit::Ok, "{}", spaced_report.stderr);
    let recorded = fixture.argv()?;
    let root_path = utf8(&std::fs::canonicalize(&root)?)?;
    let password_path = utf8(&std::fs::canonicalize(&password)?)?;
    assert!(recorded.iter().any(|arg| arg == &root_path));
    assert!(recorded.iter().any(|arg| arg == &password_path));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_kills_step_after_it_records_argv() -> Result {
    let fixture = Fixture::new()?;
    let pause = fixture.home.path().join("pause.fifo");
    nix::unistd::mkfifo(
        &pause,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .expect("fifo");
    let env = fixture
        .environment()
        .with_override("RIVETHUB_TEST_PAUSE", &pause);
    let shutdown = CancellationToken::new();
    let task_shutdown = shutdown.clone();
    let args = command(&fixture, true, &["token", "node", "rec-node.example.test"])?;
    let task = tokio::spawn(async move { invoke(&env, &task_shutdown, args).await });
    let opened = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::fs::OpenOptions::new().write(true).open(&pause),
    )
    .await
    .expect("step did not open the fifo")
    .expect("fifo open");
    shutdown.cancel();
    drop(opened);
    let report = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancellation timed out")
        .expect("task join")?;
    assert_eq!(report.exit, Exit::Failure);
    assert!(
        report.stderr.contains("command cancelled"),
        "{}",
        report.stderr
    );
    assert!(fixture.argv()?.iter().any(|arg| arg == "token"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crl_and_status_replay_the_recorded_authority() -> Result {
    let fixture = Fixture::new()?;
    let server = Replay::start().await?;
    let mut status_args = vec![
        "ca".to_owned(),
        "--ca-url".to_owned(),
        server.url.as_str().to_owned(),
        "--root".to_owned(),
        utf8(&server.root_pem)?,
        "status".to_owned(),
    ];
    let status = fixture.run(status_args.clone()).await?;
    assert_eq!(
        status.exit,
        Exit::Ok,
        "{} {:?}",
        status.stderr,
        server.failure()
    );
    assert!(status.stdout.contains("Certificate authority ready."));
    assert!(
        status.stdout.contains("Example Test Root"),
        "{}",
        status.stdout
    );
    assert!(status.stdout.contains("expires"), "{}", status.stdout);
    let directory = fixture.home.path().join("published");
    std::fs::create_dir(&directory)?;
    let published = directory.join("list.pem");
    std::fs::write(&published, b"stale")?;
    status_args.pop();
    status_args.push("--crl-out".to_owned());
    status_args.push(utf8(&published)?);
    status_args.push("crl".to_owned());
    let crl = fixture.run(status_args.clone()).await?;
    assert_eq!(crl.exit, Exit::Ok, "{} {:?}", crl.stderr, server.failure());
    assert_eq!(crl.stdout, server.crl_pem);
    assert_eq!(std::fs::read_to_string(&published)?, server.crl_pem);
    assert_eq!(
        std::fs::metadata(&published)?.permissions().mode() & 0o777,
        0o644
    );
    assert_eq!(names(&directory)?, vec!["list.pem".to_owned()]);
    let again = fixture.run(status_args).await?;
    assert_eq!(again.stdout, server.crl_pem);
    assert_eq!(std::fs::read(&published)?, server.crl_pem.as_bytes());
    assert_eq!(names(&directory)?, vec!["list.pem".to_owned()]);
    Ok(())
}

#[test]
fn stepca_recordings_for_this_slice_are_exercised_or_skipped() -> Result {
    #[derive(Debug, serde::Deserialize)]
    struct Catalog {
        cases: Vec<Case>,
        #[serde(rename = "crlPem")]
        crl_pem: String,
    }
    #[derive(Debug, serde::Deserialize)]
    struct Case {
        id: String,
    }
    const EXERCISED: &[&str] = &["roots", "crl"];
    const SKIPPED: &[(&str, &str)] = &[
        (
            "sign-with-token",
            "Signing with a one-time token is the rh cert sign exchange, and rh ca only mints that token through the step command line.",
        ),
        (
            "sign-token-reused",
            "A reused one-time token is refused by the step-ca sign endpoint, which rh cert calls and which rh ca never calls.",
        ),
        (
            "sign-san-not-granted",
            "An ungranted name is a step-ca sign refusal. rh ca pins roster names into the token and does not submit a certificate signing request.",
        ),
        (
            "renew-mtls",
            "Mutual-TLS renewal is the holder command rh cert renew, and rh ca does not present a holder certificate to step-ca.",
        ),
        (
            "renew-without-certificate",
            "Renewal without a certificate is a holder exchange against step-ca, and rh ca does not call the renew endpoint.",
        ),
        (
            "revoke-own-mtls",
            "The recorded revoke exchange is an authenticated holder call. rh ca revoke shells out to the step CLI and does not perform that HTTP exchange.",
        ),
        (
            "renew-revoked",
            "Renewal of a revoked certificate is an rh cert exchange, and rh ca does not call the step-ca renew endpoint.",
        ),
    ];
    let catalog: Catalog =
        serde_json::from_slice(&std::fs::read(testkit::recording("stepca/api.json"))?)?;
    assert!(catalog.crl_pem.contains("BEGIN X509 CRL"));
    let mut expected = std::collections::BTreeSet::new();
    for case in &catalog.cases {
        assert!(
            expected.insert(case.id.as_str()),
            "duplicate case {}",
            case.id
        );
    }
    assert!(expected.insert("crl"));
    let mut covered = std::collections::BTreeSet::new();
    for name in EXERCISED {
        assert!(covered.insert(*name), "duplicate exercised case {name}");
    }
    for (name, reason) in SKIPPED {
        assert!(reason.len() >= 60, "{name} reason is too short");
        assert!(covered.insert(*name), "duplicate skipped case {name}");
    }
    assert_eq!(covered, expected);
    Ok(())
}

fn command(fixture: &Fixture, roster: bool, tail: &[&str]) -> Result<Vec<String>> {
    let mut args = vec![
        "ca".to_owned(),
        "--ca-url".to_owned(),
        "https://ca.example.test".to_owned(),
        "--root".to_owned(),
        utf8(&fixture.root)?,
        "--provisioner-password-file".to_owned(),
        utf8(&fixture.password)?,
    ];
    if roster {
        args.push("--roster".to_owned());
        args.push(utf8(&fixture.roster)?);
    }
    args.extend(tail.iter().copied().map(str::to_owned));
    Ok(args)
}

fn endpoint(fixture: &Fixture, url: &str) -> Result<Vec<String>> {
    Ok(vec![
        "--ca-url".to_owned(),
        canonical_url(url)?,
        "--root".to_owned(),
        utf8(&std::fs::canonicalize(&fixture.root)?)?,
        "--provisioner-password-file".to_owned(),
        utf8(&std::fs::canonicalize(&fixture.password)?)?,
    ])
}

fn strip_flag(args: &[String], flag: &str) -> Vec<String> {
    let mut output = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if arg == flag {
            skip = true;
            continue;
        }
        output.push(arg.clone());
    }
    output
}

fn replace_flag(args: &mut [String], flag: &str, value: &str) {
    let mut pairs = args.iter_mut();
    while let Some(arg) = pairs.next() {
        if arg == flag
            && let Some(slot) = pairs.next()
        {
            value.clone_into(slot);
        }
    }
}

async fn roster_failure(fixture: &Fixture, text: &str, needle: &str) -> Result {
    let path = fixture
        .home
        .path()
        .join(format!("roster-{}.txt", needle.len()));
    std::fs::write(&path, text)?;
    let mut args = command(fixture, false, &["token", "node", "rec-node.example.test"])?;
    args.push("--roster".to_owned());
    args.push(utf8(&path)?);
    let report = fixture.run(args).await?;
    assert_eq!(report.exit, Exit::Failure, "{needle}: {}", report.stderr);
    assert!(report.stderr.contains(needle), "{}", report.stderr);
    assert!(!fixture.argv.exists(), "{needle}");
    Ok(())
}

fn names(directory: &Path) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let name = entry?.file_name();
        let text = name.to_str().ok_or("non-UTF8 directory entry")?;
        found.push(text.to_owned());
    }
    found.sort();
    Ok(found)
}
