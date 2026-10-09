//! Project file mentions read from their captured host before local attachment admission.
use bootty_config::config::RemoteConfig;
use bootty_control::{
    BoundAppCommandSender, CommandCancellation, CommandInvocation, CommandOutcome,
};
use bootty_host::{file_reader::FileReader, files::FileResponse, remote::RemoteHost};
use std::{
    io,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub enum AttachmentTemporary {
    Clipboard(Arc<tempfile::NamedTempFile>),
    Project(Arc<tempfile::TempDir>),
}

#[derive(Clone)]
pub struct FileCompletionSource {
    pub invocation: CommandInvocation,
    pub remote: Option<RemoteConfig>,
}

pub struct StagedAttachment {
    pub path: PathBuf,
    pub temporary: AttachmentTemporary,
}

impl FileCompletionSource {
    /// Read on an IO worker. The command resolves the file on the exact selected binding.
    /// # Errors
    /// Rejects unavailable hosts, changed sources, unsafe names and files over 50 MB.
    pub fn stage(&self, sender: &BoundAppCommandSender) -> Result<StagedAttachment, String> {
        let cancellation = CommandCancellation::new();
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(60))
            .ok_or("Attachment deadline overflow")?;
        let receiver = sender
            .submit(self.invocation.clone(), deadline, cancellation.clone())
            .map_err(|_| "Attachment host is unavailable")?;
        let response = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let outcome = response.map_err(|_| {
            _ = cancellation.cancel();
            "Attachment host disconnected or timed out".to_owned()
        })?;
        let CommandOutcome::Success { value, .. } = outcome else {
            return Err(crate::commands::command_outcome_message(&outcome)
                .unwrap_or_else(|| "Could not read the attachment source".into()));
        };
        let FileResponse::Source(descriptor) =
            serde_json::from_value(value).map_err(|error| error.to_string())?
        else {
            return Err("Attachment host returned no file source".into());
        };
        if descriptor.len > bootty_agents::MAX_NATIVE_ATTACHMENT_FILE_BYTES {
            return Err("File exceeds 50 MB".into());
        }
        if descriptor.name.is_empty()
            || descriptor.name == "."
            || descriptor.name == ".."
            || descriptor.name.contains(['/', '\\', '\0'])
        {
            return Err("Attachment host returned an invalid file name".into());
        }
        let remote = self.remote.clone().map(RemoteHost::new);
        let mut reader =
            FileReader::open(&descriptor, remote.as_ref()).map_err(|error| error.to_string())?;
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = directory.path().join(&descriptor.name);
        let mut file = std::fs::File::create(&path).map_err(|error| error.to_string())?;
        let bytes = io::copy(&mut reader, &mut file).map_err(|error| error.to_string())?;
        reader.verify_source().map_err(|error| error.to_string())?;
        if bytes != descriptor.len {
            return Err("Attachment source changed while reading".into());
        }
        Ok(StagedAttachment {
            path,
            temporary: AttachmentTemporary::Project(Arc::new(directory)),
        })
    }
}
