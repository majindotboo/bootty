use bootty_control::{CommandDescriptor, CompactSchema, ResourceKind, ValueType};
use bootty_host::files::FileRequest;

command_actions! {
    FileAction {
        Browse => ("files.browse", "Browse Files", ["path"], Write),
        Open => ("files.open", "Open Document", ["path", "line", "column"], Write),
        List => ("files.list", "List Directory", ["path", "offset"], Read),
        Read => ("files.read", "Read Document", ["path"], Read),
        Save => ("files.save", "Save Document Revision", ["path", "digest", "content_base64"], Write),
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
            title: title.to_owned(),
            description: format!(
                "{title} on the target binding's host. Paths must be absolute; documents are UTF-8, up to 512 KiB."
            ),
            arguments: CompactSchema { arguments },
            mutation,
            target: Some(ResourceKind::Binding),
            palette: false,
        }
    }

    pub(super) fn request(self, args: &[String]) -> Result<FileRequest, String> {
        match (self, args) {
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
            (Self::Save, [path, digest, content]) => Ok(FileRequest::Save {
                path: path.clone(),
                expected_digest: digest.clone(),
                content_base64: content.clone(),
            }),
            (Self::Browse | Self::Open, _) => {
                Err("Opening a file panel requires a window".to_owned())
            }
            _ => Err("Invalid file command arguments".to_owned()),
        }
    }
}
