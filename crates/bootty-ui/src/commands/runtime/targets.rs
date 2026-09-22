//! Desktop command target projection and validation against live mux generations.

use crate::{commands::ExactMuxTarget, error_catalog::ErrorNotice, state::AppState};
use bootty_control::{CommandOutcome, CommandTarget, ResourceKind};
use bootty_mux::{controller::SpaceId, target, terminal::decode_scoped_pane_id};

impl AppState {
    pub(super) fn resolve_command_target(
        &self,
        command: &str,
        expected: Option<ResourceKind>,
        supplied: Option<&CommandTarget>,
    ) -> Result<(Option<CommandTarget>, Option<ExactMuxTarget>), CommandOutcome> {
        let Some(expected) = expected else {
            return if supplied.is_none() {
                Ok((None, None))
            } else {
                Err(CommandOutcome::Denied {
                    message: ErrorNotice::CommandDoesNotAcceptTarget.to_string(),
                })
            };
        };
        if supplied.is_some_and(|target| {
            target.kind != expected
                && !(command == "new_tab" && target.kind == ResourceKind::Binding)
        }) {
            return Err(CommandOutcome::Denied {
                message: ErrorNotice::CommandRequiresTarget(format!(
                    "command requires a {expected:?} target"
                ))
                .raw_message(),
            });
        }
        if let Some(supplied) = supplied {
            if self
                .current_command_target_for(command, expected)
                .is_some_and(|current| current == *supplied)
            {
                return Ok((
                    Some(supplied.clone()),
                    self.current_exact_mux_target_for(command, expected),
                ));
            }
            if let Some(exact) = self.resolve_binding_target(command, expected, supplied) {
                return Ok((Some(supplied.clone()), Some(exact)));
            }
            return Err(CommandOutcome::StaleTarget {
                message: ErrorNotice::StaleCommandTarget(format!(
                    "the {expected:?} target is stale"
                ))
                .raw_message(),
            });
        }
        let Some(current) = self.current_command_target_for(command, expected) else {
            return Err(CommandOutcome::Unavailable {
                message: ErrorNotice::NoCurrentTarget(format!(
                    "no current {expected:?} target is available"
                ))
                .raw_message(),
            });
        };
        // The opaque handle is only an equality token. Build the typed target from current mux
        // state after the complete wire target (kind, handle, and generation) has matched.
        let exact = self.current_exact_mux_target_for(command, expected);
        Ok((Some(current), exact))
    }

    fn resolve_binding_target(
        &self,
        command: &str,
        expected: ResourceKind,
        supplied: &CommandTarget,
    ) -> Option<ExactMuxTarget> {
        let active = &self.workspace.active.binding;
        let resolve = |binding: &bootty_mux::workspace::BindingRuntime| {
            let scope = binding.scope();
            let mux = binding.mux();
            let handle = self.binding_target_handle(scope, mux.binding_generation());
            target::exact_mux_target(scope, mux, supplied, &handle)
        };
        resolve(active).or_else(|| {
            if allows_cross_binding(command, expected) {
                self.workspace.all_bindings().find_map(resolve)
            } else {
                None
            }
        })
    }

    pub(super) fn activate_terminal_target(
        &mut self,
        target: &ExactMuxTarget,
    ) -> Result<(), CommandOutcome> {
        let scope = target.scope();
        let (session, window, pane) = target.ids();
        let Some(session) = session.map(str::to_owned) else {
            return Ok(());
        };
        let window = window.map(str::to_owned);
        let pane = pane.map(str::to_owned);
        self.workspace
            .activate_target(scope, &session, window.as_deref(), &self.repaint)
            .map_err(|error| CommandOutcome::Failed {
                code: "execution_failed".to_owned(),
                message: error.to_string(),
            })?;
        if let Some(pane) = pane {
            self.workspace.active.binding.focus_pane(&pane);
        }
        self.sync_terminal_panes_now();
        (self.repaint)();
        Ok(())
    }

    pub(crate) fn current_exact_mux_target_for(
        &self,
        command: &str,
        kind: ResourceKind,
    ) -> Option<ExactMuxTarget> {
        let scope = self.workspace.active.binding.scope();
        let (session_id, window_id, pane_id) = self.selected_mux_resource_path();
        match kind {
            ResourceKind::Binding => Some(ExactMuxTarget::Binding(scope)),
            ResourceKind::Session => session_id
                .map(|session_id| ExactMuxTarget::Session(scope, session_id))
                .or_else(|| (command == "new_tab").then_some(ExactMuxTarget::Binding(scope))),
            ResourceKind::MuxWindow => Some(ExactMuxTarget::Window(scope, session_id?, window_id?)),
            ResourceKind::Pane => Some(ExactMuxTarget::Pane(
                scope,
                session_id?,
                window_id?,
                pane_id?,
            )),
            ResourceKind::Terminal => match (session_id, window_id, pane_id) {
                (Some(session), Some(window), Some(pane)) => {
                    Some(ExactMuxTarget::Pane(scope, session, window, pane))
                }
                (Some(session), Some(window), None) => {
                    Some(ExactMuxTarget::Window(scope, session, window))
                }
                (Some(session), None, _) => Some(ExactMuxTarget::Session(scope, session)),
                (None, _, _) => Some(ExactMuxTarget::Binding(scope)),
            },
            ResourceKind::Instance | ResourceKind::ApplicationWindow => None,
        }
    }

    pub(crate) fn current_command_target_for(
        &self,
        command: &str,
        kind: ResourceKind,
    ) -> Option<CommandTarget> {
        let target = self.current_command_target(kind);
        if target.is_some() || command != "new_tab" || kind != ResourceKind::Session {
            return target;
        }
        self.current_command_target(ResourceKind::Binding)
            .map(|binding| CommandTarget {
                kind,
                handle: serde_json::Value::from(vec!["no-session", binding.handle.as_str()])
                    .to_string(),
                generation: binding.generation,
            })
    }

    pub(crate) fn current_command_target(&self, kind: ResourceKind) -> Option<CommandTarget> {
        let (process, instance_generation, window_generation) = self.commands.target_identity();
        let (handle, generation) = match kind {
            ResourceKind::Instance => (process.to_owned(), instance_generation),
            ResourceKind::ApplicationWindow => (
                serde_json::Value::from(vec![process, self.window_state_key.as_str()]).to_string(),
                window_generation,
            ),
            _ => {
                let binding = &self.workspace.active.binding;
                let mux = binding.mux();
                let handle = self.binding_target_handle(binding.scope(), mux.binding_generation());
                let exact = if kind == ResourceKind::Terminal {
                    self.current_terminal_target()
                } else {
                    self.current_exact_mux_target_for("", kind)?
                };
                return exact.command_target(kind, mux, &handle);
            }
        };
        Some(CommandTarget {
            kind,
            handle,
            generation,
        })
    }

    fn current_terminal_target(&self) -> ExactMuxTarget {
        let scope = self.workspace.active.binding.scope();
        match self.selected_mux_resource_path() {
            (Some(session), Some(window), Some(pane)) => {
                ExactMuxTarget::Pane(scope, session, window, pane)
            }
            (Some(session), _, _) => ExactMuxTarget::Session(scope, session),
            (None, _, _) => ExactMuxTarget::Binding(scope),
        }
    }

    pub(crate) fn selected_mux_resource_path(
        &self,
    ) -> (Option<String>, Option<String>, Option<String>) {
        let binding = &self.workspace.active.binding;
        let mux = binding.mux();
        let Some(anchor) = mux.selected_session_anchor() else {
            return (None, None, None);
        };
        let session = anchor.session_id.clone();
        let mux_window = mux.selected_window().map(str::to_owned).or_else(|| {
            mux.sessions()
                .iter()
                .find(|candidate| candidate.id == session)
                .and_then(|candidate| candidate.active_window_id.clone())
        });
        let pane = if self.uses_native_terminal_layout() {
            binding.terminal().focused_pane_id().map(|pane_id| {
                decode_scoped_pane_id(pane_id).map_or_else(
                    || pane_id.to_owned(),
                    |(scope, pane_id)| {
                        debug_assert_eq!(scope, binding.scope());
                        pane_id
                    },
                )
            })
        } else {
            anchor.pane_id.clone()
        };
        (Some(session), mux_window, pane)
    }

    pub(crate) fn mux_resource_target(
        &self,
        scope: SpaceId,
        kind: ResourceKind,
        session_id: &str,
        window_id: Option<&str>,
    ) -> Option<CommandTarget> {
        let binding_runtime = self.workspace.binding(scope)?;
        let mux = binding_runtime.mux();
        let session = mux.backend_session_by_id_or_name(session_id)?;
        let exact = match kind {
            ResourceKind::Session => ExactMuxTarget::Session(scope, session.id.clone()),
            ResourceKind::MuxWindow => ExactMuxTarget::window(scope, &session.id, window_id?),
            _ => return None,
        };
        let binding = self.binding_target_handle(scope, mux.binding_generation());
        exact.command_target(kind, mux, &binding)
    }

    pub(super) fn mux_terminal_target(
        &self,
        scope: SpaceId,
        session_id: &str,
        window_id: &str,
    ) -> Option<CommandTarget> {
        let binding_runtime = self.workspace.binding(scope)?;
        let pane_id = binding_runtime
            .mux()
            .sessions()
            .iter()
            .find(|session| session.id == session_id)?
            .windows
            .iter()
            .find(|window| window.id == window_id)?
            .anchor
            .pane_id
            .as_deref()?;
        let binding = self.binding_target_handle(scope, binding_runtime.mux().binding_generation());
        ExactMuxTarget::Pane(
            scope,
            session_id.to_owned(),
            window_id.to_owned(),
            pane_id.to_owned(),
        )
        .command_target(ResourceKind::Terminal, binding_runtime.mux(), &binding)
    }

    pub(crate) fn binding_target_handle(&self, scope: SpaceId, generation: u64) -> String {
        let (process, _, window_generation) = self.commands.target_identity();
        serde_json::Value::Array(vec![
            process.into(),
            self.window_state_key.clone().into(),
            window_generation.into(),
            scope.persistence_value().to_string().into(),
            generation.into(),
        ])
        .to_string()
    }
}

// These commands explicitly support a target outside the active Space.
fn allows_cross_binding(command: &str, expected: ResourceKind) -> bool {
    match expected {
        ResourceKind::Binding => {
            command.starts_with("git.")
                || command.starts_with("files.")
                || matches!(
                    command,
                    "jobs.start" | "transfers.start" | "forwards.open" | "history.search"
                )
        }
        ResourceKind::Session => command.starts_with("pane."),
        ResourceKind::Terminal => {
            matches!(command, "link.open" | "agents.focus") || command.ends_with(".acknowledge")
        }
        _ => false,
    }
}
