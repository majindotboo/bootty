//! Settings-window title row with one owner for the navigation and tab geometry.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, Styled as _,
    div, px,
};

use super::components::SETTINGS_SIDEBAR_WIDTH_REMS;

/// Joins the platform title row to the settings navigation and the document tab bar.
///
/// The navigation body owns the vertical separator. When navigation is absent, this row owns the
/// one horizontal content boundary above the full-width keymap or document.
#[derive(IntoElement)]
pub struct SettingsTitleBar {
    has_navigation: bool,
    tabs: AnyElement,
}

impl SettingsTitleBar {
    pub fn new(has_navigation: bool, tabs: impl IntoElement) -> Self {
        Self {
            has_navigation,
            tabs: tabs.into_any_element(),
        }
    }
}

impl RenderOnce for SettingsTitleBar {
    fn render(self, window: &mut gpui_kit::Window, cx: &mut gpui_kit::App) -> impl IntoElement {
        let colors = cx.theme().colors;
        // Native traffic lights are 14 logical pixels high and do not scale with UI text.
        // Center their platform inset on the same row as the segmented controls.
        let height = px((f32::from(window.rem_size()) * 2.5).max(38.0));
        #[cfg(target_os = "macos")]
        {
            window.set_traffic_light_position(gpui_kit::point(
                px(12.0),
                px((f32::from(height) - 14.0) / 2.0),
            ));
        }

        div()
            .id("settings-window-tabs")
            .debug_selector(|| "settings-window-tabs".to_owned())
            .w_full()
            .relative()
            .h(height)
            .flex_none()
            .bg(gpui_kit::component::Theme::global(cx).colors.sidebar)
            .flex()
            // Tabs keep the same native-control inset on every page; the sidebar only
            // changes where the content separator starts.
            .child(
                div()
                    .id("settings-window-titlebar-navigation")
                    .debug_selector(|| "settings-window-titlebar-navigation".to_owned())
                    .map(|this| {
                        if cfg!(target_os = "macos") {
                            this.w(px(80.0))
                        } else {
                            this.w_0()
                        }
                    })
                    .self_stretch()
                    .flex_none(),
            )
            .child(
                div()
                    .id("settings-window-titlebar-content")
                    .debug_selector(|| "settings-window-titlebar-content".to_owned())
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_center()
                    .px_2()
                    // TabBar's internal edge treatment may paint one pixel past its content;
                    // keep that surface inside the title content lane.
                    .overflow_hidden()
                    .child(self.tabs),
            )
            .child(
                div()
                    .debug_selector(|| "settings-window-titlebar-separator".to_owned())
                    .absolute()
                    .bottom_0()
                    .right_0()
                    .left_0()
                    .when(self.has_navigation, |this| {
                        // Include the body divider's final pixel at the shared corner.
                        this.left(px(
                            f32::from(window.rem_size()).mul_add(SETTINGS_SIDEBAR_WIDTH_REMS, -1.0)
                        ))
                    })
                    .h(px(1.0))
                    .bg(colors.border),
            )
    }
}
