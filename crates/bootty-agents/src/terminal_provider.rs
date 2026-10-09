use std::{
    env, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{AgentKind, AgentLaunch, PiAccountSelector};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TerminalProviderStatus {
    pub executable: Option<PathBuf>,
    pub version: Option<String>,
    pub installer: Option<String>,
    pub authenticated: Option<bool>,
    pub account: Option<String>,
    pub auth_method: Option<String>,
    pub subscription: Option<String>,
    pub message: Option<String>,
}

fn executable(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return path.canonicalize().ok();
    }
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths).find_map(|directory| {
            let path = directory.join(program);
            path.is_file().then(|| path.canonicalize().ok()).flatten()
        })
    })
}

/// Recognize only the installation that owns the selected executable.
/// Unknown installations remain manual until their owning installer can be proven.
#[must_use]
pub fn terminal_provider_installer(provider: AgentKind, executable: &Path) -> Option<Vec<String>> {
    if provider == AgentKind::Claude
        && executable.parent()?.file_name()? == "versions"
        && executable.parent()?.parent()?.file_name()? == "claude"
    {
        return Some(vec![
            executable.to_string_lossy().into_owned(),
            "update".to_owned(),
        ]);
    }
    let package = match provider {
        AgentKind::Codex => "@openai/codex",
        AgentKind::Pi => "@earendil-works/pi-coding-agent",
        AgentKind::Claude => "@anthropic-ai/claude-code",
    };
    for directory in executable.ancestors() {
        let manifest = directory.join("package.json");
        let Ok(metadata) = fs::metadata(&manifest) else {
            continue;
        };
        if metadata.len() > 64 * 1024 {
            continue;
        }
        let value: serde_json::Value = serde_json::from_slice(&fs::read(manifest).ok()?).ok()?;
        if value.get("name").and_then(serde_json::Value::as_str) != Some(package) {
            continue;
        }
        let root = directory.parent()?.parent()?;
        if root.file_name()? != "node_modules" {
            return None;
        }
        let prefix = root.parent()?;
        if prefix.file_name()? == "global" && prefix.parent()?.file_name()? == "install" {
            let bun = prefix.parent()?.parent()?.join("bin/bun");
            if bun.is_file() && cfg!(unix) {
                return Some(vec![
                    "env".to_owned(),
                    format!("BUN_INSTALL_GLOBAL_DIR={}", prefix.display()),
                    format!("BUN_INSTALL_BIN={}", bun.parent()?.display()),
                    bun.to_string_lossy().into_owned(),
                    "update".to_owned(),
                    "--global".to_owned(),
                    "--latest".to_owned(),
                    package.to_owned(),
                ]);
            }
        }
        if prefix.file_name()? == "lib" {
            let prefix = prefix.parent()?;
            let npm = prefix.join("bin/npm");
            if npm.is_file() {
                return Some(vec![
                    npm.to_string_lossy().into_owned(),
                    "install".to_owned(),
                    "--global".to_owned(),
                    "--prefix".to_owned(),
                    prefix.to_string_lossy().into_owned(),
                    format!("{package}@latest"),
                ]);
            }
        }
        return None;
    }
    None
}

#[must_use]
pub fn terminal_provider_status(
    provider: AgentKind,
    program: &str,
    directory: Option<&str>,
    model_provider: Option<&str>,
) -> TerminalProviderStatus {
    let selector = model_provider.map(PiAccountSelector::provider);
    terminal_provider_status_with_pi_selector(provider, program, directory, selector.as_ref())
}

#[must_use]
pub fn terminal_provider_status_with_pi_selector(
    provider: AgentKind,
    program: &str,
    directory: Option<&str>,
    selector: Option<&PiAccountSelector>,
) -> TerminalProviderStatus {
    let executable = executable(program);
    let mut status = TerminalProviderStatus {
        executable: executable.clone(),
        version: None,
        installer: executable
            .as_deref()
            .and_then(|path| terminal_provider_installer(provider, path))
            .and_then(|argv| {
                argv.iter()
                    .find(|program| program.ends_with("/bun") || program.ends_with("/npm"))
                    .or_else(|| argv.first())
                    .map(|program| {
                        if program.ends_with("/bun") {
                            "Bun"
                        } else if program.ends_with("/npm") {
                            "npm"
                        } else {
                            "Native"
                        }
                        .to_owned()
                    })
            }),
        authenticated: None,
        account: None,
        auth_method: None,
        subscription: None,
        message: None,
    };
    if executable.is_none() {
        status.message =
            Some("Executable not found. Set its path or install the provider.".to_owned());
        return status;
    }
    match crate::terminal_process::query_output(program, &["--version"], || false) {
        Ok(output) if output.successful => {
            status.version = String::from_utf8(output.stdout).ok().and_then(|text| {
                text.lines()
                    .next()
                    .filter(|line| line.len() <= 256 && !line.chars().any(char::is_control))
                    .map(str::to_owned)
            });
        }
        Ok(_) => status.message = Some("The executable did not report a version.".to_owned()),
        Err(error) => status.message = Some(error),
    }
    match crate::terminal_account_status_with_pi_selector_in(provider, program, selector, directory)
    {
        Ok(account) => {
            status.authenticated = account.authenticated;
            status.account = account.account;
            status.auth_method = account.auth_method;
            status.subscription = account.subscription;
            if account.detail.is_some() {
                status.message = account.detail;
            }
        }
        Err(error) => status.message = Some(error),
    }
    status
}

/// # Errors
/// Rejects installation paths without a proven owning installer.
pub fn terminal_provider_update(provider: AgentKind, program: &str) -> Result<AgentLaunch, String> {
    let path = executable(program).ok_or("Provider executable not found")?;
    let argv = terminal_provider_installer(provider, &path)
        .ok_or("Update this installation with its original installer")?;
    if !cfg!(unix) {
        return Err(
            "Automatic provider updates currently require a local POSIX terminal".to_owned(),
        );
    }
    // Keep a short-lived installer's outcome visible. All dynamic values remain argv.
    let mut arguments = vec![
        "-c".to_owned(),
        r#""$@"; status=$?; printf '\nUpdate exited with status %s. Press Enter to close.\n' "$status"; read -r reply; exit "$status""#.to_owned(),
        "bootty-provider-update".to_owned(),
    ];
    arguments.extend(argv);
    Ok(AgentLaunch {
        program: "/bin/sh".to_owned(),
        cwd: None,
        arguments,
        ephemeral: true,
        account_directory: None,
    })
}
