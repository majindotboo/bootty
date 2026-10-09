//! Durable private files on the captured host, using the existing verified transfer stream.
use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use crate::{
    CommandRunner,
    jobs::transfer::{Receipt, WireRequest, copy_bytes, line, source, unchanged, write_line},
    remote::RemoteHost,
};

pub const MAX_PRIVATE_FILE_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateFileDirectory {
    pub root: String,
    /// A digest of the provider's durable conversation identity, never a local pane number.
    pub owner: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct PrivateFileReceipt {
    pub path: PathBuf,
    #[serde(flatten)]
    pub receipt: Receipt,
}

impl PrivateFileDirectory {
    fn validate(&self) -> Result<()> {
        ensure!(
            Path::new(&self.root).is_absolute()
                && self.root.len() <= 8192
                && !self.root.contains('\0'),
            "Private file account must be absolute"
        );
        ensure!(
            self.owner.len() == 64
                && self
                    .owner
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "Invalid private file owner"
        );
        Ok(())
    }

    pub(crate) fn prepare(&self) -> Result<PathBuf> {
        self.validate()?;
        ensure!(
            cfg!(unix),
            "Private remote attachment storage requires a Unix host"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            // Providers may create their account store on first launch. Prepare only
            // missing directories; never change an existing account's permissions.
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.root)?;
        }
        let mut path =
            fs::canonicalize(&self.root).context("Resolve captured account directory")?;
        ensure!(path.is_dir(), "Captured account is not a directory");
        let identity = match bootty_config::ApplicationIdentity::for_process() {
            bootty_config::ApplicationIdentity::Production => "bootty",
            bootty_config::ApplicationIdentity::Development => "bootty-dev",
        };
        for component in ["bootty-attachments", identity, self.owner.as_str()] {
            path.push(component);
            let builder = fs::DirBuilder::new();
            #[cfg(unix)]
            let builder = {
                use std::os::unix::fs::DirBuilderExt as _;
                let mut builder = builder;
                builder.mode(0o700);
                builder
            };
            match builder.create(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                metadata.file_type().is_dir(),
                "Private file storage is not a directory"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                ensure!(
                    metadata.mode().trailing_zeros() >= 6
                        && metadata.uid() == rustix::process::geteuid().as_raw(),
                    "Private file storage is not owned and private"
                );
            }
        }
        Ok(path)
    }

    /// Call on a worker with the owning host's bounded, cancellable runner.
    /// # Errors
    /// Returns invalid identities, permissions, or remote preparation errors.
    pub fn prepare_remote(
        &self,
        remote: &RemoteHost,
        runner: &impl CommandRunner,
    ) -> Result<PathBuf> {
        self.validate()?;
        let request = WireRequest::PreparePrivate {
            directory: self.clone(),
        };
        let output = request_remote(remote, runner, &request, Vec::new())?;
        line(&mut Cursor::new(output))
    }

    /// Repeated uploads accept only the exact previously received bytes; existing files are never replaced.
    /// Remote copies remain with the provider's durable history, including across transport loss.
    /// # Errors
    /// Returns source changes, checksum conflicts, permissions, or cancelled transport errors.
    pub fn upload_remote(
        &self,
        remote: &RemoteHost,
        runner: &impl CommandRunner,
        name: &str,
        local_path: &Path,
    ) -> Result<PathBuf> {
        self.validate()?;
        validate_file_name(name)?;
        let (mut input, metadata) = source(local_path.to_str().context("Attachment path")?)?;
        ensure!(
            metadata.len() <= MAX_PRIVATE_FILE_BYTES,
            "Private upload exceeds 50 MiB"
        );
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len())?.saturating_add(128));
        let sent = copy_bytes(&mut input, &mut bytes, metadata.len(), |_| Ok(()))?;
        unchanged(&input, &metadata)?;
        write_line(&sent, &mut bytes)?;
        let request = WireRequest::UploadPrivate {
            directory: self.clone(),
            name: name.into(),
            bytes: metadata.len(),
        };
        let output = request_remote(remote, runner, &request, bytes)?;
        let received: PrivateFileReceipt = line(&mut Cursor::new(output))?;
        ensure!(
            received.receipt == sent,
            "Remote destination checksum does not match"
        );
        ensure!(
            received.path.is_absolute()
                && received.path.file_name().is_some_and(|file| file == name),
            "Invalid remote attachment path"
        );
        Ok(received.path)
    }
}

fn request_remote(
    remote: &RemoteHost,
    runner: &impl CommandRunner,
    request: &WireRequest,
    bytes: Vec<u8>,
) -> Result<Vec<u8>> {
    remote.ensure_daemon_with(runner)?;
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(request)?);
    let (program, arguments) =
        remote.proxy_command(crate::REMOTE_DAEMON_PROGRAM, &["transfer".into(), payload])?;
    let result = runner.run_with_input(&program, &arguments, bytes)?;
    crate::require_success(&program, &arguments, result).map(String::into_bytes)
}

pub(crate) fn validate_file_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name != "."
            && name != ".."
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-.".contains(&byte)),
        "Invalid private file name"
    );
    Ok(())
}

pub(crate) fn verify_existing(path: &Path, expected: &Receipt) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_file() && metadata.len() == expected.bytes,
        "Remote attachment already exists with different bytes"
    );
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        ensure!(
            metadata.mode().trailing_zeros() >= 6
                && metadata.uid() == rustix::process::geteuid().as_raw(),
            "Remote attachment is not owned and private"
        );
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed());
    }
    let mut file = options.open(path)?;
    let before = file.metadata()?;
    let actual = copy_bytes(&mut file, std::io::sink(), expected.bytes, |_| Ok(()))?;
    unchanged(&file, &before)?;
    ensure!(
        &actual == expected,
        "Remote attachment already exists with different bytes"
    );
    Ok(())
}
