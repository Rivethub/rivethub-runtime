#![cfg(test)]

use cli::{Cli, Dispatch, Report};
use config::Environment;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

pub(crate) fn home() -> TempDir {
    tempfile::tempdir().expect("temporary home")
}

pub(crate) fn environment(home: &TempDir) -> Environment {
    Environment::from_vars([("HOME".as_ref(), home.path().as_os_str())])
}

pub(crate) fn local_owner(
    home: &std::path::Path,
) -> std::sync::Arc<gateway::local_owner::LocalOwner> {
    let env = Environment::from_vars([("HOME".as_ref(), home.as_os_str())]);
    let state = gateway::state_dir(&env).expect("state directory");
    std::sync::Arc::new(
        gateway::local_owner::LocalOwner::load_or_create(&state).expect("local owner token"),
    )
}

pub(crate) fn install_owner_token(env: &Environment, token: &gateway::local_owner::OwnerToken) {
    use std::os::unix::fs::PermissionsExt as _;
    let state = gateway::state_dir(env).expect("state directory");
    std::fs::create_dir_all(&state).expect("state directory");
    let path = gateway::local_owner::token_path(&state);
    std::fs::write(&path, token.expose()).expect("owner token");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
}

pub(crate) fn config(home: &TempDir, extra: &str) {
    std::fs::create_dir_all(home.path().join(".rivethub")).expect("state directory");
    std::fs::write(home.path().join(".rivethub/config.yaml"), format!("runtime:\n  workspace: /home/user/work\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    type: openai-compatible\n    base_url: http://127.0.0.1:1/v1\n    model: m\n    api_key: secret-that-must-stay-private\n{extra}")).expect("config");
}

pub(crate) async fn run(args: &[&str], env: &Environment) -> Report {
    let parsed = Cli::parse_args(
        std::iter::once("rh")
            .chain(args.iter().copied())
            .map(Into::into),
    );
    let cli = match parsed {
        Ok(cli) => cli,
        Err(report) => return report,
    };
    let output = cli::dispatch(cli, env, &CancellationToken::new()).await;
    let Dispatch::Complete(report) = output else {
        panic!("unexpected service start")
    };
    report
}
