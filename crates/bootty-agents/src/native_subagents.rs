//! Native subagent lifecycles and Pi's versioned agent records.
/* Subagent lifecycle behavior adapted from T3 Code.
MIT License

Copyright (c) 2026 T3 Tools Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/

use crate::native_protocol::field;
use crate::{NativeSessionSnapshot, NativeToolStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct NativeSubagent {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub model: Option<String>,
    /// Only Codex reports a provider thread here. Pi's task IDs are not conversation IDs.
    pub thread_id: Option<String>,
    pub owner_session: Option<String>,
    pub status: NativeToolStatus,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NativeSubagentDetail {
    pub agent: NativeSubagent,
    pub transcript: Vec<crate::NativeTranscriptItem>,
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
            if agent.title.is_empty() {
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
            let title = text(field(item, "agentPath"), 256);
            updated = true;
            self.subagent(
                NativeSubagent {
                    id: id.into(),
                    title,
                    prompt: text(field(item, "prompt"), 2048),
                    model: model(field(item, "model")),
                    thread_id: Some(id.into()),
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
                title: id.into(),
                prompt: text(field(row, "description"), 2048),
                model: model(field(row, "model")),
                thread_id: None,
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
        let tool = field(value, "tool_use_id").as_str().and_then(|tool| {
            self.transcript
                .iter()
                .find(|item| item.id == format!("claude-tool-{tool}"))
                .and_then(|item| item.tool.as_ref())
        });
        if known.is_none()
            && (subtype != "task_started"
                || !tool.is_some_and(|tool| matches!(tool.name.as_str(), "Agent" | "Task")))
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
            match field(value, "status").as_str() {
                Some("completed") => NativeToolStatus::Completed,
                Some("failed") => NativeToolStatus::Failed,
                Some("stopped") => NativeToolStatus::Interrupted,
                _ => NativeToolStatus::Running,
            }
        };
        let title = value
            .get("description")
            .unwrap_or_else(|| field(&args, "description"));
        self.subagent(
            NativeSubagent {
                id: id.into(),
                title: text(title, 256),
                prompt: text(field(&args, "prompt"), 2048),
                model: model(field(&args, "model")),
                thread_id: None,
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
    /// Read a child only when this provider thread reported its identity; never acquire its writer.
    /// # Errors
    /// Rejects unknown children, missing provider transcripts or mismatched read replies.
    pub fn read_subagent(&self, id: &str) -> Result<NativeSubagentDetail, String> {
        let snapshot = self.snapshot();
        let agent = snapshot
            .transcript
            .iter()
            .find_map(|item| item.subagent.as_ref().filter(|agent| agent.id == id))
            .cloned()
            .ok_or("The provider did not report this subagent")?;
        if agent.owner_session != snapshot.session_id {
            return Err("This subagent belongs to the source conversation".into());
        }
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
        Ok(NativeSubagentDetail {
            agent,
            transcript: history.transcript,
        })
    }
}
