//! Tooltips that end with their element.
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    Action, AnyView, App, InteractiveElement, ParentElement as _, SharedString, Window, div,
};

/// A tooltip owned by its element, so it closes when the element goes away.
///
/// A kit control's own `tooltip` shows through one window overlay that hides only on hover-out,
/// so a control that disappears or moves under a still pointer would leave it up.
pub trait ElementTooltip: InteractiveElement + Sized {
    #[must_use]
    fn element_tooltip(self, label: impl Into<SharedString>) -> Self {
        let label = label.into();
        self.element_tooltip_view(move |window, cx| labelled(&label).build(window, cx))
    }

    /// The tooltip with the key binding for `action` in `context`.
    #[must_use]
    fn element_tooltip_with_action(
        self,
        label: impl Into<SharedString>,
        action: &dyn Action,
        context: Option<&str>,
    ) -> Self {
        let (label, action) = (label.into(), action.boxed_clone());
        let context = context.map(SharedString::from);
        self.element_tooltip_view(move |window, cx| {
            labelled(&label)
                .action(action.as_ref(), context.as_deref())
                .build(window, cx)
        })
    }

    #[must_use]
    fn element_tooltip_view(
        mut self,
        build: impl Fn(&mut Window, &mut App) -> AnyView + 'static,
    ) -> Self {
        self.interactivity().tooltip(build);
        self
    }
}

impl<E: InteractiveElement> ElementTooltip for E {}

fn labelled(label: &SharedString) -> Tooltip {
    let label = label.clone();
    Tooltip::element(move |_, _| {
        let selector = format!("tooltip:{label}");
        div().debug_selector(move || selector).child(label.clone())
    })
}
