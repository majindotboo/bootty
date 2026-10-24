#![cfg(test)]
//! Unstarted sessions are independent drafts, not backend processes.
use assert_fs::TempDir;
use bootty_config::config::{BoottyConfig, MultiplexerBackendConfig};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget,
};
use bootty_ui::{
    AppState,
    gpui::{DialogId, DialogIntent},
    presentation::dialogs::{DialogProjection, NEW_SESSION_ID},
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::Value;
use std::{
    path::Path,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

#[path = "support/idle_frames.rs"]
mod frames;
mod support;
#[path = "support/config.rs"]
mod test_config;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn host(config: BoottyConfig) -> TestResult<(AppState, mpsc::Receiver<()>)> {
    let (wake, wakes) = mpsc::channel();
    let state = AppState::new(
        config,
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )?;
    Ok((state, wakes))
}

fn config(directory: &Path) -> BoottyConfig {
    let mut config = test_config::config(
        directory.join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.session.working_directory = Some(directory.to_owned());
    config.agents.codex.enabled = false;
    config.agents.claude.enabled = false;
    config.agents.pi.enabled = false;
    config
}

fn submit(
    state: &mut AppState,
    wakes: &mpsc::Receiver<()>,
    invocation: CommandInvocation,
) -> TestResult<CommandOutcome> {
    let command = invocation.command.clone();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("deadline overflow")?;
    let reply = state
        .app_command_sender(Caller::Socket)
        .submit(invocation, deadline, CommandCancellation::new())
        .map_err(|error| format!("Command submission: {error:?}"))?;
    loop {
        state.update_frame(frames::idle_frame(Instant::now()));
        if let Ok(outcome) = reply.try_recv() {
            return Ok(outcome);
        }
        wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| format!("Waiting for {command}: {error}"))?;
    }
}

fn success(outcome: CommandOutcome) -> TestResult<Value> {
    if let CommandOutcome::Success { value, .. } = outcome {
        Ok(value)
    } else {
        Err(format!("Command failed: {outcome:?}").into())
    }
}

fn binding(state: &mut AppState, wakes: &mpsc::Receiver<()>) -> TestResult<CommandTarget> {
    let value = success(submit(
        state,
        wakes,
        CommandInvocation::new("spaces.list", Vec::new(), Caller::Socket),
    )?)?;
    let target = value
        .get(0)
        .and_then(|space| space.get("target"))
        .cloned()
        .ok_or("Missing binding target")?;
    Ok(serde_json::from_value(target)?)
}

fn list(
    state: &mut AppState,
    wakes: &mpsc::Receiver<()>,
    target: &CommandTarget,
) -> TestResult<Value> {
    let mut invocation = CommandInvocation::new("session.drafts", Vec::new(), Caller::Socket);
    invocation.target = Some(target.clone());
    success(submit(state, wakes, invocation)?)
}

fn wait_for_drafts(
    state: &mut AppState,
    wakes: &mpsc::Receiver<()>,
    target: &CommandTarget,
    count: usize,
) -> TestResult<Value> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("deadline overflow")?;
    loop {
        let drafts = list(state, wakes, target)?;
        if drafts
            .as_array()
            .is_some_and(|drafts| drafts.len() == count)
        {
            return Ok(drafts);
        }
        if Instant::now() >= deadline {
            return Err(format!("Expected {count} drafts, got {drafts}").into());
        }
        wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| format!("Waiting for {count} drafts after {drafts}: {error}"))?;
    }
}

fn type_prompt(state: &mut AppState, text: &str) {
    state.apply_dialog_intent(
        &DialogIntent::TextChanged {
            dialog: DialogId::new(NEW_SESSION_ID),
            value: text.to_owned(),
        },
        &mut Vec::new(),
    );
}

fn shown_prompt(state: &mut AppState) -> Option<String> {
    match state.dialog_projection()? {
        DialogProjection::Dialog(spec) => spec.text,
        DialogProjection::SpaceEditor(_) => None,
    }
}

#[rstest]
fn drafts_survive_navigation_restart_and_explicit_discard() -> TestResult {
    let directory = TempDir::new()?;
    let config = config(directory.path());
    let (mut state, wakes) = host(config.clone())?;
    let target = binding(&mut state, &wakes)?;
    let scope = state.mux_scope();
    state.open_new_session_dialog_in(scope, directory.path().to_string_lossy().into_owned());
    type_prompt(&mut state, "First unfinished idea");
    let first = wait_for_drafts(&mut state, &wakes, &target, 1)?[0]["identity"]
        .as_str()
        .ok_or("draft identity")?
        .to_owned();
    state.apply_dialog_intent(
        &DialogIntent::Dismiss {
            dialog: DialogId::new(NEW_SESSION_ID),
        },
        &mut Vec::new(),
    );
    state.open_new_session_dialog_in(scope, directory.path().to_string_lossy().into_owned());
    assert_eq!(shown_prompt(&mut state).as_deref(), Some(""));
    type_prompt(&mut state, "Second unfinished idea");
    let drafts = wait_for_drafts(&mut state, &wakes, &target, 2)?;
    let second = drafts
        .as_array()
        .ok_or("draft array")?
        .iter()
        .find(|draft| draft["identity"] != first)
        .and_then(|draft| draft["identity"].as_str())
        .ok_or("second draft")?
        .to_owned();
    assert_eq!(
        state.mux().all_sessions().len(),
        0,
        "drafts do not start processes"
    );
    drop(state);
    let (mut state, wakes) = host(config)?;
    let target = binding(&mut state, &wakes)?;
    assert_eq!(
        wait_for_drafts(&mut state, &wakes, &target, 2)?
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    let mut open =
        CommandInvocation::new("session.draft.open", vec![first.clone()], Caller::Socket);
    open.target = Some(target.clone());
    success(submit(&mut state, &wakes, open)?)?;
    assert_eq!(
        shown_prompt(&mut state).as_deref(),
        Some("First unfinished idea")
    );
    let mut discard = CommandInvocation::new("session.draft.discard", vec![second], Caller::Socket);
    discard.target = Some(target.clone());
    let outcome = submit(&mut state, &wakes, discard.clone())?;
    let CommandOutcome::ConfirmationRequired { confirmation } = outcome else {
        return Err("Discard must ask first".into());
    };
    discard.confirmation = Some(*confirmation);
    success(submit(&mut state, &wakes, discard)?)?;
    let remaining_drafts = wait_for_drafts(&mut state, &wakes, &target, 1)?;
    assert_eq!(remaining_drafts[0]["identity"], first);
    assert_eq!(state.mux().all_sessions().len(), 0);
    Ok(())
}

#[rstest]
fn clearing_the_restored_draft_removes_its_saved_entry() -> TestResult {
    let directory = TempDir::new()?;
    let config = config(directory.path());
    let (mut state, wakes) = host(config.clone())?;
    let target = binding(&mut state, &wakes)?;
    let scope = state.mux_scope();
    state.open_new_session_dialog_in(scope, directory.path().to_string_lossy().into_owned());
    type_prompt(&mut state, "Keep this draft");
    wait_for_drafts(&mut state, &wakes, &target, 1)?;
    drop(state);

    let (mut state, wakes) = host(config.clone())?;
    let target = binding(&mut state, &wakes)?;
    let drafts = wait_for_drafts(&mut state, &wakes, &target, 1)?;
    let identity = drafts[0]["identity"].as_str().ok_or("draft identity")?;
    let mut open = CommandInvocation::new(
        "session.draft.open",
        vec![identity.to_owned()],
        Caller::Socket,
    );
    open.target = Some(target.clone());
    success(submit(&mut state, &wakes, open)?)?;
    assert_eq!(shown_prompt(&mut state).as_deref(), Some("Keep this draft"));
    type_prompt(&mut state, "");
    wait_for_drafts(&mut state, &wakes, &target, 0)?;
    drop(state);

    let (mut state, wakes) = host(config)?;
    let target = binding(&mut state, &wakes)?;
    wait_for_drafts(&mut state, &wakes, &target, 0)?;
    let scope = state.mux_scope();
    state.open_new_session_dialog_in(scope, directory.path().to_string_lossy().into_owned());
    assert_eq!(shown_prompt(&mut state).as_deref(), Some(""));
    Ok(())
}

#[rstest]
fn a_failed_discard_keeps_the_draft_and_its_text() -> TestResult {
    let directory = TempDir::new()?;
    let (mut state, wakes) = host(config(directory.path()))?;
    let target = binding(&mut state, &wakes)?;
    let scope = state.mux_scope();
    state.open_new_session_dialog_in(scope, directory.path().to_string_lossy().into_owned());
    type_prompt(&mut state, "Keep this unfinished work");
    let drafts = wait_for_drafts(&mut state, &wakes, &target, 1)?;
    let identity = drafts[0]["identity"]
        .as_str()
        .ok_or("draft identity")?
        .to_owned();
    let catalog = std::fs::read_dir(directory.path())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".drafts.json"))
        })
        .ok_or("draft catalog")?;
    let backup = catalog.with_extension("json.backup");
    std::fs::rename(&catalog, &backup)?;
    std::fs::create_dir(&catalog)?;
    let mut discard = CommandInvocation::new(
        "session.draft.discard",
        vec![identity.clone()],
        Caller::Socket,
    );
    discard.target = Some(target.clone());
    let CommandOutcome::ConfirmationRequired { confirmation } =
        submit(&mut state, &wakes, discard.clone())?
    else {
        return Err("Discard must ask first".into());
    };
    discard.confirmation = Some(*confirmation);
    let outcome = submit(&mut state, &wakes, discard)?;
    if !matches!(outcome, CommandOutcome::Failed { .. }) {
        return Err(format!("Discard must report its failed write: {outcome:?}").into());
    }
    assert_eq!(list(&mut state, &wakes, &target)?[0]["identity"], identity);
    assert_eq!(
        shown_prompt(&mut state).as_deref(),
        Some("Keep this unfinished work")
    );
    std::fs::remove_dir(&catalog)?;
    std::fs::rename(backup, catalog)?;
    Ok(())
}

#[cfg(unix)]
#[rstest]
fn automatic_terminal_restore_keeps_the_open_draft() -> TestResult {
    let directory = TempDir::new()?;
    let mut config = config(directory.path());
    config.session.shell = Some("/bin/sh".into());
    let (mut state, wakes) = host(config)?;
    let target = binding(&mut state, &wakes)?;
    let mut create = CommandInvocation::new(
        "session.create",
        vec![
            "restore-fixture".into(),
            directory.path().to_string_lossy().into_owned(),
            "[]".into(),
            "restore-before-draft".into(),
            "Saved terminal".into(),
        ],
        Caller::Socket,
    );
    create.target = Some(target.clone());
    let created = success(submit(&mut state, &wakes, create)?)?;
    let session = serde_json::from_value::<CommandTarget>(created["created"].clone())?;
    let mut close = CommandInvocation::new("session.close", Vec::new(), Caller::Socket);
    close.target = Some(session);
    close.confirmation = Some(close.confirmation());
    success(submit(&mut state, &wakes, close)?)?;
    let scope = state.mux_scope();
    state.open_new_session_dialog_in(scope, directory.path().to_string_lossy().into_owned());
    type_prompt(&mut state, "Do not replace this unfinished prompt");
    let mut reopen = CommandInvocation::new(
        "session.reopen",
        vec!["restore-before-draft".into(), "preserve".into()],
        Caller::Socket,
    );
    reopen.target = Some(target);
    success(submit(&mut state, &wakes, reopen)?)?;
    assert_eq!(
        shown_prompt(&mut state).as_deref(),
        Some("Do not replace this unfinished prompt")
    );
    let spaces = success(submit(
        &mut state,
        &wakes,
        CommandInvocation::new("spaces.list", Vec::new(), Caller::Socket),
    )?)?;
    let mut close = CommandInvocation::new("session.close", Vec::new(), Caller::Socket);
    close.target = Some(serde_json::from_value(
        spaces[0]["sessions"][0]["target"].clone(),
    )?);
    close.confirmation = Some(close.confirmation());
    success(submit(&mut state, &wakes, close)?)?;
    Ok(())
}
