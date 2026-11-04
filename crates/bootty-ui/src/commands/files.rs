use bootty_control::{CommandDescriptor, CompactSchema, ResourceKind, ValueType};
use bootty_host::files::FileRequest;

command_actions! {
    FileAction {
        Complete => ("files.complete", "Find Files", ["base", "query"], Read),
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
            .map(|name| super::argument(name, ValueType::String))
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
                format!("{title} on the target binding's host. Paths must be absolute.")
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
            (Self::Reveal | Self::OpenIn, _) => {
                Err("Handing a file to the desktop requires a window".to_owned())
            }
            _ => Err("Invalid file command arguments".to_owned()),
        }
    }
}
