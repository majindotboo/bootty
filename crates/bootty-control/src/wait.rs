//! Snapshot reconciliation around bounded event waits. Events are wakeups, never state authority.
use crate::{
    Caller, CommandDescriptor, CommandInvocation, CommandOutcome, InstanceDescriptor,
    MutationClass, invoke_instance_timeout,
};
use anyhow::{Context as _, Result, bail, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub struct WaitRequest {
    pub invocation: CommandInvocation,
    pub topics: Vec<String>,
    pub pointer: String,
    pub expected: Value,
    pub deadline: Instant,
}
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WaitOutcome {
    Matched { value: Value },
    TimedOut { value: Option<Value> },
    Cancelled { value: Option<Value> },
}
struct Subscription<'a> {
    instance: &'a InstanceDescriptor,
    id: String,
}
impl Drop for Subscription<'_> {
    fn drop(&mut self) {
        let _ = invoke_instance_timeout(
            self.instance,
            "event.unsubscribe",
            json!({"subscription":self.id}),
            Duration::from_secs(1),
        );
    }
}
fn rpc(
    instance: &InstanceDescriptor,
    method: &str,
    params: Value,
    deadline: Instant,
) -> Result<Value> {
    let response = invoke_instance_timeout(
        instance,
        method,
        params,
        deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(5)),
    )?;
    if let Some(error) = response.error {
        bail!("{}: {}", error.code, error.message);
    }
    response.result.context("control response has no result")
}
fn read(
    instance: &InstanceDescriptor,
    command: &CommandInvocation,
    deadline: Instant,
) -> Result<Value> {
    let outcome: CommandOutcome = serde_json::from_value(rpc(
        instance,
        "command.invoke",
        json!({"invocation":command}),
        deadline,
    )?)?;
    match outcome {
        CommandOutcome::Success { value, .. } => Ok(value),
        other => bail!(
            "snapshot command failed: {}",
            serde_json::to_string(&other)?
        ),
    }
}
fn subscribe<'a>(
    instance: &'a InstanceDescriptor,
    request: &WaitRequest,
) -> Result<Subscription<'a>> {
    let result = rpc(
        instance,
        "event.subscribe",
        json!({"topics":request.topics}),
        request.deadline,
    )?;
    Ok(Subscription {
        instance,
        id: result
            .get("subscription")
            .and_then(Value::as_str)
            .context("subscription response has no ID")?
            .to_owned(),
    })
}

/// # Errors
/// Returns invalid request, snapshot, subscription, or control transport errors.
pub fn wait_for_command(
    instance: &InstanceDescriptor,
    mut request: WaitRequest,
    cancelled: &AtomicBool,
) -> Result<WaitOutcome> {
    if cancelled.load(Ordering::Relaxed) {
        return Ok(WaitOutcome::Cancelled { value: None });
    }
    if Instant::now() >= request.deadline {
        return Ok(WaitOutcome::TimedOut { value: None });
    }
    ensure!(
        request.pointer.is_empty() || request.pointer.starts_with('/'),
        "condition must be a JSON pointer"
    );
    ensure!(
        !request.topics.is_empty(),
        "at least one event topic is required"
    );
    resolve_snapshot_target(instance, &mut request)?;
    let mut subscription = subscribe(instance, &request)?;
    let mut cursor = 0;
    let mut snapshot = None;
    let mut refresh = true;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(WaitOutcome::Cancelled { value: snapshot });
        }
        if Instant::now() >= request.deadline {
            return Ok(WaitOutcome::TimedOut { value: snapshot });
        }
        if refresh {
            let value = read(instance, &request.invocation, request.deadline)?;
            if value.pointer(&request.pointer) == Some(&request.expected) {
                return Ok(WaitOutcome::Matched { value });
            }
            snapshot = Some(value);
        }
        let remaining = request.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            continue;
        }
        let response = match invoke_instance_timeout(
            instance,
            "event.wait",
            json!({"subscription":subscription.id, "cursor":cursor, "timeout_ms": remaining.as_millis().min(4000)}),
            remaining.min(Duration::from_secs(5)),
        ) {
            Ok(response) => response,
            Err(_) if cancelled.load(Ordering::Relaxed) => {
                return Ok(WaitOutcome::Cancelled { value: snapshot });
            }
            Err(_) if Instant::now() >= request.deadline => {
                return Ok(WaitOutcome::TimedOut { value: snapshot });
            }
            Err(error) => return Err(error),
        };
        if response
            .error
            .as_ref()
            .is_some_and(|error| error.code == -32005)
        {
            // Subscribe first, then snapshot again: mutations during reconciliation remain queued.
            drop(subscription);
            subscription = subscribe(instance, &request)?;
            cursor = 0;
            refresh = true;
            continue;
        }
        if let Some(error) = response.error {
            bail!("event wait failed ({}): {}", error.code, error.message);
        }
        let batch = response.result.context("event wait has no result")?;
        cursor = batch
            .get("cursor")
            .and_then(Value::as_u64)
            .context("event wait has no cursor")?;
        refresh = batch
            .get("events")
            .and_then(Value::as_array)
            .context("event wait has no events")?
            .iter()
            .any(|event| {
                event.get("topic").and_then(Value::as_str) != Some("command.completed")
                    || event.pointer("/payload/command").and_then(Value::as_str)
                        != Some(request.invocation.command.as_str())
            });
    }
}

fn resolve_snapshot_target(instance: &InstanceDescriptor, request: &mut WaitRequest) -> Result<()> {
    let descriptor: CommandDescriptor = serde_json::from_value(rpc(
        instance,
        "command.describe",
        json!({"command":request.invocation.command}),
        request.deadline,
    )?)?;
    ensure!(
        descriptor.mutation == MutationClass::Read,
        "wait snapshots must use a read-only command"
    );
    if let Some(kind) = descriptor.target
        && request.invocation.target.is_none()
    {
        let name = serde_json::to_value(kind)?
            .as_str()
            .context("resource kind")?
            .to_owned();
        let resource = CommandInvocation::new("resource.current", vec![name], Caller::Cli);
        request.invocation.target = Some(serde_json::from_value(
            read(instance, &resource, request.deadline)?
                .get("target")
                .context("current resource response has no target")?
                .clone(),
        )?);
    }
    Ok(())
}
