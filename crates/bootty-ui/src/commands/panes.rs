use bootty_control::{CommandDescriptor, CompactSchema, ResourceKind, ValueType};
use bootty_mux::command::{MuxCommand, MuxDirection};

command_actions! {
    PaneAction {
        Merge => ("pane.merge", "Merge Tabs", ["source_window", "target_window"], Write),
        Swap => ("pane.swap", "Swap Panes", ["source", "target"], Write),
        Move => ("pane.move", "Move Pane Beside Another", ["source", "target", "direction"], Write),
        Extract => ("pane.extract", "Extract Pane into a Tab", ["source"], Write),
    }
}

impl PaneAction {
    #[must_use]
    pub fn descriptor(self) -> CommandDescriptor {
        let (id, title, names, mutation) = self.metadata();
        let arguments = names
            .iter()
            .copied()
            .map(|name| {
                let mut argument = super::argument(name, ValueType::String);
                if name == "direction" {
                    argument.choices = ["left", "right", "up", "down"].map(str::to_owned).to_vec();
                }
                argument
            })
            .collect();
        CommandDescriptor {
            id: id.to_owned(),
            title: title.to_owned(),
            description: format!(
                "{title} within the target session, preserving running processes. Arguments are backend pane IDs (window IDs for merge)."
            ),
            arguments: CompactSchema { arguments },
            mutation,
            target: Some(ResourceKind::Session),
            palette: false,
        }
    }
    pub(super) fn command(self, session_id: String, args: &[String]) -> Result<MuxCommand, String> {
        match (self, args) {
            (Self::Merge, [source, target]) => Ok(MuxCommand::MergeWindows {
                session_id,
                source_window_id: source.clone(),
                target_window_id: target.clone(),
            }),
            (Self::Swap, [source, target]) => Ok(MuxCommand::SwapPanes {
                session_id,
                source_pane_id: source.clone(),
                target_pane_id: target.clone(),
            }),
            (Self::Move, [source, target, direction]) => Ok(MuxCommand::MovePane {
                session_id,
                pane_id: source.clone(),
                target_pane_id: target.clone(),
                direction: match direction.as_str() {
                    "left" => MuxDirection::Left,
                    "right" => MuxDirection::Right,
                    "up" => MuxDirection::Up,
                    "down" => MuxDirection::Down,
                    _ => return Err("Invalid pane direction".to_owned()),
                },
            }),
            (Self::Extract, [source]) => Ok(MuxCommand::ExtractPane {
                session_id,
                pane_id: source.clone(),
            }),
            _ => Err("Invalid pane command arguments".to_owned()),
        }
    }
}

impl crate::AppState {
    pub(crate) fn supports_pane_operation(
        &self,
        operation: bootty_mux::capability::BindingOperation,
    ) -> bool {
        let binding = &self.workspace.active.binding;
        matches!(
            binding
                .mux()
                .operation_outcome(binding.multiplexer(), operation),
            bootty_mux::capability::BindingOperationOutcome::Supported(())
        )
    }
}
