//! Native subagent lifecycles and Pi's versioned agent records.

use crate::native_protocol::field;
use crate::{NativeSessionSnapshot, NativeToolStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct NativeSubagent {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<crate::AgentKind>,
    pub title: String,
    pub prompt: String,
    pub model: Option<String>,
    /// Only Codex reports a provider thread here. Pi's task IDs are not conversation IDs.
    pub thread_id: Option<String>,
    /// The provider saved this subagent's conversation to its own file: a Claude agent task (a
    /// background shell has none) or a child of Pi's subagents extension.
    #[serde(
        default,
        alias = "claude_file",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub own_file: bool,
    /// Where a Claude background command writes its output, as Claude reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_file: Option<String>,
    pub owner_session: Option<String>,
    pub status: NativeToolStatus,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

impl NativeSubagent {
    /// Whether the subagent's own conversation can be read: a Codex thread, its own file, or a
    /// background command's output.
    #[must_use]
    pub const fn readable(&self) -> bool {
        self.thread_id.is_some() || self.own_file || self.output_file.is_some()
    }

    /// Whether its parent can ask the provider to stop it: a running Codex thread or Claude task.
    #[must_use]
    pub fn stoppable(&self) -> bool {
        self.status == NativeToolStatus::Running
            && (self.thread_id.is_some() || self.provider == Some(crate::AgentKind::Claude))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NativeSubagentDetail {
    pub agent: NativeSubagent,
    pub transcript: Vec<crate::NativeTranscriptItem>,
}

/// Where a Claude tool result says background command `task` writes its output: "Output is
/// being written to: PATH".
fn claude_output_file(result: &str, task: &str) -> Option<String> {
    let (_, rest) = result.split_once("Output is being written to: ")?;
    output_file(rest.lines().next()?.trim(), task)
}

/// `path` when it can be `task`'s output: an absolute path to a file named for the task, so a
/// reported path can't point a tab at some other file.
fn output_file(path: &str, task: &str) -> Option<String> {
    let file = std::path::Path::new(path);
    (path.len() <= 4096
        && !path.chars().any(char::is_control)
        && file.is_absolute()
        && file.file_name()? == std::ffi::OsStr::new(&format!("{task}.output")))
    .then(|| path.to_owned())
}

fn model(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|model| {
            !model.is_empty() && model.len() <= 256 && !model.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

fn text(value: &Value, limit: usize) -> String {
    value
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(limit)
        .collect()
}

impl NativeSessionSnapshot {
    /// Subagents this live session still reports as running. A stopped or failed process has
    /// none, even when its last reported state said otherwise.
    #[must_use]
    pub fn running_subagents(&self) -> usize {
        if matches!(
            self.status,
            crate::NativeSessionStatus::Stopped | crate::NativeSessionStatus::Error
        ) {
            return 0;
        }
        self.transcript
            .iter()
            .filter_map(|item| item.subagent.as_ref())
            .filter(|agent| {
                agent.status == NativeToolStatus::Running
                    && agent
                        .owner_session
                        .as_deref()
                        .is_none_or(|owner| self.session_id.as_deref() == Some(owner))
            })
            .count()
    }

    /// A relaunched provider cannot continue tools or subagents its previous process started.
    pub(crate) fn interrupt_unfinished_work(&mut self) {
        self.settle_running_tools();
        for agent in self
            .transcript
            .iter_mut()
            .filter_map(|item| item.subagent.as_mut())
        {
            if agent.status == NativeToolStatus::Running {
                agent.status = NativeToolStatus::Interrupted;
            }
        }
    }

    fn subagent(&mut self, mut agent: NativeSubagent, mut output: String) {
        let id = format!("subagent:{}", agent.id);
        let old = self
            .transcript
            .iter()
            .find(|item| item.id == id)
            .and_then(|item| item.subagent.clone());
        if output.is_empty() {
            output = self
                .transcript
                .iter()
                .find(|item| item.id == id)
                .map_or_else(String::new, |item| item.text.clone());
        }
        if let Some(old) = old {
            agent.started_at = agent.started_at.or(old.started_at);
            if agent.title.is_empty()
                || agent.title.starts_with("/root/")
                    && !old.title.is_empty()
                    && !old.title.starts_with("/root/")
            {
                agent.title = old.title;
            }
            if agent.prompt.is_empty() {
                agent.prompt = old.prompt;
            }
            agent.model = agent.model.or(old.model);
            agent.owner_session = agent.owner_session.or(old.owner_session);
        }
        self.message(
            id.clone(),
            "subagent",
            output,
            agent.status != NativeToolStatus::Running,
        );
        if let Some(item) = self.transcript.iter_mut().find(|item| item.id == id) {
            agent.started_at = agent.started_at.or(item.created_at);
            if agent.status != NativeToolStatus::Running {
                agent.completed_at = agent.completed_at.or(item.updated_at);
            }
            item.subagent = Some(agent);
        }
    }

    pub(crate) fn codex_subagents(&mut self, item: &Value, allow_new: bool) -> bool {
        let kind = field(item, "type").as_str().unwrap_or_default();
        let spawn = kind == "collabAgentToolCall" && field(item, "tool") == "spawnAgent";
        let activity = kind == "subAgentActivity";
        if !activity && kind != "collabAgentToolCall" {
            return false;
        }
        let mut updated = false;
        let receivers = if activity {
            vec![field(item, "agentThreadId")]
        } else {
            field(item, "receiverThreadIds")
                .as_array()
                .map_or_else(Vec::new, |ids| ids.iter().take(64).collect())
        };
        for receiver in receivers {
            let Some(id) = receiver.as_str().filter(|id| {
                !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
            }) else {
                continue;
            };
            let known = self
                .transcript
                .iter()
                .any(|item| item.subagent.as_ref().is_some_and(|agent| agent.id == id));
            if !known && (!allow_new || !(spawn || activity && field(item, "kind") == "started")) {
                continue;
            }
            let state = field(field(item, "agentsStates"), id);
            let status = if activity
                && field(item, "kind") == "interacted"
                && let Some(old) = self
                    .transcript
                    .iter()
                    .find_map(|item| item.subagent.as_ref().filter(|agent| agent.id == id))
            {
                old.status
            } else {
                match field(state, "status")
                    .as_str()
                    .or_else(|| field(item, "kind").as_str())
                {
                    Some("completed") => NativeToolStatus::Completed,
                    Some("interrupted" | "shutdown") => NativeToolStatus::Interrupted,
                    Some("errored" | "notFound") => NativeToolStatus::Failed,
                    _ => NativeToolStatus::Running,
                }
            };
            let title = text(
                item.get("agentNickname")
                    .or_else(|| item.get("agentName"))
                    .filter(|value| value.as_str().is_some_and(|name| !name.trim().is_empty()))
                    .unwrap_or_else(|| field(item, "agentPath")),
                256,
            );
            updated = true;
            self.subagent(
                NativeSubagent {
                    id: id.into(),
                    provider: Some(crate::AgentKind::Codex),
                    title,
                    prompt: text(field(item, "prompt"), 2048),
                    model: model(field(item, "model")),
                    thread_id: Some(id.into()),
                    own_file: false,
                    output_file: None,
                    owner_session: self.session_id.clone(),
                    status,
                    started_at: None,
                    completed_at: None,
                },
                text(field(state, "message"), 10000),
            );
        }
        updated
    }

    pub(crate) fn pi_subagents(&mut self, call: &str, name: &str, result: &Value, complete: bool) {
        let details = field(result, "details");
        if field(details, "version") == 1 && field(details, "tool") == name {
            let rows = if name == "spawn_agent" {
                vec![field(details, "agent")]
            } else if name == "list_agents" {
                field(details, "agents")
                    .as_array()
                    .map_or_else(Vec::new, |rows| rows.iter().take(64).collect())
            } else {
                Vec::new()
            };
            for row in rows {
                self.pi_agent_record(row);
            }
            let update = if name == "wait_agent" {
                field(details, "update")
            } else {
                field(details, "input")
            };
            if let Some(target) = field(update, "target").as_str()
                && let Some(mut agent) = self
                    .transcript
                    .iter()
                    .find_map(|item| item.subagent.as_ref().filter(|agent| agent.id == target))
                    .cloned()
            {
                let status = if name == "interrupt_agent" {
                    Some("interrupted")
                } else if name == "wait_agent" {
                    field(update, "agentStatus").as_str()
                } else {
                    None
                };
                if let Some(status) = status {
                    agent.status = match status {
                        "idle" => NativeToolStatus::Completed,
                        "failed" => NativeToolStatus::Failed,
                        "interrupted" => NativeToolStatus::Interrupted,
                        "queued" | "running" => NativeToolStatus::Running,
                        _ => agent.status,
                    };
                    if agent.status == NativeToolStatus::Running {
                        agent.completed_at = None;
                    }
                    self.subagent(agent, String::new());
                }
            }
        } else if name == "subagent" {
            self.pi_official_subagents(call, details, complete);
        }
    }

    fn pi_official_subagents(&mut self, call: &str, details: &Value, complete: bool) {
        for (index, row) in field(details, "results")
            .as_array()
            .into_iter()
            .flatten()
            .take(64)
            .enumerate()
        {
            let title = text(field(row, "agent"), 256);
            let prompt = text(field(row, "task"), 2048);
            if title.is_empty() || prompt.is_empty() {
                continue;
            }
            let finished = complete || field(row, "finished") == true;
            let status = if !finished {
                NativeToolStatus::Running
            } else if field(row, "stopReason") == "aborted" {
                NativeToolStatus::Interrupted
            } else if field(row, "exitCode")
                .as_i64()
                .is_some_and(|code| code != 0)
                || field(row, "stopReason") == "error"
            {
                NativeToolStatus::Failed
            } else {
                NativeToolStatus::Completed
            };
            let output = field(row, "messages")
                .as_array()
                .into_iter()
                .flatten()
                .rev()
                .find(|message| field(message, "role") == "assistant")
                .map_or_else(String::new, |message| {
                    crate::native_protocol::content_text(field(message, "content"))
                });
            self.subagent(
                NativeSubagent {
                    provider: Some(crate::AgentKind::Pi),
                    id: format!(
                        "{call}:{}",
                        field(row, "step")
                            .as_u64()
                            .unwrap_or_else(|| u64::try_from(index).unwrap_or_default())
                    ),
                    title,
                    prompt,
                    model: model(field(row, "model")),
                    thread_id: None,
                    own_file: false,
                    output_file: None,
                    owner_session: self.session_id.clone(),
                    status,
                    started_at: None,
                    completed_at: None,
                },
                output.chars().take(10000).collect(),
            );
        }
    }

    fn pi_agent_record(&mut self, row: &Value) {
        let Some(id) = field(row, "id")
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
        else {
            return;
        };
        let status = match field(row, "status").as_str() {
            Some("queued" | "running") => NativeToolStatus::Running,
            Some("idle") => NativeToolStatus::Completed,
            Some("failed") => NativeToolStatus::Failed,
            Some("interrupted") => NativeToolStatus::Interrupted,
            _ => return,
        };
        self.subagent(
            NativeSubagent {
                id: id.into(),
                provider: Some(crate::AgentKind::Pi),
                title: id.into(),
                prompt: text(field(row, "description"), 2048),
                model: model(field(row, "model")),
                thread_id: None,
                // The extension gives each child its own session, named in the parent's.
                own_file: true,
                output_file: None,
                owner_session: self.session_id.clone(),
                status,
                started_at: field(row, "startedAt").as_i64(),
                completed_at: field(row, "completedAt").as_i64(),
            },
            text(
                row.get("error")
                    .or_else(|| row.get("output"))
                    .unwrap_or(&Value::Null),
                10000,
            ),
        );
    }

    /// Records where Claude's background command `task` writes its output, from `result`, what
    /// the tool that started it returned.
    pub(crate) fn claude_command_started(&mut self, task: &str, result: &str) {
        let Some(path) = claude_output_file(result, task) else {
            return;
        };
        if let Some(agent) = self
            .transcript
            .iter_mut()
            .find_map(|item| item.subagent.as_mut().filter(|agent| agent.id == task))
        {
            agent.output_file = Some(path);
        }
    }

    pub(crate) fn claude_subagents(&mut self, value: &Value) {
        let Some(id) = field(value, "task_id")
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
        else {
            return;
        };
        let subtype = field(value, "subtype").as_str().unwrap_or_default();
        let known = self
            .transcript
            .iter()
            .find_map(|item| item.subagent.as_ref().filter(|agent| agent.id == id))
            .cloned();
        let tool_item = field(value, "tool_use_id").as_str().and_then(|tool| {
            self.transcript
                .iter()
                .find(|item| item.id == format!("claude-tool-{tool}"))
        });
        let tool = tool_item.and_then(|item| item.tool.as_ref());
        // Agent tools start subagents, and a backgrounded task (such as a shell) keeps running
        // after its turn. Foreground commands are already visible as tools.
        if known.is_none()
            && (subtype != "task_started"
                || !(field(value, "is_backgrounded") == true
                    || tool.is_some_and(|tool| matches!(tool.name.as_str(), "Agent" | "Task"))))
        {
            return;
        }
        let args = tool
            .and_then(|tool| serde_json::from_str::<Value>(&tool.input).ok())
            .unwrap_or(Value::Null);
        let status = if subtype == "task_progress"
            && let Some(old) = &known
        {
            old.status
        } else {
            // `task_updated` carries its status change in a patch.
            match field(value, "status")
                .as_str()
                .or_else(|| field(field(value, "patch"), "status").as_str())
            {
                Some("completed") => NativeToolStatus::Completed,
                Some("failed") => NativeToolStatus::Failed,
                Some("stopped" | "killed") => NativeToolStatus::Interrupted,
                _ if subtype == "task_updated" => known
                    .as_ref()
                    .map_or(NativeToolStatus::Running, |old| old.status),
                _ => NativeToolStatus::Running,
            }
        };
        let title = value
            .get("description")
            .unwrap_or_else(|| field(&args, "description"));
        let output_file = field(value, "output_file")
            .as_str()
            .and_then(|path| output_file(path, id))
            .or_else(|| tool_item.and_then(|item| claude_output_file(&item.text, id)))
            .or_else(|| known.as_ref().and_then(|known| known.output_file.clone()));
        self.subagent(
            NativeSubagent {
                id: id.into(),
                provider: Some(crate::AgentKind::Claude),
                title: text(title, 256),
                prompt: text(
                    args.get("prompt")
                        .unwrap_or_else(|| field(&args, "command")),
                    2048,
                ),
                model: model(field(&args, "model")),
                thread_id: None,
                // An agent task saves its conversation; a backgrounded shell doesn't.
                own_file: known.as_ref().is_some_and(|known| known.own_file)
                    || field(value, "task_type") == "local_agent"
                    || tool.is_some_and(|tool| matches!(tool.name.as_str(), "Agent" | "Task")),
                output_file,
                owner_session: self.session_id.clone(),
                status,
                started_at: None,
                completed_at: None,
            },
            text(field(value, "summary"), 10000),
        );
    }
}

impl crate::NativeAgentSession {
    /// Claude keeps each subagent's transcript in its own session file, and a background
    /// command's output in a file of its own.
    fn read_claude_subagent(
        &self,
        snapshot: &NativeSessionSnapshot,
        agent: NativeSubagent,
    ) -> Result<NativeSubagentDetail, String> {
        if let Some(path) = agent.output_file.as_deref().filter(|_| !agent.own_file) {
            let summary = snapshot
                .transcript
                .iter()
                .find(|item| item.id == format!("subagent:{}", agent.id))
                .map_or("", |item| item.text.as_str());
            let transcript = crate::native_history_claude::command_transcript(
                &self.config,
                &agent,
                path,
                summary,
            )?;
            return Ok(NativeSubagentDetail { agent, transcript });
        }
        let session = snapshot
            .session_id
            .as_deref()
            .or(self.config.session_id.as_deref())
            .ok_or("Claude has no captured session identity")?;
        let history =
            crate::native_history_claude::subagent_transcript(&self.config, session, &agent.id)?;
        Ok(NativeSubagentDetail {
            agent,
            transcript: history.transcript,
        })
    }

    /// Read a child only when this provider thread reported its identity; never acquire its writer.
    /// # Errors
    /// Rejects unknown children, missing provider transcripts or mismatched read replies.
    pub fn read_subagent(&self, id: &str) -> Result<NativeSubagentDetail, String> {
        let snapshot = self.snapshot();
        let mut agent = own_subagent(&snapshot, id)?;
        if self.config.provider == crate::AgentKind::Claude {
            return self.read_claude_subagent(&snapshot, agent);
        }
        if self.config.provider == crate::AgentKind::Pi && agent.own_file {
            let history =
                crate::native_history_pi::subagent_transcript(&self.config, &snapshot, &agent.id)?;
            return Ok(NativeSubagentDetail {
                agent,
                transcript: history.transcript,
            });
        }
        let original = agent.clone();
        let thread = agent
            .thread_id
            .as_deref()
            .ok_or("The provider exposes this subagent through its task output")?;
        let result = self.rpc(
            "thread/read",
            serde_json::json!({"threadId":thread,"includeTurns":true}),
        )?;
        let response = field(&result, "thread");
        if field(response, "id").as_str() != Some(thread) {
            return Err("The provider returned a different subagent thread".into());
        }
        let turns = field(response, "turns")
            .as_array()
            .ok_or("The provider returned no subagent transcript")?;
        let mut history = NativeSessionSnapshot::new(self.config.provider);
        history.session_id = Some(thread.into());
        for turn in turns {
            history.history_turn(turn);
        }
        let status = match field(field(response, "status"), "type").as_str() {
            Some("active") => Some(NativeToolStatus::Running),
            Some("systemError") => Some(NativeToolStatus::Failed),
            thread_status => match turns.last().and_then(|turn| field(turn, "status").as_str()) {
                Some("completed") => Some(NativeToolStatus::Completed),
                Some("failed") => Some(NativeToolStatus::Failed),
                Some("interrupted") => Some(NativeToolStatus::Interrupted),
                Some("inProgress") if thread_status == Some("notLoaded") => {
                    Some(NativeToolStatus::Interrupted)
                }
                Some("inProgress") => Some(NativeToolStatus::Running),
                _ if thread_status == Some("notLoaded") => Some(NativeToolStatus::Interrupted),
                _ if thread_status == Some("idle") => Some(NativeToolStatus::Completed),
                _ => None,
            },
        };
        if let Some(status) = status {
            agent.status = status;
            agent.completed_at = if status == NativeToolStatus::Running {
                None
            } else {
                turns
                    .last()
                    .and_then(|turn| field(turn, "completedAt").as_i64())
                    .and_then(|time| time.checked_mul(1000))
                    .or(agent.completed_at)
            };
            let mut current = self
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let changed = current.transcript.iter_mut().any(|item| {
                let Some(child) = &mut item.subagent else {
                    return false;
                };
                if child.id != id || child.owner_session != agent.owner_session {
                    return false;
                }
                // A parent notification received during the read is newer than its reply.
                if *child != original {
                    agent.clone_from(child);
                    return false;
                }
                if *child == agent {
                    return false;
                }
                child.clone_from(&agent);
                item.complete = status != NativeToolStatus::Running;
                true
            });
            if changed {
                current.changed();
            }
            drop(current);
            if changed {
                crate::native_session::publish_change(&self.change_handler);
            }
        }
        Ok(NativeSubagentDetail {
            agent,
            transcript: history.transcript,
        })
    }

    /// Ask the provider to stop a running child. Its own report of the stop settles the status.
    /// # Errors
    /// Rejects unknown or finished children, providers that can't stop one, or transport errors.
    pub fn stop_subagent(&self, id: &str) -> Result<(), String> {
        let snapshot = self.snapshot();
        let agent = own_subagent(&snapshot, id)?;
        if agent.status != NativeToolStatus::Running {
            return Err("This subagent is not running".into());
        }
        match self.config.provider {
            crate::AgentKind::Claude => {
                self.rpc("stop_task", serde_json::json!({"task_id":id}))?;
            }
            crate::AgentKind::Codex => {
                let thread = agent
                    .thread_id
                    .as_deref()
                    .ok_or("The provider did not report this subagent's thread")?;
                let result = self.rpc(
                    "thread/read",
                    serde_json::json!({"threadId":thread,"includeTurns":true}),
                )?;
                let turn = field(field(&result, "thread"), "turns")
                    .as_array()
                    .and_then(|turns| {
                        turns
                            .iter()
                            .rev()
                            .find(|turn| field(turn, "status") == "inProgress")
                    })
                    .and_then(|turn| field(turn, "id").as_str())
                    .ok_or("This subagent has no running turn")?;
                self.rpc(
                    "turn/interrupt",
                    serde_json::json!({"threadId":thread,"turnId":turn}),
                )?;
            }
            crate::AgentKind::Pi => return Err("Pi cannot stop one subagent".into()),
        }
        Ok(())
    }
}

/// The child this conversation reported, not one a source conversation reported before a fork.
fn own_subagent(snapshot: &NativeSessionSnapshot, id: &str) -> Result<NativeSubagent, String> {
    let agent = snapshot
        .transcript
        .iter()
        .find_map(|item| item.subagent.as_ref().filter(|agent| agent.id == id))
        .cloned()
        .ok_or("The provider did not report this subagent")?;
    if agent.owner_session != snapshot.session_id {
        return Err("This subagent belongs to the source conversation".into());
    }
    Ok(agent)
}
