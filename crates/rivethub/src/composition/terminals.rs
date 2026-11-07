use std::sync::Arc;

use config::{Config, Environment};
use gateway::sessions::SessionRegistry;
use gateway::term::{ClaudeTrust, LaunchCommand, TerminalOptions, TerminalService};
use kernel::terminal::TerminalMux;
use tokio_util::sync::CancellationToken;

use super::ComposeError;

pub(super) async fn build(
    config: &Config,
    env: &Environment,
    sessions: Arc<SessionRegistry>,
    owner: &gateway::tenancy::UserId,
    shutdown: &CancellationToken,
    tasks: &tokio_util::task::TaskTracker,
) -> Result<Arc<TerminalService>, ComposeError> {
    let gateway = config.gateway.as_ref().ok_or_else(|| {
        ComposeError::Invalid("gateway settings are required for terminals".into())
    })?;
    let roots = config.terminal_roots(env);
    let paths = config::Paths::new(env)?;
    let state_dir = paths.state_dir();
    let roster_path =
        terminal::resolve_roster_path(gateway.commands_file.as_deref(), env, &state_dir);
    let home = env
        .home()
        .map(std::path::PathBuf::from)
        .ok_or_else(|| ComposeError::Invalid("HOME is required for terminals".into()))?;
    let roster = terminal::load_roster(&roster_path, &home).await?;
    let settings = terminal::TerminalSettings::from_config(
        config,
        env,
        crate::identity::resolve(config, env)?.name,
    );
    let max_ptys = settings.limits.max_ptys;
    let search = match env.get("PATH") {
        Some(path) => std::env::split_paths(path).collect::<Vec<_>>(),
        None => Vec::new(),
    };
    let plan = terminal::mux::resolve(
        env.term_mux_choice(),
        &search,
        env.home().map(std::path::Path::new),
        &state_dir,
        gateway.port,
    )
    .await;
    match &plan {
        terminal::MuxPlan::Unavailable(unavailable) => {
            return Err(unavailable.reason.to_error().into());
        }
        terminal::MuxPlan::Herdr(setup) => {
            terminal::herdr::ui::write_ui_config(&setup.config_home).await?;
        }
        terminal::MuxPlan::Direct => {}
    }
    let mux = Arc::new(terminal::TerminalManager::new(settings, plan));
    mux.run_tracked(shutdown.child_token(), tasks);
    let mut adopted = mux.adopt_namespace(&state_dir, owner.as_str()).await?;
    let commands = launch_commands(roster.commands);
    let built = TerminalService::new(
        Arc::clone(&mux) as Arc<dyn TerminalMux>,
        sessions,
        TerminalOptions {
            roots,
            default: roster.default_id,
            commands,
            max_ptys,
        },
    )
    .await?
    .with_shutdown(shutdown.child_token())
    .with_harness(Arc::new(
        harness_acp::AcpHarnessAdapter::new(config)
            .with_search_paths(search)
            .with_discovery_dir(paths.discovery_dir()),
    ))
    .with_session_lister(Arc::new(capture::listing::StoreSessionLister::new(
        super::store_roots(env),
    )))
    .with_startup_dir(state_dir.join("terminal-startup"));
    let service = Arc::new(with_claude_trust(built, config, env));
    claim_adopted(&service, &mux, owner, &mut adopted)?;
    start_discovery(&service).await;
    Ok(service)
}

async fn start_discovery(service: &TerminalService) {
    if let Some(adapter) = service.adapter()
        && let Err(error) = adapter.offerings().await
    {
        tracing::warn!(%error, "harness discovery could not start");
    }
}

fn with_claude_trust(
    service: TerminalService,
    config: &Config,
    env: &Environment,
) -> TerminalService {
    let enabled = config
        .gateway
        .as_ref()
        .and_then(|gateway| gateway.terminal.as_ref())
        .is_some_and(|terminal| terminal.trust_claude_launch_dirs == Some(true));
    match capture::listing::claude_config_file(env) {
        Some(file) if enabled => service.with_claude_trust(ClaudeTrust {
            config: file,
            roots: harness_directories(config),
        }),
        _ => service,
    }
}

fn harness_directories(config: &Config) -> Vec<std::path::PathBuf> {
    config
        .tasks
        .as_ref()
        .and_then(|tasks| tasks.harnesses.as_ref())
        .into_iter()
        .flat_map(|harnesses| harnesses.values())
        .filter_map(|entry| entry.cwd.clone())
        .collect()
}

fn launch_commands(commands: Vec<terminal::ResolvedCommand>) -> Vec<LaunchCommand> {
    commands
        .into_iter()
        .map(|command| LaunchCommand {
            id: command.id,
            label: command.label,
            argv: command.argv,
            cwd: command.cwd,
            harness: command.harness,
            session: command.session,
            env: command.env,
        })
        .collect()
}

fn claim_adopted(
    service: &TerminalService,
    mux: &terminal::TerminalManager,
    owner: &gateway::tenancy::UserId,
    adopted: &mut terminal::Adoption,
) -> Result<(), ComposeError> {
    for session in std::mem::take(&mut adopted.adopted) {
        let remembered = gateway::term::RememberedSession {
            id: session.id.to_string(),
            session: session.session.as_str().to_owned(),
            command: session.roster_key.as_str().to_owned(),
        };
        match service.remember(owner, remembered) {
            Ok(()) => {}
            Err(gateway::error::ApiError::SessionForbidden) => {
                mux.release(&session.id);
                terminal::herdr::namespace::note_skip(
                    &mut adopted.skipped,
                    terminal::herdr::namespace::SkipRule::ForeignSessionOwner,
                );
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
