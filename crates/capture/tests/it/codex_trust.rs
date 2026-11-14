use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use capture::harnesses::HarnessSetupService;
use capture::harnesses::SetupChange;
use chrono::TimeZone;
use chrono::Utc;
use config::Environment;
use kernel::harness_setup::HarnessProbe;
use kernel::harness_setup::SetupError;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const RH: &str = "/opt/rh/bin/rh";
const STOP_HASH: &str = "sha256:55bf93377473d62205c771b08faf86810bc0095a455546f01f0dbdb63861b0e3";
const SESSION_END_HASH: &str =
    "sha256:16a43a02836bcd17972221e7a98efc19cc30ba0a6547a4ed538e56369f77d875";
const WAITING: &str = "waiting for you to trust it in Codex: the next time you open `codex` it shows \"Hooks need review\"; choose \"Trust all and continue\"";

struct Machine {
    home: tempfile::TempDir,
    service: HarnessSetupService,
}

fn codex_answering(version: &str) -> Result<Machine, Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let bin = home.path().join("bin");
    fs::create_dir_all(&bin)?;
    let codex = bin.join("codex");
    fs::write(&codex, format!("#!/bin/sh\necho '{version}'\n"))?;
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o755))?;
    fs::create_dir_all(home.path().join(".codex"))?;
    let env = Environment::from_vars(vec![
        (OsString::from("HOME"), home.path().as_os_str().to_owned()),
        (OsString::from("PATH"), bin.as_os_str().to_owned()),
    ]);
    let service = HarnessSetupService::new(
        &env,
        PathBuf::from(RH),
        home.path().join(".rivethub").join("backups"),
    );
    Ok(Machine { home, service })
}

fn connected_codex(version: &str) -> Result<(Machine, SetupChange), Box<dyn std::error::Error>> {
    let machine = codex_answering(version)?;
    let change = machine.connect()?;
    Ok((machine, change))
}

impl Machine {
    fn hooks_path(&self) -> String {
        self.home
            .path()
            .join(".codex/hooks.json")
            .display()
            .to_string()
    }

    fn connect(&self) -> Result<SetupChange, SetupError> {
        let now = Utc
            .with_ymd_and_hms(2026, 11, 14, 12, 0, 0)
            .single()
            .unwrap_or_default();
        self.service.connect_now("codex", None, false, now)
    }

    fn probe(&self) -> Result<HarnessProbe, Box<dyn std::error::Error>> {
        self.service
            .probe_now()
            .into_iter()
            .find(|probe| probe.id == "codex")
            .ok_or_else(|| "codex is not in the registry".into())
    }

    fn write_config(&self, states: &[(&str, &str)]) -> TestResult {
        let mut text = "model = \"gpt-5\"\n\n[mcp_servers.rivethub]\ncommand = \"/opt/rh/bin/rh\"\nargs = [\"mcp\"]\n".to_owned();
        for (event, body) in states {
            let key = format!("{}:{event}:0:0", self.hooks_path());
            write!(text, "\n[hooks.state.\"{key}\"]\n{body}\n")?;
        }
        fs::write(self.home.path().join(".codex/config.toml"), text)?;
        Ok(())
    }

    fn config(&self) -> Option<String> {
        fs::read_to_string(self.home.path().join(".codex/config.toml")).ok()
    }
}

fn trusted(hash: &str) -> String {
    format!("trusted_hash = \"{hash}\"")
}

#[test]
fn hooks_codex_has_not_trusted_are_reported_as_waiting_and_connect_says_what_to_do() -> TestResult {
    let (machine, change) = connected_codex("codex-cli 0.169.0")?;
    let config = machine.config().ok_or("recall wrote no config.toml")?;
    assert!(config.contains("[mcp_servers.rivethub]"));
    assert!(!config.contains("hooks.state"));
    let note = change.notes.first().ok_or("connect gave no note")?;
    assert!(note.contains("\"Hooks need review\""), "{note}");
    assert!(note.contains("\"Trust all and continue\""), "{note}");
    assert!(
        note.contains("`codex exec` runs are not captured"),
        "{note}"
    );

    let probe = machine.probe()?;
    assert_eq!(probe.version.as_deref(), Some("0.169.0"));
    assert!(!probe.capture_installed);
    assert!(!probe.can_connect);
    assert!(
        probe.install_detail.contains(WAITING),
        "{}",
        probe.install_detail
    );
    Ok(())
}

#[test]
fn hooks_trusted_with_codexs_own_hashes_read_as_trusted() -> TestResult {
    let (machine, _) = connected_codex("codex-cli 0.169.0")?;
    machine.write_config(&[
        ("session_end", &trusted(SESSION_END_HASH)),
        ("stop", &trusted(STOP_HASH)),
    ])?;
    let probe = machine.probe()?;
    assert!(probe.capture_installed);
    assert!(
        probe
            .install_detail
            .ends_with("hooks.json, trusted in Codex"),
        "{}",
        probe.install_detail
    );
    let again = machine.connect()?;
    assert_eq!(again.outcome.diff, "");
    assert_eq!(again.notes, Vec::<String>::new());
    Ok(())
}

#[test]
fn a_hash_that_no_longer_matches_reads_as_changed_since_trusted() -> TestResult {
    let stale = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
    let (machine, _) = connected_codex("codex-cli 0.169.0")?;
    machine.write_config(&[
        ("session_end", &trusted(SESSION_END_HASH)),
        ("stop", &trusted(stale)),
    ])?;
    let probe = machine.probe()?;
    assert!(!probe.capture_installed);
    assert!(
        probe
            .install_detail
            .contains("changed since you trusted it in Codex"),
        "{}",
        probe.install_detail
    );
    Ok(())
}

#[test]
fn one_hook_trusted_and_the_other_new_still_waits() -> TestResult {
    let (machine, _) = connected_codex("codex-cli 0.169.0")?;
    machine.write_config(&[("stop", &trusted(STOP_HASH))])?;
    let probe = machine.probe()?;
    assert!(!probe.capture_installed);
    assert!(
        probe.install_detail.contains(WAITING),
        "{}",
        probe.install_detail
    );
    Ok(())
}

#[test]
fn a_hook_turned_off_in_codex_says_so() -> TestResult {
    let (machine, _) = connected_codex("codex-cli 0.169.0")?;
    machine.write_config(&[
        ("session_end", &trusted(SESSION_END_HASH)),
        ("stop", &format!("enabled = false\n{}", trusted(STOP_HASH))),
    ])?;
    let probe = machine.probe()?;
    assert!(!probe.capture_installed);
    assert!(probe.install_detail.contains("turned off in Codex"));
    Ok(())
}

#[test]
fn rh_says_nothing_about_trust_when_it_cannot_tell() -> TestResult {
    let stale = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
    let cases: [(&str, Vec<(&str, String)>); 4] = [
        ("codex-cli 0.128.0", Vec::new()),
        ("not a version", Vec::new()),
        (
            "codex-cli 0.150.0",
            vec![
                ("session_end", trusted(SESSION_END_HASH)),
                ("stop", trusted(stale)),
            ],
        ),
        (
            "codex-cli 0.169.0",
            vec![("stop", "trusted_hash = [".to_owned())],
        ),
    ];
    for (version, states) in cases {
        let (machine, _) = connected_codex(version)?;
        let states: Vec<(&str, &str)> = states
            .iter()
            .map(|(event, body)| (*event, body.as_str()))
            .collect();
        if !states.is_empty() {
            machine.write_config(&states)?;
        }
        let probe = machine.probe()?;
        assert!(probe.capture_installed, "{version}");
        assert!(
            probe.install_detail.ends_with(".codex/hooks.json"),
            "{version}: {}",
            probe.install_detail
        );
    }
    Ok(())
}

#[test]
fn a_trusted_hook_from_an_older_codex_with_the_same_hash_still_reads_as_trusted() -> TestResult {
    let (machine, _) = connected_codex("codex-cli 0.140.0")?;
    machine.write_config(&[
        ("session_end", &trusted(SESSION_END_HASH)),
        ("stop", &trusted(STOP_HASH)),
    ])?;
    let probe = machine.probe()?;
    assert!(probe.install_detail.ends_with("trusted in Codex"));
    Ok(())
}
