//! Host transport shared by terminal backends, files, Git and clipboard uploads.
use crate::{CommandOutput, CommandRunner, SystemCommandRunner, ssh::SshRemote, wsl::WslRemote};
use anyhow::Result;
use bootty_config::config::RemoteConfig;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteHost {
    Ssh(SshRemote),
    Wsl(WslRemote),
}
impl RemoteHost {
    pub fn new(config: impl Into<RemoteConfig>) -> Self {
        match config.into() {
            RemoteConfig::Ssh(config) => Self::Ssh(SshRemote::new(config)),
            RemoteConfig::Wsl(config) => Self::Wsl(WslRemote::new(config)),
        }
    }
    #[must_use]
    pub fn target(&self) -> RemoteConfig {
        match self {
            Self::Ssh(remote) => remote.target().clone().into(),
            Self::Wsl(remote) => remote.target().clone().into(),
        }
    }
    #[must_use]
    pub const fn as_ssh(&self) -> Option<&SshRemote> {
        match self {
            Self::Ssh(remote) => Some(remote),
            Self::Wsl(_) => None,
        }
    }
    #[must_use]
    pub fn host(&self) -> &str {
        match self {
            Self::Ssh(remote) => remote.host(),
            Self::Wsl(remote) => remote.target().distribution.as_str(),
        }
    }
    #[must_use]
    pub fn destination(&self) -> String {
        match self {
            Self::Ssh(remote) => remote.destination(),
            Self::Wsl(remote) => format!("wsl:{}", remote.target().distribution.as_str()),
        }
    }
    /// # Errors
    /// Returns daemon discovery or installation errors from the selected transport.
    pub fn ensure_daemon(&self) -> Result<()> {
        self.ensure_daemon_with(&SystemCommandRunner)
    }
    /// # Errors
    /// Returns daemon discovery or installation errors from the selected transport.
    pub fn ensure_daemon_with(&self, runner: &impl CommandRunner) -> Result<()> {
        match self {
            Self::Ssh(remote) => remote.ensure_daemon_with(runner),
            Self::Wsl(remote) => remote.ensure_daemon_with(runner),
        }
    }
    #[must_use]
    pub fn command(&self, program: &str, args: &[String]) -> (String, Vec<String>) {
        match self {
            Self::Ssh(remote) => remote.command(program, args),
            Self::Wsl(remote) => remote.command(program, args),
        }
    }
    /// # Errors
    /// Returns an error if the selected transport cannot encode the command.
    pub fn proxy_command(&self, program: &str, args: &[String]) -> Result<(String, Vec<String>)> {
        match self {
            Self::Ssh(remote) => remote.proxy_command(program, args),
            Self::Wsl(remote) => remote.proxy_command(program, args, false),
        }
    }
    /// # Errors
    /// Returns an error if the selected transport cannot encode the PTY command.
    pub fn proxy_tty_command(
        &self,
        program: &str,
        args: &[String],
    ) -> Result<(String, Vec<String>)> {
        match self {
            Self::Ssh(remote) => remote.proxy_tty_command(program, args),
            Self::Wsl(remote) => remote.proxy_command(program, args, true),
        }
    }
}
impl From<SshRemote> for RemoteHost {
    fn from(remote: SshRemote) -> Self {
        Self::Ssh(remote)
    }
}

#[derive(Clone, Debug)]
pub struct RemoteCommandRunner<R> {
    remote: RemoteHost,
    runner: R,
}
impl<R> RemoteCommandRunner<R> {
    pub fn new(remote: impl Into<RemoteHost>, runner: R) -> Self {
        Self {
            remote: remote.into(),
            runner,
        }
    }
}
impl<R: CommandRunner> CommandRunner for RemoteCommandRunner<R> {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        self.remote.ensure_daemon_with(&self.runner)?;
        let (program, args) = self.remote.proxy_command(program, args)?;
        self.runner.run(&program, &args)
    }
    fn run_with_input(
        &self,
        program: &str,
        args: &[String],
        input: Vec<u8>,
    ) -> Result<CommandOutput> {
        self.remote.ensure_daemon_with(&self.runner)?;
        let (program, args) = self.remote.proxy_command(program, args)?;
        self.runner.run_with_input(&program, &args, input)
    }
    fn run_disowned(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        self.remote.ensure_daemon_with(&self.runner)?;
        let (program, args) = self.remote.proxy_command(program, args)?;
        self.runner.run_disowned(&program, &args)
    }
}

pub use crate::ssh::{
    REMOTE_DAEMON_PROGRAM, REMOTE_DAEMON_PROTOCOL_VERSION, remote_daemon_failure,
};
