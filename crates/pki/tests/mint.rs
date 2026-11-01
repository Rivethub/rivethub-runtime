use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;

use kernel::ca::Principal;
use tokio_util::sync::CancellationToken;
use url::Url;

type TestError = Box<dyn std::error::Error>;

struct Rig {
    _directory: tempfile::TempDir,
    argv: PathBuf,
    root: PathBuf,
    password: PathBuf,
    endpoint: pki::Endpoint,
    env: pki::ProcessEnv,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_minter_argv_matches_node_agent_and_device_plans() -> Result<(), TestError> {
    let rig = prepare()?;
    let roster = pki::parse_roster("rec-node.example.test:192.0.2.15:rec-agent.example.test\n")?;
    let node = pki::plan_cli(
        pki::CliToken::Node("rec-node.example.test".to_owned()),
        Some(&roster),
    )?;
    expect_mint(
        &rig,
        &node,
        &[
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
        ],
    )
    .await?;
    let agent = pki::plan_principal(
        &Principal::Agent {
            node: "rec-node.example.test".to_owned(),
            agent: "rec-agent.example.test".to_owned(),
        },
        Some(&roster),
    )?;
    expect_mint(
        &rig,
        &agent,
        &[
            "ca",
            "token",
            "spiffe://rivet.mesh/agent/rec-node.example.test/rec-agent.example.test",
            "--provisioner",
            "agents",
            "--san",
            "spiffe://rivet.mesh/agent/rec-node.example.test/rec-agent.example.test",
        ],
    )
    .await?;
    let device = pki::plan_principal(
        &Principal::Device {
            user: "alice".to_owned(),
            device: "phone".to_owned(),
        },
        None,
    )?;
    expect_mint(
        &rig,
        &device,
        &[
            "ca",
            "token",
            "spiffe://rivet.mesh/device/alice/phone",
            "--provisioner",
            "devices",
            "--san",
            "spiffe://rivet.mesh/device/alice/phone",
        ],
    )
    .await?;
    Ok(())
}

fn prepare() -> Result<Rig, TestError> {
    let directory = tempfile::tempdir()?;
    let bin = directory.path().join("bin");
    std::fs::create_dir(&bin)?;
    let argv = directory.path().join("argv.txt");
    install_step(&bin)?;
    let root = std::fs::canonicalize(write_file(directory.path(), "root.pem", "pem\n")?)?;
    let password = std::fs::canonicalize(write_file(directory.path(), "password", "secret\n")?)?;
    let endpoint = pki::Endpoint {
        url: Url::parse("https://ca.example.test")?,
        root: root.clone(),
        password_file: password.clone(),
    };
    let env = pki::ProcessEnv::from_pairs([
        ("PATH", path_text(&bin)?),
        ("RIVETHUB_TEST_ARGV", path_text(&argv)?),
    ]);
    Ok(Rig {
        _directory: directory,
        argv,
        root,
        password,
        endpoint,
        env,
    })
}

fn write_file(dir: &Path, name: &str, text: &str) -> Result<PathBuf, TestError> {
    let path = dir.join(name);
    std::fs::write(&path, text)?;
    Ok(path)
}

async fn expect_mint(rig: &Rig, plan: &pki::TokenPlan, head: &[&str]) -> Result<(), TestError> {
    pki::mint(plan, &rig.endpoint, &rig.env, &CancellationToken::new()).await?;
    assert_eq!(read_argv(&rig.argv)?, plan_argv(head, rig)?);
    Ok(())
}

fn plan_argv(head: &[&str], rig: &Rig) -> Result<Vec<String>, TestError> {
    let mut argv = head
        .iter()
        .map(|item| (*item).to_owned())
        .collect::<Vec<_>>();
    argv.extend([
        "--ca-url".to_owned(),
        "https://ca.example.test/".to_owned(),
        "--root".to_owned(),
        path_text(&rig.root)?.to_owned(),
        "--provisioner-password-file".to_owned(),
        path_text(&rig.password)?.to_owned(),
    ]);
    Ok(argv)
}

fn install_step(bin: &Path) -> Result<(), TestError> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(bin.join("step"))?;
    std::io::Write::write_all(
        &mut file,
        br#"#!/bin/sh
{
  printf '%s\n' "$0"
  for arg do
    printf '%s\n' "$arg"
  done
} > "$RIVETHUB_TEST_ARGV"
printf '%s\n' 'one-time-token'
"#,
    )?;
    Ok(())
}

fn read_argv(path: &Path) -> Result<Vec<String>, TestError> {
    let text = std::fs::read_to_string(path)?;
    Ok(text.lines().skip(1).map(str::to_owned).collect())
}

fn path_text(path: &Path) -> Result<&str, TestError> {
    path.to_str()
        .ok_or_else(|| Box::<dyn std::error::Error>::from(std::io::Error::other("non-utf8 path")))
}
