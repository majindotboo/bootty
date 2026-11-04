use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use bootty_config::ApplicationIdentity;

pub fn run_remote_ping() {
    println!(
        "{}:{}",
        bootty_host::REMOTE_DAEMON_PROTOCOL_VERSION,
        env!("CARGO_PKG_VERSION")
    );
}

/// # Errors
/// Returns argument, host, or command execution errors.
pub fn run_remote_exec(args: &[String]) -> Result<i32> {
    let payload = args.first().context("remote-exec requires a payload")?;
    reject_extra(args.iter().skip(1))?;
    bootty_host::run_remote_command(payload)
}

/// # Errors
/// Returns argument, host, or command execution errors.
pub fn run_remote_project(args: &[String]) -> Result<()> {
    let Some((command, arguments)) = args.split_first() else {
        bail!("remote-project requires a command")
    };
    let home = home_dir();
    match command.as_str() {
        "list" => {
            if !arguments.is_empty() {
                bail!("remote-project list takes no arguments")
            }
            println!(
                "{}",
                serde_json::to_string(&bootty_git::discover_project_picker_entries(
                    home.as_deref()
                ))?
            );
        }
        "favorite" => {
            let path = required_option(arguments, "--path")?;
            println!(
                "{}",
                bootty_git::toggle_favorite_project_path(home.as_deref(), &path)?
            );
        }
        "add" => {
            let path = required_option(arguments, "--path")?;
            println!(
                "{}",
                bootty_git::add_favorite_project_path(home.as_deref(), &path)?
            );
        }
        _ => bail!("unknown remote-project command {command:?}"),
    }
    Ok(())
}

/// # Errors
/// Returns argument, host, or command execution errors.
pub fn run_remote_worktree(args: &[String]) -> Result<()> {
    let Some((command, arguments)) = args.split_first() else {
        bail!("remote-worktree requires a command")
    };
    match command.as_str() {
        "list" => {
            let project = required_option(arguments, "--project")?;
            let open_cwds = option_values(arguments, "--open-cwd")?;
            let mut entries = bootty_git::discover_worktree_picker_entries(&project);
            bootty_git::mark_occupied_worktrees(&mut entries, &open_cwds);
            println!("{}", serde_json::to_string(&entries)?);
        }
        "create" => {
            let project = required_option(arguments, "--project")?;
            let request = if arguments
                .as_chunks::<2>()
                .0
                .iter()
                .any(|pair| pair[0] == "--request")
            {
                serde_json::from_str::<bootty_git::WorktreeRequest>(&required_option(
                    arguments,
                    "--request",
                )?)?
            } else {
                bootty_git::WorktreeRequest {
                    branch: required_option(arguments, "--branch")?,
                    name: None,
                    start_ref: None,
                    directory: None,
                }
            };
            println!(
                "{}",
                serde_json::to_string(
                    &bootty_git::Git::new()
                        .create_worktree(&project, &request)
                        .map_err(anyhow::Error::msg)?
                )?
            );
        }
        "clean" => {
            let request = serde_json::from_str::<bootty_git::CleanupRequest>(&required_option(
                arguments,
                "--request",
            )?)?;
            println!(
                "{}",
                serde_json::to_string(
                    &bootty_git::Git::new()
                        .clean_worktrees(&request)
                        .map_err(anyhow::Error::msg)?
                )?
            );
        }
        _ => bail!("unknown remote-worktree command {command:?}"),
    }
    Ok(())
}

/// # Errors
/// Returns argument, host, or command execution errors.
pub fn parse_application_identity(
    mut args: Vec<String>,
) -> Result<(ApplicationIdentity, Vec<String>)> {
    if args
        .first()
        .is_some_and(|arg| arg == "--application-identity")
    {
        if args.len() < 2 {
            bail!("--application-identity requires a value")
        }
        let value = args.remove(1);
        let identity = ApplicationIdentity::parse(&value)
            .with_context(|| format!("unknown application identity {value:?}"))?;
        args.remove(0);
        Ok((identity, args))
    } else {
        Ok((ApplicationIdentity::Production, args))
    }
}

fn home_dir() -> Option<PathBuf> {
    bootty_git::home_dir()
}

fn required_option(args: &[String], name: &str) -> Result<String> {
    let (chunks, remainder) = args.as_chunks::<2>();
    let mut value = None;
    for chunk in chunks {
        if chunk[0] == name && value.replace(chunk[1].clone()).is_some() {
            bail!("duplicate option {name}")
        }
    }
    if !remainder.is_empty() {
        bail!("options require values")
    }
    value.with_context(|| format!("missing option {name}"))
}

fn option_values(args: &[String], name: &str) -> Result<Vec<String>> {
    let (chunks, remainder) = args.as_chunks::<2>();
    let values = chunks
        .iter()
        .filter(|chunk| chunk[0] == name)
        .map(|chunk| chunk[1].clone())
        .collect();
    if !remainder.is_empty() {
        bail!("options require values")
    }
    Ok(values)
}

fn reject_extra<'a>(mut args: impl Iterator<Item = &'a String>) -> Result<()> {
    if let Some(extra) = args.next() {
        bail!("unexpected argument {extra:?}")
    }
    Ok(())
}

/// # Errors
/// Returns argument, host, or command execution errors.
pub fn run_clipboard_upload(args: &[String]) -> Result<()> {
    use std::io::Write as _;
    let [length, digest] = args else {
        bail!("clipboard-upload requires byte count and SHA-256 digest")
    };
    let path = bootty_host::clipboard_image::receive_clipboard_image(
        std::io::stdin().lock(),
        length.parse().context("invalid image byte count")?,
        digest,
        &std::env::temp_dir(),
    )?;
    let response = serde_json::to_string(&path)?;
    if let Err(error) = writeln!(std::io::stdout().lock(), "{response}") {
        let _ = std::fs::remove_file(&path);
        return Err(error).context("report uploaded image");
    }
    Ok(())
}

/// # Errors
/// Returns argument, host, or command execution errors.
pub fn run_file(args: &[String]) -> Result<()> {
    use std::io::Write as _;
    if !args.is_empty() {
        bail!("file takes its request on standard input");
    }
    let response = bootty_host::files::receive_file_request(std::io::stdin().lock())?;
    writeln!(
        std::io::stdout().lock(),
        "{}",
        serde_json::to_string(&response)?
    )?;
    Ok(())
}
