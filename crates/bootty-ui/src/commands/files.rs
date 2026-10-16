use bootty_control::{CommandDescriptor, CompactSchema, ResourceKind, ValueType};
use bootty_host::files::FileRequest;

command_actions! {
    FileAction {
        Browse => ("files.browse", "Browse Files", ["path"], Write),
        Open => ("files.open", "Open Document", ["path", "line", "column"], Write),
        Complete => ("files.complete", "Find Files", ["base", "query"], Read),
        List => ("files.list", "List Directory", ["path", "offset"], Read),
        Read => ("files.read", "Read Document", ["path"], Read),
        Source => ("files.source", "Open File Source", ["path", "root"], Read),
        Save => ("files.save", "Save Document Revision", ["path", "digest", "content_base64"], Write),
        Format => ("files.format", "Format Document", ["path", "content_base64"], Read),
        Reveal => ("files.reveal", "Reveal in File Manager", ["path"], Read),
        OpenIn => ("files.open_in", "Open in Editor", ["path", "editor"], Read),
    }
}

/// What this platform calls showing a file in its file manager.
#[must_use]
pub const fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else if cfg!(target_os = "windows") {
        "Show in Explorer"
    } else {
        "Show in File Manager"
    }
}

impl FileAction {
    #[must_use]
    pub fn descriptor(self) -> CommandDescriptor {
        let (id, title, names, mutation) = self.metadata();
        let arguments = names
            .iter()
            .copied()
            .map(|name| {
                let numeric = matches!(name, "line" | "column" | "offset");
                let mut argument = super::argument(
                    name,
                    if numeric {
                        ValueType::Integer
                    } else {
                        ValueType::String
                    },
                );
                if numeric {
                    argument.required = false;
                    argument.minimum = Some(i64::from(name != "offset"));
                    argument.maximum = Some(i64::from(u32::MAX));
                }
                argument
            })
            .collect();
        CommandDescriptor {
            id: id.to_owned(),
            title: if self == Self::Reveal {
                reveal_label().to_owned()
            } else {
                title.to_owned()
            },
            description: if self == Self::OpenIn {
                "Open an absolute path on this computer in an installed editor: vscode, cursor, zed, windsurf, sublime, intellij or xcode. A remote host's files are unsupported.".to_owned()
            } else {
                format!(
                    "{title} on the target binding's host. Paths must be absolute; UTF-8 text documents are limited to 512 KiB. Image/video reads return seekable media metadata. File sources return revision-checked metadata within their captured root."
                )
            },
            arguments: CompactSchema { arguments },
            mutation,
            target: Some(ResourceKind::Binding),
            palette: false,
        }
    }

    pub(super) fn request(self, args: &[String]) -> Result<FileRequest, String> {
        match (self, args) {
            (Self::Complete, [base, query]) => Ok(FileRequest::Complete {
                base: base.clone(),
                query: query.clone(),
            }),
            (Self::List, [path, rest @ ..]) => Ok(FileRequest::List {
                path: path.clone(),
                offset: rest
                    .first()
                    .map(|value| value.parse())
                    .transpose()
                    .map_err(|_| "Directory offset must be a number")?
                    .unwrap_or(0),
            }),
            (Self::Read, [path]) => Ok(FileRequest::Read { path: path.clone() }),
            (Self::Source, [path, root]) => Ok(FileRequest::OpenReader {
                path: path.clone(),
                root: root.clone(),
            }),
            (Self::Save, [path, digest, content]) => Ok(FileRequest::Save {
                path: path.clone(),
                expected_digest: digest.clone(),
                content_base64: content.clone(),
            }),
            (Self::Format, [path, content]) => Ok(FileRequest::Format {
                path: path.clone(),
                content_base64: content.clone(),
            }),
            (Self::Browse | Self::Open | Self::Reveal | Self::OpenIn, _) => {
                Err("Opening a file panel requires a window".to_owned())
            }
            _ => Err("Invalid file command arguments".to_owned()),
        }
    }
}
