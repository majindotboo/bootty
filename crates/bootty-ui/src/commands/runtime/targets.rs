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
        let process = process.to_owned();
        let window = &self.window_state_key;
        let binding = &self.workspace.active.binding;
        let mux = binding.mux();
        let scope = binding.scope();
        let binding_generation = mux.binding_generation();
        let binding_handle = self.binding_target_handle(scope, binding_generation);
        let (session, mux_window, pane) = self.selected_mux_resource_path();
        let (handle, generation) = match kind {
            ResourceKind::Instance => (process, instance_generation),
            ResourceKind::ApplicationWindow => (
                serde_json::Value::from(vec![process.as_str(), window.as_str()]).to_string(),
                window_generation,
            ),
            ResourceKind::Binding => (binding_handle, binding_generation),
            ResourceKind::Session => {
                let session = session?;
                (
                    serde_json::Value::from(vec![binding_handle.as_str(), session.as_str()])
                        .to_string(),
                    mux.session_generation(&session)?,
                )
            }
            ResourceKind::MuxWindow => {
                let (session, mux_window) = (session?, mux_window?);
                (
                    serde_json::Value::from(vec![
                        binding_handle.as_str(),
                        session.as_str(),
                        mux_window.as_str(),
                    ])
                    .to_string(),
                    mux.window_generation(&session, &mux_window)?,
                )
            }
            ResourceKind::Pane => {
                let (session, mux_window, pane) = (session?, mux_window?, pane?);
                (
                    serde_json::Value::from(vec![
                        binding_handle.as_str(),
                        session.as_str(),
                        mux_window.as_str(),
                        pane.as_str(),
                    ])
                    .to_string(),
                    mux.pane_generation(&session, &mux_window, &pane)?,
                )
            }
            ResourceKind::Terminal => {
                return self.current_terminal_target(
                    &binding_handle,
                    binding_generation,
                    (session, mux_window, pane),
                );
            }
        };
        Some(CommandTarget {
            kind,
            handle,
            generation,
        })
    }

    fn current_terminal_target(
        &self,
        binding_handle: &str,
        binding_generation: u64,
        path: (Option<String>, Option<String>, Option<String>),
    ) -> Option<CommandTarget> {
        let mux = self.workspace.active.binding.mux();
        let (handle, generation) = match path {
            (Some(session), Some(mux_window), Some(pane)) => (
                serde_json::Value::from(vec![
                    binding_handle,
                    session.as_str(),
                    mux_window.as_str(),
                    pane.as_str(),
                ])
                .to_string(),
                mux.terminal_generation(&session, &mux_window, &pane)?,
            ),
            (Some(session), _, _) => (
                serde_json::Value::from(vec![binding_handle, session.as_str()]).to_string(),
                mux.session_generation(&session)?,
            ),
            (None, _, _) => (
                serde_json::Value::from(vec![binding_handle, "active_terminal"]).to_string(),
                binding_generation,
            ),
        };
        Some(CommandTarget {
            kind: ResourceKind::Terminal,
            handle,
            generation,
        })
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
        let binding = self.binding_target_handle(scope, binding_runtime.mux().binding_generation());
        let (handle, generation) = match kind {
            ResourceKind::Session => (
                serde_json::Value::from(vec![binding.as_str(), session_id]).to_string(),
                binding_runtime
                    .mux()
                    .session_generation(session_id)
                    .unwrap_or(1),
            ),
            ResourceKind::MuxWindow => {
                let window_id = window_id?;
                (
                    serde_json::Value::from(vec![binding.as_str(), session_id, window_id])
                        .to_string(),
                    binding_runtime
                        .mux()
                        .window_generation(session_id, window_id)
                        .unwrap_or(1),
                )
            }
            _ => return None,
        };
        Some(CommandTarget {
            kind,
            handle,
            generation,
        })
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
        Some(CommandTarget {
            kind: ResourceKind::Terminal,
            handle: serde_json::Value::from(vec![binding.as_str(), session_id, window_id, pane_id])
                .to_string(),
            generation: binding_runtime
                .mux()
                .pane_generation(session_id, window_id, pane_id)?,
        })
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
