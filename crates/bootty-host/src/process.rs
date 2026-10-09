use anyhow::{Context, Result, bail};
use rmux_os::process_tree::{ConsoleWindowBehavior, ProcessTreeChild};
use std::{
    io::{self, Read, Write},
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
use std::{env, ffi::OsStr, path::Path};

#[cfg(target_os = "macos")]
use std::sync::atomic::AtomicU64;

#[cfg(all(unix, not(target_os = "macos")))]
use std::os::unix::process::CommandExt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandBytes {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
impl From<Output> for CommandBytes {
    fn from(output: Output) -> Self {
        Self {
            success: output.status.success(),
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }
}

// Captured control/Git commands are bounded. Larger results need the job or transfer stream.
const MAX_CAPTURE_BYTES: u64 = 16 * 1024 * 1024;

pub trait CommandRunner {
    /// Run in the captured host directory without changing the application's directory.
    /// # Errors
    /// Returns unsupported operation, setup, cancellation, execution or decoding errors.
    fn run_in(&self, _cwd: &str, _program: &str, _args: &[String]) -> Result<CommandOutput> {
        bail!("command runner does not support a working directory")
    }

    /// Capture non-UTF-8 command output without a lossy text conversion.
    /// # Errors
    /// Returns unsupported operation, process setup, execution, cancellation, or output limit errors.
    fn run_bytes(&self, _program: &str, _args: &[String]) -> Result<CommandBytes> {
        bail!("command runner does not support byte output")
    }

    /// # Errors
    /// Returns process setup, execution, cancellation, or output decoding errors.
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput>;

    /// # Errors
    /// Returns process setup, input delivery, execution, cancellation, or output errors.
    fn run_with_input(
        &self,
        _program: &str,
        _args: &[String],
        _input: Vec<u8>,
    ) -> Result<CommandOutput> {
        bail!("command runner does not support streamed input")
    }

    /// # Errors
    /// Returns an error if detached execution is unsupported or the process cannot be started.
    fn run_disowned(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        self.run(program, args)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run_in(&self, cwd: &str, program: &str, args: &[String]) -> Result<CommandOutput> {
        cancellable_command_output(
            program,
            args,
            &CommandCancellation::default(),
            None,
            None,
            None,
            Some(cwd),
        )
    }

    fn run_bytes(&self, program: &str, args: &[String]) -> Result<CommandBytes> {
        cancellable_command_bytes(
            program,
            args,
            &CommandCancellation::default(),
            None,
            None,
            None,
            None,
        )
        .map(CommandBytes::from)
    }

    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        cancellable_command_output(
            program,
            args,
            &CommandCancellation::default(),
            None,
            None,
            None,
            None,
        )
    }

    fn run_with_input(
        &self,
        program: &str,
        args: &[String],
        input: Vec<u8>,
    ) -> Result<CommandOutput> {
        cancellable_command_output(
            program,
            args,
            &CommandCancellation::default(),
            None,
            None,
            Some(input),
            None,
        )
    }

    fn run_disowned(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        disowned_command_output(program, args)
    }
}
#[derive(Clone, Debug, Default)]
pub struct CommandCancellation(Arc<AtomicBool>);

impl CommandCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}

#[derive(Clone)]
pub struct CancellableCommandRunner {
    cancellation: CommandCancellation,
    deadline: Option<Instant>,
    cancellation_check: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl std::fmt::Debug for CancellableCommandRunner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CancellableCommandRunner")
            .field("cancellation", &self.cancellation)
            .field("deadline", &self.deadline)
            .field("cancellation_check", &self.cancellation_check.is_some())
            .finish()
    }
}

impl CancellableCommandRunner {
    #[must_use]
    pub fn new(cancellation: CommandCancellation) -> Self {
        Self {
            cancellation,
            deadline: None,
            cancellation_check: None,
        }
    }

    /// Run commands until `deadline`, terminating a child that outlives it.
    #[must_use]
    pub fn with_deadline(cancellation: CommandCancellation, deadline: Instant) -> Self {
        Self {
            cancellation,
            deadline: Some(deadline),
            cancellation_check: None,
        }
    }

    /// Run commands with a caller-owned cancellation signal and deadline.
    ///
    /// The local token keeps the host API independent from higher-level command transports. A
    /// caller can inject its own cancellation state when that state already belongs to another
    /// crate.
    pub fn with_deadline_and_cancellation_check(
        cancellation: CommandCancellation,
        deadline: Instant,
        cancellation_check: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            cancellation,
            deadline: Some(deadline),
            cancellation_check: Some(Arc::new(cancellation_check)),
        }
    }
}

impl CommandRunner for CancellableCommandRunner {
    fn run_in(&self, cwd: &str, program: &str, args: &[String]) -> Result<CommandOutput> {
        cancellable_command_output(
            program,
            args,
            &self.cancellation,
            self.deadline,
            self.cancellation_check.as_deref(),
            None,
            Some(cwd),
        )
    }

    fn run_bytes(&self, program: &str, args: &[String]) -> Result<CommandBytes> {
        cancellable_command_bytes(
            program,
            args,
            &self.cancellation,
            self.deadline,
            self.cancellation_check.as_deref(),
            None,
            None,
        )
        .map(CommandBytes::from)
    }

    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        cancellable_command_output(
            program,
            args,
            &self.cancellation,
            self.deadline,
            self.cancellation_check.as_deref(),
            None,
            None,
        )
    }
    fn run_with_input(
        &self,
        program: &str,
        args: &[String],
        input: Vec<u8>,
    ) -> Result<CommandOutput> {
        cancellable_command_output(
            program,
            args,
            &self.cancellation,
            self.deadline,
            self.cancellation_check.as_deref(),
            Some(input),
            None,
        )
    }
}

fn cancellable_command_output(
    program: &str,
    args: &[String],
    cancellation: &CommandCancellation,
    deadline: Option<Instant>,
    cancellation_check: Option<&(dyn Fn() -> bool + Send + Sync)>,
    input: Option<Vec<u8>>,
    cwd: Option<&str>,
) -> Result<CommandOutput> {
    let output = cancellable_command_bytes(
        program,
        args,
        cancellation,
        deadline,
        cancellation_check,
        input,
        cwd,
    )?;
    command_output(program, Ok(output))
}

fn cancellable_command_bytes(
    program: &str,
    args: &[String],
    cancellation: &CommandCancellation,
    deadline: Option<Instant>,
    cancellation_check: Option<&(dyn Fn() -> bool + Send + Sync)>,
    input: Option<Vec<u8>>,
    cwd: Option<&str>,
) -> Result<Output> {
    if is_cancelled(cancellation, deadline, cancellation_check) {
        bail!("command canceled")
    }
    let mut command = Command::new(program);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child =
        ProcessTreeChild::spawn_with_console_window(&mut command, ConsoleWindowBehavior::Suppress)
            .with_context(|| format!("run {program}"))?;
    let waiter = thread::current();
    let writer = input
        .map(|input| -> Result<_> {
            let mut stdin = child
                .child_mut()
                .stdin
                .take()
                .context("piped command input")?;
            let waiter = waiter.clone();
            Ok(thread::spawn(move || {
                let result = stdin.write_all(&input);
                drop(stdin);
                waiter.unpark();
                result
            }))
        })
        .transpose()?;
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout = read_pipe(
        child.child_mut().stdout.take().context("capture stdout")?,
        exceeded.clone(),
        waiter.clone(),
    );
    let stderr = read_pipe(
        child.child_mut().stderr.take().context("capture stderr")?,
        exceeded.clone(),
        waiter,
    );
    let status = loop {
        if exceeded.load(Ordering::Acquire)
            || is_cancelled(cancellation, deadline, cancellation_check)
        {
            let _ = child.terminate();
            let _ = child.wait();
            for reader in [stdout, stderr] {
                let _ = join_pipe(reader);
            }
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            if exceeded.load(Ordering::Acquire) {
                bail!("command output exceeds the 16 MiB capture limit")
            }
            bail!("command canceled")
        }
        if child
            .has_exited()
            .with_context(|| format!("wait for {program}"))?
            && stdout.is_finished()
            && stderr.is_finished()
            && writer.as_ref().is_none_or(thread::JoinHandle::is_finished)
            && !exceeded.load(Ordering::Acquire)
        {
            // Keep the tree cancellable while descendants still own a captured pipe.
            // Normal completion preserves deliberately detached backend processes.
            break child.wait().with_context(|| format!("reap {program}"))?;
        }
        thread::park_timeout(Duration::from_millis(10));
    };
    if let Some(writer) = writer {
        let written = writer
            .join()
            .map_err(|_| anyhow::anyhow!("command input writer stopped"))?;
        // On remote rejection preserve its stderr instead of replacing it with a broken pipe.
        if status.success() {
            written.context("write command input")?;
        }
    }
    Ok(Output {
        status,
        stdout: join_pipe(stdout)?,
        stderr: join_pipe(stderr)?,
    })
}

fn is_cancelled(
    cancellation: &CommandCancellation,
    deadline: Option<Instant>,
    cancellation_check: Option<&(dyn Fn() -> bool + Send + Sync)>,
) -> bool {
    cancellation.0.load(Ordering::Acquire)
        || cancellation_check.is_some_and(|check| check())
        || deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

fn read_pipe(
    pipe: impl Read + Send + 'static,
    exceeded: Arc<AtomicBool>,
    waiter: thread::Thread,
) -> thread::JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = pipe.take(MAX_CAPTURE_BYTES + 1).read_to_end(&mut bytes);
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CAPTURE_BYTES {
            exceeded.store(true, Ordering::Release);
        }
        waiter.unpark();
        result.map(|_| bytes)
    })
}

fn join_pipe(reader: thread::JoinHandle<io::Result<Vec<u8>>>) -> Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("command output reader stopped"))?
        .context("read command output")
}

#[cfg(target_os = "macos")]
static DISOWNED_COMMAND_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(target_os = "macos")]
const DISOWNED_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(target_os = "macos")]
const LAUNCHD_START_GRACE: Duration = Duration::from_millis(50);

#[cfg(target_os = "macos")]
const LAUNCHD_SUBMIT_SCRIPT: &str = r#"program=$1
shift
exec "$program" "$@"
"#;

#[cfg(target_os = "macos")]
fn disowned_command_output(program: &str, args: &[String]) -> Result<CommandOutput> {
    let resolved_program = resolve_program(program)?;
    let launchctl = resolve_program("launchctl")?;
    let shell = resolve_program("sh")?;
    let id = DISOWNED_COMMAND_COUNTER.fetch_add(1, Ordering::Relaxed);
    let label = format!("dev.bootty.disowned.{}.{}", std::process::id(), id);
    let mut script = macos_shell_environment_prelude();
    script.push_str(LAUNCHD_SUBMIT_SCRIPT);
    let directory = tempfile::tempdir().context("prepare detached command output")?;
    let stdout = tempfile::NamedTempFile::new_in(directory.path())?;
    let stderr = tempfile::NamedTempFile::new_in(directory.path())?;

    let output = command_output(
        "launchctl",
        Command::new(&launchctl)
            .args(["submit", "-l", &label, "-o"])
            .arg(stdout.path())
            .arg("-e")
            .arg(stderr.path())
            .args(["--", &shell, "-c"])
            .arg(script)
            .args(["bootty-disowned", &resolved_program])
            .args(args)
            .output(),
    )?;
    if !output.success {
        return Ok(output);
    }

    let status = wait_for_launchd_exit_with_capture(
        &launchctl,
        &label,
        DISOWNED_COMMAND_TIMEOUT,
        &[stdout.path(), stderr.path()],
    )
    .with_context(|| format!("wait for disowned {program}"));
    let _ = Command::new(&launchctl).args(["remove", &label]).output();
    let status = status?;
    let stdout = read_disowned_output(&stdout)?;
    let stderr = read_disowned_output(&stderr)?;
    Ok(CommandOutput {
        success: status == 0,
        stdout,
        stderr: if status != 0 && stderr.is_empty() {
            format!("process exited with status {status}")
        } else {
            stderr
        },
    })
}

#[cfg(target_os = "macos")]
fn read_disowned_output(file: &tempfile::NamedTempFile) -> Result<String> {
    let mut bytes = Vec::new();
    file.as_file()
        .take(MAX_CAPTURE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())? > MAX_CAPTURE_BYTES {
        bail!("command output exceeds the 16 MiB capture limit")
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(target_os = "macos")]
#[must_use]
pub fn macos_shell_environment_prelude() -> String {
    macos_shell_environment_prelude_from(env::vars_os())
}

#[cfg(target_os = "macos")]
pub fn macos_shell_environment_prelude_from<I, K, V>(vars: I) -> String
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let mut script = String::new();
    for (key, value) in vars {
        let key = key.as_ref().to_string_lossy();
        if !is_shell_identifier(&key) {
            continue;
        }
        script.push_str(&key);
        script.push('=');
        script.push_str(&shell_single_quote(&value.as_ref().to_string_lossy()));
        script.push_str("; export ");
        script.push_str(&key);
        script.push('\n');
    }
    script
}

#[cfg(target_os = "macos")]
fn is_shell_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

#[cfg(target_os = "macos")]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
/// # Errors
/// Returns launchctl execution, status parsing, or timeout errors.
pub fn wait_for_launchd_exit(launchctl: &str, label: &str, timeout: Duration) -> Result<i32> {
    wait_for_launchd_exit_with_capture(launchctl, label, timeout, &[])
}

#[cfg(target_os = "macos")]
fn wait_for_launchd_exit_with_capture(
    launchctl: &str,
    label: &str,
    timeout: Duration,
    captures: &[&Path],
) -> Result<i32> {
    let start = Instant::now();
    let mut observed_pid = false;
    while start.elapsed() < timeout {
        // launchd writes to private files, so stop an overflowing job while it is live.
        for path in captures {
            if std::fs::metadata(path)?.len() > MAX_CAPTURE_BYTES {
                bail!("command output exceeds the 16 MiB capture limit")
            }
        }
        let output = Command::new(launchctl).args(["list", label]).output()?;
        let text = String::from_utf8_lossy(&output.stdout);
        if text.contains("\"PID\"") {
            observed_pid = true;
        } else if observed_pid || start.elapsed() >= LAUNCHD_START_GRACE {
            return parse_launchd_exit_status(&text)
                .with_context(|| format!("parse launchd status for {label}"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    bail!("disowned command did not exit before timeout")
}

#[cfg(target_os = "macos")]
fn parse_launchd_exit_status(text: &str) -> Result<i32> {
    text.lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("\"LastExitStatus\" = ")
                .and_then(|value| value.trim_end_matches(';').parse().ok())
        })
        .context("missing LastExitStatus")
}

#[cfg(target_os = "macos")]
/// # Errors
/// Returns an error if the executable cannot be resolved on the host.
pub fn resolve_program(program: &str) -> Result<String> {
    resolve_program_with_path(program, env::var_os("PATH").as_deref())
}

#[cfg(target_os = "macos")]
fn resolve_program_with_path(program: &str, path: Option<&OsStr>) -> Result<String> {
    if Path::new(program).is_absolute() || program.contains(std::path::MAIN_SEPARATOR) {
        return Ok(program.to_owned());
    }
    path.into_iter()
        .flat_map(env::split_paths)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .map(|found| found.to_string_lossy().into_owned())
        .ok_or_else(|| anyhow::anyhow!("program {program:?} not found in PATH"))
}

#[cfg(not(target_os = "macos"))]
fn disowned_command_output(program: &str, args: &[String]) -> Result<CommandOutput> {
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null());

    #[cfg(unix)]
    command.process_group(0);

    command_output(program, command.output())
}

fn command_output(program: &str, output: std::io::Result<Output>) -> Result<CommandOutput> {
    let output = output.with_context(|| format!("run {program}"))?;
    Ok(CommandOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// # Errors
/// Returns an error containing stderr when the command reports failure.
pub fn require_success(_program: &str, _args: &[String], output: CommandOutput) -> Result<String> {
    if output.success {
        return Ok(output.stdout);
    }

    let detail = output.stderr.trim();
    if detail.is_empty() {
        bail!("command failed")
    }
    bail!("{detail}")
}
