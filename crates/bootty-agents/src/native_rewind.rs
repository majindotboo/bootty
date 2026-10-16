//! Rewinding a conversation: the provider forgets a prompt's turn and every later one, and the
//! transcript lets them go. The files stay as they are; `native_checkpoints` puts those back.

use crate::native_protocol::{field, native_error_message};
use crate::native_session::{lock, publish_change};
use crate::{AgentKind, NativeAgentSession, NativeSessionSnapshot, NativeSessionStatus};
use serde_json::{Value, json};

/// Turns Codex lists a page while looking for a prompt's turn.
// Small pages keep each reply under the frame limit; raise it if rewinding far back gets slow.
const CODEX_TURN_PAGE: u32 = 8;

impl NativeAgentSession {
    /// Rewinds the conversation to before `prompt`: the provider forgets that message's turn and
    /// every later one, and so does the transcript.
    /// # Errors
    /// Returns a running turn, a message the provider no longer has, or the provider's refusal.
    pub fn rewind_conversation(&self, prompt: &str) -> Result<(), String> {
        let snapshot = self.snapshot();
        if snapshot.status == NativeSessionStatus::Working {
            return Err("Stop the turn before rewinding".to_owned());
        }
        if !snapshot
            .transcript
            .iter()
            .any(|item| item.id == prompt && item.role == "user")
        {
            return Err("That message is no longer in this conversation".to_owned());
        }
        let forgotten = match self.config.provider {
            AgentKind::Codex => self.rewind_codex(&snapshot, prompt)?,
            AgentKind::Claude => self.rewind_claude(&snapshot, prompt)?,
            AgentKind::Pi => self.rewind_pi(prompt)?,
        };
        lock(&self.snapshot).forget_from(&forgotten);
        publish_change(&self.change_handler);
        Ok(())
    }

    /// Reverts Codex's thread to before the turn `prompt` started or steered, and returns that
    /// turn's messages.
    fn rewind_codex(
        &self,
        snapshot: &NativeSessionSnapshot,
        prompt: &str,
    ) -> Result<Vec<String>, String> {
        let thread = snapshot
            .session_id
            .as_deref()
            .ok_or("Codex hasn't started this conversation")?;
        let failed = |error: String| {
            format!(
                "Codex couldn't rewind the conversation: {}",
                native_error_message(&error)
            )
        };
        let mut cursor = Value::Null;
        loop {
            let page = self
                .rpc(
                    "thread/turns/list",
                    json!({"threadId":thread,"cursor":cursor,"limit":CODEX_TURN_PAGE,"sortDirection":"desc"}),
                )
                .map_err(failed)?;
            for turn in field(&page, "data").as_array().into_iter().flatten() {
                let prompts = field(turn, "items")
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|item| field(item, "type") == "userMessage")
                    .filter_map(|item| field(item, "id").as_str())
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if !prompts.iter().any(|id| id == prompt) {
                    continue;
                }
                let turn = field(turn, "id")
                    .as_str()
                    .ok_or("Codex listed a turn without an identity")?;
                self.rpc(
                    "thread/revert",
                    json!({"threadId":thread,"beforeTurnId":turn}),
                )
                .map_err(failed)?;
                return Ok(prompts);
            }
            cursor = page
                .get("nextCursor")
                .filter(|cursor| cursor.is_string())
                .cloned()
                .ok_or("Codex no longer has that message in this conversation")?;
        }
    }

    /// Asks Claude to forget `prompt` and everything after it.
    fn rewind_claude(
        &self,
        snapshot: &NativeSessionSnapshot,
        prompt: &str,
    ) -> Result<Vec<String>, String> {
        // Claude refuses when its conversation has moved past the newest message shown here.
        let latest = snapshot
            .transcript
            .iter()
            .rev()
            .find(|item| item.role == "user")
            .map_or(prompt, |item| item.id.as_str());
        let reply = self
            .rpc(
                "rewind_conversation",
                json!({"target_message_uuid":prompt,"last_seen_user_message_uuid":latest}),
            )
            .map_err(|error| {
                format!(
                    "Claude couldn't rewind the conversation: {}",
                    native_error_message(&error)
                )
            })?;
        if field(&reply, "rewound") == true {
            return Ok(vec![prompt.to_owned()]);
        }
        Err(match field(&reply, "reason").as_str() {
            Some("stale_target") => {
                "Claude's conversation has messages this one doesn't show; reopen it and try again"
                    .to_owned()
            }
            _ => format!(
                "Claude didn't rewind the conversation: {}",
                field(&reply, "error")
                    .as_str()
                    .unwrap_or("it gave no reason")
            ),
        })
    }

    /// Moves Pi's conversation back to before `prompt` with Bootty's Pi extension, and checks Pi
    /// now holds fewer messages.
    fn rewind_pi(&self, prompt: &str) -> Result<Vec<String>, String> {
        let sent = prompt
            .strip_prefix("pi-user-")
            .filter(|at| !at.is_empty() && at.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or("Pi can't find that message")?;
        let commands = self.rpc("get_commands", json!({}))?;
        if !field(&commands, "commands")
            .as_array()
            .is_some_and(|commands| {
                commands
                    .iter()
                    .any(|command| field(command, "name") == crate::tool_bridge::PI_REWIND_COMMAND)
            })
        {
            return Err("This Pi conversation can't rewind without Bootty's tools".to_owned());
        }
        let count = |state: &Value| field(state, "messageCount").as_u64();
        let before = count(&self.rpc("get_state", json!({}))?);
        self.rpc(
            "prompt",
            json!({"message":format!("/{} {sent}", crate::tool_bridge::PI_REWIND_COMMAND)}),
        )?;
        let after = count(&self.rpc("get_state", json!({}))?);
        if !matches!((before, after), (Some(before), Some(after)) if after < before) {
            return Err("Pi kept the conversation as it was".to_owned());
        }
        Ok(vec![prompt.to_owned()])
    }
}

impl NativeSessionSnapshot {
    /// Lets go of the transcript from the first of `messages` it shows.
    fn forget_from(&mut self, messages: &[String]) {
        if let Some(at) = self
            .transcript
            .iter()
            .position(|item| messages.contains(&item.id))
        {
            self.transcript.truncate(at);
            self.changed();
        }
    }
}
