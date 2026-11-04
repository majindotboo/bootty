//! A completed terminal creation is reflected directly from the mux snapshot.
use super::GpuiWorkspace;
use crate::surface_creation::PendingNewSurface;
use bootty_control::{CommandTarget, ResourceKind};
use gpui_kit::{Context, Window};
impl GpuiWorkspace {
    pub(super) fn attach_new_surface(
        &mut self,
        _: &PendingNewSurface,
        _: &CommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(dock) = self.tools.clone() {
            let binding = &self.state.workspace.active.binding;
            let target = self
                .state
                .current_command_target_for("pane.move", ResourceKind::Session);
            dock.update(cx, |dock, cx| {
                dock.sync_terminals(binding, target, window, cx);
            });
        }
        if let Some(pane) = self.state.focused_pane() {
            self.focus_terminal_pane(&pane, window, cx);
        }
        cx.notify();
    }
}
