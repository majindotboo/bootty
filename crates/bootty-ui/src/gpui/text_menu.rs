//! Text fields' right-click menu, drawn by Bootty like its other menus.
//!
//! GPUI Kit shows a text field's menu through the OS on macOS. A field's own menu builder runs
//! when that menu would open, and the kit shows nothing for an empty menu, so [`text_menu`]
//! opens Bootty's menu instead and returns none. One [`TextMenu`] per window draws it.
//!
//! GPUI Kit's `NumberInput` takes no menu builder, so number fields keep the OS menu until it
//! does.

use gpui_kit::base::{
    Root, RootPlugin,
    input::{InputBaseState, InputModeKind},
};
use gpui_kit::component::{
    input::{Copy, Cut, GoToDefinition, Paste, SelectAll, ToggleCodeActions},
    menu::PopupMenu,
    native_menu::NativeMenu,
};
use gpui_kit::{
    App, Context, DismissEvent, Entity, Focusable as _, IntoElement, ParentElement as _, Pixels,
    Point, Render, Subscription, Window, anchored, deferred, div, prelude::FluentBuilder as _, px,
};

/// The builder a text field passes to its `context_menu`, which opens Bootty's menu for `state`.
pub fn text_menu<M: InputModeKind>(
    state: &Entity<InputBaseState<M>>,
) -> impl Fn(NativeMenu, &mut Window, &mut App) -> NativeMenu + 'static {
    let state = state.downgrade();
    move |_, window, cx| {
        if let Some(state) = state.upgrade() {
            open(&state, window, cx);
        }
        NativeMenu::new()
    }
}

/// Registers the menu's host in every window opened after this.
pub fn init_text_menu(cx: &mut App) {
    Root::register_plugin::<TextMenu>(cx, |_, _| TextMenu::default());
}

/// A window's open text menu.
#[derive(Default)]
pub struct TextMenu {
    open: Option<(Entity<PopupMenu>, Point<Pixels>, Subscription)>,
}

impl RootPlugin for TextMenu {}

impl Render for TextMenu {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().when_some(self.open.as_ref(), |layer, (menu, position, _)| {
            layer.child(
                deferred(
                    anchored()
                        .position(*position)
                        .snap_to_window_with_margin(px(8.))
                        .child(menu.clone()),
                )
                .with_priority(1),
            )
        })
    }
}

/// Opens the menu for the field `state` where the pointer is, with the actions the field allows.
fn open<M: InputModeKind>(state: &Entity<InputBaseState<M>>, window: &mut Window, cx: &mut App) {
    let Some(host) = window
        .root::<Root>()
        .flatten()
        .and_then(|root| root.read(cx).plugin::<TextMenu>())
    else {
        return;
    };
    let capabilities = state.read(cx).context_menu_capabilities();
    let field = state.read(cx).focus_handle(cx);
    let position = window.mouse_position();
    let menu = PopupMenu::build(window, cx, move |menu, _, _| {
        let enabled = !capabilities.is_disabled();
        let editable = capabilities.is_editable();
        menu.action_context(field)
            .when(capabilities.is_code_editor(), |menu| {
                menu.menu_with_disabled(
                    "Go to Definition",
                    Box::new(GoToDefinition),
                    !(enabled && capabilities.has_definition()),
                )
                .menu_with_disabled(
                    "Show Code Actions",
                    Box::new(ToggleCodeActions),
                    !(editable && capabilities.has_code_actions()),
                )
                .separator()
            })
            .menu_with_disabled(
                "Cut",
                Box::new(Cut),
                !(editable && capabilities.is_copyable()),
            )
            .menu_with_disabled("Copy", Box::new(Copy), !capabilities.is_copyable())
            .menu_with_disabled("Paste", Box::new(Paste), !editable)
            .separator()
            .menu("Select All", Box::new(SelectAll))
    });
    // The field keeps its selection showing while its menu is open.
    let menu_focus = menu.focus_handle(cx);
    state.update(cx, |state, cx| {
        state.set_selection_focus(Some(menu_focus.clone()), cx);
    });
    let (field, closing, window_handle) =
        (state.downgrade(), host.downgrade(), window.window_handle());
    let subscription = cx.subscribe(&menu, move |_, _: &DismissEvent, cx| {
        let (field, closing) = (field.clone(), closing.clone());
        // Closed after its own event, since the host holds this subscription.
        cx.defer(move |cx| {
            if let Some(field) = field.upgrade() {
                field.update(cx, |field, cx| field.set_selection_focus(None, cx));
                let focus = field.focus_handle(cx);
                _ = window_handle.update(cx, |_, window, cx| focus.focus(window, cx));
            }
            _ = closing.update(cx, |host, cx| {
                host.open = None;
                cx.notify();
            });
        });
    });
    menu_focus.focus(window, cx);
    host.update(cx, |host, cx| {
        host.open = Some((menu, position, subscription));
        cx.notify();
    });
}
