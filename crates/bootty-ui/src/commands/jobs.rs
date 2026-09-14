use bootty_control::{CommandDescriptor, CompactSchema, ResourceKind, ValueType};
command_actions! {
    JobAction {
        Start => ("jobs.start", "Start Host Job", ["spec"], Write),
        Transfer => ("transfers.start", "Start File Transfer", ["spec"], Write),
        RetryTransfer => ("transfers.retry", "Retry File Transfer", ["job"], Write),
        List => ("jobs.list", "List Host Jobs", [], Read),
        Read => ("jobs.read", "Read Job Output", ["job", "cursor", "wait_ms"], Read),
        Cancel => ("jobs.cancel", "Cancel Host Job", ["job"], Write),
        Forget => ("jobs.forget", "Forget Completed Job", ["job"], Write),
    }
}

impl JobAction {
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
            title: title.to_owned(),
            description: format!(
                "{title}. Jobs own their process tree and retain bounded stdout/stderr; they do not run in terminal panes."
            ),
            arguments: CompactSchema { arguments },
            mutation,
            target: Some(if matches!(self, Self::Start | Self::Transfer) {
                ResourceKind::Binding
            } else {
                ResourceKind::ApplicationWindow
            }),
            palette: false,
        }
    }
}
