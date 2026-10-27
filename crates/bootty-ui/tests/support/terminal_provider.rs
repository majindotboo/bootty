use serde_json::{Value, json};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub fn transport(
    finished: bool,
    ready: Option<std::sync::Arc<std::os::unix::net::UnixListener>>,
) -> bootty_agents::TerminalProviderTransport {
    let observers = std::sync::Mutex::new(std::collections::HashSet::new());
    let defaults = super::provider_queries::terminal_transport(None);
    bootty_agents::TerminalProviderTransport {
        output: std::sync::Arc::new(move |command, cancelled| {
            if cancelled() {
                return Err("provider query cancelled".into());
            }
            let program = std::path::Path::new(command.get_program());
            let arguments: Vec<_> = command.get_args().collect();
            if !(arguments == ["agents", "--json"] || arguments == ["agents", "--json", "--all"])
                || program
                    .file_name()
                    .is_none_or(|name| name != "claude-fixture.py")
            {
                return (defaults.output)(command, cancelled);
            }
            if arguments.len() == 3
                && observers
                    .lock()
                    .map_err(|error| error.to_string())?
                    .insert(std::thread::current().id())
                && let Some(ready) = &ready
            {
                ready.accept().map_err(|error| error.to_string())?;
            }
            let result =
                command_query_fixture(program, finished).map_err(|error| error.to_string());
            if cancelled() {
                Err("provider query cancelled".into())
            } else {
                result
            }
        }),
        launch: defaults.launch,
        observer_launch: defaults.observer_launch,
    }
}

fn command_query_fixture(
    program: &std::path::Path,
    finished: bool,
) -> Result<bootty_agents::TerminalQueryOutput> {
    let directory = program
        .parent()
        .ok_or("missing command fixture directory")?;
    let selected = directory.join("query-session");
    let source = if selected.exists() {
        selected
    } else {
        directory.join("agent-output")
    };
    let rows = if source.exists() {
        let observed: Value = serde_json::from_slice(&std::fs::read(source)?)?;
        let mut row = json!({"sessionId":observed["session_id"],"waitingFor":observed["detail"]});
        if finished {
            row["state"] = json!("done");
        } else {
            row["status"] = json!("busy");
        }
        if let Some(status) = observed.get("status") {
            row["status"] = status.clone();
        }
        vec![row]
    } else {
        Vec::new()
    };
    Ok(bootty_agents::TerminalQueryOutput {
        stdout: serde_json::to_vec(&rows)?,
        stderr: Vec::new(),
        exit_code: Some(0),
        successful: true,
    })
}
