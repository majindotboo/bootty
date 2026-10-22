use std::fmt::Write as _;

use std::io::Cursor;
use std::path::PathBuf;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// Images use the existing prompt attachment count; bytes stay bounded independently.
pub const MAX_NATIVE_PROMPT_IMAGES: usize = crate::MAX_NATIVE_PROMPT_ATTACHMENTS;
pub const MAX_NATIVE_PROMPT_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_NATIVE_PROMPT_TEXT_BYTES: usize = 64 * 1024;
const MAX_NATIVE_PROMPT_CITATIONS: usize = 16;
// Admitted PNGs fit in 8 MiB raw / 11.2 MiB base64. Revisit only for a supported
// provider format requiring a larger bounded envelope; ordinary control records stay at 1 MiB.
pub const MAX_NATIVE_IMAGE_ENVELOPE_BYTES: usize = 16 * 1024 * 1024;
const MAX_DECODE_BYTES: usize = 64 * 1024 * 1024;

/// Host-issued immutable image identity. No filesystem location or encoded pixels are retained.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeImageReference {
    pub id: String,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

/// An admitted host/store PNG, never a deserializable command argument or caller-supplied path.
#[derive(Clone)]
pub struct NativePromptImage {
    reference: NativeImageReference,
    png: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct NativePromptAttachments {
    images: Vec<NativePromptImage>,
    files: Vec<NativePromptFile>,
    references: Vec<crate::NativeAttachmentReference>,
}

#[derive(Clone)]
struct NativePromptFile {
    reference: crate::NativeAttachmentReference,
    path: PathBuf,
}

impl std::fmt::Debug for NativePromptFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativePromptFile")
            .field("reference", &self.reference)
            .finish_non_exhaustive()
    }
}

impl NativePromptAttachments {
    pub(super) fn from_resolved(
        attachments: Vec<crate::native_attachments::ResolvedNativeAttachment>,
    ) -> Result<Self, String> {
        if attachments.len() > crate::MAX_NATIVE_PROMPT_ATTACHMENTS {
            return Err("Attach at most 16 files to a prompt".to_owned());
        }
        let mut result = Self::default();
        for attachment in attachments {
            let reference = attachment.reference;
            match attachment.image_png {
                Some(png) => result.images.push(NativePromptImage::from_host_png(
                    NativeImageReference {
                        id: reference.id.clone(),
                        pixel_width: reference.pixel_width.ok_or("Native image has no width")?,
                        pixel_height: reference.pixel_height.ok_or("Native image has no height")?,
                    },
                    png,
                )?),
                None => result.files.push(NativePromptFile {
                    reference: reference.clone(),
                    path: attachment.path,
                }),
            }
            result.references.push(reference);
        }
        Ok(result)
    }
}

impl std::fmt::Debug for NativePromptImage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativePromptImage")
            .field("reference", &self.reference)
            .field("png_bytes", &self.png.len())
            .finish()
    }
}

impl NativePromptImage {
    /// The invocation owner resolves this ID in its exact task/store before constructing it.
    /// # Errors
    /// Rejects invalid references, corrupt PNGs, dimensions, animation, or bounded decode failures.
    pub fn from_host_png(reference: NativeImageReference, bytes: Vec<u8>) -> Result<Self, String> {
        if reference.id.is_empty()
            || reference.id.len() > 256
            || !reference
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
            || bytes.is_empty()
            || bytes.len() > MAX_NATIVE_PROMPT_IMAGE_BYTES
            || reference.pixel_width == 0
            || reference.pixel_height == 0
            || reference.pixel_width > 8192
            || reference.pixel_height > 8192
            || u64::from(reference.pixel_width).saturating_mul(u64::from(reference.pixel_height))
                > 16 * 1024 * 1024
        {
            return Err("Native image reference or PNG exceeds its bounded limits".to_owned());
        }
        let decoder = png::Decoder::new_with_limits(
            Cursor::new(&bytes),
            png::Limits {
                bytes: MAX_DECODE_BYTES,
            },
        );
        let mut reader = decoder
            .read_info()
            .map_err(|_| "Native image is not a supported PNG")?;
        if reader.info().width != reference.pixel_width
            || reader.info().height != reference.pixel_height
            || reader.info().animation_control.is_some()
        {
            return Err(
                "Native PNG dimensions or animation do not match the captured image".to_owned(),
            );
        }
        let size = reader
            .output_buffer_size()
            .filter(|size| *size <= MAX_DECODE_BYTES)
            .ok_or("Native PNG exceeds the decode budget")?;
        let mut pixels = vec![0; size];
        reader
            .next_frame(&mut pixels)
            .map_err(|_| "Native PNG pixels could not be decoded")?;
        reader
            .finish()
            .map_err(|_| "Native PNG is incomplete or corrupt")?;
        drop(reader);
        Ok(Self {
            reference,
            png: bytes,
        })
    }

    #[must_use]
    pub const fn reference(&self) -> &NativeImageReference {
        &self.reference
    }

    fn base64(&self) -> String {
        STANDARD.encode(&self.png)
    }
}

/// One bounded user submission. Image bytes are transport-only; references are durable.
#[derive(Clone, Debug)]
pub struct NativePrompt {
    text: String,
    authored_bytes: usize,
    images: Vec<NativePromptImage>,
    files: Vec<NativePromptFile>,
    attachment_references: Vec<crate::NativeAttachmentReference>,
    citations: Vec<crate::NativeResponseCitation>,
    review_comments: Vec<crate::NativeReviewComment>,
    skills: Vec<crate::NativeCompletionOption>,
    applications: Vec<crate::NativeApplicationMention>,
    provider_mentions: Vec<crate::NativeProviderMention>,
    history_context: Option<String>,
    /// Other conversations the message mentions, quoted for the provider only.
    mentioned_conversations: Option<String>,
}

/// Private catalog payload. Restore attachment bytes through their owning store;
/// never deserialize provider file paths or admitted images from a command.
#[derive(Clone, Deserialize, Serialize)]
pub struct SavedNativePrompt {
    pub(crate) text: String,
    authored_bytes: usize,
    images: Vec<(NativeImageReference, Vec<u8>)>,
    pub(crate) attachments: Vec<crate::NativeAttachmentReference>,
    citations: Vec<crate::NativeResponseCitation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) review_comments: Vec<crate::NativeReviewComment>,
    applications: Vec<crate::NativeApplicationMention>,
    #[serde(default)]
    provider_mentions: Vec<crate::NativeProviderMention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mentioned_conversations: Option<String>,
}

impl SavedNativePrompt {
    pub(crate) fn preview_text(&self) -> String {
        crate::native_citations::queued_prompt_preview(
            &self.text,
            &self.citations,
            &self.attachments,
            &self.review_comments,
        )
    }

    pub(crate) fn capture(prompt: &NativePrompt) -> Self {
        Self {
            text: prompt.text.clone(),
            authored_bytes: prompt.authored_bytes,
            images: prompt
                .images
                .iter()
                .filter(|image| {
                    !prompt
                        .attachment_references
                        .iter()
                        .any(|attachment| attachment.id == image.reference.id)
                })
                .map(|image| (image.reference.clone(), image.png.clone()))
                .collect(),
            attachments: prompt.attachment_references.clone(),
            citations: prompt.citations.clone(),
            review_comments: prompt.review_comments.clone(),
            applications: prompt.applications.clone(),
            provider_mentions: prompt.provider_mentions.clone(),
            mentioned_conversations: prompt.mentioned_conversations.clone(),
        }
    }

    pub(crate) fn restore(
        &self,
        attachments: NativePromptAttachments,
    ) -> Result<NativePrompt, String> {
        if self.images.len() > MAX_NATIVE_PROMPT_IMAGES {
            return Err("Queued message has too many images".into());
        }
        let images = self
            .images
            .iter()
            .map(|(reference, png)| {
                NativePromptImage::from_host_png(reference.clone(), png.clone())
            })
            .collect::<Result<Vec<_>, _>>()?;
        NativePrompt::new_with_review_comments(
            self.text.clone(),
            images,
            attachments,
            self.citations.clone(),
            self.review_comments.clone(),
        )?
        .with_attachment_ranges(
            self.attachments
                .iter()
                .map(|attachment| (attachment.id.clone(), attachment.prompt_ranges.clone()))
                .collect(),
        )?
        .with_applications(self.applications.clone())?
        .with_provider_mentions(self.provider_mentions.clone())?
        .with_authored_prefix(self.authored_bytes)?
        .with_mentioned_conversations(self.mentioned_conversations.clone())
    }
}

impl NativePrompt {
    /// # Errors
    /// Rejects an empty submission or the text/count/aggregate-image byte budgets.
    pub fn new(text: String, images: Vec<NativePromptImage>) -> Result<Self, String> {
        Self::new_with_context(text, images, NativePromptAttachments::default(), Vec::new())
    }

    /// Construct an image/file/citation submission, allowing an empty message only when it
    /// contains admitted media.
    /// # Errors
    /// Rejects empty submissions or exceeded text, count, image, or citation budgets.
    pub fn new_with_context(
        text: String,
        images: Vec<NativePromptImage>,
        attachments: NativePromptAttachments,
        citations: Vec<crate::NativeResponseCitation>,
    ) -> Result<Self, String> {
        Self::new_with_review_comments(text, images, attachments, citations, Vec::new())
    }

    /// Construct a submission with structured review comments attached to inline chips.
    /// # Errors
    /// Rejects invalid comments or references and the same aggregate prompt budgets.
    pub fn new_with_review_comments(
        text: String,
        images: Vec<NativePromptImage>,
        attachments: NativePromptAttachments,
        citations: Vec<crate::NativeResponseCitation>,
        review_comments: Vec<crate::NativeReviewComment>,
    ) -> Result<Self, String> {
        crate::native_review_comments::validate(&text, &review_comments)?;
        if text.len() > MAX_NATIVE_PROMPT_TEXT_BYTES
            || citations.len() > MAX_NATIVE_PROMPT_CITATIONS
        {
            return Err("Native prompts exceed the text or citation limit".to_owned());
        }
        let mut end = 0;
        let mut ranges = citations
            .iter()
            .filter_map(|citation| citation.prompt_range.as_ref())
            .collect::<Vec<_>>();
        ranges.sort_by_key(|range| range.start);
        for range in ranges {
            if range.start < end || text.get(range.clone()) != Some("[quote]") {
                return Err("Response quote no longer matches its inline prompt token".to_owned());
            }
            end = range.end;
        }
        let prompt = Self {
            authored_bytes: text.len(),
            text,
            images,
            files: Vec::new(),
            attachment_references: Vec::new(),
            citations,
            review_comments,
            skills: Vec::new(),
            applications: Vec::new(),
            provider_mentions: Vec::new(),
            history_context: None,
            mentioned_conversations: None,
        }
        .with_attachments(attachments)?;
        if prompt.text.is_empty()
            && prompt.images.is_empty()
            && prompt.files.is_empty()
            && prompt.citations.is_empty()
            && prompt.review_comments.is_empty()
        {
            return Err("Native prompts require text or an attachment".to_owned());
        }
        prompt.validate_image_budget()?;
        prompt.provider_message()?;
        Ok(prompt)
    }

    /// Preserve the existing nonempty, bounded text-only prompt contract.
    /// # Errors
    /// Rejects empty or oversized text.
    pub fn text(text: &str) -> Result<Self, String> {
        Self::new(text.to_owned(), Vec::new())
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn image_references(&self) -> Vec<NativeImageReference> {
        self.images
            .iter()
            .map(|image| image.reference.clone())
            .collect()
    }

    #[must_use]
    pub fn attachment_references(&self) -> Vec<crate::NativeAttachmentReference> {
        self.attachment_references.clone()
    }

    #[must_use]
    pub fn citations(&self) -> &[crate::NativeResponseCitation] {
        &self.citations
    }

    #[must_use]
    pub fn review_comments(&self) -> &[crate::NativeReviewComment] {
        &self.review_comments
    }

    /// Attach references admitted by this session's native attachment store.
    /// # Errors
    /// Rejects attachment count, image count, aggregate image bytes, or duplicate IDs.
    fn with_attachments(mut self, attachments: NativePromptAttachments) -> Result<Self, String> {
        let mut ids = std::collections::BTreeSet::new();
        if self
            .attachment_references
            .len()
            .saturating_add(attachments.references.len())
            > crate::MAX_NATIVE_PROMPT_ATTACHMENTS
            || self.images.len().saturating_add(attachments.images.len()) > MAX_NATIVE_PROMPT_IMAGES
            || self
                .attachment_references
                .iter()
                .chain(&attachments.references)
                .any(|reference| !ids.insert(reference.id.as_str()))
        {
            return Err("Native prompt attachments exceed their count or image limits".to_owned());
        }
        self.images.extend(attachments.images);
        self.files.extend(attachments.files);
        self.attachment_references.extend(attachments.references);
        self.validate_image_budget()?;
        Ok(self)
    }

    #[must_use]
    pub const fn image_count(&self) -> usize {
        self.images.len()
    }

    /// Attach explicit application mentions without expanding account or OS permissions.
    /// # Errors
    /// Rejects malformed windows or mentions that no longer match the authored prompt.
    pub fn with_applications(
        mut self,
        applications: Vec<crate::NativeApplicationMention>,
    ) -> Result<Self, String> {
        if applications.len() > 8 {
            return Err("Mention at most eight application windows".into());
        }
        for mention in &applications {
            mention.validate()?;
            if !self
                .text
                .get(mention.prompt_range.clone())
                .is_some_and(|text| text.starts_with('@'))
            {
                return Err("Application mention no longer matches the prompt".into());
            }
        }
        self.applications = applications;
        self.provider_message()?;
        Ok(self)
    }
    #[must_use]
    pub fn applications(&self) -> &[crate::NativeApplicationMention] {
        &self.applications
    }

    /// Retain exact editor tokens for presentation without changing the provider input.
    /// # Errors
    /// Rejects unknown references, overlapping spans or tokens that differ from the admitted file.
    pub fn with_attachment_ranges(
        mut self,
        ranges: std::collections::BTreeMap<String, Vec<std::ops::Range<usize>>>,
    ) -> Result<Self, String> {
        if ranges.len() > self.attachment_references.len() {
            return Err("Unknown inline attachment".into());
        }
        let mut spans = self
            .citations
            .iter()
            .filter_map(|citation| citation.prompt_range.clone())
            .chain(
                self.review_comments
                    .iter()
                    .map(|comment| comment.prompt_range.clone()),
            )
            .collect::<Vec<_>>();
        for (id, ranges) in ranges {
            let reference = self
                .attachment_references
                .iter_mut()
                .find(|reference| reference.id == id)
                .ok_or("Unknown inline attachment")?;
            if ranges.len() > 16
                || ranges.iter().any(|range| {
                    self.text.get(range.clone()) != Some(format!("[{}]", reference.name).as_str())
                })
            {
                return Err("Attachment no longer matches its inline prompt token".into());
            }
            spans.extend(ranges.iter().cloned());
            reference.prompt_ranges = ranges;
        }
        spans.sort_by_key(|range| range.start);
        if spans
            .windows(2)
            .any(|pair| matches!(pair, [left, right] if left.end > right.start))
        {
            return Err("Inline prompt references overlap".into());
        }
        Ok(self)
    }

    /// Attach provider references without enabling or installing anything.
    /// # Errors
    /// Rejects invalid chips, paths, overlapping ranges, or input outside authored text.
    pub fn with_provider_mentions(
        mut self,
        mentions: Vec<crate::NativeProviderMention>,
    ) -> Result<Self, String> {
        if mentions.len() > 32 {
            return Err("Mention at most 32 apps and plugins".into());
        }
        let mut ranges: Vec<_> = self
            .attachment_references
            .iter()
            .flat_map(|attachment| attachment.prompt_ranges.iter().cloned())
            .chain(
                self.citations
                    .iter()
                    .filter_map(|citation| citation.prompt_range.clone()),
            )
            .chain(
                self.applications
                    .iter()
                    .map(|mention| mention.prompt_range.clone()),
            )
            .collect();
        for mention in &mentions {
            let prefix = match mention.option.kind {
                crate::NativeCompletionKind::App => "app://",
                crate::NativeCompletionKind::Plugin => "plugin://",
                _ => return Err("Invalid provider mention kind".into()),
            };
            if !mention
                .option
                .path
                .as_deref()
                .is_some_and(|path| path.starts_with(prefix) && path.len() > prefix.len())
                || mention.prompt_range.end > self.authored_bytes
                || self.text.get(mention.prompt_range.clone())
                    != Some(format!("@{}", mention.option.display_name).as_str())
            {
                return Err("Provider mention no longer matches authored input".into());
            }
            ranges.push(mention.prompt_range.clone());
        }
        ranges.sort_by_key(|range| range.start);
        if ranges
            .windows(2)
            .any(|pair| matches!(pair, [left, right] if left.end > right.start))
        {
            return Err("Provider mentions overlap".into());
        }
        self.provider_mentions = mentions;
        Ok(self)
    }

    pub(crate) const fn has_provider_mentions(&self) -> bool {
        !self.provider_mentions.is_empty()
    }

    pub(crate) fn validate_provider_mentions(
        &self,
        catalog: &crate::NativeCompletionCatalog,
    ) -> Result<(), String> {
        for mention in &self.provider_mentions {
            if catalog.unavailable.contains(&mention.option.kind) {
                return Err("Couldn’t verify an attached app or plugin. Refresh the Add menu and try again.".into());
            }
            if !catalog.options.iter().any(|option| {
                option.kind == mention.option.kind
                    && option.name == mention.option.name
                    && option.path == mention.option.path
            }) {
                return Err("An attached app or plugin is no longer available for this account. Remove it or refresh the Add menu.".into());
            }
        }
        Ok(())
    }

    /// The UI separates authored text from appended reference material.
    /// # Errors
    /// Rejects a non-source byte boundary outside this prompt.
    pub fn with_authored_prefix(mut self, bytes: usize) -> Result<Self, String> {
        if self.text.get(..bytes).is_none()
            || self
                .provider_mentions
                .iter()
                .any(|mention| mention.prompt_range.end > bytes)
        {
            return Err("Invalid authored prompt range".into());
        }
        self.authored_bytes = bytes;
        Ok(self)
    }
    fn authored_text(&self) -> &str {
        self.text.get(..self.authored_bytes).unwrap_or_default()
    }

    pub(crate) fn compact_instructions(&self) -> Result<Option<String>, String> {
        self.command_arguments("/compact")
    }

    /// A command of Codex's terminal app the message starts with, which its app server can't run
    /// and its model would only read as words.
    pub(crate) fn codex_terminal_command(&self) -> Option<&str> {
        const TERMINAL: [&str; 19] = [
            "approvals",
            "archive",
            "clean",
            "clear",
            "delete",
            "exit",
            "fast",
            "feedback",
            "fork",
            "init",
            "logout",
            "mcp",
            "mention",
            "permissions",
            "ps",
            "quit",
            "resume",
            "skills",
            "status",
        ];
        let command = self.authored_text().trim().strip_prefix('/')?;
        let name = command.split_whitespace().next()?;
        TERMINAL.contains(&name).then_some(name)
    }

    /// The words after a provider command such as `/review`, when the message is that command
    /// alone. A command can't carry references.
    pub(crate) fn command_arguments(&self, command: &str) -> Result<Option<String>, String> {
        let text = self.authored_text().trim();
        let Some(rest) = text.strip_prefix(command) else {
            return Ok(None);
        };
        if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
            return Ok(None);
        }
        if self.authored_bytes != self.text.len()
            || !self.images.is_empty()
            || !self.files.is_empty()
            || !self.citations.is_empty()
            || !self.applications.is_empty()
            || !self.provider_mentions.is_empty()
        {
            return Err(format!(
                "Send references in a message of their own, not with {command}"
            ));
        }
        Ok(Some(rest.trim().into()))
    }

    pub(crate) fn needs_skill_catalog(&self) -> bool {
        !self.provider_mentions.is_empty()
            || self
                .authored_text()
                .split_whitespace()
                .any(|token| token.starts_with('$'))
    }

    pub(crate) fn with_advertised_skills(&self, catalog: &crate::NativeCompletionCatalog) -> Self {
        let mut prompt = self.clone();
        for token in self.authored_text().split_whitespace() {
            let Some(name) = token.strip_prefix('$') else {
                continue;
            };
            if prompt.skills.iter().any(|skill| skill.name == name) {
                continue;
            }
            if let Some(skill) = catalog.options.iter().find(|option| {
                option.kind == crate::NativeCompletionKind::Skill && option.name == name
            }) {
                prompt.skills.push(skill.clone());
            }
        }
        prompt
    }

    /// Where advertised skill chips sit in the text, in order. Only authored text is scanned;
    /// attached files and quoted agent responses are references.
    fn skill_mentions(&self) -> Vec<std::ops::Range<usize>> {
        let mut ranges = Vec::new();
        let mut offset = 0_usize;
        for token in self.authored_text().split_inclusive(char::is_whitespace) {
            let authored = token.trim_end();
            if self
                .skills
                .iter()
                .any(|skill| authored.strip_prefix('$') == Some(&skill.name))
            {
                ranges.push(offset..offset.saturating_add(authored.len()));
            }
            offset = offset.saturating_add(token.len());
        }
        ranges
    }

    // Hoist advertised skill chips into Pi's leading native skill command.
    fn pi_skill_message(&self) -> String {
        if self.skills.is_empty() {
            return self.text.clone();
        }
        let mut body = self.text.clone();
        for range in self.skill_mentions().into_iter().rev() {
            body.replace_range(range, "");
        }
        let body = body.trim();
        let prefix = self
            .skills
            .iter()
            .map(|skill| format!("/skill:{}", skill.name))
            .collect::<Vec<_>>()
            .join(" ");
        if body.is_empty() {
            prefix
        } else {
            format!("{prefix} {body}")
        }
    }

    pub(crate) fn codex_input(&self) -> Result<Value, String> {
        let mut input = Vec::new();
        let message = self.provider_message()?;
        if !message.is_empty() {
            input.push(json!({"type":"text","text":message}));
        }
        input.extend(self.skills.iter().filter_map(|skill| {
            skill
                .path
                .as_ref()
                .map(|path| json!({"type":"skill","name":skill.name,"path":path}))
        }));
        input.extend(self.provider_mentions.iter().map(|mention| json!({"type":"mention","name":mention.option.display_name,"path":mention.option.path})) );
        input.extend(self.images.iter().map(|image| json!({"type":"image","url":format!("data:image/png;base64,{}", image.base64())})));
        Ok(input.into())
    }

    pub(crate) fn pi_parameters(&self) -> Result<Value, String> {
        let mut params = serde_json::Map::from_iter([(
            "message".to_owned(),
            self.provider_message_with_text(self.pi_skill_message())?
                .into(),
        )]);
        if !self.images.is_empty() {
            params.insert("images".to_owned(), self
                .images
                .iter()
                .map(|image| json!({"type":"image","mimeType":"image/png","data":image.base64()}))
                .collect::<Vec<_>>()
                .into());
        }
        Ok(Value::Object(params))
    }

    /// Claude runs one skill per message, from a `/name` that opens its last text block; the
    /// text after the name is the skill's arguments. The last skill chip becomes that command.
    /// Earlier chips read `/name` in the leading text, so the model can still run them.
    fn claude_skill_command(&self) -> Option<(String, String)> {
        let mut mentions = self.skill_mentions();
        let last = mentions.pop()?;
        let mut leading = self.text.get(..last.start)?.to_owned();
        for mention in mentions {
            leading.replace_range(mention.start..mention.start.saturating_add(1), "/");
        }
        let command = format!("/{}", self.text.get(last.start.saturating_add(1)..)?);
        Some((leading.trim_end().to_owned(), command.trim_end().to_owned()))
    }

    pub(crate) fn claude_content(&self) -> Result<Value, String> {
        let images = self.images.iter().map(|image| json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":image.base64()}}));
        let Some((leading, command)) = self.claude_skill_command() else {
            let message = self.provider_message()?;
            if self.images.is_empty() {
                return Ok(message.into());
            }
            let mut content = Vec::new();
            if !message.is_empty() {
                content.push(json!({"type":"text","text":message}));
            }
            content.extend(images);
            return Ok(content.into());
        };
        let message = self.provider_message_with_text(command)?;
        if leading.is_empty() && self.images.is_empty() {
            return Ok(message.into());
        }
        let mut content = Vec::new();
        if !leading.is_empty() {
            content.push(json!({"type":"text","text":leading}));
        }
        content.extend(images);
        content.push(json!({"type":"text","text":message}));
        Ok(content.into())
    }

    fn validate_image_budget(&self) -> Result<(), String> {
        if self.images.len() > MAX_NATIVE_PROMPT_IMAGES
            || self
                .images
                .iter()
                .map(|image| image.png.len())
                .sum::<usize>()
                > MAX_NATIVE_PROMPT_IMAGE_BYTES
        {
            return Err(format!(
                "Native prompts support up to {MAX_NATIVE_PROMPT_IMAGES} PNGs within 8 MiB total"
            ));
        }
        Ok(())
    }

    pub(crate) const fn requires_large_envelope(&self) -> bool {
        self.image_count() > 0 || self.history_context.is_some()
    }

    pub(crate) fn history_echo_text(
        &self,
        provider: crate::AgentKind,
    ) -> Result<Option<String>, String> {
        if self.history_context.is_none() {
            return Ok(None);
        }
        if provider == crate::AgentKind::Pi {
            self.provider_message_with_text(self.pi_skill_message())
                .map(Some)
        } else {
            self.provider_message().map(Some)
        }
    }

    /// Quotes other conversations the message mentions, for the provider; the transcript keeps
    /// the message as written.
    /// # Errors
    /// Rejects a quote that doesn't fit the provider message.
    pub fn with_mentioned_conversations(mut self, quoted: Option<String>) -> Result<Self, String> {
        self.mentioned_conversations = quoted.filter(|quoted| !quoted.is_empty());
        self.provider_message()?;
        Ok(self)
    }

    pub(crate) fn with_history(&self, history: String) -> Result<Self, String> {
        if history.len() > 2 * 1024 * 1024 {
            return Err("The conversation exceeds the side-chat context limit".into());
        }
        let mut prompt = self.clone();
        prompt.history_context = Some(history);
        prompt.provider_message()?;
        Ok(prompt)
    }

    fn provider_message(&self) -> Result<String, String> {
        self.provider_message_with_text(self.text.clone())
    }

    fn provider_message_with_text(&self, mut message: String) -> Result<String, String> {
        if !self.applications.is_empty() {
            let references = self
                .applications
                .iter()
                .map(|mention| {
                    let application = crate::NativeApplication::from(&mention.target);
                    serde_json::json!({"application": application.bundle_id, "name": application.name, "window": mention.target.title})
                })
                .collect::<Vec<_>>();
            message.push_str("\n\n<application_mentions>\nThe user allowed these apps for this conversation. Pass the bundle id as application to computer_elements, computer_snapshot or computer_input. computer_elements observes controls without an image; press with allow_click false to require an accessibility action; set_value replaces a writable control value without focus or key events and does not submit it. OS permissions still apply.\n");
            message
                .push_str(&serde_json::to_string(&references).map_err(|error| error.to_string())?);
            message.push_str("\n</application_mentions>");
        }
        crate::native_citations::append_citation_context(&mut message, &self.citations)?;
        crate::native_review_comments::append_context(&mut message, &self.review_comments)?;
        if let Some(quoted) = &self.mentioned_conversations {
            message.push_str("\n\n<mentioned_conversations>\n");
            message.push_str(quoted);
            message.push_str("\n</mentioned_conversations>");
        }
        for file in &self.files {
            if !message.is_empty() {
                message.push_str("\n\n");
            }
            let name =
                serde_json::to_string(&file.reference.name).map_err(|error| error.to_string())?;
            write!(
                message,
                "[Attached file {name} is saved at: {}]",
                file.path.display()
            )
            .map_err(|error| error.to_string())?;
        }
        let limit = self.history_context.as_ref().map_or(MAX_NATIVE_PROMPT_TEXT_BYTES, |history| {
            message.push_str("\n\n<prior_conversation>\n");
            message.push_str(history);
            message.push_str("\n</prior_conversation>\nUse this completed parent conversation as context for this independent side chat.");
            MAX_NATIVE_PROMPT_TEXT_BYTES + 2 * 1024 * 1024
        });
        if message.len() > limit {
            return Err("Native prompt context exceeds the provider message limit".to_owned());
        }
        Ok(message)
    }
}

// Correlated capture results, exact image echoes and provider history use this bounded validator.
// Pixels never enter durable snapshots.
pub fn tool_image_bytes(contents: &[&Value]) -> Option<usize> {
    let mut encoded_bytes = 0_usize;
    let mut raw_bytes = 0_usize;
    let mut image_count = 0_usize;
    for content in contents {
        for block in content.as_array()? {
            if block.get("type")? == "text" {
                if block.get("text")?.as_str()?.len() > MAX_NATIVE_PROMPT_TEXT_BYTES {
                    return None;
                }
                continue;
            }
            if block.get("type")? != "image" {
                return None;
            }
            let data = if block
                .get("mimeType")
                .is_some_and(|mime| mime == "image/png")
            {
                block.get("data")?.as_str()?
            } else {
                block
                    .get("url")?
                    .as_str()?
                    .strip_prefix("data:image/png;base64,")?
            };
            encoded_bytes = encoded_bytes.checked_add(data.len())?;
            image_count = image_count.saturating_add(1);
            if encoded_bytes > MAX_NATIVE_PROMPT_IMAGE_BYTES.div_ceil(3).saturating_mul(4)
                || image_count > MAX_NATIVE_PROMPT_IMAGES
            {
                return None;
            }
            let bytes = STANDARD.decode(data).ok()?;
            raw_bytes = raw_bytes.checked_add(bytes.len())?;
            if raw_bytes > MAX_NATIVE_PROMPT_IMAGE_BYTES {
                return None;
            }
            let reader = png::Decoder::new_with_limits(
                Cursor::new(&bytes),
                png::Limits {
                    bytes: MAX_DECODE_BYTES,
                },
            )
            .read_info()
            .ok()?;
            let reference = NativeImageReference {
                id: "computer_snapshot".to_owned(),
                pixel_width: reader.info().width,
                pixel_height: reader.info().height,
            };
            drop(reader);
            NativePromptImage::from_host_png(reference, bytes).ok()?;
        }
    }
    (image_count > 0).then_some(encoded_bytes)
}
