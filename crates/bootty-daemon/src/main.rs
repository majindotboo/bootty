#![cfg_attr(windows, feature(windows_process_exit_code_from))]

mod command_runtime;

use std::process::ExitCode;

use anyhow::{Result, bail};

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("Error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let (identity, args) =
        command_runtime::parse_application_identity(std::env::args().skip(1).collect())?;
    bootty_mux::rmux::prepare_local_rmux_daemon(identity)?;
    if let Some(code) = bootty_mux::rmux::run_embedded_rmux_daemon()? {
        return Ok(exit_code(code));
    }
    let Some((command, arguments)) = args.split_first() else {
        bail!("bootty-daemon requires a command");
    };
    match command.as_str() {
        "remote-ping" => {
            command_runtime::run_remote_ping();
            Ok(ExitCode::SUCCESS)
        }
        "transfer" => {
            let [request] = arguments else {
                bail!("transfer requires one request");
            };
            bootty_host::jobs::serve_transfer(request)?;
            Ok(ExitCode::SUCCESS)
        }
        "job" => {
            let [request] = arguments else {
                bail!("job requires one request");
            };
            bootty_host::jobs::serve(request)?;
            Ok(ExitCode::SUCCESS)
        }
        "shell-history" => {
            let result = bootty_host::shell_history::receive(std::io::stdin().lock())?;
            println!("{}", serde_json::to_string(&result)?);
            Ok(ExitCode::SUCCESS)
        }
        "file" => command_runtime::run_file(arguments).map(|()| ExitCode::SUCCESS),
        "clipboard-upload" => {
            command_runtime::run_clipboard_upload(arguments).map(|()| ExitCode::SUCCESS)
        }
        "remote-exec" => command_runtime::run_remote_exec(arguments).map(exit_code),
        "remote-rmux" => command_runtime::run_remote_rmux(arguments).map(exit_code),
        "remote-space" => {
            let paths = command_runtime::remote_space_paths_from_environment(identity)?;
            command_runtime::run_remote_space(arguments, &paths).map(|()| ExitCode::SUCCESS)
        }
        "remote-project" => {
            command_runtime::run_remote_project(arguments).map(|()| ExitCode::SUCCESS)
        }
        "remote-worktree" => {
            command_runtime::run_remote_worktree(arguments).map(|()| ExitCode::SUCCESS)
        }
        command => bail!("unknown command {command:?}"),
    }
}

#[cfg(unix)]
fn exit_code(code: i32) -> ExitCode {
    let [status, ..] = code.to_le_bytes();
    ExitCode::from(status)
}

#[cfg(windows)]
fn exit_code(code: i32) -> ExitCode {
    use std::os::windows::process::ExitCodeExt;

    ExitCode::from_raw(u32::from_ne_bytes(code.to_ne_bytes()))
}

#[cfg(not(any(unix, windows)))]
fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
