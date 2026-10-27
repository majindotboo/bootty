use std::{
    io::{BufRead, BufReader, Read, Write},
    process::Stdio,
    sync::mpsc,
    thread,
    time::Duration,
};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::TerminalAccountStatus;

// This query needs only initialization and account metadata, not full session messages.
const MAX_MESSAGE: u64 = 64 * 1024;
const MAX_OUTPUT: usize = 1024 * 1024;

#[derive(Deserialize)]
struct Response {
    id: Option<u64>,
    result: Option<Value>,
    error: Option<serde::de::IgnoredAny>,
}

fn response(reader: &mut impl BufRead, id: u64) -> Result<Value, String> {
    let mut total = 0_usize;
    loop {
        let mut bytes = Vec::new();
        let count = Read::take(&mut *reader, MAX_MESSAGE + 1)
            .read_until(b'\n', &mut bytes)
            .map_err(|_| "Codex account transport failed")?;
        total = total.saturating_add(count);
        if count == 0 {
            return Err("Codex exited before reporting its account".to_owned());
        }
        if u64::try_from(count).unwrap_or(u64::MAX) > MAX_MESSAGE || total > MAX_OUTPUT {
            return Err("Codex account response exceeds the query limit".to_owned());
        }
        let response: Response =
            serde_json::from_slice(&bytes).map_err(|_| "Codex account response is malformed")?;
        if response.id != Some(id) {
            continue;
        }
        if response.error.is_some() {
            return Err("Codex account request failed".to_owned());
        }
        return response
            .result
            .ok_or_else(|| "Codex account response has no result".to_owned());
    }
}

fn send(writer: &mut impl Write, message: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *writer, message)
        .map_err(|_| "Codex account request could not be written")?;
    writer
        .write_all(b"\n")
        .and_then(|()| writer.flush())
        .map_err(|_| "Codex account transport failed".to_owned())
}

pub fn query(
    program: &str,
    directory: Option<&str>,
    transport: &crate::TerminalProviderTransport,
) -> Result<TerminalAccountStatus, String> {
    let _provider_use = crate::ProviderUse::acquire(program)?;
    let mut command = bootty_host::local_command(program);
    command
        .args(["app-server", "--listen", "stdio://"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(directory) = directory {
        command.env("CODEX_HOME", directory);
    }
    let crate::NativeChild {
        stdout,
        stdin: mut writer,
        control: mut owner,
        ..
    } = (transport.launch)(command)?;
    let result = {
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let result = (|| {
                let mut reader = BufReader::new(stdout);
                send(
                    &mut writer,
                    &json!({
                        "id": 1,
                        "method": "initialize",
                        "params": {
                            "clientInfo": {
                                "name": "bootty_account_check",
                                "version": env!("CARGO_PKG_VERSION")
                            },
                            "capabilities": {"experimentalApi": false}
                        }
                    }),
                )?;
                response(&mut reader, 1)?;
                send(&mut writer, &json!({"method": "initialized"}))?;
                send(
                    &mut writer,
                    &json!({"id": 2, "method": "account/read", "params": {"refreshToken": false}}),
                )?;
                crate::terminal_account_response::codex(&response(&mut reader, 2)?)
            })();
            let _ = sender.send(result);
        });
        receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "Codex account query timed out".to_owned())
            .and_then(std::convert::identity)
    };
    let _ = owner.terminate();
    let _ = owner.wait();
    result
}
