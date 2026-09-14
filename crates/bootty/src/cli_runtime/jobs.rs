use anyhow::{Context as _, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty::cli::{Cli, RunArgs};
use bootty_control::{Caller, CommandInvocation, CommandOutcome, InstanceDescriptor};
use bootty_host::jobs::{JobRead, JobSpec, JobStatus, JobStream, JobSummary};
use std::{
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

fn invoke(
    instance: &InstanceDescriptor,
    name: &str,
    arguments: Vec<String>,
) -> Result<serde_json::Value> {
    let result = super::invoke_control_command_on_instance(
        instance,
        CommandInvocation::new(name, arguments, Caller::Cli),
        false,
        false,
    )?;
    if let Some(error) = result.error {
        return Err(super::rpc_failure(&error));
    }
    let outcome: CommandOutcome =
        serde_json::from_value(result.result.context("job command returned no result")?)?;
    super::command_result(Some(outcome))?.context("job command returned no value")
}
struct OwnedJob {
    instance: InstanceDescriptor,
    id: String,
    keep: bool,
}
impl Drop for OwnedJob {
    fn drop(&mut self) {
        let _ = invoke(&self.instance, "jobs.cancel", vec![self.id.clone()]);
        if !self.keep {
            let _ = invoke(&self.instance, "jobs.forget", vec![self.id.clone()]);
        }
    }
}

pub fn run_job(cli: &Cli, args: &RunArgs) -> Result<()> {
    let instance = bootty_control::select_or_start(cli.start())
        .map_err(|error| super::transport_failure(&error))?;
    let (program, command_args) = args
        .command
        .split_first()
        .context("run requires a command")?;
    let spec = JobSpec {
        program: program.clone(),
        args: command_args.to_vec(),
        cwd: args.cwd.clone(),
        timeout_seconds: args.timeout,
    };
    let mut invocation = CommandInvocation::new(
        "jobs.start",
        vec![serde_json::to_string(&spec)?],
        Caller::Cli,
    );
    invocation.target = args
        .target
        .as_ref()
        .map(|target| serde_json::from_str(target))
        .transpose()?;
    let response = super::invoke_control_command_on_instance(&instance, invocation, false, false)?;
    if let Some(error) = response.error {
        return Err(super::rpc_failure(&error));
    }
    let outcome: CommandOutcome =
        serde_json::from_value(response.result.context("job start returned no result")?)?;
    let CommandOutcome::Success { value, .. } = outcome else {
        return super::command_result(Some(outcome)).map(|_| ());
    };
    let job: JobSummary = serde_json::from_value(value)?;
    if args.detach {
        println!("{}", serde_json::to_string(&job)?);
        return Ok(());
    }
    let json = cli.json();
    let owned = OwnedJob {
        instance,
        id: job.id,
        keep: args.keep,
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let completed = runtime.block_on(async move {
        let handler = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                signal.store(true, Ordering::Relaxed);
            }
        });
        let result = tokio::task::spawn_blocking(move || follow(&owned, json, &cancel)).await?;
        handler.abort();
        result
    })?;
    let code = if completed.timed_out {
        124
    } else if completed.cancel_requested {
        130
    } else {
        match completed.status {
            JobStatus::Exited {
                code: Some(code), ..
            } => u8::try_from(code).unwrap_or(1),
            JobStatus::Exited {
                signal: Some(signal),
                ..
            } => u8::try_from(signal.saturating_add(128)).unwrap_or(1),
            JobStatus::Failed { message } => bail!("job failed: {message}"),
            _ => bail!("job ended without an observed exit status"),
        }
    };
    if code == 0 {
        Ok(())
    } else {
        Err(super::CliFailure {
            code,
            message: format!("job finished with exit status {code}"),
        }
        .into())
    }
}
fn follow(owned: &OwnedJob, json: bool, cancel: &AtomicBool) -> Result<JobSummary> {
    let instance = &owned.instance;
    let mut cursor = 0;
    let mut cancelled = false;
    loop {
        if cancel.load(Ordering::Relaxed) && !cancelled {
            invoke(instance, "jobs.cancel", vec![owned.id.clone()])?;
            cancelled = true;
        }
        let batch: JobRead = serde_json::from_value(invoke(
            instance,
            "jobs.read",
            vec![owned.id.clone(), cursor.to_string(), "4000".to_owned()],
        )?)?;
        if batch.gap {
            bail!(
                "job output exceeded retained history; output is incomplete (job {})",
                owned.id
            );
        }
        cursor = batch.cursor;
        if json {
            println!("{}", serde_json::to_string(&batch)?);
        } else {
            for chunk in batch.chunks {
                let data = STANDARD.decode(chunk.data)?;
                match chunk.stream {
                    JobStream::Stdout => std::io::stdout().write_all(&data)?,
                    JobStream::Stderr => std::io::stderr().write_all(&data)?,
                }
            }
            std::io::stdout().flush()?;
            std::io::stderr().flush()?;
        }
        if batch.job.status.finished() && cursor == batch.job.next_cursor {
            return Ok(batch.job);
        }
    }
}
