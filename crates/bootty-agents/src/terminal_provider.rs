use std::{
    fs,
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
    bootty_host::find_executable(program)
}

/// Recognize only the installation that owns the selected executable.
/// Unknown installations remain manual until their owning installer can be proven.
#[must_use]
pub fn terminal_provider_installer(provider: AgentKind, executable: &Path) -> Option<Vec<String>> {
    #[cfg(windows)]
    {
        windows_installer(provider, executable)
    }
    #[cfg(not(windows))]
    {
        unix_installer(provider, executable)
    }
}

#[cfg(not(windows))]
fn unix_installer(provider: AgentKind, executable: &Path) -> Option<Vec<String>> {
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
        let Some(value) = read_package_manifest(&directory.join("package.json")) else {
            continue;
        };
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

#[cfg(windows)]
fn windows_installer(provider: AgentKind, executable: &Path) -> Option<Vec<String>> {
    // Resolve links for ownership, but retain the selected path when launching.
    let resolved = executable.canonicalize().ok()?;
    if provider == AgentKind::Claude {
        if resolved.ends_with(
            "AppData/Local/Microsoft/WinGet/Packages/Anthropic.ClaudeCode_Microsoft.Winget.Source_8wekyb3d8bbwe/claude.exe",
        ) {
            return Some(vec![
                bootty_host::find_executable("winget")?.to_string_lossy().into_owned(),
                "upgrade".into(),
                "--id".into(),
                "Anthropic.ClaudeCode".into(),
                "--exact".into(),
                "--source".into(),
                "winget".into(),
                "--scope".into(),
                "user".into(),
                "--silent".into(),
                "--disable-interactivity".into(),
                "--accept-source-agreements".into(),
                "--accept-package-agreements".into(),
            ]);
        }
        if resolved.ends_with(".local/bin/claude.exe")
            || resolved.parent()?.file_name()? == "versions"
                && resolved.parent()?.parent()?.file_name()? == "claude"
        {
            return Some(vec![
                executable.to_string_lossy().into_owned(),
                "update".into(),
            ]);
        }
    }
    windows_npm_installer(provider, executable)
}

fn read_package_manifest(path: &Path) -> Option<serde_json::Value> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

#[cfg(windows)]
fn windows_npm_installer(provider: AgentKind, executable: &Path) -> Option<Vec<String>> {
    let (command, package) = match provider {
        AgentKind::Codex => ("codex", "@openai/codex"),
        AgentKind::Claude => ("claude", "@anthropic-ai/claude-code"),
        AgentKind::Pi => ("pi", "@earendil-works/pi-coding-agent"),
    };
    if !executable
        .file_name()?
        .to_str()?
        .eq_ignore_ascii_case(&format!("{command}.cmd"))
    {
        return None;
    }
    let prefix = executable.parent()?;
    let directory = prefix.join("node_modules").join(package);
    let manifest = read_package_manifest(&directory.join("package.json"))?;
    if manifest.get("name")?.as_str()? != package {
        return None;
    }
    let bin = manifest.get("bin")?;
    let entry = bin.as_str().or_else(|| bin.get(command)?.as_str())?;
    let entry = Path::new(entry);
    if !entry.components().all(|part| {
        matches!(
            part,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    }) {
        return None;
    }
    let entry: PathBuf = entry
        .components()
        .filter(|part| *part != std::path::Component::CurDir)
        .collect();
    if !directory.join(&entry).is_file() {
        return None;
    }
    let metadata = fs::metadata(executable).ok()?;
    if metadata.len() > 64 * 1024 {
        return None;
    }
    let shim = fs::read_to_string(executable).ok()?;
    let target = format!("node_modules/{package}/{}", entry.display()).replace('/', "\\");
    if !shim.contains(&format!("\"%dp0%\\{target}\""))
        && !shim.contains(&format!("\"%~dp0\\{target}\""))
    {
        return None;
    }
    let npm = bootty_host::find_executable(prefix.join("npm.cmd"))
        .or_else(|| bootty_host::find_executable("npm"))?;
    Some(vec![
        npm.to_string_lossy().into_owned(),
        "install".into(),
        "--global".into(),
        "--prefix".into(),
        prefix.to_string_lossy().into_owned(),
        format!("{package}@latest"),
    ])
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
                    .find(|program| installer_name(program).is_some())
                    .or_else(|| argv.first())
                    .map(|program| installer_name(program).unwrap_or("Native").to_owned())
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

fn installer_name(program: &str) -> Option<&'static str> {
    match Path::new(program).file_stem()?.to_str()? {
        "bun" => Some("Bun"),
        "npm" => Some("npm"),
        "winget" => Some("WinGet"),
        _ => None,
    }
}

/// # Errors
/// Rejects installation paths without a proven owning installer.
pub fn terminal_provider_update(provider: AgentKind, program: &str) -> Result<AgentLaunch, String> {
    let path = executable(program).ok_or("Provider executable not found")?;
    let argv = terminal_provider_installer(provider, &path)
        .ok_or("Update this installation with its original installer")?;
    if cfg!(windows) {
        let (program, arguments) = argv
            .split_first()
            .ok_or("The installer has no executable")?;
        return Ok(AgentLaunch {
            program: program.clone(),
            cwd: None,
            arguments: arguments.to_vec(),
            ephemeral: true,
            account_directory: None,
        });
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
