use gpui_kit::component::button::Button;
use gpui_kit::{App, IntoElement, ParentElement, Window};

/// Attach chrome content to Kit's pointer and keyboard activation handler.
pub(super) fn activated_button<W>(
    wrapper: W,
    button: Button,
    activate: impl Fn(&mut Window, &mut App) + 'static,
) -> gpui_kit::AnyElement
where
    W: IntoElement + ParentElement,
{
    wrapper
        .child(button.on_click(move |_, window, cx| activate(window, cx)))
        .into_any_element()
}
