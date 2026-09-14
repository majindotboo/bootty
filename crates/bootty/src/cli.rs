use std::{
    fs,
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use bootty_config::{
    ApplicationIdentity,
    config::{BoottyConfig, load_config_from_path},
};
use clap::{Args, Parser, Subcommand, ValueEnum};

mod config_overrides;

use config_overrides::ConfigOverrides;

#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Launch the Bootty terminal application.
    App(Box<AppArgs>),
    /// Report how to install the complete latest Bootty release package.
    Update,
    /// List commands exposed by a running Bootty instance.
    Commands,
    /// Read live owner, protocol and backend capability diagnostics.
    Doctor,
    /// Run a batch job on the selected binding host with streamed output and an observed exit.
    Run(RunArgs),
    /// Wait for a read-only command snapshot to match a JSON condition.
    Wait(WaitArgs),
    /// Describe one command exposed by a running Bootty instance.
    Describe { name: String },
    /// Invoke a command through the owner-local control plane.
    #[command(name = "command")]
    Invoke {
        name: String,
        #[arg(num_args = 0.., allow_hyphen_values = true)]
        arguments: Vec<String>,
        /// Prepend stdin verbatim as the first command argument.
        #[arg(long, conflicts_with = "stdin_json")]
        stdin: bool,
        /// Read the command's string or number arguments as a JSON array from stdin.
        #[arg(long, conflicts_with = "arguments")]
        stdin_json: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long = "detach")]
        detached: bool,
    },
    /// Inspect or cancel one detached command task.
    #[command(name = "task", subcommand)]
    Task(TaskCommand),
    /// Create, poll, or remove one bounded event subscription.
    #[command(name = "events", subcommand)]
    Events(EventCommand),
    /// Legacy remote Space protocol retained while daemon installations roll out.
    #[command(name = "remote-space", hide = true, subcommand)]
    RemoteSpace(RemoteSpaceCommand),
    /// Legacy remote command transport retained while daemon installations roll out.
    #[command(name = "remote-exec", hide = true)]
    RemoteExec { payload: String },
    /// Legacy remote availability probe retained while daemon installations roll out.
    #[command(name = "remote-ping", hide = true)]
    RemotePing,
    /// Legacy remote terminal protocol retained while daemon installations roll out.
    #[command(name = "remote-rmux", hide = true)]
    RemoteRmux { payload: String },
    /// Invoke a command discovered from a running Bootty instance.
    #[command(external_subcommand)]
    Dynamic(Vec<String>),
}

#[derive(Clone, Debug, Args)]
pub struct RunArgs {
    /// Working directory on the target host.
    #[arg(long)]
    pub cwd: String,
    #[arg(long, default_value_t = 3600, value_parser=clap::value_parser!(u32).range(1..=86400))]
    pub timeout: u32,
    /// Return the job ID without waiting; closing its Bootty window still stops the job.
    #[arg(long)]
    pub detach: bool,
    /// Retain the completed job and output for later inspection.
    #[arg(long)]
    pub keep: bool,
    /// Binding target as JSON. Defaults to the selected binding at launch.
    #[arg(long)]
    pub target: Option<String>,
    #[arg(last = true, required = true)]
    pub command: Vec<String>,
}

#[derive(Clone, Debug, Args)]
pub struct WaitArgs {
    pub command: String,
    #[arg(long = "topic", required = true)]
    pub topics: Vec<String>,
    #[arg(long, default_value = "")]
    pub pointer: String,
    #[arg(long)]
    pub equals: String,
    #[arg(long, default_value_t = 60)]
    pub timeout: u64,
    /// Captured command target as JSON. Otherwise the current target is captured once.
    #[arg(long)]
    pub target: Option<String>,
    #[arg(last = true)]
    pub arguments: Vec<String>,
}

#[derive(Clone, Debug, Default, Args)]
pub struct AppArgs {
    /// Load config from this TOML file instead of the default XDG path.
    #[arg(long, value_name = "PATH", conflicts_with = "defaults")]
    config: Option<PathBuf>,

    /// Ignore user config and start from built-in defaults with isolated temp sidecar state.
    #[arg(long, conflicts_with = "config")]
    defaults: bool,

    /// Stable persistence identity for this application window.
    #[arg(long, default_value = "main", hide = true)]
    window_state_key: String,

    #[command(flatten)]
    overrides: ConfigOverrides,
}

#[derive(Clone, Debug, PartialEq, Eq, Subcommand)]
pub enum TaskCommand {
    Status { task: String },
    Cancel { task: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Subcommand)]
pub enum EventCommand {
    Wait {
        subscription: String,
        #[arg(long, default_value_t = 0)]
        cursor: u64,
        #[arg(long, default_value_t = 4000, value_parser = clap::value_parser!(u64).range(0..=4000))]
        timeout_ms: u64,
    },
    Subscribe {
        #[arg(required = true, num_args = 1..)]
        topics: Vec<String>,
    },
    Poll {
        subscription: String,
        #[arg(long, default_value_t = 0)]
        cursor: u64,
    },
    Unsubscribe {
        subscription: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Subcommand)]
pub enum RemoteSpaceCommand {
    List,
    Create {
        #[arg(long)]
        name: String,
        #[arg(long, value_enum)]
        backend: RemoteSpaceBackend,
    },
    Snapshot {
        #[arg(long)]
        id: String,
        #[arg(long, value_enum)]
        backend: RemoteSpaceBackend,
    },
    Execute {
        #[arg(long)]
        id: String,
        #[arg(long, value_enum)]
        backend: RemoteSpaceBackend,
        #[arg(long)]
        payload: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RemoteSpaceBackend {
    Rmux,
    Tmux,
}

impl From<RemoteSpaceBackend> for bootty_mux::MuxBackendKind {
    fn from(value: RemoteSpaceBackend) -> Self {
        match value {
            RemoteSpaceBackend::Rmux => Self::Rmux,
            RemoteSpaceBackend::Tmux => Self::Tmux,
        }
    }
}
#[derive(Debug, Parser)]
#[command(name = "bootty", version, about = "Bootty terminal emulator")]
pub struct Cli {
    /// Print the exact JSON-RPC response.
    #[arg(long, global = true)]
    json: bool,

    /// Start a Bootty instance when none is running.
    #[arg(long, global = true)]
    start: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

impl Cli {
    /// Load the selected configuration and apply command-line overrides.
    ///
    /// # Errors
    /// Returns an error if the configuration cannot be read, created, or validated.
    pub fn load_config(&self) -> Result<BoottyConfig> {
        self.app_args().load_config()
    }

    #[must_use]
    pub fn window_state_key(&self) -> &str {
        match self.command.as_ref() {
            Some(Command::App(args)) => args.window_state_key(),
            _ => "main",
        }
    }

    #[must_use]
    pub const fn subcommand(&self) -> Option<&Command> {
        self.command.as_ref()
    }

    #[must_use]
    pub const fn json(&self) -> bool {
        self.json
    }

    #[must_use]
    pub const fn start(&self) -> bool {
        self.start
    }

    fn app_args(&self) -> AppArgs {
        match self.command.as_ref() {
            Some(Command::App(args)) => (**args).clone(),
            _ => AppArgs::default(),
        }
    }
}

impl AppArgs {
    /// Load the selected configuration and apply command-line overrides.
    ///
    /// # Errors
    /// Returns an error if the configuration cannot be read, created, or validated.
    pub fn load_config(&self) -> Result<BoottyConfig> {
        let path = self.selected_config_path();
        if self.defaults {
            create_parent_dir_for_defaults(&path)?;
        }
        let mut config = load_config_from_path(&path)?;
        self.overrides.apply(&mut config)?;
        Ok(config)
    }

    #[must_use]
    pub fn window_state_key(&self) -> &str {
        &self.window_state_key
    }

    fn selected_config_path(&self) -> PathBuf {
        if self.defaults {
            return isolated_defaults_config_path();
        }
        self.config
            .clone()
            .unwrap_or_else(|| ApplicationIdentity::current().default_config_path())
    }
}

fn isolated_defaults_config_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir()
        .join(format!(
            "{}-defaults-{}-{nanos}",
            ApplicationIdentity::current().cli_name(),
            process::id()
        ))
        .join("config.toml")
}

fn create_parent_dir_for_defaults(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create isolated defaults directory {}",
                parent.display()
            )
        })?;
    }
    Ok(())
}
