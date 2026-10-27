#![cfg(test)]

use bootty_agents::{NativeLaunch, NativeLauncher};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use std::{
    io::{BufReader, Cursor},
    path::Path,
    sync::Arc,
};

pub(crate) fn launcher() -> NativeLauncher {
    Arc::new(|launch: &NativeLaunch<'_>| {
        let command = launch.command()?;
        if Path::new(&launch.config.program)
            .file_name()
            .is_none_or(|name| name != "provider.py")
            || !Path::new(&launch.config.program).is_file()
        {
            return Err("The in-process tools provider is unavailable".into());
        }
        let arguments = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(&arguments[..3], ["app-server", "--listen", "stdio://"]);
        let mut config = serde_json::Map::new();
        let (pairs, remainder) = arguments[3..].as_chunks::<2>();
        let empty: &[&str] = &[];
        assert_eq!(remainder, empty);
        for pair in pairs {
            assert_eq!(pair[0], "--config");
            let (key, value) = pair[1].split_once('=').unwrap();
            config.insert(key.into(), serde_json::from_str::<Value>(value).unwrap());
        }
        let key = config
            .keys()
            .find(|key| {
                key.starts_with("mcp_servers.")
                    && key
                        .rsplit_once('.')
                        .is_some_and(|(_, field)| field == "args")
            })
            .unwrap();
        let name = key.strip_suffix(".args").unwrap();
        assert_eq!(config[&format!("{name}.required")], true);
        assert_ne!(config[&format!("{name}.command")].as_str().unwrap(), "");
        let args = config[key].as_array().unwrap();
        assert_eq!(args[0], "--agent-tool-stdio");
        let connection = std::path::PathBuf::from(args[1].as_str().unwrap());
        let server = name.strip_prefix("mcp_servers.").unwrap().to_owned();
        let tools = move |method: &str, params: Value| {
            let mut input = serde_json::to_vec(
                &json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}),
            )
            .unwrap();
            input.push(b'\n');
            let mut output = Vec::new();
            bootty_host::private_stdio::tool_stdio(
                &connection,
                &mut BufReader::new(Cursor::new(input)),
                &mut output,
            )
            .unwrap();
            serde_json::from_slice::<Value>(&output).unwrap()["result"].clone()
        };
        let mut initial_catalog = Value::Null;
        Ok(super::native_peer::peer(move |value| {
            let mut events = Vec::new();
            let result = match value["method"].as_str() {
                Some("initialized") => return events,
                Some("thread/start" | "thread/resume") => {
                    initial_catalog = tools("tools/list", json!({}));
                    json!({"thread":{"id":"tools-thread","turns":[]}})
                }
                Some("__tools") => json!({
                    "catalog":initial_catalog,
                    "read":tools("tools/call", json!({"name":"terminal_read","arguments":{"scope":"history","max_lines":7}})),
                    "status":tools("tools/call", json!({"name":"get_agent_status"})),
                    "models":tools("tools/call", json!({"name":"list_models"})),
                    "computer":tools("tools/call", json!({"name":"computer_snapshot","arguments":{"application":"unmentioned"}})),
                }),
                Some("__approval") => {
                    let name = value["params"]["server"].as_str().unwrap();
                    let name = if name == "attached" {
                        server.as_str()
                    } else {
                        name
                    };
                    events.push(json!({"id":"approval","method":"mcpServer/elicitation/request","params":{"threadId":"tools-thread","turnId":null,"serverName":name,"mode":"form","message":"Allow this tool?","requestedSchema":{"type":"object","properties":{}}}}));
                    json!({})
                }
                _ => json!({}),
            };
            events.push(json!({"id":value["id"],"result":result}));
            events
        }))
    })
}
