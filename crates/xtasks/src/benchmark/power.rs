use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use serde::Serialize;

use crate::clock::{Timer, utc_datetime};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Directory to receive metadata and raw sample logs.
    pub output_dir: PathBuf,
    /// Command to measure. Pass it after `--`.
    #[arg(last = true, required = true)]
    pub command: Vec<OsString>,
}

#[derive(Serialize)]
struct Metadata {
    schema_version: u8,
    event: &'static str,
    recorded_at_utc: String,
    uname: String,
    command: String,
}

#[derive(Serialize)]
struct Sampler<'a> {
    schema_version: u8,
    event: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<&'a str>,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    log: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'static str>,
}

#[derive(Serialize)]
struct CommandRecord {
    schema_version: u8,
    event: &'static str,
    status: &'static str,
    exit_code: i32,
    duration_s: u64,
    stdout: String,
    stderr: String,
}

/// # Errors
/// Returns invalid command, sampler, subprocess, or measurement output errors.
pub fn run(args: &Args) -> Result<()> {
    crate::cancellation::install()?;
    if args.command.is_empty() {
        bail!("a command is required after --");
    }
    fs::create_dir_all(&args.output_dir)?;
    let mut summary = File::create(args.output_dir.join("summary.jsonl"))?;
    let seconds = environment_u64("BOOTTY_POWER_SECONDS", 10)?;
    let interval_ms = environment_u64("BOOTTY_POWER_INTERVAL_MS", 1_000)?;
    write_json(
        &mut summary,
        &Metadata {
            schema_version: 1,
            event: "power_metadata",
            recorded_at_utc: utc_datetime()?,
            uname: uname(),
            command: args
                .command
                .iter()
                .map(|part| shell_quote(part.as_os_str()))
                .collect::<Vec<_>>()
                .join(" "),
        },
    )?;

    let timer = Timer::start();
    let mut sampler = start_sampler(&args.output_dir, seconds, interval_ms)?;
    match &sampler {
        Some(sampler) => sampler.write_started(&mut summary)?,
        None => write_json(
            &mut summary,
            &Sampler {
                schema_version: 1,
                event: "power_sampler",
                tool: None,
                status: "skipped",
                log: None,
                detail: Some("no powermetrics, pidstat, or nvidia-smi found"),
            },
        )?,
    }

    let stdout_path = args.output_dir.join("command.stdout");
    let stderr_path = args.output_dir.join("command.stderr");
    let (executable, arguments) = args
        .command
        .split_first()
        .context("power measurement requires a command")?;
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdout(File::create(&stdout_path)?)
        .stderr(File::create(&stderr_path)?);
    let outcome = wait_for_command(&mut command)?;
    if let Some(sampler) = &mut sampler {
        let _status = sampler.finish()?;
    }
    let (exit_code, success) = match outcome {
        CommandOutcome::Exited(status) => {
            (crate::cancellation::exit_code(status), status.success())
        }
        CommandOutcome::StartFailed(error) => {
            writeln!(File::create(&stderr_path)?, "{error}")?;
            (127, false)
        }
        CommandOutcome::Interrupted => return Err(crate::cancellation::Interrupted.into()),
    };
    write_json(
        &mut summary,
        &CommandRecord {
            schema_version: 1,
            event: "power_command",
            status: if success { "pass" } else { "fail" },
            exit_code,
            duration_s: timer.elapsed().as_secs(),
            stdout: stdout_path.display().to_string(),
            stderr: stderr_path.display().to_string(),
        },
    )?;
    println!(
        "Wrote power/thermal evidence: {}",
        args.output_dir.display()
    );
    exit_with(exit_code)
}

fn exit_with(exit_code: i32) -> Result<()> {
    if exit_code == 0 {
        Ok(())
    } else {
        Err(CommandFailure(exit_code).into())
    }
}

#[derive(Debug)]
pub struct CommandFailure(i32);

impl CommandFailure {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        self.0
    }
}

impl std::fmt::Display for CommandFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "measured command exited with {}", self.0)
    }
}

impl std::error::Error for CommandFailure {}

/// Owns a sampling process and its deadline, independent of sampler discovery.
pub struct RunningSampler {
    child: Child,
    tool: &'static str,
    log: PathBuf,
    deadline: Duration,
    timer: Timer,
}

impl RunningSampler {
    /// Starts a sampler in its own process group with an explicit clock.
    /// # Errors
    /// Returns log creation or process startup failures.
    pub fn start(
        mut command: Command,
        tool: &'static str,
        log: PathBuf,
        deadline: Duration,
        now: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> Result<Self> {
        let file = File::create(&log)?;
        crate::process::configure_group(&mut command);
        let child = command
            .stdin(Stdio::null())
            .stdout(file.try_clone()?)
            .stderr(file)
            .spawn()?;
        Ok(Self {
            child,
            tool,
            log,
            deadline,
            timer: Timer::with_clock(now),
        })
    }

    /// Records the sampler selected for this measurement.
    /// # Errors
    /// Returns evidence serialization or output errors.
    pub fn write_started(&self, output: &mut impl Write) -> Result<()> {
        write_json(
            output,
            &Sampler {
                schema_version: 1,
                event: "power_sampler",
                tool: Some(self.tool),
                status: "started",
                log: Some(self.log.display().to_string()),
                detail: None,
            },
        )
    }

    /// Waits for completion, cancellation, or the sampling deadline, then reaps the process.
    /// # Errors
    /// Returns process observation or cancellation failures.
    pub fn finish(&mut self) -> Result<ExitStatus> {
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if crate::cancellation::interrupted() {
                crate::process::terminate_group(&mut self.child);
                return Err(crate::cancellation::Interrupted.into());
            }
            if self.timer.elapsed() >= self.deadline {
                crate::process::terminate_group(&mut self.child);
                return self.child.wait().map_err(Into::into);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

enum CommandOutcome {
    Exited(ExitStatus),
    StartFailed(std::io::Error),
    Interrupted,
}

fn wait_for_command(command: &mut Command) -> Result<CommandOutcome> {
    crate::process::configure_group(command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return Ok(CommandOutcome::StartFailed(error)),
    };
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(CommandOutcome::Exited(status));
        }
        if crate::cancellation::interrupted() {
            crate::process::terminate_group(&mut child);
            return Ok(CommandOutcome::Interrupted);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

impl Drop for RunningSampler {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            crate::process::terminate_group(&mut self.child);
        }
    }
}

fn start_sampler(
    output_dir: &Path,
    seconds: u64,
    interval_ms: u64,
) -> Result<Option<RunningSampler>> {
    let Some(plan) = sampler_plan(output_dir, seconds, interval_ms, |program| {
        program_exists(program)
    }) else {
        return Ok(None);
    };
    let nominal = Duration::from_millis(interval_ms.saturating_mul(seconds));
    RunningSampler::start(
        plan.command,
        plan.tool,
        plan.log,
        nominal.saturating_add(Duration::from_secs(1)),
        Arc::new(Instant::now),
    )
    .map(Some)
}

/// The selected sampler command and its output location.
pub struct SamplerPlan {
    pub command: Command,
    pub tool: &'static str,
    pub log: PathBuf,
}

/// Selects a hardware sampler using the available programs on the execution host.
#[must_use]
pub fn sampler_plan(
    output_dir: &Path,
    seconds: u64,
    interval_ms: u64,
    available: impl Fn(&str) -> bool,
) -> Option<SamplerPlan> {
    let (tool, log, command) = if available("powermetrics") {
        let mut command = Command::new("sudo");
        command.args([
            "powermetrics",
            "--samplers",
            "cpu_power,gpu_power,thermal",
            "-i",
            &interval_ms.to_string(),
            "-n",
            &seconds.to_string(),
        ]);
        ("powermetrics", output_dir.join("powermetrics.log"), command)
    } else if available("pidstat") {
        let mut command = Command::new("pidstat");
        command.args(["-durh", "1", &seconds.to_string()]);
        ("pidstat", output_dir.join("pidstat.log"), command)
    } else if available("nvidia-smi") {
        let mut command = Command::new("nvidia-smi");
        command.args([
            "--query-gpu=timestamp,power.draw,utilization.gpu,temperature.gpu",
            "--format=csv",
            "-l",
            "1",
        ]);
        ("nvidia-smi", output_dir.join("nvidia-smi.log"), command)
    } else {
        return None;
    };
    Some(SamplerPlan { command, tool, log })
}

fn environment_u64(name: &str, default: u64) -> Result<u64> {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .with_context(|| format!("{name} must be a non-negative integer")),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error).with_context(|| format!("failed to read {name}")),
    }
}

fn program_exists(program: impl AsRef<OsStr>) -> bool {
    let program = Path::new(program.as_ref());
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

fn uname() -> String {
    Command::new("uname")
        .arg("-a")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map_or_else(
            || "unknown".into(),
            |o| String::from_utf8_lossy(&o.stdout).trim_end().to_owned(),
        )
}

#[cfg(unix)]
fn shell_quote(value: &OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;
    let bytes = value.as_bytes();
    if !bytes.is_empty()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(byte))
    {
        return value.to_string_lossy().into_owned();
    }
    if bytes.is_empty() {
        return "''".into();
    }
    let mut quoted = String::new();
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte) {
            quoted.push(char::from(byte));
        } else if byte.is_ascii_graphic() || byte == b' ' {
            quoted.push('\\');
            quoted.push(char::from(byte));
        } else {
            let _ = write!(quoted, "$'\\x{byte:02x}'");
        }
    }
    quoted
}

#[cfg(not(unix))]
fn shell_quote(value: &OsStr) -> String {
    value.to_string_lossy().into_owned()
}

fn write_json(output: &mut impl Write, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    writeln!(output)?;
    Ok(())
}
