#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(windows, feature(windows_process_exit_code_from))]

use std::{process::ExitCode, sync::Arc};

use anyhow::Result;
use bootty::cli::{Cli, Command, EventCommand, RemoteSpaceCommand, TaskCommand};
use bootty_config::ApplicationIdentity;
use bootty_control as control;
use bootty_control::{Caller, CommandInvocation};
use clap::Parser;

mod cli_runtime;
#[cfg(target_os = "macos")]
mod macos_cli;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::from(cli_runtime::exit_code(&error))
        }
    }
}

fn run() -> Result<ExitCode> {
    let identity = ApplicationIdentity::current();
    bootty_mux::rmux::prepare_local_rmux_daemon(identity)?;
    if let Some(code) = bootty_mux::rmux::run_embedded_rmux_daemon()? {
        return Ok(process_exit_code(code));
    }
    let backends = Arc::new(bootty_mux::provider::MuxBackendRegistry::desktop()?);
    // Correct a stale `$SHELL` to the OS login shell before any child inherits
    // it; tmux otherwise bakes the wrong shell into the server's default-shell.
    // Finder launches need the account login environment before any service starts.
    if let Some(code) = bootty::shell_env::initialize_shell_environment()? {
        return Ok(process_exit_code(code));
    }
    #[cfg(target_os = "macos")]
    if let Err(error) = macos_cli::ensure_cli_link() {
        eprintln!("Could not install the Bootty command: {error}");
    }

    run_command(&Cli::parse(), backends)
}

fn run_command(
    cli: &Cli,
    backends: Arc<bootty_mux::provider::MuxBackendRegistry>,
) -> Result<ExitCode> {
    match cli.subcommand() {
        Some(Command::Run(args)) => {
            return cli_runtime::run_job(cli, args).map(|()| ExitCode::SUCCESS);
        }
        Some(Command::Doctor) => return cli_runtime::doctor(cli).map(|()| ExitCode::SUCCESS),
        Some(Command::Wait(args)) => {
            return cli_runtime::wait_for_snapshot(cli, args).map(|()| ExitCode::SUCCESS);
        }
        Some(Command::Commands) => {
            cli_runtime::print_control_response(
                cli_runtime::control_request(cli.start(), "command.list", serde_json::Value::Null)?,
                cli.json(),
            )?;
            return Ok(ExitCode::SUCCESS);
        }
        Some(Command::Describe { name }) => {
            cli_runtime::print_control_response(
                cli_runtime::control_request(
                    cli.start(),
                    "command.describe",
                    serde_json::json!({"command": name}),
                )?,
                cli.json(),
            )?;
            return Ok(ExitCode::SUCCESS);
        }
        Some(Command::Invoke {
            name,
            arguments,
            stdin,
            stdin_json,
            yes,
            detached,
        }) => {
            let arguments = if *stdin_json {
                cli_runtime::read_stdin_arguments()?
            } else if *stdin {
                std::iter::once(cli_runtime::read_stdin()?)
                    .chain(arguments.iter().cloned())
                    .collect()
            } else {
                arguments.clone()
            };
            let invocation = CommandInvocation::new(name, arguments, Caller::Cli);
            let response = cli_runtime::invoke_control_command(cli, invocation, *yes, *detached)?;
            if *detached {
                cli_runtime::print_control_response(response, cli.json())?;
            } else {
                cli_runtime::print_command_response(response, cli.json())?;
            }
            return Ok(ExitCode::SUCCESS);
        }
        Some(Command::Task(command)) => {
            let (method, params) = match command {
                TaskCommand::Status { task } => ("task.status", serde_json::json!({"task": task})),
                TaskCommand::Cancel { task } => ("task.cancel", serde_json::json!({"task": task})),
            };
            cli_runtime::print_control_response(
                cli_runtime::control_request(cli.start(), method, params)?,
                cli.json(),
            )?;
            return Ok(ExitCode::SUCCESS);
        }
        Some(Command::Events(command)) => {
            events(cli, command)?;
            return Ok(ExitCode::SUCCESS);
        }
        Some(Command::Dynamic(arguments)) => {
            cli_runtime::invoke_dynamic_command(cli, arguments)?;
            return Ok(ExitCode::SUCCESS);
        }
        Some(Command::Update) => {
            return bootty::update::update().map(|()| ExitCode::SUCCESS);
        }
        Some(Command::RemoteSpace(command)) => {
            remote_space(cli, &backends, command)?;
            return Ok(ExitCode::SUCCESS);
        }
        Some(Command::RemoteExec { payload }) => {
            return Ok(process_exit_code(bootty_host::run_remote_command(payload)?));
        }
        Some(Command::RemotePing) => return Ok(ExitCode::SUCCESS),
        Some(Command::RemoteRmux { payload }) => {
            return Ok(process_exit_code(
                bootty_mux::rmux::run_remote_rmux_command(payload)?,
            ));
        }
        Some(Command::App(_)) | None => {}
    }
    launch_app(cli, backends)
}

fn launch_app(
    cli: &Cli,
    backends: Arc<bootty_mux::provider::MuxBackendRegistry>,
) -> Result<ExitCode> {
    let config = cli.load_config()?;
    if control::running_instance()?.is_some() {
        if config.cli_default_open_behavior.opens_new_window() {
            cli_runtime::invoke_control_command(
                cli,
                CommandInvocation::from_action("new_window", Caller::Cli),
                false,
                false,
            )?;
        }
        return Ok(ExitCode::SUCCESS);
    }
    let window_state_key = cli.window_state_key().to_owned();

    bootty_ui::native_host::run(config, window_state_key, backends).map(|()| ExitCode::SUCCESS)
}

fn events(cli: &Cli, command: &EventCommand) -> Result<()> {
    let (method, params) = match command {
        EventCommand::Wait {
            subscription,
            cursor,
            timeout_ms,
        } => (
            "event.wait",
            serde_json::json!({"subscription":subscription,"cursor":cursor,"timeout_ms":timeout_ms}),
        ),
        EventCommand::Subscribe { topics } => {
            ("event.subscribe", serde_json::json!({"topics": topics}))
        }
        EventCommand::Poll {
            subscription,
            cursor,
        } => (
            "event.subscribe",
            serde_json::json!({
                "subscription": subscription,
                "cursor": cursor,
            }),
        ),
        EventCommand::Unsubscribe { subscription } => (
            "event.unsubscribe",
            serde_json::json!({"subscription": subscription}),
        ),
    };
    cli_runtime::print_control_response(
        cli_runtime::control_request(cli.start(), method, params)?,
        cli.json(),
    )?;
    Ok(())
}

fn remote_space(
    cli: &Cli,
    backends: &bootty_mux::provider::MuxBackendRegistry,
    command: &RemoteSpaceCommand,
) -> Result<()> {
    let config = cli.load_config()?;
    match command {
        RemoteSpaceCommand::List => {
            println!(
                "{}",
                serde_json::to_string(&bootty_mux::remote_space::list(&config)?)?
            );
        }
        RemoteSpaceCommand::Create { name, backend } => {
            println!(
                "{}",
                serde_json::to_string(&bootty_mux::remote_space::create(
                    &config,
                    name,
                    (*backend).into(),
                )?)?
            );
        }
        RemoteSpaceCommand::Snapshot { id, backend } => {
            println!(
                "{}",
                serde_json::to_string(&bootty_mux::remote_space::snapshot(
                    &config,
                    backends,
                    id,
                    (*backend).into(),
                )?)?
            );
        }
        RemoteSpaceCommand::Execute {
            id,
            backend,
            payload,
        } => {
            bootty_mux::remote_space::execute(&config, backends, id, (*backend).into(), payload)?;
        }
    }
    Ok(())
}

fn process_exit_code(code: i32) -> ExitCode {
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitCodeExt;
        ExitCode::from_raw(u32::from_ne_bytes(code.to_ne_bytes()))
    }
    #[cfg(not(windows))]
    {
        // Unix exposes only the low eight bits of a normal process exit status.
        let [status, ..] = code.to_le_bytes();
        ExitCode::from(status)
    }
}
