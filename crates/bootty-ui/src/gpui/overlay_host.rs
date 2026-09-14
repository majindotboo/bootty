//! One-owner presentation and focus lifecycle for anchored terminal overlays.

use gpui_kit::{
    AnyView, App, Context, DismissEvent, Entity, EntityId, FocusHandle, Focusable, IntoElement,
    ManagedView, ParentElement, Render, Subscription, Window, div, prelude::*, rems,
};

/// The host-level anchor for an overlay surface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverlayPlacement {
    #[default]
    Center,
    TopRight,
    BottomRight,
}

/// An anchored managed view. Modal surfaces belong to the window-level Root dialog.
pub trait OverlayView: ManagedView {
    fn overlay_placement(&self) -> OverlayPlacement {
        OverlayPlacement::Center
    }
}

trait OverlayViewHandle {
    fn view(&self) -> AnyView;
    fn overlay_placement(&self, cx: &App) -> OverlayPlacement;
    fn subscribe_dismiss(&self, window: &Window, cx: &mut Context<OverlayHost>) -> Subscription;
}

impl<V: OverlayView> OverlayViewHandle for Entity<V> {
    fn view(&self) -> AnyView {
        self.clone().into()
    }

    fn overlay_placement(&self, cx: &App) -> OverlayPlacement {
        self.read(cx).overlay_placement()
    }

    fn subscribe_dismiss(&self, window: &Window, cx: &mut Context<OverlayHost>) -> Subscription {
        cx.subscribe_in(self, window, |host, _, _: &DismissEvent, window, cx| {
            host.dismiss(window, cx);
        })
    }
}

struct ActiveOverlay {
    view: Box<dyn OverlayViewHandle>,
    entity_id: EntityId,
    view_focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    _dismiss_subscription: Subscription,
}

/// Hosts exactly one anchored overlay and restores the focus it replaced.
pub struct OverlayHost {
    active: Option<ActiveOverlay>,
}

impl Default for OverlayHost {
    fn default() -> Self {
        Self::new()
    }
}

impl OverlayHost {
    #[must_use]
    pub const fn new() -> Self {
        Self { active: None }
    }

    /// Present `view`, replacing any active overlay through the normal dismissal path.
    pub fn present<V: OverlayView>(
        &mut self,
        view: Entity<V>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Replacement keeps the focus from before the first anchored surface, rather than
        // restoring focus to the outgoing surface while it is being replaced.
        let previous_focus = self
            .active
            .as_ref()
            .and_then(|active| active.previous_focus.clone())
            .or_else(|| window.focused(cx));
        self.remove_active(window, false, cx);

        let entity_id = view.entity_id();
        let view_focus = Focusable::focus_handle(&view, cx);
        let dismiss_subscription = view.subscribe_dismiss(window, cx);
        self.active = Some(ActiveOverlay {
            view: Box::new(view),
            entity_id,
            view_focus: view_focus.clone(),
            previous_focus,
            _dismiss_subscription: dismiss_subscription,
        });

        cx.defer_in(window, move |host, window, cx| {
            if host
                .active
                .as_ref()
                .is_some_and(|active| active.entity_id == entity_id)
            {
                view_focus.focus(window, cx);
                cx.notify();
            }
        });
        cx.notify();
    }

    /// Dismiss the active overlay and restore the exact focus captured when it opened.
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.remove_active(window, true, cx)
    }

    /// Remove an anchored view after its owner has already committed the state change.
    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.remove_active(window, false, cx)
    }

    fn remove_active(
        &mut self,
        window: &mut Window,
        emit_dismissed: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(active) = self.active.take() else {
            return false;
        };
        if active.view_focus.is_focused(window)
            && let Some(previous_focus) = active.previous_focus
        {
            previous_focus.focus(window, cx);
        }
        if emit_dismissed {
            cx.emit(DismissEvent);
        }
        cx.notify();
        true
    }

    #[must_use]
    pub const fn has_active(&self) -> bool {
        self.active.is_some()
    }

    #[must_use]
    pub fn active<V: 'static>(&self) -> Option<Entity<V>> {
        self.active.as_ref()?.view.view().downcast().ok()
    }
}

impl gpui_kit::EventEmitter<DismissEvent> for OverlayHost {}

impl Render for OverlayHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(active) = &self.active else {
            return div().into_any_element();
        };
        let view = active.view.view();
        match active.view.overlay_placement(cx) {
            OverlayPlacement::Center => view.into_any_element(),
            OverlayPlacement::TopRight => div()
                .absolute()
                .top(rems(0.75))
                .right(rems(0.75))
                .child(view)
                .into_any_element(),
            OverlayPlacement::BottomRight => div()
                .absolute()
                .bottom(rems(0.75))
                .right(rems(0.75))
                .child(view)
                .into_any_element(),
        }
    }
}
