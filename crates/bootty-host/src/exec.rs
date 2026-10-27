use bootty_config::ApplicationIdentity;
use std::{
    borrow::Cow,
    fmt::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

pub const REMOTE_DAEMON_PROGRAM: &str = "bootty-daemon";
// Older daemons accept restore requests but omit tmux's saved output rows.
pub const REMOTE_DAEMON_PROTOCOL_VERSION: &str = "23";
#[must_use]
pub fn remote_exec_program() -> &'static str {
    static PROGRAM: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        format!(
            "./.bootty/bin/bootty-daemon-{REMOTE_DAEMON_PROTOCOL_VERSION}-{}.exe",
            env!("CARGO_PKG_VERSION")
        )
    });
    &PROGRAM
}
pub const REMOTE_EXEC_SUBCOMMAND: &str = "remote-exec";
pub const REMOTE_PING_SUBCOMMAND: &str = "remote-ping";
const MAX_REMOTE_COMMAND_PAYLOAD: usize = 1024 * 1024;

/// Start a Windows terminal command through the owning executable's typed relay.
///
/// PowerShell only assigns environment values and passes a bounded base64 payload;
/// the Rust relay retains responsibility for native and batch argument escaping.
/// # Errors
/// Returns invalid environment names, an unrepresentable owner path, or an oversized command.
pub fn windows_terminal_argv(
    owner: &Path,
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    environment: &[(String, String)],
) -> Result<Vec<String>> {
    if !owner.is_absolute() {
        bail!("terminal owner executable must be an absolute path");
    }
    let owner = owner
        .to_str()
        .context("terminal owner executable is not UTF-8")?;
    let quote = |value: &str| format!("'{}'", value.replace('\'', "''"));
    let payload = serde_json::to_vec(&RemoteCommand {
        program: program.to_owned(),
        args: args.to_vec(),
        terminal: true,
        cwd: cwd.map(str::to_owned),
    })
    .context("encode terminal command")?;
    let mut script =
        "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; ".to_owned();
    for (name, value) in environment {
        let mut chars = name.chars();
        if !chars
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
            || !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            || value.contains('\0')
        {
            bail!("invalid terminal environment entry");
        }
        write!(script, "$env:{name}={}; ", quote(value))?;
    }
    write!(
        script,
        "$child=Start-Process -FilePath {} -ArgumentList @('remote-exec','{}') -NoNewWindow -Wait -PassThru; exit $child.ExitCode",
        quote(owner),
        URL_SAFE_NO_PAD.encode(payload),
    )?;
    let bytes = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    if encoded.len() > 29_000 {
        bail!("Agent launch exceeds the Windows command-line limit");
    }
    Ok(vec![
        "powershell.exe".into(),
        "-NoProfile".into(),
        "-EncodedCommand".into(),
        encoded,
    ])
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RemoteCommand {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) terminal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
}

pub fn proxy_command_line(program: &str, args: &[String], terminal: bool) -> Result<String> {
    let args = proxy_command_args(program, args, terminal)?;
    Ok(remote_program_line(&args))
}

pub fn proxy_command_args(program: &str, args: &[String], terminal: bool) -> Result<Vec<String>> {
    proxy_command_args_in(program, args, terminal, None)
}

pub fn proxy_command_args_in(
    program: &str,
    args: &[String],
    terminal: bool,
    cwd: Option<&str>,
) -> Result<Vec<String>> {
    let payload = serde_json::to_vec(&RemoteCommand {
        program: program.to_owned(),
        args: args.to_vec(),
        terminal,
        cwd: cwd.map(str::to_owned),
    })
    .context("encode remote command")?;
    let mut arguments = remote_identity_args(ApplicationIdentity::for_process());
    arguments.extend([
        REMOTE_EXEC_SUBCOMMAND.to_owned(),
        URL_SAFE_NO_PAD.encode(payload),
    ]);
    Ok(arguments)
}

pub fn decode_remote_command(payload: &str) -> Result<RemoteCommand> {
    if payload.len() > MAX_REMOTE_COMMAND_PAYLOAD {
        bail!("remote command payload is too large")
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .context("decode remote command payload")?;
    let command: RemoteCommand =
        serde_json::from_slice(&bytes).context("parse remote command payload")?;
    if command.program.is_empty() {
        bail!("remote command program cannot be empty")
    }
    Ok(command)
}

/// # Errors
/// Returns invalid protocol payload, command setup, or execution errors.
pub fn run_remote_command(payload: &str) -> Result<i32> {
    let command = decode_remote_command(payload)?;
    let program = if command.program == REMOTE_DAEMON_PROGRAM {
        std::env::current_exe().context("resolve Bootty daemon executable")?
    } else {
        PathBuf::from(&command.program)
    };
    let mut child = crate::local_command(program);
    child.args(
        daemon_program_args(
            &command.program,
            &command.args,
            ApplicationIdentity::for_process(),
        )
        .as_ref(),
    );
    if let Some(cwd) = command.cwd {
        child.current_dir(cwd);
    }
    if command.terminal {
        // Host execution does not depend on the terminal engine. The terminal adapter may add
        // a vendored terminfo path when it launches an attach client.
        child.env("TERM", "xterm-256color");
    }
    let status = child
        .status()
        .with_context(|| format!("run remote command {}", command.program))?;
    Ok(status.code().unwrap_or(1))
}

// Preserve explicit wire caller identities; otherwise inherit the invoking app.
#[must_use]
pub fn daemon_program_args<'a>(
    program: &str,
    args: &'a [String],
    identity: ApplicationIdentity,
) -> Cow<'a, [String]> {
    if program == REMOTE_DAEMON_PROGRAM
        && args
            .first()
            .is_none_or(|arg| arg != "--application-identity")
        && identity == ApplicationIdentity::Development
    {
        let mut prefixed = remote_identity_args(identity);
        prefixed.extend_from_slice(args);
        Cow::Owned(prefixed)
    } else {
        Cow::Borrowed(args)
    }
}

fn remote_identity_args(identity: ApplicationIdentity) -> Vec<String> {
    match identity {
        ApplicationIdentity::Production => Vec::new(),
        ApplicationIdentity::Development => {
            vec!["--application-identity".into(), "bootty-dev".into()]
        }
    }
}

// A development client keeps its own namespace on a differently built daemon host.
pub fn remote_program_command(args: &[String]) -> (String, Vec<String>) {
    let identity = ApplicationIdentity::for_process();
    if identity == ApplicationIdentity::Development {
        let mut prefixed = vec![
            format!(
                "{}={}",
                bootty_config::DEVELOPMENT_NAMESPACE_ENV,
                identity.namespace()
            ),
            remote_exec_program().to_owned(),
        ];
        prefixed.extend_from_slice(args);
        ("/usr/bin/env".to_owned(), prefixed)
    } else {
        (remote_exec_program().to_owned(), args.to_vec())
    }
}

pub fn remote_program_line(args: &[String]) -> String {
    let (program, args) = remote_program_command(args);
    format!("{program} {}", args.join(" "))
}
