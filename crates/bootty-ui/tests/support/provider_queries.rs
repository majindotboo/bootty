use serde_json::{Value, json};
#[path = "observer_provider.rs"]
mod observer_provider;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub fn terminal_transport(
    ready: Option<std::sync::Arc<std::os::unix::net::UnixListener>>,
) -> bootty_agents::TerminalProviderTransport {
    let observers = std::sync::Mutex::new(std::collections::HashSet::new());
    bootty_agents::TerminalProviderTransport {
        output: std::sync::Arc::new(move |command, cancelled| {
            if cancelled() {
                return Err("provider query cancelled".into());
            }
            let program = std::path::Path::new(command.get_program());
            let name = program.file_name().and_then(|name| name.to_str());
            let arguments: Vec<_> = command.get_args().collect();
            if !(arguments == ["agents", "--json"] || arguments == ["agents", "--json", "--all"])
                || !matches!(
                    name,
                    Some("claude-fixture.py" | "interrupt-provider.py" | "provider.py")
                )
            {
                return Err(format!(
                    "No provider query fixture for {:?}",
                    command.get_program()
                ));
            }
            let first = arguments.len() == 3
                && observers
                    .lock()
                    .map_err(|error| error.to_string())?
                    .insert(std::thread::current().id());
            if first {
                ready
                    .as_ref()
                    .ok_or("No provider readiness fixture")?
                    .accept()
                    .map_err(|error| error.to_string())?;
            }
            let result = query_fixture(&command).map_err(|error| error.to_string());
            if cancelled() {
                Err("provider query cancelled".into())
            } else {
                result
            }
        }),
        launch: std::sync::Arc::new(|command| {
            Err(format!(
                "No stdio query fixture for {:?}",
                command.get_program()
            ))
        }),
        observer_launch: std::sync::Arc::new(|command| observer_provider::launch(&command)),
    }
}

fn query_fixture(command: &std::process::Command) -> Result<bootty_agents::TerminalQueryOutput> {
    let program = std::path::Path::new(command.get_program());
    let directory = program.parent().ok_or("missing fixture directory")?;
    let data = if program
        .file_name()
        .is_some_and(|name| name == "provider.py")
    {
        let account = command
            .get_envs()
            .find_map(|(key, value)| (key == "CLAUDE_CONFIG_DIR").then_some(value).flatten())
            .ok_or("missing captured Claude account")?;
        let account = std::path::Path::new(account);
        json!([{"sessionId":std::fs::read_to_string(account.join("identity"))?,"state":std::fs::read_to_string(account.join("state"))?}])
    } else if program
        .file_name()
        .is_some_and(|name| name == "interrupt-provider.py")
    {
        json!([{"sessionId":std::fs::read_to_string(directory.join("raw-session"))?,"status":"busy"}])
    } else {
        let sessions = std::fs::read_dir(directory)?
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.starts_with("session-")
                        && entry
                            .path()
                            .extension()
                            .is_some_and(|extension| extension == "json")
                })
            })
            .map(|entry| {
                let session: Value = serde_json::from_slice(&std::fs::read(entry.path())?)?;
                Ok(json!({"sessionId":session["session_id"],"status":"idle"}))
            })
            .collect::<Result<Vec<_>>>()?;
        json!(sessions)
    };
    Ok(bootty_agents::TerminalQueryOutput {
        stdout: serde_json::to_vec(&data)?,
        stderr: Vec::new(),
        exit_code: Some(0),
        successful: true,
    })
}
