use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use capture::harnesses::HarnessSetupService;
use capture::harnesses::REGISTRY;
use capture::harnesses::SetupChange;
use chrono::TimeZone;
use chrono::Utc;
use config::Environment;
use kernel::harness_setup::CaptureMethod;
use kernel::harness_setup::ConnectPart;
use kernel::harness_setup::HarnessProbe;
use kernel::harness_setup::RecallState;
use kernel::harness_setup::SetupError;
use serde::Deserialize;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const RH: &str = "/opt/rh/bin/rh";
const SETTINGS: &str = "{\n  \"theme\": \"dark\",\n  \"hooks\": {\n    \"Stop\": [\n      {\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"/usr/bin/notify\"\n          }\n        ]\n      }\n    ]\n  }\n}\n";

struct Machine {
    home: tempfile::TempDir,
    service: HarnessSetupService,
}

const PLUGIN_TEMPLATE: &str =
    "export default { id: 'rivethub-capture' }\nconst COMMAND: string = '__COMMAND__'\n";

fn machine() -> Result<Machine, Box<dyn std::error::Error>> {
    machine_with_plugin(true)
}

fn machine_with_plugin(plugin: bool) -> Result<Machine, Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let bin = home.path().join("bin");
    fs::create_dir_all(&bin)?;
    let source = home.path().join("plugin-source.ts");
    fs::write(&source, PLUGIN_TEMPLATE)?;
    let mut vars = vec![
        (OsString::from("HOME"), home.path().as_os_str().to_owned()),
        (OsString::from("PATH"), bin.as_os_str().to_owned()),
    ];
    if plugin {
        vars.push((
            OsString::from("RIVETHUB_OPENCODE_PLUGIN"),
            source.into_os_string(),
        ));
    }
    let env = Environment::from_vars(vars);
    let service = HarnessSetupService::new(
        &env,
        PathBuf::from(RH),
        home.path().join(".rivethub").join("backups"),
    );
    Ok(Machine { home, service })
}

impl Machine {
    fn path(&self, relative: &str) -> PathBuf {
        self.home.path().join(relative)
    }

    fn write(&self, relative: &str, text: &str) -> TestResult {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)?;
        Ok(())
    }

    fn read(&self, relative: &str) -> Result<String, Box<dyn std::error::Error>> {
        Ok(fs::read_to_string(self.path(relative))?)
    }

    fn probe(&self, id: &str) -> Result<HarnessProbe, Box<dyn std::error::Error>> {
        self.service
            .probe_now()
            .into_iter()
            .find(|probe| probe.id == id)
            .ok_or_else(|| "harness is not in the registry".into())
    }

    fn connect(
        &self,
        id: &str,
        part: Option<ConnectPart>,
        dry_run: bool,
    ) -> Result<kernel::harness_setup::ConnectOutcome, SetupError> {
        self.change(id, part, dry_run).map(|change| change.outcome)
    }

    fn change(
        &self,
        id: &str,
        part: Option<ConnectPart>,
        dry_run: bool,
    ) -> Result<SetupChange, SetupError> {
        let now = Utc
            .with_ymd_and_hms(2026, 11, 6, 12, 0, 0)
            .single()
            .unwrap_or_default();
        self.service.connect_now(id, part, dry_run, now)
    }

    fn backups(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.path(".rivethub/backups"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

#[derive(Deserialize)]
struct Written {
    theme: Option<String>,
    #[serde(default)]
    hooks: BTreeMap<String, Vec<Group>>,
    #[serde(default, rename = "mcpServers")]
    servers: BTreeMap<String, Server>,
    #[serde(default)]
    mcp: BTreeMap<String, Server>,
}

#[derive(Deserialize)]
struct Group {
    matcher: Option<String>,
    hooks: Vec<Run>,
}

#[derive(Deserialize)]
struct Run {
    command: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Deserialize)]
struct Server {
    r#type: Option<String>,
    command: ServerCommand,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ServerCommand {
    Line(String),
    Argv(Vec<String>),
}

fn json_text(text: &str) -> Result<Written, serde_json::Error> {
    serde_json::from_str(text)
}

impl Written {
    fn run(&self, event: &str, group: usize) -> Option<&Run> {
        self.hooks
            .get(event)
            .and_then(|groups| groups.get(group))
            .and_then(|group| group.hooks.first())
    }
}

#[test]
fn the_registry_has_one_row_per_harness_with_the_ranked_methods() {
    let methods = |id: &str| {
        REGISTRY
            .iter()
            .find(|spec| spec.id == id)
            .and_then(|spec| spec.capture.first().copied())
    };
    let ids: Vec<&str> = REGISTRY.iter().map(|spec| spec.id).collect();
    assert_eq!(
        ids,
        [
            "claude-code",
            "codex",
            "grok-build",
            "opencode",
            "cursor",
            "dsh",
            "cowork",
            "kimi",
            "omp",
            "hermes",
            "antigravity",
            "kiro"
        ]
    );
    for id in ["claude-code", "codex", "grok-build", "dsh"] {
        assert_eq!(methods(id), Some(CaptureMethod::Hook), "{id}");
    }
    assert_eq!(methods("opencode"), Some(CaptureMethod::Plugin));
    for id in ["cursor", "cowork", "kimi", "omp", "hermes"] {
        assert_eq!(methods(id), Some(CaptureMethod::StoreScan), "{id}");
    }
    for id in ["antigravity", "kiro"] {
        assert_eq!(methods(id), Some(CaptureMethod::ClientPush), "{id}");
    }
}

#[test]
fn an_empty_machine_finds_nothing_and_offers_nothing() -> TestResult {
    let machine = machine()?;
    for probe in machine.service.probe_now() {
        assert!(!probe.found, "{}", probe.id);
        assert!(!probe.can_connect, "{}", probe.id);
        assert!(!probe.capture_installed, "{}", probe.id);
        assert_eq!(
            probe.recall.state,
            RecallState::NotConnected,
            "{}",
            probe.id
        );
    }
    Ok(())
}

#[test]
fn a_binary_on_the_path_is_found_and_reported_with_its_path() -> TestResult {
    let machine = machine()?;
    machine.write("bin/claude", "#!/bin/sh\n")?;
    let probe = machine.probe("claude-code")?;
    assert!(probe.found);
    assert_eq!(
        probe.path,
        Some(machine.path("bin/claude").display().to_string())
    );
    assert!(probe.can_connect);
    Ok(())
}

#[test]
fn connecting_claude_capture_adds_the_hooks_next_to_the_users_own() -> TestResult {
    let machine = machine()?;
    machine.write(".claude/settings.json", SETTINGS)?;
    let outcome = machine.connect("claude-code", Some(ConnectPart::Capture), false)?;
    let after = machine.read(".claude/settings.json")?;
    let settings = json_text(&after)?;
    let events = [
        "UserPromptSubmit",
        "PostToolUse",
        "Stop",
        "SubagentStop",
        "SessionEnd",
    ];
    for event in events {
        assert!(settings.hooks.contains_key(event), "{event}");
    }
    assert_eq!(settings.theme.as_deref(), Some("dark"));
    assert_eq!(settings.hooks.get("Stop").map(Vec::len), Some(2));
    assert_eq!(
        settings.run("Stop", 1).map(|run| run.command.as_str()),
        Some("/opt/rh/bin/rh capture hook")
    );
    assert_eq!(
        settings
            .hooks
            .get("PostToolUse")
            .and_then(|groups| groups.first())
            .and_then(|group| group.matcher.as_deref()),
        Some("*")
    );
    assert!(
        outcome
            .diff
            .contains("+            \"command\": \"/opt/rh/bin/rh capture hook\"")
    );
    assert!(outcome.diff.starts_with("--- a/"));
    assert_eq!(machine.backups().len(), 1);
    assert_eq!(
        fs::read_to_string(&outcome.backup_path)?,
        SETTINGS,
        "the backup holds the file as it was"
    );
    assert!(machine.probe("claude-code")?.capture_installed);
    Ok(())
}

#[test]
fn connecting_twice_changes_nothing_the_second_time() -> TestResult {
    let machine = machine()?;
    machine.write(".claude/settings.json", SETTINGS)?;
    machine.write(".codex/config.toml", "model = \"x\"\n")?;
    for (id, part, file) in [
        ("claude-code", ConnectPart::Capture, ".claude/settings.json"),
        ("claude-code", ConnectPart::Recall, ".claude.json"),
        ("codex", ConnectPart::Capture, ".codex/hooks.json"),
        ("codex", ConnectPart::Recall, ".codex/config.toml"),
    ] {
        machine.connect(id, Some(part), false)?;
        let once = machine.read(file)?;
        let backups = machine.backups().len();
        let again = machine.connect(id, Some(part), false)?;
        assert_eq!(machine.read(file)?, once, "{id} {part}");
        assert_eq!(again.diff, "", "{id} {part}");
        assert_eq!(again.backup_path, "", "{id} {part}");
        assert_eq!(machine.backups().len(), backups, "{id} {part}");
    }
    Ok(())
}

#[test]
fn a_dry_run_prints_the_diff_and_writes_nothing() -> TestResult {
    let machine = machine()?;
    machine.write(".claude/settings.json", SETTINGS)?;
    let outcome = machine.connect("claude-code", None, true)?;
    assert!(outcome.diff.contains("capture hook"));
    assert!(outcome.diff.contains("mcpServers"));
    assert_eq!(outcome.backup_path, "");
    assert_eq!(machine.read(".claude/settings.json")?, SETTINGS);
    assert!(!machine.path(".claude.json").exists());
    assert_eq!(machine.backups(), Vec::<String>::new());
    Ok(())
}

#[test]
fn recall_is_recognised_when_an_entry_runs_rh_mcp_through_a_wrapper() -> TestResult {
    let machine = machine()?;
    machine.write(".claude/.keep", "")?;
    machine.write(
        ".claude.json",
        "{\"mcpServers\": {\"rivetos\": {\"type\": \"stdio\", \"command\": \"bash\", \"args\": [\"-c\", \"set -a; . \\\"$HOME/.rivethub/.env\\\"; set +a; exec \\\"$HOME/.local/bin/rh\\\" mcp\"]}}}\n",
    )?;
    let probe = machine.probe("claude-code")?;
    assert_eq!(probe.recall.state, RecallState::Connected);
    let again = machine.connect("claude-code", Some(ConnectPart::Recall), false)?;
    assert_eq!(again.diff, "");
    Ok(())
}

#[test]
fn another_memory_server_under_the_same_name_reads_as_elsewhere() -> TestResult {
    let machine = machine()?;
    machine.write(
        ".cursor/mcp.json",
        "{\"mcpServers\": {\"rivetos\": {\"url\": \"https://memory.example.com/mcp\"}}}\n",
    )?;
    assert_eq!(
        machine.probe("cursor")?.recall.state,
        RecallState::Elsewhere
    );
    Ok(())
}

#[test]
fn connecting_recall_edits_each_format_in_place() -> TestResult {
    let machine = machine()?;
    machine.write(
        ".cursor/mcp.json",
        "{\n  \"mcpServers\": {\n    \"other\": {\n      \"command\": \"x\"\n    }\n  }\n}\n",
    )?;
    machine.write(
        ".config/opencode/opencode.json",
        "{\n  \"theme\": \"x\"\n}\n",
    )?;
    machine.write(
        ".grok/config.toml",
        "[mcp_servers.other]\ncommand = \"x\"\n",
    )?;
    machine.connect("cursor", Some(ConnectPart::Recall), false)?;
    machine.connect("opencode", Some(ConnectPart::Recall), false)?;
    machine.connect("grok-build", Some(ConnectPart::Recall), false)?;
    let cursor = json_text(&machine.read(".cursor/mcp.json")?)?;
    assert!(matches!(
        cursor.servers.get("other").map(|server| &server.command),
        Some(ServerCommand::Line(line)) if line == "x"
    ));
    let ours = cursor.servers.get("rivethub").ok_or("the rivethub entry")?;
    assert!(matches!(&ours.command, ServerCommand::Line(line) if line == RH));
    assert_eq!(ours.args, ["mcp"]);
    assert_eq!(ours.r#type, None);
    let opencode = json_text(&machine.read(".config/opencode/opencode.json")?)?;
    let local = opencode.mcp.get("rivethub").ok_or("the rivethub entry")?;
    assert_eq!(local.r#type.as_deref(), Some("local"));
    assert!(matches!(&local.command, ServerCommand::Argv(argv) if argv == &[RH, "mcp"]));
    let grok = machine.read(".grok/config.toml")?;
    assert!(grok.starts_with("[mcp_servers.other]\ncommand = \"x\"\n\n[mcp_servers.rivethub]\n"));
    let parsed: toml::Table = toml::from_str(&grok)?;
    assert!(
        parsed
            .get("mcp_servers")
            .and_then(|servers| servers.get("rivethub"))
            .and_then(|entry| entry.get("args"))
            .is_some_and(toml::Value::is_array)
    );
    for (id, state) in [
        ("cursor", RecallState::Connected),
        ("opencode", RecallState::Connected),
        ("grok-build", RecallState::Connected),
    ] {
        assert_eq!(machine.probe(id)?.recall.state, state, "{id}");
    }
    Ok(())
}

#[test]
fn grok_capture_writes_its_own_hook_file_and_opencode_its_plugin() -> TestResult {
    let machine = machine()?;
    machine.write(".grok/.keep", "")?;
    machine.write(".config/opencode/.keep", "")?;
    machine.connect("grok-build", Some(ConnectPart::Capture), false)?;
    machine.connect("opencode", Some(ConnectPart::Capture), false)?;
    let hooks = json_text(&machine.read(".grok/hooks/rivethub-capture.json")?)?;
    assert_eq!(
        hooks.run("PreCompact", 0).map(|run| run.args.clone()),
        Some(vec![
            "capture".to_owned(),
            "hook".to_owned(),
            "--harness".to_owned(),
            "grok-build".to_owned()
        ])
    );
    assert_eq!(
        hooks.run("Stop", 0).map(|run| run.command.as_str()),
        Some(RH)
    );
    let plugin = machine.read(".config/opencode/plugins/rivethub-capture.ts")?;
    assert!(
        plugin.starts_with("export default { id: 'rivethub-capture' }\nconst COMMAND: string = ")
    );
    assert!(plugin.contains("capture hook --harness opencode"));
    assert!(plugin.contains(RH));
    assert!(!plugin.contains("__COMMAND__"));
    assert!(machine.probe("grok-build")?.capture_installed);
    assert!(machine.probe("opencode")?.capture_installed);
    let again = machine.connect("grok-build", Some(ConnectPart::Capture), false)?;
    assert_eq!(again.diff, "");
    Ok(())
}

#[test]
fn an_oddly_formatted_config_is_backed_up_exactly_and_rewritten_as_two_space_json() -> TestResult {
    let machine = machine()?;
    let odd = "{\"zeta\":1,\n\t\"mcpServers\":{\"other\":{\"command\":\"x\"}},   \"alpha\":[1,2]}";
    machine.write(".cursor/mcp.json", odd)?;
    let connected = machine.connect("cursor", Some(ConnectPart::Recall), false)?;
    assert_eq!(fs::read_to_string(&connected.backup_path)?, odd);
    let after = machine.read(".cursor/mcp.json")?;
    assert!(after.starts_with("{\n  \"zeta\": 1,\n  \"mcpServers\": {\n    \"other\""));
    assert!(after.ends_with("\n}\n"));
    assert!(after.contains("\"rivethub\""));
    let again = machine.connect("cursor", Some(ConnectPart::Recall), false)?;
    assert_eq!(again.diff, "");
    let now = Utc
        .with_ymd_and_hms(2026, 11, 6, 13, 0, 0)
        .single()
        .unwrap_or_default();
    machine
        .service
        .disconnect_now("cursor", Some(ConnectPart::Recall), false, now)?;
    let restored = machine.read(".cursor/mcp.json")?;
    assert_eq!(
        restored,
        "{\n  \"zeta\": 1,\n  \"mcpServers\": {\n    \"other\": {\n      \"command\": \"x\"\n    }\n  },\n  \"alpha\": [\n    1,\n    2\n  ]\n}\n"
    );
    Ok(())
}

#[test]
fn opencode_capture_without_an_override_installs_the_plugin_embedded_in_rh() -> TestResult {
    let machine = machine_with_plugin(false)?;
    machine.write(".config/opencode/.keep", "")?;
    let connected = machine.change("opencode", Some(ConnectPart::Capture), false)?;
    assert!(connected.skipped.is_none());
    let plugin = machine.read(".config/opencode/plugins/rivethub-capture.ts")?;
    assert!(plugin.contains("id: 'rivethub-capture'"));
    assert!(plugin.contains("ctx.event.subscribe({ signal })"));
    assert!(plugin.contains(&format!("exec {RH} capture hook --harness opencode")));
    assert!(!plugin.contains("__COMMAND__"));
    assert!(machine.probe("opencode")?.capture_installed);
    Ok(())
}

#[test]
fn an_opencode_override_that_cannot_be_read_names_its_path() -> TestResult {
    let machine = machine()?;
    machine.write(".config/opencode/.keep", "")?;
    fs::remove_file(machine.path("plugin-source.ts"))?;
    let error = machine
        .connect("opencode", Some(ConnectPart::Capture), false)
        .err()
        .ok_or("connect should refuse")?;
    assert!(matches!(error, SetupError::Io { .. }));
    assert!(error.to_string().contains("plugin-source.ts"), "{error}");
    assert!(!machine.path(".config/opencode/plugins").exists());
    Ok(())
}

#[test]
fn connecting_both_parts_names_the_part_rh_cannot_write() -> TestResult {
    let machine = machine()?;
    machine.write(".cursor/.keep", "")?;
    let change = machine.change("cursor", None, false)?;
    assert!(matches!(
        change.skipped,
        Some(SetupError::Unsupported {
            part: ConnectPart::Capture,
            ..
        })
    ));
    assert!(change.outcome.diff.contains("rivethub"));
    let opencode = machine_with_plugin(false)?;
    opencode.write(".config/opencode/.keep", "")?;
    let both = opencode.change("opencode", None, false)?;
    assert!(both.skipped.is_none());
    assert!(opencode.probe("opencode")?.capture_installed);
    Ok(())
}

#[test]
fn disconnecting_removes_only_rhs_entries_and_leaves_the_rest_as_it_was() -> TestResult {
    let machine = machine()?;
    machine.write(".claude/settings.json", SETTINGS)?;
    let mcp = "{\n  \"numStartups\": 3,\n  \"mcpServers\": {\n    \"other\": {\n      \"command\": \"x\"\n    }\n  }\n}\n";
    machine.write(".claude.json", mcp)?;
    machine.write(".codex/config.toml", "model = \"x\"\n")?;
    machine.write(".grok/.keep", "")?;
    machine.connect("claude-code", None, false)?;
    machine.connect("codex", None, false)?;
    machine.connect("grok-build", Some(ConnectPart::Capture), false)?;
    let now = Utc
        .with_ymd_and_hms(2026, 11, 6, 13, 0, 0)
        .single()
        .unwrap_or_default();
    machine
        .service
        .disconnect_now("claude-code", None, false, now)?;
    machine.service.disconnect_now("codex", None, false, now)?;
    machine
        .service
        .disconnect_now("grok-build", None, false, now)?;
    assert_eq!(machine.read(".claude/settings.json")?, SETTINGS);
    assert_eq!(machine.read(".claude.json")?, mcp);
    assert_eq!(machine.read(".codex/config.toml")?, "model = \"x\"\n");
    assert!(!machine.path(".grok/hooks/rivethub-capture.json").exists());
    assert!(!machine.probe("claude-code")?.capture_installed);
    Ok(())
}

#[test]
fn what_rh_cannot_write_is_refused_with_a_reason() -> TestResult {
    let machine = machine()?;
    machine.write(".hermes/config.yaml", "mcp_servers: {}\n")?;
    machine.write(".cursor/.keep", "")?;
    for (id, part) in [
        ("hermes", ConnectPart::Recall),
        ("cursor", ConnectPart::Capture),
    ] {
        assert!(
            matches!(
                machine.connect(id, Some(part), false),
                Err(SetupError::Unsupported { .. })
            ),
            "{id} {part}"
        );
    }
    assert!(matches!(
        machine.connect("nope", None, false),
        Err(SetupError::UnknownHarness(_))
    ));
    assert!(matches!(
        machine.connect("kiro", Some(ConnectPart::Recall), false),
        Err(SetupError::Unsupported { .. })
    ));
    Ok(())
}

#[test]
fn a_config_the_editor_cannot_parse_is_left_alone() -> TestResult {
    let machine = machine()?;
    machine.write(".cursor/mcp.json", "{ // comment\n}\n")?;
    assert!(
        machine
            .connect("cursor", Some(ConnectPart::Recall), false)
            .is_err()
    );
    assert_eq!(machine.read(".cursor/mcp.json")?, "{ // comment\n}\n");
    assert_eq!(machine.backups(), Vec::<String>::new());
    Ok(())
}

#[test]
fn hermes_recall_is_detected_from_its_yaml() -> TestResult {
    let machine = machine()?;
    machine.write(
        ".hermes/config.yaml",
        "mcp_servers:\n  memory:\n    command: /usr/local/bin/rh\n    args: [mcp]\n",
    )?;
    let probe = machine.probe("hermes")?;
    assert_eq!(probe.recall.state, RecallState::Connected);
    assert!(!probe.can_connect);
    Ok(())
}

#[test]
fn a_symlinked_config_keeps_its_link() -> TestResult {
    let machine = machine()?;
    machine.write("dotfiles/mcp.json", "{\n  \"mcpServers\": {}\n}\n")?;
    fs::create_dir_all(machine.path(".cursor"))?;
    std::os::unix::fs::symlink(
        machine.path("dotfiles/mcp.json"),
        machine.path(".cursor/mcp.json"),
    )?;
    machine.connect("cursor", Some(ConnectPart::Recall), false)?;
    assert!(
        fs::symlink_metadata(machine.path(".cursor/mcp.json"))?
            .file_type()
            .is_symlink()
    );
    assert!(machine.read("dotfiles/mcp.json")?.contains("rivethub"));
    Ok(())
}

fn fake_dsh(machine: &Machine) -> TestResult {
    let lib = machine.path("bin/node_modules/@deepseek-ai/dsh-mcp-client/lib");
    fs::create_dir_all(&lib)?;
    fs::write(machine.path("bin/dsh"), "")?;
    fs::write(lib.join("index.js"), "")?;
    Ok(())
}

fn dsh_patch(machine: &Machine) -> PathBuf {
    machine.path(".dsh/cordis.patch.yml")
}

#[test]
fn dsh_without_a_binary_connects_capture_and_skips_recall() -> TestResult {
    let machine = machine()?;
    machine.write(".dsh/.keep", "")?;
    let change = machine.change("dsh", None, false)?;
    assert!(matches!(
        change.skipped,
        Some(SetupError::Unsupported {
            part: ConnectPart::Recall,
            ..
        })
    ));
    let patch = machine.read(".dsh/cordis.patch.yml")?;
    assert!(patch.contains("rivethub-capture"), "{patch}");
    assert!(!patch.contains("rivethub-recall"), "{patch}");
    assert!(machine.path(".dsh/plugins/rivethub-capture.mjs").is_file());
    assert!(
        !machine
            .probe("dsh")?
            .recall
            .detail
            .contains("every profile")
    );
    Ok(())
}

#[test]
fn dsh_connect_wires_capture_and_recall_for_every_profile() -> TestResult {
    let machine = machine()?;
    fake_dsh(&machine)?;
    machine.write(".dsh/.keep", "")?;
    let outcome = machine.connect("dsh", None, false)?;
    assert!(
        outcome.diff.contains("rivethub-capture.mjs"),
        "{}",
        outcome.diff
    );
    let patch = machine.read(".dsh/cordis.patch.yml")?;
    let client = fs::canonicalize(
        machine.path("bin/node_modules/@deepseek-ai/dsh-mcp-client/lib/index.js"),
    )?;
    assert!(patch.contains(&client.display().to_string()), "{patch}");
    assert!(patch.contains("serverName: rivethub"), "{patch}");
    assert!(patch.contains("failOnStartupError: false"), "{patch}");
    assert!(patch.contains(RH), "{patch}");
    let plugin = machine.read(".dsh/plugins/rivethub-capture.mjs")?;
    assert!(plugin.contains("turn/end"), "{plugin}");
    assert!(plugin.contains("inject = ['sessions']"), "{plugin}");
    assert!(plugin.contains("capture hook --harness dsh"), "{plugin}");
    assert!(plugin.contains(RH), "{plugin}");
    assert!(!plugin.contains("__COMMAND__"), "{plugin}");
    let probe = machine.probe("dsh")?;
    assert!(probe.capture_installed);
    assert_eq!(probe.recall.state, RecallState::Connected);
    assert!(!probe.can_connect);
    assert_eq!(
        probe.install_detail,
        format!(
            "home patch {} covers every profile, including profiles created later",
            dsh_patch(&machine).display()
        )
    );
    machine.write(".dsh/profiles/headless/package.json", "{}\n")?;
    assert!(machine.probe("dsh")?.install_detail.contains("(headless)"));
    Ok(())
}

#[test]
fn dsh_dry_run_writes_nothing() -> TestResult {
    let machine = machine()?;
    fake_dsh(&machine)?;
    machine.write(".dsh/.keep", "")?;
    let outcome = machine.connect("dsh", None, true)?;
    assert!(
        outcome.diff.contains("cordis.patch.yml"),
        "{}",
        outcome.diff
    );
    assert_eq!(outcome.backup_path, "");
    assert!(!dsh_patch(&machine).exists());
    assert!(!machine.path(".dsh/plugins/rivethub-capture.mjs").exists());
    assert_eq!(machine.backups(), Vec::<String>::new());
    Ok(())
}

#[test]
fn dsh_connect_twice_changes_nothing_the_second_time() -> TestResult {
    let machine = machine()?;
    fake_dsh(&machine)?;
    machine.write(".dsh/cordis.patch.yml", "[]\n")?;
    let first = machine.connect("dsh", None, false)?;
    assert!(!first.backup_path.is_empty(), "{}", first.backup_path);
    let backups = machine.backups();
    let patch = machine.read(".dsh/cordis.patch.yml")?;
    let plugin = machine.read(".dsh/plugins/rivethub-capture.mjs")?;
    let second = machine.connect("dsh", None, false)?;
    assert!(second.diff.is_empty(), "{}", second.diff);
    assert_eq!(machine.backups(), backups);
    assert_eq!(machine.read(".dsh/cordis.patch.yml")?, patch);
    assert_eq!(machine.read(".dsh/plugins/rivethub-capture.mjs")?, plugin);
    Ok(())
}

#[test]
fn dsh_disconnect_restores_an_empty_patch_and_profile_files() -> TestResult {
    let machine = machine()?;
    fake_dsh(&machine)?;
    let package = "{\n  \"name\": \"headless\"\n}\n";
    let profile = "# keep\n[]\n";
    let generated = "services: {}\n";
    machine.write(".dsh/profiles/headless/package.json", package)?;
    machine.write(".dsh/profiles/headless/cordis.patch.yml", profile)?;
    machine.write(".dsh/profiles/headless/cordis.yml", generated)?;
    machine.write(".dsh/cordis.patch.yml", "[]\n")?;
    machine.connect("dsh", None, false)?;
    let now = Utc
        .with_ymd_and_hms(2026, 11, 6, 13, 0, 0)
        .single()
        .unwrap_or_default();
    machine.service.disconnect_now("dsh", None, false, now)?;
    assert_eq!(
        machine.read(".dsh/profiles/headless/package.json")?,
        package
    );
    assert_eq!(
        machine.read(".dsh/profiles/headless/cordis.patch.yml")?,
        profile
    );
    assert_eq!(
        machine.read(".dsh/profiles/headless/cordis.yml")?,
        generated
    );
    assert_eq!(machine.read(".dsh/cordis.patch.yml")?, "[]\n");
    assert!(!machine.path(".dsh/plugins/rivethub-capture.mjs").exists());
    assert!(!machine.probe("dsh")?.capture_installed);
    Ok(())
}

#[test]
fn dsh_disconnect_restores_comments_around_an_empty_patch() -> TestResult {
    let machine = machine()?;
    fake_dsh(&machine)?;
    let original = "# c\n[]\n";
    machine.write(".dsh/cordis.patch.yml", original)?;
    machine.connect("dsh", None, false)?;
    let now = Utc
        .with_ymd_and_hms(2026, 11, 6, 13, 0, 0)
        .single()
        .unwrap_or_default();
    machine.service.disconnect_now("dsh", None, false, now)?;
    assert_eq!(machine.read(".dsh/cordis.patch.yml")?, original);
    Ok(())
}

#[test]
fn dsh_connect_keeps_existing_patch_items() -> TestResult {
    let machine = machine()?;
    fake_dsh(&machine)?;
    let original = "- insert:\n    - id: mine\n      name: ./mine.mjs\n";
    machine.write(".dsh/cordis.patch.yml", original)?;
    machine.connect("dsh", None, false)?;
    let patched = machine.read(".dsh/cordis.patch.yml")?;
    assert!(patched.starts_with(original), "{patched}");
    assert!(patched.contains("rivethub-capture"), "{patched}");
    let now = Utc
        .with_ymd_and_hms(2026, 11, 6, 13, 0, 0)
        .single()
        .unwrap_or_default();
    machine.service.disconnect_now("dsh", None, false, now)?;
    assert_eq!(machine.read(".dsh/cordis.patch.yml")?, original);
    Ok(())
}

#[test]
fn dsh_leaves_a_patch_it_cannot_parse_untouched() -> TestResult {
    let machine = machine()?;
    machine.write(".dsh/cordis.patch.yml", "name: mine\n")?;
    assert!(
        machine
            .connect("dsh", Some(ConnectPart::Capture), false)
            .is_err()
    );
    assert_eq!(machine.read(".dsh/cordis.patch.yml")?, "name: mine\n");
    assert_eq!(machine.backups(), Vec::<String>::new());
    machine.write(".dsh/cordis.patch.yml", "[ ]\n")?;
    let error = machine
        .connect("dsh", Some(ConnectPart::Capture), false)
        .err()
        .ok_or("flow-style patch should be refused")?;
    assert!(error.to_string().contains("flow-style"), "{error}");
    assert_eq!(machine.read(".dsh/cordis.patch.yml")?, "[ ]\n");
    Ok(())
}

#[test]
fn dsh_capture_only_does_not_add_recall() -> TestResult {
    let machine = machine()?;
    fake_dsh(&machine)?;
    machine.write(".dsh/.keep", "")?;
    machine.connect("dsh", Some(ConnectPart::Capture), false)?;
    let patch = machine.read(".dsh/cordis.patch.yml")?;
    assert!(patch.contains("rivethub-capture"), "{patch}");
    assert!(!patch.contains("rivethub-recall"), "{patch}");
    machine.connect("dsh", Some(ConnectPart::Recall), false)?;
    let both = machine.read(".dsh/cordis.patch.yml")?;
    assert!(both.contains("rivethub-recall"), "{both}");
    assert!(both.contains("rivethub-capture"), "{both}");
    let third = machine.connect("dsh", None, false)?;
    assert!(third.diff.is_empty(), "{}", third.diff);
    Ok(())
}

#[test]
fn dsh_recall_only_does_not_write_the_plug_in() -> TestResult {
    let machine = machine()?;
    fake_dsh(&machine)?;
    machine.write(".dsh/.keep", "")?;
    machine.connect("dsh", Some(ConnectPart::Recall), false)?;
    let patch = machine.read(".dsh/cordis.patch.yml")?;
    assert!(patch.contains("rivethub-recall"), "{patch}");
    assert!(!patch.contains("rivethub-capture"), "{patch}");
    assert!(!machine.path(".dsh/plugins/rivethub-capture.mjs").exists());
    Ok(())
}

#[test]
fn dsh_recall_alone_is_an_error_when_rh_cannot_find_the_client() -> TestResult {
    let machine = machine()?;
    machine.write(".dsh/.keep", "")?;
    let missing = machine
        .connect("dsh", Some(ConnectPart::Recall), false)
        .err()
        .ok_or("recall should fail closed")?;
    assert!(
        missing.to_string().contains("cannot find the dsh binary"),
        "{missing}"
    );
    assert!(!dsh_patch(&machine).exists());
    fs::write(machine.path("bin/dsh"), "")?;
    let unpackaged = machine
        .connect("dsh", Some(ConnectPart::Recall), false)
        .err()
        .ok_or("recall should name the missing package")?;
    assert!(
        unpackaged
            .to_string()
            .contains("no @deepseek-ai/dsh-mcp-client"),
        "{unpackaged}"
    );
    assert!(!dsh_patch(&machine).exists());
    Ok(())
}

#[test]
fn dsh_status_names_profiles_the_home_patch_does_not_cover() -> TestResult {
    let machine = machine()?;
    machine.write(
        ".dsh/profiles/headless/plugin/index.mjs",
        "capture hook --harness dsh\n",
    )?;
    machine.write(
        ".dsh/profiles/headless/package.json",
        "{\n  \"dependencies\": {\n    \"cap\": \"link:./plugin\"\n  }\n}\n",
    )?;
    machine.write(".dsh/profiles/web/package.json", "{}\n")?;
    let probe = machine.probe("dsh")?;
    assert!(!probe.capture_installed);
    assert_eq!(
        probe.install_detail,
        "plug-in linked in headless; not wired: web"
    );
    assert!(probe.can_connect);
    let patch = dsh_patch(&machine);
    machine.write(".dsh/profiles/headless/package.json", "{}\n")?;
    machine.write(".dsh/profiles/web/package.json", "{}\n")?;
    let bare = machine.probe("dsh")?;
    assert_eq!(
        bare.install_detail,
        format!(
            "no rh plug-in in {}; not wired: headless, web",
            patch.display()
        )
    );
    assert_eq!(
        bare.recall.detail,
        format!(
            "no rh mcp entry in {}; not wired: headless, web",
            patch.display()
        )
    );
    Ok(())
}

#[test]
fn dsh_status_names_a_missing_plug_in_and_connect_writes_it() -> TestResult {
    let machine = machine()?;
    machine.write(
        ".dsh/cordis.patch.yml",
        "- insert:\n    - id: rivethub-capture\n      name: ./plugins/rivethub-capture.mjs\n",
    )?;
    let probe = machine.probe("dsh")?;
    assert!(!probe.capture_installed);
    assert!(
        probe
            .install_detail
            .contains("home patch names a missing plug-in"),
        "{}",
        probe.install_detail
    );
    machine.connect("dsh", Some(ConnectPart::Capture), false)?;
    assert!(machine.path(".dsh/plugins/rivethub-capture.mjs").is_file());
    assert!(machine.probe("dsh")?.capture_installed);
    Ok(())
}

#[test]
fn dsh_disconnect_leaves_a_recall_row_rh_did_not_write() -> TestResult {
    let machine = machine()?;
    fake_dsh(&machine)?;
    let original = format!(
        "- insert:\n    - id: mine\n      name: ./mcp.js\n      config:\n        serverName: mine\n        command: {RH}\n        args:\n          - mcp\n"
    );
    machine.write(".dsh/cordis.patch.yml", &original)?;
    machine.connect("dsh", None, false)?;
    let patched = machine.read(".dsh/cordis.patch.yml")?;
    assert!(patched.contains("id: mine"), "{patched}");
    assert!(patched.contains("rivethub-capture"), "{patched}");
    assert!(!patched.contains("rivethub-recall"), "{patched}");
    let now = Utc
        .with_ymd_and_hms(2026, 11, 6, 13, 0, 0)
        .single()
        .unwrap_or_default();
    machine
        .service
        .disconnect_now("dsh", Some(ConnectPart::Recall), false, now)?;
    assert_eq!(machine.read(".dsh/cordis.patch.yml")?, patched);
    machine.service.disconnect_now("dsh", None, false, now)?;
    assert_eq!(machine.read(".dsh/cordis.patch.yml")?, original);
    Ok(())
}

#[test]
fn dsh_refuses_a_recall_name_that_already_runs_something_else() -> TestResult {
    let machine = machine()?;
    fake_dsh(&machine)?;
    let original = "- insert:\n    - id: rivethub-recall\n      name: ./other.js\n      config:\n        serverName: rivethub\n        command: /bin/true\n        args:\n          - stop\n";
    machine.write(".dsh/cordis.patch.yml", original)?;
    let error = machine
        .connect("dsh", Some(ConnectPart::Recall), false)
        .err()
        .ok_or("a taken recall name should be refused")?;
    assert!(
        error.to_string().contains("already runs something else"),
        "{error}"
    );
    assert_eq!(machine.read(".dsh/cordis.patch.yml")?, original);
    assert_eq!(machine.backups(), Vec::<String>::new());
    Ok(())
}

#[test]
fn dsh_connect_keeps_an_unrelated_mcp_row_without_a_server_name() -> TestResult {
    let machine = machine()?;
    fake_dsh(&machine)?;
    let original = "- insert:\n    - id: other\n      name: ./other.js\n      config:\n        command: /bin/true\n        args:\n          - stop\n";
    machine.write(".dsh/cordis.patch.yml", original)?;
    machine.connect("dsh", Some(ConnectPart::Recall), false)?;
    let patch = machine.read(".dsh/cordis.patch.yml")?;
    assert!(patch.contains("id: other"), "{patch}");
    assert!(patch.contains("id: rivethub-recall"), "{patch}");
    assert!(patch.contains("serverName: rivethub"), "{patch}");
    Ok(())
}
