use std::{
    io::{BufReader, Read},
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use serde_json::{Value, json};

use crate::{
    AgentKind, AgentLaunch, NativeAgentSession, native_protocol::field, native_session::lock,
};

impl NativeAgentSession {
    /// Read provider account readiness without opening or copying credential stores.
    /// # Errors
    /// Returns provider authentication/protocol errors or unsupported operations.
    pub fn account_status(&self) -> Result<Value, String> {
        let status = match self.config().provider {
            AgentKind::Codex => self.rpc("account/read", json!({"refreshToken":false}))?,
            AgentKind::Claude => {
                bounded_json_command(&self.config().program, &["auth", "status", "--json"])?
            }
            AgentKind::Pi => {
                if let Ok(status) = self.rpc("get_login_providers", json!({})) {
                    status
                } else {
                    let models = self.rpc("get_available_models", json!({}))?;
                    let mut providers = std::collections::BTreeSet::new();
                    for model in field(&models, "models").as_array().into_iter().flatten() {
                        if let Some(provider) = field(model, "provider").as_str() {
                            providers.insert(provider.to_owned());
                        }
                    }
                    json!({"status":"credentials_not_verified","providers":providers.into_iter().map(|provider| json!({"id":provider,"name":provider,"authenticated":null})).collect::<Vec<_>>()})
                }
            }
        };
        Ok(status)
    }

    /// Begin native authentication; browser URLs or extension UI requests stay provider-owned.
    /// # Errors
    /// Returns unsupported/provider errors. Claude uses its interactive CLI login launch.
    pub fn account_login(&self, provider_id: Option<&str>) -> Result<Value, String> {
        match self.config().provider {
            AgentKind::Codex => {
                self.rpc("account/login/start", json!({"type":"chatgptDeviceCode"}))
            }
            AgentKind::Pi => self.rpc(
                "login",
                json!({"providerId":provider_id.ok_or("Select a Pi login provider")?}),
            ),
            AgentKind::Claude => {
                Err("Claude authentication requires its interactive login command".to_owned())
            }
        }
    }

    /// # Errors
    /// Returns provider/logout errors. Pi has no native RPC logout operation.
    pub fn account_logout(&self) -> Result<Value, String> {
        match self.config().provider {
            AgentKind::Codex => self.rpc("account/logout", json!({})),
            AgentKind::Claude => Err(
                "Stop Claude sessions before signing out through the account login terminal"
                    .to_owned(),
            ),
            AgentKind::Pi => {
                Err("Pi does not expose RPC logout; use /logout in a Pi terminal".to_owned())
            }
        }
    }

    /// # Errors
    /// Returns provider errors. Codex's cursor supports bounded native thread discovery.
    pub fn list_history(&self, cursor: Option<&str>) -> Result<Value, String> {
        match self.config().provider {
            AgentKind::Codex => self.rpc(
                "thread/list",
                json!({"cwd":self.config().cwd,"cursor":cursor,"limit":50}),
            ),
            AgentKind::Pi | AgentKind::Claude => self.history(),
        }
    }
}

/// Interactive account commands use the ordinary terminal invocation path; no shell, private
/// credential parsing or token duplication is needed for providers without a native login RPC.
#[must_use]
pub fn agent_account_launch(provider: AgentKind, program: &str, logout: bool) -> AgentLaunch {
    let arguments = match provider {
        AgentKind::Codex => {
            if logout {
                vec!["logout"]
            } else {
                vec!["login", "--device-auth"]
            }
        }
        AgentKind::Claude => vec!["auth", if logout { "logout" } else { "login" }],
        // Initial messages bypass interactive slash commands in this provider.
        AgentKind::Pi => Vec::new(),
    };
    AgentLaunch {
        program: program.to_owned(),
        cwd: None,
        arguments: arguments.into_iter().map(str::to_owned).collect(),
        ephemeral: true,
    }
}

fn bounded_json_command(program: &str, arguments: &[&str]) -> Result<Value, String> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Account stdout is not available")?;
    let child = Arc::new(Mutex::new(child));
    let waiter = Arc::clone(&child);
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = BufReader::new(stdout)
            .take(64 * 1024 + 1)
            .read_to_end(&mut bytes);
        if bytes.len() > 64 * 1024 {
            let _ = lock(&waiter).kill();
        }
        let result = read.map_err(|error| error.to_string()).and_then(|_| {
            if bytes.len() > 64 * 1024 {
                return Err("Account response exceeds 64 KiB".to_owned());
            }
            serde_json::from_slice::<Value>(&bytes).map_err(|error| error.to_string())
        });
        let _ = sender.send(result);
    });
    let result = receiver
        .recv_timeout(Duration::from_secs(15))
        .map_err(|error| error.to_string());
    let mut child = lock(&child);
    let _ = child.kill();
    let _ = child.wait();
    drop(child);
    result?
}
