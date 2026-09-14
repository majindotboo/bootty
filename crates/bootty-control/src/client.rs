use std::{
    io::{BufRead, BufReader, Read, Write},
    process::Command as ProcessCommand,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use rmux_ipc::{LocalEndpoint, connect_blocking};
use serde_json::{Value, json};

use crate::{
    lease::{ControlInstanceLease, InstanceDescriptor},
    protocol::{
        COMMAND_TIMEOUT, IO_TIMEOUT, PROTOCOL_VERSION, REQUEST_LIMIT, RpcRequest, RpcResponse,
    },
};

/// # Errors
/// Returns discovery, startup, transport, or response decoding errors.
pub fn invoke_or_start(start: bool, method: &str, params: Value) -> Result<RpcResponse> {
    let descriptor = select_or_start(start)?;
    invoke_instance(&descriptor, method, params)
}

/// # Errors
/// Returns an error when the private instance directory or lease cannot be read.
pub fn running_instance() -> Result<Option<InstanceDescriptor>> {
    discover_instance()
}

/// # Errors
/// Returns discovery or startup errors, including an absent instance when startup is disabled.
pub fn select_or_start(start: bool) -> Result<InstanceDescriptor> {
    if !start {
        return select_instance();
    }
    discover_instance()?.map_or_else(start_instance, Ok)
}

fn start_instance() -> Result<InstanceDescriptor> {
    let executable = std::env::current_exe().context("find Bootty executable")?;
    let mut child = ProcessCommand::new(executable)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("start Bootty instance")?;
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .context("inspect started Bootty instance")?
        {
            anyhow::bail!("started Bootty instance exited with {status}");
        }
        if let Some(instance) = discover_instance()?
            && invoke_instance(&instance, "instance.describe", Value::Null).is_ok()
        {
            return Ok(instance);
        }
        if started.elapsed() >= COMMAND_TIMEOUT {
            anyhow::bail!("started Bootty instance did not become ready");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// # Errors
/// Returns protocol, transport, payload limit, or response decoding errors.
pub fn invoke_instance(
    descriptor: &InstanceDescriptor,
    method: &str,
    params: Value,
) -> Result<RpcResponse> {
    invoke_instance_timeout(descriptor, method, params, IO_TIMEOUT)
}

/// # Errors
/// Returns an error for an expired timeout, incompatible protocol, failed I/O, or invalid response.
pub fn invoke_instance_timeout(
    descriptor: &InstanceDescriptor,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<RpcResponse> {
    anyhow::ensure!(!timeout.is_zero(), "control request deadline expired");
    if descriptor.protocol_version != PROTOCOL_VERSION {
        anyhow::bail!(
            "unsupported Bootty protocol version {}; expected {}",
            descriptor.protocol_version,
            PROTOCOL_VERSION
        );
    }
    let endpoint = LocalEndpoint::from_path(descriptor.endpoint.clone());
    let mut stream = connect_blocking(&endpoint, timeout.min(Duration::from_secs(2)))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let request = RpcRequest {
        jsonrpc: "2.0".to_owned(),
        id: json!(1),
        method: method.to_owned(),
        params,
    };
    serde_json::to_writer(&mut stream, &request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut response = String::new();
    BufReader::new(stream)
        .take(REQUEST_LIMIT + 1)
        .read_line(&mut response)?;
    if u64::try_from(response.len()).unwrap_or(u64::MAX) > REQUEST_LIMIT {
        anyhow::bail!("control response exceeds payload limit");
    }
    serde_json::from_str(&response).context("decode control response")
}

fn select_instance() -> Result<InstanceDescriptor> {
    discover_instance()?.context("no running Bootty application was found")
}

fn discover_instance() -> Result<Option<InstanceDescriptor>> {
    ControlInstanceLease::discover()
}
