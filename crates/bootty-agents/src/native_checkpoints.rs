//! Snapshots of a conversation's work tree around each turn: the prompt keeps the files as they
//! were before its turn, and the turn's last reply keeps them as they were after.

use crate::NativeSessionSnapshot;
use bootty_host::{SystemCommandRunner, remote::RemoteCommandRunner, remote::RemoteHost};

/// Where a conversation's files are, on this computer or a remote host.
#[derive(Clone)]
pub struct CheckpointHost {
    pub directory: String,
    pub remote: Option<bootty_config::config::RemoteConfig>,
}

impl CheckpointHost {
    /// Runs `operation` with the runner for the host the files are on.
    fn with_runner<T>(&self, operation: impl FnOnce(&dyn Runner, &str) -> T) -> T {
        match &self.remote {
            None => operation(&SystemCommandRunner, &self.directory),
            Some(host) => operation(
                &RemoteCommandRunner::new(RemoteHost::new(host.clone()), SystemCommandRunner),
                &self.directory,
            ),
        }
    }

    /// Records the files now, under the conversation `owner`, as `name`. Nothing is recorded
    /// outside Git or when Git fails; the turn goes on without a snapshot.
    pub fn capture(&self, owner: &str, name: &str) -> Option<String> {
        // Each turn records twice; outside a repository, starting Git only learns there is none.
        if self.remote.is_none()
            && !bootty_git::checkpoint::may_capture_locally(std::path::Path::new(&self.directory))
        {
            return None;
        }
        let reference = format!("{}/{name}", bootty_git::checkpoint::namespace(owner));
        self.with_runner(|runner, directory| {
            bootty_git::checkpoint::capture(&runner, directory, &reference)
        })
        .ok()
        .flatten()
    }

    pub fn diff(&self, from: &str, to: &str) -> Result<String, String> {
        self.with_runner(|runner, directory| {
            bootty_git::checkpoint::diff(&runner, directory, from, to)
        })
    }

    pub fn restore(&self, commit: &str) -> Result<(), String> {
        self.with_runner(|runner, directory| {
            bootty_git::checkpoint::restore(&runner, directory, commit)
        })
    }

    pub fn prune(&self, owner: &str) -> Result<(), String> {
        let namespace = bootty_git::checkpoint::namespace(owner);
        self.with_runner(|runner, directory| {
            bootty_git::checkpoint::prune(&runner, directory, &namespace)
        })
    }
}

/// A command runner for either host, behind one reference.
trait Runner {
    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<bootty_host::CommandOutput>;
}

impl<T: bootty_host::CommandRunner> Runner for T {
    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<bootty_host::CommandOutput> {
        bootty_host::CommandRunner::run(self, program, args)
    }
}

impl bootty_host::CommandRunner for &dyn Runner {
    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<bootty_host::CommandOutput> {
        Runner::run(*self, program, args)
    }
}

/// A name for a snapshot taken now, unique within its conversation.
pub fn snapshot_name(moment: &str) -> String {
    format!("{}-{moment}", chrono::Utc::now().timestamp_micros())
}

impl NativeSessionSnapshot {
    /// Keeps the files after the turn that just ended on its last reply.
    pub(crate) fn settle_checkpoint(&mut self, commit: String) {
        if let Some(reply) = self
            .transcript
            .iter_mut()
            .rev()
            .take_while(|item| item.role != "user")
            .find(|item| item.role == "assistant")
        {
            reply.checkpoint = Some(commit);
            self.changed();
        }
    }
}
