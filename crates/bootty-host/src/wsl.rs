//! WSL is a host transport. All Linux arguments travel through --exec without shell reparsing.
use crate::{
    CommandRunner,
    exec::{REMOTE_PING_SUBCOMMAND, proxy_command_args, remote_exec_program},
    install::{MAX_DAEMON_BYTES, daemon_matches, linux_daemon},
};
use anyhow::{Context, Result, bail};
use bootty_config::config::{WslDistribution, WslRemoteConfig};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

pub const WSL_PROGRAM: &str = "wsl.exe";

#[derive(Clone, Debug)]
pub struct WslRemote {
    config: WslRemoteConfig,
    ready: Arc<Mutex<bool>>,
}
impl PartialEq for WslRemote {
    fn eq(&self, other: &Self) -> bool {
        self.config == other.config
    }
}
impl Eq for WslRemote {}
impl WslRemote {
    #[must_use]
    pub fn new(config: WslRemoteConfig) -> Self {
        Self {
            config,
            ready: Arc::new(Mutex::new(false)),
        }
    }
    #[must_use]
    pub const fn target(&self) -> &WslRemoteConfig {
        &self.config
    }
    #[must_use]
    pub fn command(&self, program: &str, args: &[String]) -> (String, Vec<String>) {
        let mut command = vec![
            "--distribution".to_owned(),
            self.config.distribution.as_str().to_owned(),
            "--cd".to_owned(),
            "~".to_owned(),
            "--exec".to_owned(),
            program.to_owned(),
        ];
        command.extend_from_slice(args);
        (WSL_PROGRAM.to_owned(), command)
    }
    /// # Errors
    /// Returns an error if the WSL daemon command cannot be encoded.
    pub fn proxy_command(
        &self,
        program: &str,
        args: &[String],
        terminal: bool,
    ) -> Result<(String, Vec<String>)> {
        Ok(self.command(
            remote_exec_program(),
            &proxy_command_args(program, args, terminal)?,
        ))
    }
    /// # Errors
    /// Returns installer lock, daemon discovery, or WSL installation errors.
    pub fn ensure_daemon_with(&self, runner: &impl CommandRunner) -> Result<()> {
        let mut ready = self
            .ready
            .lock()
            .map_err(|_| anyhow::anyhow!("WSL daemon installer lock is poisoned"))?;
        if *ready {
            return Ok(());
        }
        let (program, args) =
            self.command(remote_exec_program(), &[REMOTE_PING_SUBCOMMAND.to_owned()]);
        if daemon_matches(&runner.run(&program, &args)?) {
            *ready = true;
            return Ok(());
        }
        let (program, args) = self.command("uname", &["-m".to_owned()]);
        let architecture = runner.run(&program, &args)?;
        if !architecture.success {
            bail!(
                "could not identify WSL architecture: {}",
                architecture.stderr.trim()
            );
        }
        let daemon = linux_daemon(&architecture.stdout)?;
        self.install_daemon(&daemon, runner)?;
        *ready = true;
        drop(ready);
        Ok(())
    }
    /// Verify a complete private candidate before atomically publishing its versioned path.
    /// # Errors
    /// Returns file, size validation, WSL execution, or remote verification errors.
    pub fn install_daemon(&self, daemon: &Path, runner: &impl CommandRunner) -> Result<()> {
        use std::io::Read as _;
        let mut file = std::fs::File::open(daemon)
            .context("open WSL daemon asset")?
            .take(MAX_DAEMON_BYTES + 1);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .context("read WSL daemon asset")?;
        if bytes.is_empty() || u64::try_from(bytes.len())? > MAX_DAEMON_BYTES {
            bail!("WSL daemon asset must be between 1 byte and 256 MiB");
        }
        let digest = crate::install::checksum(&bytes);
        let mut generation = [0_u8; 16];
        getrandom::fill(&mut generation).context("allocate WSL daemon candidate")?;
        let remote_program = remote_exec_program();
        let candidate = format!(
            "{remote_program}.{:032x}.upload",
            u128::from_ne_bytes(generation)
        );
        let expected = format!(
            "{}:{}",
            crate::exec::REMOTE_DAEMON_PROTOCOL_VERSION,
            env!("CARGO_PKG_VERSION")
        );
        let script = format!(
            "set -eu\numask 077\nmkdir -p .bootty/bin\ncandidate={}\ntrap 'rm -f -- \"$candidate\"' EXIT HUP INT TERM\ncat > \"$candidate\"\nprintf '%s  %s\\n' {} \"$candidate\" | sha256sum -c - >/dev/null\nchmod 700 \"$candidate\"\n[ \"$(\"$candidate\" {REMOTE_PING_SUBCOMMAND})\" = {} ]\nmv -f -- \"$candidate\" {remote_program}\n",
            crate::shell_quote(&candidate),
            crate::shell_quote(&digest),
            crate::shell_quote(&expected)
        );
        let (program, args) = self.command("/bin/sh", &["-c".to_owned(), script]);
        let result = runner.run_with_input(&program, &args, bytes)?;
        if !result.success {
            bail!("install WSL daemon: {}", result.stderr.trim());
        }
        let (program, args) =
            self.command(remote_exec_program(), &[REMOTE_PING_SUBCOMMAND.to_owned()]);
        if !daemon_matches(&runner.run(&program, &args)?) {
            bail!("WSL daemon did not report the expected protocol and version");
        }
        Ok(())
    }
}

/// WSL list output is normally UTF-16LE; redirected Linux command output remains UTF-8.
/// # Errors
/// Returns an error for invalid UTF-8 or UTF-16 output.
pub fn decode_wsl_text(bytes: &[u8]) -> Result<String> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.contains(&0) {
        let bytes = bytes.strip_prefix(&[0xff, 0xfe]).unwrap_or(bytes);
        if !bytes.len().is_multiple_of(2) {
            bail!("truncated UTF-16 WSL output");
        }
        let units = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        String::from_utf16(&units).context("decode UTF-16 WSL output")
    } else {
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        String::from_utf8(bytes.to_vec()).context("decode UTF-8 WSL output")
    }
}

/// # Errors
/// Returns WSL execution or distribution output decoding errors.
pub fn distributions(runner: &impl CommandRunner) -> Result<Vec<WslDistribution>> {
    let output = runner.run_bytes(WSL_PROGRAM, &["--list".to_owned(), "--quiet".to_owned()])?;
    if !output.success {
        bail!(
            "list WSL distributions: {}",
            decode_wsl_text(&output.stderr)?.trim()
        );
    }
    let mut distributions = decode_wsl_text(&output.stdout)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| WslDistribution::new(line).map_err(anyhow::Error::msg))
        .collect::<Result<Vec<_>>>()?;
    distributions.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    distributions.dedup();
    Ok(distributions)
}
