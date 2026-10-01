use std::{
    io::{BufReader, Read},
    process::{Command, Stdio},
    sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc},
    thread,
    time::Duration,
};

use serde_json::{Value, json};

use crate::{AgentKind, AgentLaunch};

/// Interactive account commands use the ordinary terminal invocation path; no shell, private
/// credential parsing or token duplication is needed for providers without a native login RPC.
#[must_use]
pub fn agent_account_launch(provider: AgentKind, program: &str, logout: bool) -> AgentLaunch {
    let arguments = match provider {
        AgentKind::Codex => {
            if logout {
                vec!["logout"]
            } else {
                vec!["login", "--device-auth"]
            }
        }
        AgentKind::Claude => vec!["auth", if logout { "logout" } else { "login" }],
        // Initial messages bypass interactive slash commands in this provider.
        AgentKind::Pi => Vec::new(),
    };
    AgentLaunch {
        program: program.to_owned(),
        cwd: None,
        arguments: arguments.into_iter().map(str::to_owned).collect(),
        ephemeral: true,
    }
}

/// Query installed account interfaces without opening a second conversation or reading secrets.
/// # Errors
/// Returns unsupported account discovery or bounded command/protocol errors.
pub fn terminal_account_status(
    provider: AgentKind,
    program: &str,
    provider_id: Option<&str>,
) -> Result<Value, String> {
    match provider {
        AgentKind::Claude => bounded_json_command(program, &["auth", "status", "--json"]),
        AgentKind::Codex => {
            let bytes = bounded_output_command(program, &["login", "status"], true)?;
            let status = String::from_utf8(bytes).map_err(|error| error.to_string())?;
            if status.trim().starts_with("Logged in") {
                Ok(json!({"loggedIn":true,"provider":"codex"}))
            } else if status.contains("Not logged in") {
                Ok(json!({"loggedIn":false,"provider":"codex"}))
            } else {
                Err("Codex account status was not recognized".to_owned())
            }
        }
        AgentKind::Pi => bounded_json_command(
            program,
            &[
                "auth",
                "check",
                "--provider",
                provider_id.ok_or("Select a Pi provider to check account readiness")?,
                "--json",
                "--no-refresh",
            ],
        ),
    }
}

fn bounded_json_command(program: &str, arguments: &[&str]) -> Result<Value, String> {
    serde_json::from_slice(&bounded_output_command(program, arguments, false)?)
        .map_err(|error| error.to_string())
}

fn bounded_output_command(
    program: &str,
    arguments: &[&str],
    stderr: bool,
) -> Result<Vec<u8>, String> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(if stderr {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stderr(if stderr {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .spawn()
        .map_err(|error| error.to_string())?;
    let stdout: Box<dyn Read + Send> = if stderr {
        Box::new(
            child
                .stderr
                .take()
                .ok_or("Account output is not available")?,
        )
    } else {
        Box::new(
            child
                .stdout
                .take()
                .ok_or("Account output is not available")?,
        )
    };
    let child = Arc::new(Mutex::new(child));
    let waiter = Arc::clone(&child);
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = BufReader::new(stdout)
            .take(64 * 1024 + 1)
            .read_to_end(&mut bytes);
        if bytes.len() > 64 * 1024 {
            let _ = lock(&waiter).kill();
        }
        let result = read.map_err(|error| error.to_string()).and_then(|_| {
            if bytes.len() > 64 * 1024 {
                return Err("Account response exceeds 64 KiB".to_owned());
            }
            Ok(bytes)
        });
        let _ = sender.send(result);
    });
    let result = receiver
        .recv_timeout(Duration::from_secs(15))
        .map_err(|error| error.to_string());
    let mut child = lock(&child);
    let _ = child.kill();
    let _ = child.wait();
    drop(child);
    result?
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
