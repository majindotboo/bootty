use bootty_control::{CommandDescriptor, CompactSchema, ResourceKind, ValueType};

command_actions! {
    ThemeAction {
        Read => ("theme.read", "Read Theme", ["name"], Read),
        Import => ("theme.import", "Import Theme", ["path"], Read),
        Save => ("theme.save", "Save Theme", ["name", "source", "revision"], Write),
        Preview => ("theme.preview", "Preview Theme", ["source", "appearance"], Write),
        Apply => ("theme.apply", "Apply Authored Theme", ["name", "appearance"], Write),
        Restore => ("theme.restore", "Restore Theme Preview", [], Write),
    }
}

impl ThemeAction {
    #[must_use]
    pub fn descriptor(self) -> CommandDescriptor {
        let (id, title, names, mutation) = self.metadata();
        let arguments = names
            .iter()
            .copied()
            .map(|name| {
                let mut argument = super::argument(name, ValueType::String);
                argument.required = name != "revision";
                if name == "appearance" {
                    argument.choices = ["light", "dark"].map(str::to_owned).to_vec();
                }
                argument
            })
            .collect();
        CommandDescriptor {
            id: id.to_owned(),
            title: title.to_owned(),
            description: format!("{title} in this application's local configuration tree."),
            arguments: CompactSchema { arguments },
            mutation,
            target: Some(ResourceKind::ApplicationWindow),
            palette: false,
        }
    }
}
