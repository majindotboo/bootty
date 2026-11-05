#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(windows, feature(windows_process_exit_code_from))]

use std::process::ExitCode;

use anyhow::Result;
use bootty::cli::{Cli, Command, EventCommand, TaskCommand};
use bootty_config::ApplicationIdentity;
use bootty_control as control;
use bootty_control::{Caller, CommandInvocation};
use clap::Parser;

use bootty::cli_runtime;
#[cfg(target_os = "macos")]
mod macos_cli;

fn main() -> ExitCode {
    report(run())
}

fn report(result: Result<ExitCode>) -> ExitCode {
    result.unwrap_or_else(|error| {
        eprintln!("{error:#}");
        ExitCode::from(cli_runtime::exit_code(&error))
    })
}

fn run() -> Result<ExitCode> {
    if bootty_mux::native::local::run_embedded()? {
        return Ok(ExitCode::SUCCESS);
    }
    let identity = ApplicationIdentity::current();
    identity.initialize_process()?;
    let cli = Cli::parse();
    let existing =
        matches!(cli.subcommand(), Some(Command::App(_)) | None) && open_running_instance(&cli)?;
    // Correct a stale `$SHELL` to the OS login shell before any child inherits it.
    // Finder launches need the account login environment before any service starts.
    if !existing
        && cli.needs_login_environment()
        && let Some(code) = bootty::shell_env::initialize_shell_environment()?
    {
        return Ok(process_exit_code(code));
    }
    #[cfg(target_os = "macos")]
    if let Err(error) = macos_cli::ensure_cli_link() {
        eprintln!("Could not install the Bootty command: {error}");
    }

    if existing {
        return Ok(ExitCode::SUCCESS);
    }
    run_command(&cli)
}

fn run_command(cli: &Cli) -> Result<ExitCode> {
    if cli_runtime::invoke(cli)? {
        return Ok(ExitCode::SUCCESS);
    }
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
        Some(Command::Update) => {
            return bootty::update::update().map(|()| ExitCode::SUCCESS);
        }
        Some(Command::RemoteExec { payload }) => {
            return Ok(process_exit_code(bootty_host::run_remote_command(payload)?));
        }
        Some(Command::RemotePing) => return Ok(ExitCode::SUCCESS),
        Some(Command::App(_) | Command::Invoke { .. } | Command::Dynamic(_)) | None => {}
    }
    launch_app(cli)
}

fn launch_app(cli: &Cli) -> Result<ExitCode> {
    if open_running_instance(cli)? {
        return Ok(ExitCode::SUCCESS);
    }
    let config = cli.load_gui_config()?;
    let window_state_key = cli.window_state_key().to_owned();

    bootty_ui::native_host::run(config, window_state_key).map(|()| ExitCode::SUCCESS)
}

fn open_running_instance(cli: &Cli) -> Result<bool> {
    if control::running_instance()?.is_none() {
        return Ok(false);
    }
    let config = cli.load_gui_config()?;
    if config.cli_default_open_behavior.opens_new_window() {
        cli_runtime::invoke_control_command(
            cli,
            CommandInvocation::from_action("new_window", Caller::Cli),
            false,
            false,
        )?;
    }
    Ok(true)
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
