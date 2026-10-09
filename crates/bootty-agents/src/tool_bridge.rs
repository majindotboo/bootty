//! Per-launch MCP attachment. User configuration and retained provider metadata stay untouched.

use bootty_control::Caller;
use std::{fs, path::Path, sync::Arc};

use crate::{
    AgentCommandExecutor, AgentKind,
    tool_policy::{
        ToolCapturedCommand, ToolChildAuthority, ToolLease, ToolPolicy, ToolScope, ToolSpawnContext,
    },
};

mod pi_extension;
mod protocol;
mod transport;

pub use protocol::{MAX_TOOL_IMAGE_RESPONSE_BYTES, MAX_TOOL_MESSAGE_BYTES, ToolProtocol};
pub use transport::tool_stdio;

pub const PI_CHECKPOINT_COMMAND: &str = "__bootty_checkpoint";

// Pi defers an empty session's file until its first message. Use its public session entries
// and reattach the same file before publishing the identity. Remove when Pi adds checkpoint RPC.
const PI_CHECKPOINT_EXTENSION: &str = r"
  if (process.argv.includes('rpc')) pi.registerCommand('__bootty_checkpoint', { handler: async (_args, ctx) => {
    const { existsSync, openSync, writeFileSync, fsyncSync, closeSync, linkSync, unlinkSync } = await import('node:fs');
    const { randomUUID } = await import('node:crypto');
    const { dirname } = await import('node:path');
    const file = ctx.sessionManager.getSessionFile();
    const header = ctx.sessionManager.getHeader();
    if (!file || !header) throw new Error('Pi did not provide a persistent session');
    if (!existsSync(file)) {
      const pending = file + '.' + randomUUID() + '.bootty';
      const fd = openSync(pending, 'wx', 0o600);
      try {
        writeFileSync(fd, [header, ...ctx.sessionManager.getEntries()].map(entry => JSON.stringify(entry)).join('\n') + '\n');
        fsyncSync(fd);
        linkSync(pending, file);
      } finally { closeSync(fd); unlinkSync(pending); }
    }
    for (const path of [file, dirname(file)]) {
      const fd = openSync(path, 'r');
      try { fsyncSync(fd); } finally { closeSync(fd); }
    }
  }});
";

/// Pi sanitizes MCP names and hashes identifiers over 64 characters. Resolve only our
/// exact server/tool pair; a readable label never substitutes for tool authority.
pub fn logical_pi_tool_name(server: &str, identifier: &str) -> Option<&'static str> {
    let nonce = server.strip_prefix("bootty_")?;
    if nonce.len() != 64 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    [
        "terminal_read",
        "get_workspace_info",
        "list_terminals",
        "browser_snapshot",
        "computer_snapshot",
        "computer_input",
        "spawn_shell",
        "read_spawned_terminal",
        "paste_spawned_terminal",
        "submit_spawned_terminal",
        "interrupt_spawned_terminal",
        "close_spawned_terminal",
        "spawn_agent",
        "interrupt_spawned_agent",
        "stop_spawned_agent",
        "get_agent_status",
        "get_agent_activity",
        "list_models",
        "list_agents",
        "list_providers",
        "list_profiles",
        "inspect_provider",
    ]
    .into_iter()
    .find(|tool| pi_extension::tool_name(server, tool) == identifier)
}

/// All values are captured by the launch owner before provider work starts.
pub struct ToolBridgeContext {
    pub scope: ToolScope,
    pub caller: Caller,
    pub policy: ToolPolicy,
    pub captures: Vec<ToolCapturedCommand>,
    pub spawn: Option<ToolSpawnContext>,
}

/// Its owner binds the returned lease after terminal registration and revokes on close/disable.
pub struct ToolBridge {
    transport: transport::PrivateTransport,
    lease: ToolLease,
    arguments: Vec<String>,
}

impl ToolBridge {
    /// Worker-only preparation. The executable must be this installed Bootty identity's binary.
    /// # Errors
    /// Returns invalid authority, unsupported platform or private runtime-file errors.
    pub fn prepare(
        context: ToolBridgeContext,
        executable: &Path,
        commands: Arc<dyn AgentCommandExecutor>,
    ) -> Result<Self, String> {
        let program = executable
            .to_str()
            .filter(|program| !program.chars().any(char::is_control))
            .filter(|_| executable.is_absolute())
            .ok_or("Tool bridge executable must be an absolute host path")?;
        let provider = context.scope.provider;
        let lease = ToolLease::issue_with_spawn(
            context.scope,
            context.caller,
            context.policy,
            context.captures,
            context.spawn,
        )?;
        Self::prepare_lease(lease, program, provider, commands)
    }

    /// # Errors
    /// Rejects stale ancestor authority, invalid executable paths or private runtime-file errors.
    pub fn prepare_child(
        authority: ToolChildAuthority,
        executable: &Path,
        commands: Arc<dyn AgentCommandExecutor>,
    ) -> Result<Self, String> {
        let program = executable
            .to_str()
            .filter(|program| !program.chars().any(char::is_control))
            .filter(|_| executable.is_absolute())
            .ok_or("Tool bridge executable must be an absolute host path")?;
        let lease = authority.into_lease()?;
        let provider = lease.scope().provider;
        Self::prepare_lease(lease, program, provider, commands)
    }

    fn prepare_lease(
        lease: ToolLease,
        program: &str,
        provider: AgentKind,
        commands: Arc<dyn AgentCommandExecutor>,
    ) -> Result<Self, String> {
        let transport = transport::PrivateTransport::prepare(lease.clone(), commands)
            .map_err(|error| error.to_string())?;
        let arguments = launch_arguments(provider, program, &transport)?;
        Ok(Self {
            transport,
            lease,
            arguments,
        })
    }

    #[must_use]
    pub fn arguments(&self) -> Vec<String> {
        self.arguments.clone()
    }

    pub(crate) fn server_name(&self) -> &str {
        &self.transport.name
    }

    #[must_use]
    pub const fn lease(&self) -> &ToolLease {
        &self.lease
    }

    /// Prepare one remote endpoint for this exact local lease; no authority moves to the host.
    pub(crate) fn remote_arguments(
        &self,
        remote: &bootty_host::remote::RemoteHost,
        daemon: &str,
        permission_extension: Option<&Path>,
    ) -> Result<
        (
            Vec<String>,
            bootty_host::private_stdio::relay::RemoteStdioRelay,
            Option<std::path::PathBuf>,
        ),
        String,
    > {
        use std::fmt::Write as _;
        let mut random = [0_u8; 32];
        getrandom::fill(&mut random).map_err(|_| "Remote tool nonce is unavailable")?;
        let mut name = String::from("bt-tool-");
        for byte in random {
            write!(&mut name, "{byte:02x}").map_err(|error| error.to_string())?;
        }
        let directory = Path::new("/tmp").join(&name);
        let identity = match bootty_config::ApplicationIdentity::for_process() {
            bootty_config::ApplicationIdentity::Production => "bootty",
            bootty_config::ApplicationIdentity::Development => "bootty-dev",
        };
        let argv = vec![
            "--application-identity".into(),
            identity.into(),
            "--agent-tool-stdio".into(),
            directory
                .join("connection.json")
                .to_string_lossy()
                .into_owned(),
        ];
        let (arguments, mut files) = launch_spec(
            self.lease.scope().provider,
            daemon,
            &self.transport.name,
            &directory,
            &argv,
        )?;
        files.push(bootty_host::private_stdio::relay::PrivateFile {
            name: "connection.json".into(),
            bytes: self.transport.remote_connection(&directory)?,
        });
        let permission_path = if let Some(path) = permission_extension {
            files.push(bootty_host::private_stdio::relay::PrivateFile {
                name: "permissions.ts".into(),
                bytes: std::fs::read(path).map_err(|error| error.to_string())?,
            });
            Some(directory.join("permissions.ts"))
        } else {
            None
        };
        let relay = bootty_host::private_stdio::relay::RemoteStdioRelay::start(
            remote,
            daemon,
            &name,
            files,
            self.transport.directory.join("tools.sock"),
        )
        .map_err(|error| error.to_string())?;
        Ok((arguments, relay, permission_path))
    }

    pub fn stop(&self) {
        self.transport.stop();
    }
}

fn launch_arguments(
    provider: AgentKind,
    program: &str,
    transport: &transport::PrivateTransport,
) -> Result<Vec<String>, String> {
    let connection = transport
        .connection
        .to_str()
        .ok_or("Tool connection path must be UTF-8")?;
    let (arguments, files) = launch_spec(
        provider,
        program,
        &transport.name,
        &transport.directory,
        &["--agent-tool-stdio".into(), connection.into()],
    )?;
    for file in files {
        private_file(&transport.directory.join(file.name), &file.bytes)?;
    }
    Ok(arguments)
}

fn launch_spec(
    provider: AgentKind,
    program: &str,
    name: &str,
    directory: &Path,
    argv: &[String],
) -> Result<
    (
        Vec<String>,
        Vec<bootty_host::private_stdio::relay::PrivateFile>,
    ),
    String,
> {
    let config = serde_json::json!({"type":"stdio", "command":program, "args":argv});
    match provider {
        AgentKind::Codex => Ok((
            vec![
                "--config".to_owned(),
                format!(
                    "mcp_servers.{}.command={}",
                    name,
                    serde_json::to_string(program).map_err(|error| error.to_string())?
                ),
                "--config".to_owned(),
                format!(
                    "mcp_servers.{}.args={}",
                    name,
                    serde_json::to_string(argv).map_err(|error| error.to_string())?
                ),
                "--config".to_owned(),
                format!("mcp_servers.{}.required=true", name),
            ],
            Vec::new(),
        )),
        AgentKind::Claude => {
            let path = directory.join("mcp.json");
            let config = serde_json::json!({"mcpServers":{name: config}});
            let files = vec![bootty_host::private_stdio::relay::PrivateFile {
                name: "mcp.json".into(),
                bytes: serde_json::to_vec(&config).map_err(|error| error.to_string())?,
            }];
            Ok((
                vec![
                    "--mcp-config".to_owned(),
                    path.to_str()
                        .ok_or("MCP config path must be UTF-8")?
                        .to_owned(),
                ],
                files,
            ))
        }
        AgentKind::Pi => {
            let path = directory.join("tools.ts");
            let name = serde_json::to_string(&name).map_err(|error| error.to_string())?;
            let config = serde_json::json!({"type":"stdio", "command":program, "args":argv, "exposure":"direct"});
            let config = serde_json::to_string(&config).map_err(|error| error.to_string())?;
            let tools = pi_extension::TOOLS;
            let extension = format!(
                "import type {{ ExtensionAPI, ToolDefinition }} from '@earendil-works/pi-coding-agent';\nexport default async function attach(pi: ExtensionAPI) {{\n  const server = {name}; const config = {config};\n{tools}\n{PI_CHECKPOINT_EXTENSION}}}\n"
            );
            let files = vec![bootty_host::private_stdio::relay::PrivateFile {
                name: "tools.ts".into(),
                bytes: extension.into_bytes(),
            }];
            Ok((
                vec![
                    "--extension".to_owned(),
                    path.to_str()
                        .ok_or("Pi tool extension path must be UTF-8")?
                        .to_owned(),
                ],
                files,
            ))
        }
    }
}

fn private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())
}
