//! Images a tool returned, such as a screenshot, shown on the tool's row. The row keeps their
//! references; the pixels wait in the live snapshot until the next commit writes them beside the
//! conversation's attachments.

use crate::NativeSessionSnapshot;
use crate::native_protocol::field;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeSet, fmt::Write as _, sync::Arc};

/// Tool image identities start with this, apart from attached files.
pub const PREFIX: &str = "tool-image-";
/// Images one tool result shows; more stay out.
const PER_TOOL: usize = 4;
/// Tool images one conversation keeps; a newer one lets the oldest go.
// Raise it with a byte budget per conversation if long computer-use runs need more.
const PER_CONVERSATION: usize = 64;

/// An image a tool returned, waiting to be written beside the conversation's attachments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingToolImage {
    pub id: String,
    pub png: Arc<[u8]>,
}

impl NativeSessionSnapshot {
    /// Shows the images in the tool result `content` on the row `id`. The same image arriving
    /// again, as history replays do, keeps its one reference.
    pub(crate) fn tool_images(&mut self, id: &str, content: &Value) {
        let encoded = encoded_images(content);
        if encoded.is_empty() {
            return;
        }
        let Some(item) = self.transcript.iter_mut().find(|item| item.id == id) else {
            return;
        };
        let tool = item
            .tool
            .as_ref()
            .map_or_else(|| "Tool".to_owned(), |tool| tool.name.clone());
        for (index, data) in encoded.into_iter().enumerate() {
            let Ok(bytes) = STANDARD.decode(data) else {
                continue;
            };
            let image = identity(&bytes);
            if item.attachments.iter().any(|known| known.id == image) {
                continue;
            }
            let name = format!("{tool} image {}.png", index.saturating_add(1));
            let Ok((reference, png)) =
                crate::native_attachments::tool_png_attachment(image, name, bytes)
            else {
                continue;
            };
            if !self
                .tool_images
                .iter()
                .any(|pending| pending.id == reference.id)
            {
                self.tool_images.push(PendingToolImage {
                    id: reference.id.clone(),
                    png: png.into(),
                });
            }
            item.attachments.push(reference);
        }
        self.forget_oldest_tool_images();
    }

    /// The tool images the transcript still shows.
    pub(crate) fn shown_tool_images(&self) -> BTreeSet<&str> {
        self.transcript
            .iter()
            .flat_map(|item| &item.attachments)
            .map(|reference| reference.id.as_str())
            .filter(|id| id.starts_with(PREFIX))
            .collect()
    }

    fn forget_oldest_tool_images(&mut self) {
        let mut extra = self
            .shown_tool_images()
            .len()
            .saturating_sub(PER_CONVERSATION);
        for item in &mut self.transcript {
            if extra == 0 {
                break;
            }
            item.attachments.retain(|reference| {
                let forget = extra > 0 && reference.id.starts_with(PREFIX);
                if forget {
                    extra = extra.saturating_sub(1);
                }
                !forget
            });
        }
        let shown = self
            .shown_tool_images()
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        self.tool_images
            .retain(|pending| shown.contains(&pending.id));
    }
}

/// The base64 images in a tool result, in the shapes MCP, Claude and Codex give them.
fn encoded_images(content: &Value) -> Vec<&str> {
    let largest = crate::MAX_NATIVE_ATTACHMENT_IMAGE_BYTES
        .div_ceil(3)
        .saturating_mul(4);
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| match field(block, "type").as_str()? {
            // MCP carries the data inline, Claude in a base64 source.
            "image" => field(block, "data")
                .as_str()
                .or_else(|| {
                    let source = field(block, "source");
                    (field(source, "type") == "base64")
                        .then(|| field(source, "data").as_str())
                        .flatten()
                })
                .or_else(|| data_url(field(block, "url"))),
            "inputImage" => data_url(field(block, "imageUrl")),
            _ => None,
        })
        .filter(|data| data.len() <= largest)
        .take(PER_TOOL)
        .collect()
}

fn data_url(value: &Value) -> Option<&str> {
    value
        .as_str()?
        .strip_prefix("data:image/")?
        .split_once(";base64,")
        .map(|(_, data)| data)
}

/// A name for `bytes` that the same image always gets.
fn identity(bytes: &[u8]) -> String {
    let mut id = PREFIX.to_owned();
    for byte in Sha256::digest(bytes).iter().take(16) {
        _ = write!(id, "{byte:02x}");
    }
    id
}
