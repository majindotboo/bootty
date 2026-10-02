use bootty_control::{CommandDescriptor, CompactSchema, ResourceKind, ValueType};

command_actions! {
    TaskAction {
        List => ("session.tasks", "List saved sessions and tasks", [], Read),
        Show => ("session.tasks.show", "Saved tasks…", [], Write),
        Set => ("session.task.set", "Set task destination", ["identity", "state"], Write),
    }
}

impl TaskAction {
    #[must_use]
    pub fn descriptor(self) -> CommandDescriptor {
        let (id, title, names, mutation) = self.metadata();
        let arguments = names
            .iter()
            .map(|name| {
                let mut argument = super::argument(name, ValueType::String);
                if *name == "state" {
                    argument.choices = ["active", "settled", "archived"]
                        .map(str::to_owned)
                        .to_vec();
                }
                argument
            })
            .collect();
        CommandDescriptor {
            id: id.to_owned(),
            title: title.to_owned(),
            description: match self {
                Self::List => "List all saved session membership records in the target Space in saved order, including dormant and archived tasks. lifecycle is null for ordinary terminals; use the saved identity to explicitly promote one with session.task.set. attached_observed reflects the last backend snapshot; it does not report process or provider status.",
                Self::Show => "Open saved tasks for the target Space to inspect their saved destinations and observed attachments, or settle, archive and restore them without starting or stopping terminals.",
                Self::Set => "Promote a claimed session identity to a durable task or change its saved destination to active, settled or archived. Records survive terminal close and missing attachments. This manual metadata command never starts or stops a process, resumes a task, or changes sidebar visibility.",
            }.to_owned(),
            arguments: CompactSchema { arguments },
            mutation,
            target: Some(ResourceKind::Binding),
            palette: matches!(self, Self::Show),
        }
    }
}
