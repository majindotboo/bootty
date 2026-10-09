//! Bounded activity for the exact captured conversation; account and launch paths stay private.
use super::{NativeAgentService, lock};
use crate::{AgentKind, NativeSessionStatus, NativeToolStatus};
use bootty_control::CommandTarget;
use serde::Serialize;

pub const MAX_NATIVE_ACTIVITY_ITEMS: usize = 32;
const MAX_ACTIVITY_TEXT_BYTES: usize = 8 * 1024;

#[derive(Debug, Serialize)]
pub struct NativeActivityPage {
    pub id: String,
    pub provider: AgentKind,
    pub status: NativeSessionStatus,
    pub total: usize,
    /// Most recent first, from accepted retained history. This does not fetch older provider pages.
    pub items: Vec<NativeActivityItem>,
}

#[derive(Debug, Serialize)]
pub struct NativeActivityItem {
    pub id: String,
    pub role: String,
    pub text: String,
    pub text_truncated: bool,
    pub complete: bool,
    pub tool: Option<NativeActivityTool>,
}

#[derive(Debug, Serialize)]
pub struct NativeActivityTool {
    pub name: String,
    pub status: NativeToolStatus,
}

impl NativeAgentService {
    /// Read accepted recent activity without exposing launch configuration or tool inputs.
    /// # Errors
    /// Rejects a stale identity or an unbounded request.
    pub fn recent_activity(
        &self,
        target: &CommandTarget,
        limit: usize,
    ) -> Result<NativeActivityPage, String> {
        if !(1..=MAX_NATIVE_ACTIVITY_ITEMS).contains(&limit) {
            return Err("Activity limit must be between 1 and 32".into());
        }
        let store = lock(&self.store);
        let record = store
            .records
            .iter()
            .find(|record| record.target() == *target)
            .ok_or("Native session target is unknown or stale")?;
        let transcript = &record.snapshot.transcript;
        let page = NativeActivityPage {
            id: record.id.clone(),
            provider: record.config.provider,
            status: record.snapshot.status,
            total: transcript.len(),
            items: transcript
                .iter()
                .rev()
                .take(limit)
                .map(|item| {
                    let mut text = item.text.clone();
                    text.truncate(
                        text.floor_char_boundary(text.len().min(MAX_ACTIVITY_TEXT_BYTES)),
                    );
                    NativeActivityItem {
                        id: item.id.clone(),
                        role: item.role.clone(),
                        text_truncated: text.len() < item.text.len(),
                        text,
                        complete: item.complete,
                        tool: item.tool.as_ref().map(|tool| NativeActivityTool {
                            name: tool.name.clone(),
                            status: tool.status,
                        }),
                    }
                })
                .collect(),
        };
        drop(store);
        Ok(page)
    }
}
