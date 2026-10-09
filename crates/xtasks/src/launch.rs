#[cfg(not(target_os = "macos"))]
use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

#[cfg(not(target_os = "macos"))]
use anyhow::bail;
use anyhow::{Context, Result};
use bootty_config::DEVELOPMENT_NAMESPACE_ENV;
use clap::Args as ClapArgs;

#[derive(Clone, Debug, ClapArgs)]
#[command(trailing_var_arg = true)]
pub struct Args {
    /// Arguments passed to Bootty.
    #[arg(allow_hyphen_values = true)]
    pub arguments: Vec<OsString>,
}

/// # Errors
/// Returns workspace discovery, development packaging, or application launch errors.
pub fn run(args: Args) -> Result<()> {
    let binary = build_launch_binary()?;
    let names = crate::development_names()?;
    let mut command = Command::new(&binary);
    #[cfg(target_os = "macos")]
    if args
        .arguments
        .iter()
        .find(|arg| !matches!(arg.to_str(), Some("--json" | "--start")))
        .is_none_or(|arg| arg == "app")
    {
        let bundle = binary
            .parent()
            .and_then(std::path::Path::parent)
            .and_then(std::path::Path::parent)
            .context("development app bundle unavailable")?;
        // Launch Services publishes the bundle/process identity used by exact-window computer use.
        // Command invocations keep their direct stdin, stdout and observed exit status.
        let logs = bundle
            .parent()
            .context("development log directory unavailable")?;
        let stdout = logs.join("launch.stdout.log");
        let stderr = logs.join("launch.stderr.log");
        eprintln!(
            "Development app logs: {} and {}",
            stdout.display(),
            stderr.display()
        );
        command = Command::new("/usr/bin/open");
        command
            .args(["-n", "-W"])
            .arg(bundle)
            .arg("--stdout")
            .arg(stdout)
            .arg("--stderr")
            .arg(stderr)
            .arg("--env")
            .arg(format!("{DEVELOPMENT_NAMESPACE_ENV}={}", names.namespace()))
            .arg("--args");
    }
    command
        .args(args.arguments)
        .env(DEVELOPMENT_NAMESPACE_ENV, names.namespace());
    // Cargo's test-only bundle lookup override must not replace the app's identity.
    #[cfg(target_os = "macos")]
    command.env_remove("CFProcessPath");
    #[cfg(not(target_os = "macos"))]
    configure_library_path(&mut command)?;
    execute(command, &binary)
}

#[cfg(target_os = "macos")]
fn build_launch_binary() -> Result<PathBuf> {
    let args = crate::package::Args {
        dev: true,
        ..Default::default()
    };
    crate::package::run(args)?;
    let layout = crate::package::Layout::from_args(args)?;
    Ok(layout
        .dist_dir
        .join(format!("{}.app", layout.app_name))
        .join("Contents/MacOS/bootty"))
}

#[cfg(not(target_os = "macos"))]
fn build_launch_binary() -> Result<PathBuf> {
    let build = crate::build::BuildArgs {
        fast: false,
        static_linkage: false,
    };
    crate::build::run_with_features(&build, true)?;

    let target_root =
        env::var_os("CARGO_TARGET_DIR").map_or_else(|| PathBuf::from("target"), PathBuf::from);
    let profile = target_root.join("dynamic-release");
    let binary = profile.join(if cfg!(windows) {
        "bootty.exe"
    } else {
        "bootty"
    });
    if !binary.is_file() {
        bail!("built binary not found at {}", binary.display());
    }
    Ok(binary)
}

#[cfg(not(target_os = "macos"))]
fn configure_library_path(command: &mut Command) -> Result<()> {
    let target_root =
        env::var_os("CARGO_TARGET_DIR").map_or_else(|| PathBuf::from("target"), PathBuf::from);
    let profile = target_root.join("dynamic-release");
    let rust_libdir =
        crate::command::stdout(Command::new("rustc").args(["--print", "target-libdir"]))?;
    let mut library_dirs = vec![profile.join("deps"), PathBuf::from(rust_libdir.trim())];
    #[cfg(windows)]
    if let Some(directory) = find_ghostty_dll(&profile)?.parent() {
        library_dirs.push(directory.to_path_buf());
    }
    let inherited = env::var_os(library_path_variable()).unwrap_or_default();
    if !inherited.is_empty() {
        library_dirs.extend(env::split_paths(&inherited));
    }
    let library_path = env::join_paths(library_dirs).context("failed to construct library path")?;

    command.env(library_path_variable(), library_path);
    Ok(())
}

#[cfg(unix)]
fn execute(mut command: Command, binary: &std::path::Path) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let error = command.exec();
    Err(error).with_context(|| format!("failed to execute {}", binary.display()))
}

#[cfg(windows)]
fn execute(mut command: Command, _binary: &std::path::Path) -> Result<()> {
    crate::command::run(&mut command)
}

#[cfg(not(target_os = "macos"))]
const fn library_path_variable() -> &'static str {
    if cfg!(windows) {
        "PATH"
    } else {
        "LD_LIBRARY_PATH"
    }
}

#[cfg(windows)]
fn find_ghostty_dll(profile: &std::path::Path) -> Result<PathBuf> {
    crate::filesystem::files_recursive(profile)?
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == "ghostty-vt.dll")
        })
        .context("ghostty-vt.dll was not built")
}
