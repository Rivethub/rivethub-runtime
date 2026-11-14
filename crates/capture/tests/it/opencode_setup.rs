use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use capture::harnesses::HarnessSetupService;
use capture::harnesses::SetupChange;
use chrono::TimeZone;
use chrono::Utc;
use config::Environment;
use kernel::harness_setup::ConnectPart;
use kernel::harness_setup::HarnessProbe;
use kernel::harness_setup::SetupError;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const RH: &str = "/opt/rh/bin/rh";
const PLUGIN: &str = ".config/opencode/plugins/rivethub-capture.ts";
const OLD_PLUGIN: &str = "export default {\n  id: 'rivethub-capture',\n  setup: () => {},\n}\nconst COMMAND: string = \"printf \\\"%s\\\" \\\"$1\\\" | exec /opt/rh/bin/rh capture hook --harness opencode\"\n";

struct Machine {
    home: tempfile::TempDir,
    service: HarnessSetupService,
}

fn opencode_answering(script: &str) -> Result<Machine, Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let bin = home.path().join("bin");
    fs::create_dir_all(&bin)?;
    let opencode = bin.join("opencode");
    fs::write(&opencode, format!("#!/bin/sh\n{script}\n"))?;
    fs::set_permissions(&opencode, fs::Permissions::from_mode(0o755))?;
    fs::create_dir_all(home.path().join(".config/opencode"))?;
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

fn opencode_version(version: &str) -> Result<Machine, Box<dyn std::error::Error>> {
    opencode_answering(&format!("echo '{version}'"))
}

impl Machine {
    fn probe(&self) -> Result<HarnessProbe, Box<dyn std::error::Error>> {
        self.service
            .probe_now()
            .into_iter()
            .find(|probe| probe.id == "opencode")
            .ok_or_else(|| "opencode is not in the registry".into())
    }

    fn connect(&self, part: Option<ConnectPart>) -> Result<SetupChange, SetupError> {
        let now = Utc
            .with_ymd_and_hms(2026, 11, 14, 12, 0, 0)
            .single()
            .unwrap_or_default();
        self.service.connect_now("opencode", part, false, now)
    }

    fn plugin(&self) -> Option<String> {
        fs::read_to_string(self.home.path().join(PLUGIN)).ok()
    }

    fn write_plugin(&self, text: &str) -> TestResult {
        let path = self.home.path().join(PLUGIN);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)?;
        Ok(())
    }
}

#[test]
fn the_installed_plugin_offers_an_entry_point_to_each_opencode_line() -> TestResult {
    let machine = opencode_version("1.19.5")?;
    machine.connect(Some(ConnectPart::Capture))?;
    let plugin = machine.plugin().ok_or("no plug-in written")?;
    assert!(plugin.contains("setup: (ctx: Context)"));
    assert!(plugin.contains("server: async (input?: ServerInput)"));
    assert!(plugin.contains("'session.idle'"));
    assert!(plugin.contains("session.execution.interrupted"));
    assert!(plugin.contains(&format!("exec {RH} capture hook --harness opencode")));
    Ok(())
}

#[test]
fn both_version_formats_are_read_and_named_by_connect_and_status() -> TestResult {
    for (answer, version) in [("1.19.5", "1.19.5"), ("opencode v2.3.0", "2.3.0")] {
        let machine = opencode_version(answer)?;
        let change = machine.connect(Some(ConnectPart::Capture))?;
        assert_eq!(
            change.notes,
            vec![format!(
                "OpenCode {version} loads this plug-in when it starts; rh has checked capture on OpenCode 1.3.4, 1.19.5, 2.0.0, 2.2.5 and 2.3.0."
            )]
        );
        let probe = machine.probe()?;
        assert_eq!(probe.version.as_deref(), Some(version));
        assert!(probe.capture_installed);
        assert!(
            probe
                .install_detail
                .ends_with(&format!("rivethub-capture.ts for OpenCode {version}")),
            "{}",
            probe.install_detail
        );
    }
    Ok(())
}

#[test]
fn an_opencode_older_than_1_3_4_gets_recall_only_and_a_status_that_says_why() -> TestResult {
    let machine = opencode_version("1.3.3")?;
    let change = machine.connect(None)?;
    assert!(matches!(
        change.skipped,
        Some(SetupError::Unsupported {
            part: ConnectPart::Capture,
            ..
        })
    ));
    assert_eq!(change.notes, Vec::<String>::new());
    assert_eq!(machine.plugin(), None);
    let refused = machine.connect(Some(ConnectPart::Capture));
    assert!(matches!(refused, Err(SetupError::Unsupported { .. })));

    machine.write_plugin(OLD_PLUGIN)?;
    let probe = machine.probe()?;
    assert_eq!(probe.version.as_deref(), Some("1.3.3"));
    assert!(!probe.capture_installed);
    assert!(!probe.can_connect);
    assert_eq!(
        probe.install_detail,
        "OpenCode 1.3.3 cannot load rh's capture plug-in; update OpenCode to 1.3.4 or later and run `rh harness connect opencode`"
    );
    Ok(())
}

#[test]
fn a_plugin_from_an_older_rh_is_reported_out_of_date_and_connect_replaces_it() -> TestResult {
    let machine = opencode_version("1.19.5")?;
    machine.write_plugin(OLD_PLUGIN)?;
    let stale = machine.probe()?;
    assert!(!stale.capture_installed);
    assert!(stale.can_connect);
    assert!(
        stale.install_detail.contains("is out of date"),
        "{}",
        stale.install_detail
    );

    let replaced = machine.connect(Some(ConnectPart::Capture))?;
    assert_ne!(replaced.outcome.backup_path, "");
    let plugin = machine.plugin().ok_or("no plug-in written")?;
    assert!(plugin.contains("server: async"));
    assert!(machine.probe()?.capture_installed);

    let again = machine.connect(Some(ConnectPart::Capture))?;
    assert_eq!(again.outcome.diff, "");
    Ok(())
}

#[test]
fn an_opencode_whose_version_cannot_be_read_gets_no_version_claim() -> TestResult {
    for script in [
        "echo '0.0.0-dev-202611092046'",
        "exit 3",
        "echo 'not a version'",
    ] {
        let machine = opencode_answering(script)?;
        let change = machine.connect(Some(ConnectPart::Capture))?;
        assert_eq!(
            change.notes,
            vec!["rh could not read the OpenCode version; this plug-in loads on OpenCode 1.3.4 and later (checked on 1.3.4, 1.19.5, 2.0.0, 2.2.5 and 2.3.0).".to_owned()]
        );
        let probe = machine.probe()?;
        assert_eq!(probe.version, None);
        assert!(probe.capture_installed);
        assert!(!probe.install_detail.contains("for OpenCode"));
    }
    Ok(())
}

#[test]
fn a_newer_major_line_is_connected_and_named_as_unchecked() -> TestResult {
    let machine = opencode_version("3.0.1")?;
    let change = machine.connect(Some(ConnectPart::Capture))?;
    assert!(machine.plugin().is_some());
    assert!(change.notes.iter().any(|note| note.starts_with(
        "OpenCode 3.0.1 is newer than the versions rh has checked (1.3.4, 1.19.5, 2.0.0, 2.2.5 and 2.3.0)"
    )));
    let probe = machine.probe()?;
    assert!(probe.capture_installed);
    assert!(
        probe
            .install_detail
            .contains("for OpenCode 3.0.1, newer than the versions rh has checked")
    );
    Ok(())
}

#[test]
fn an_opencode_that_never_answers_is_given_up_on() -> TestResult {
    let machine = opencode_answering("exec /bin/sleep 60")?;
    let started = Instant::now();
    let probe = machine.probe()?;
    assert!(started.elapsed() < Duration::from_secs(15));
    assert_eq!(probe.version, None);
    assert!(probe.found);
    Ok(())
}

#[test]
fn recall_alone_says_nothing_about_the_plugin() -> TestResult {
    let machine = opencode_version("1.19.5")?;
    let change = machine.connect(Some(ConnectPart::Recall))?;
    assert_eq!(change.notes, Vec::<String>::new());
    assert_eq!(machine.plugin(), None);
    Ok(())
}
