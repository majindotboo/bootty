#![cfg(test)]

use bootty_ui::gpui::{
    SETTINGS_SIDEBAR_WIDTH_PX, ScalarValue, SettingsCategory, SettingsControl, SettingsIntent,
    SettingsPage, SettingsPageItem, SettingsRow, SettingsTitleBar, ToggleFocusNav, UiPalette,
    init_theme, update_ui_font,
};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::{
    Action as _, AppContext as _, Bounds, Context, InteractiveElement as _, IntoElement, Modifiers,
    ParentElement as _, Render, Styled as _, TestAppContext, Window, div, point, px,
};
use settings_support::GpuiSettingsSnapshot;

#[path = "support/settings.rs"]
mod settings_support;
use settings_support::SettingsProbe as SettingsWindowProbe;

fn init_zed_ui(cx: &TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
}

struct SettingsTitleBarProbe;

impl Render for SettingsTitleBarProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(SettingsTitleBar::new(
                true,
                TabBar::new("settings-test-tabs")
                    .segmented()
                    .selected_index(0)
                    .child(
                        Tab::new()
                            .label("Settings")
                            .debug_selector(|| "settings-test-tab".to_owned()),
                    ),
            ))
            .child(
                div().flex_1().flex().child(
                    div()
                        .debug_selector(|| "settings-test-body-navigation".to_owned())
                        .w(px(SETTINGS_SIDEBAR_WIDTH_PX))
                        .flex_none(),
                ),
            )
    }
}

struct FullWidthSettingsTitleBarProbe;

impl Render for FullWidthSettingsTitleBarProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(SettingsTitleBar::new(
            false,
            TabBar::new("settings-test-tabs")
                .segmented()
                .selected_index(0)
                .child(
                    Tab::new()
                        .label("Keymap")
                        .debug_selector(|| "settings-test-keymap-tab".to_owned()),
                ),
        ))
    }
}

#[gpui_kit::test]
fn settings_title_and_body_share_one_navigation_spine(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (_, cx) = cx.add_window_view(|_, _| SettingsTitleBarProbe);

    let navigation = cx
        .debug_bounds("settings-window-titlebar-navigation")
        .expect("title navigation lane");
    let title_content = cx
        .debug_bounds("settings-window-titlebar-content")
        .expect("title content lane");
    let body = cx
        .debug_bounds("settings-test-body-navigation")
        .expect("body navigation lane");
    let title = cx
        .debug_bounds("settings-window-tabs")
        .expect("settings title row");

    assert_eq!(navigation.right(), title_content.left());
    assert_eq!(navigation.left(), body.left());
    let separator = cx
        .debug_bounds("settings-window-titlebar-separator")
        .expect("content separator");
    assert_close(separator.left().into(), f32::from(body.right()) - 1.0, 1.0);
    assert_close(title_content.right().into(), title.right().into(), 1.0);
}

#[gpui_kit::test]
fn settings_title_navigation_uses_the_component_sidebar_surface(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (_, cx) = cx.add_window_view(|_, _| SettingsTitleBarProbe);

    let navigation = cx
        .debug_bounds("settings-window-titlebar-navigation")
        .expect("title navigation lane");
    let content = cx
        .debug_bounds("settings-window-titlebar-content")
        .expect("title content lane");
    assert_eq!(navigation.right(), content.left());

    cx.update(|_, cx| {
        let component = gpui_kit::component::Theme::global(cx);
        assert_eq!(component.colors.sidebar, component.tokens.sidebar.color);
    });
}

#[gpui_kit::test]
fn settings_title_content_is_full_width_without_navigation(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (_, cx) = cx.add_window_view(|_, _| FullWidthSettingsTitleBarProbe);

    let navigation = cx
        .debug_bounds("settings-window-titlebar-navigation")
        .expect("native window-control lane remains when body navigation is absent");
    let title = cx
        .debug_bounds("settings-window-tabs")
        .expect("full-width title lane");
    let content = cx
        .debug_bounds("settings-window-titlebar-content")
        .expect("full-width title content");
    assert_eq!(navigation.left(), title.left());
    assert_eq!(navigation.right(), content.left());
    assert_eq!(title.right(), content.right());
}

struct SwitchingSettingsTitleBarProbe {
    has_navigation: bool,
}

impl Render for SwitchingSettingsTitleBarProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(SettingsTitleBar::new(
            self.has_navigation,
            TabBar::new("settings-test-tabs")
                .segmented()
                .selected_index(0)
                .child(
                    Tab::new()
                        .label("Settings")
                        .debug_selector(|| "settings-test-tab".to_owned()),
                ),
        ))
    }
}

#[gpui_kit::test]
fn settings_title_tabs_stay_left_aligned_when_navigation_disappears(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, _| SwitchingSettingsTitleBarProbe {
        has_navigation: true,
    });
    let with_navigation = cx
        .debug_bounds("settings-window-titlebar-content")
        .expect("title content lane with navigation");
    let tab = cx
        .debug_bounds("settings-test-tab")
        .expect("segmented settings tab");
    assert_close(
        tab.center().y.into(),
        with_navigation.center().y.into(),
        1.0,
    );
    assert!(tab.top() > with_navigation.top());
    assert!(tab.bottom() < with_navigation.bottom());

    probe.update(cx, |probe, cx| {
        probe.has_navigation = false;
        cx.notify();
    });
    cx.run_until_parked();

    let without_navigation = cx
        .debug_bounds("settings-window-titlebar-content")
        .expect("title content lane without navigation");
    assert_eq!(without_navigation.left(), with_navigation.left());
    assert_eq!(without_navigation.size.height, with_navigation.size.height);
    if cfg!(target_os = "macos") {
        assert!(without_navigation.left() >= px(80.0));
    }
    let tab = cx
        .debug_bounds("settings-test-tab")
        .expect("segmented settings tab");
    assert_close(
        tab.center().y.into(),
        without_navigation.center().y.into(),
        1.0,
    );
    assert_eq!(
        cx.debug_bounds("settings-window-titlebar-navigation")
            .expect("traffic-light title lane")
            .right(),
        without_navigation.left()
    );

    cx.update(|window, cx| {
        update_ui_font(&[], 24.0, cx);
        window.set_rem_size(px(24.0));
    });
    cx.refresh().expect("render larger interface font");
    let enlarged = cx
        .debug_bounds("settings-window-titlebar-content")
        .expect("scaled title content");
    let tab = cx.debug_bounds("settings-test-tab").expect("scaled tab");
    assert!(enlarged.size.height > without_navigation.size.height);
    assert_eq!(enlarged.left(), without_navigation.left());
    assert_close(tab.center().y.into(), enlarged.center().y.into(), 1.0);
}

#[gpui_kit::test]
fn focused_settings_window_emits_close_on_escape(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let window = cx.update(|cx| {
        cx.open_window(gpui_kit::WindowOptions::default(), |_, cx| {
            cx.new(|cx| {
                SettingsWindowProbe::with_snapshot(
                    snapshot_with_category(SettingsCategory::General),
                    cx,
                )
            })
        })
        .expect("open settings window")
    });
    window
        .update(cx, |probe, window, cx| {
            probe
                .settings
                .update(cx, |settings, cx| settings.focus(window, cx));
        })
        .expect("focus settings window");

    cx.simulate_keystrokes(*window, "escape");

    window
        .update(cx, |probe, _, _| {
            assert!(matches!(
                probe.intents.borrow().as_slice(),
                [SettingsIntent::Close]
            ));
        })
        .expect("read settings intent");
}

#[gpui_kit::test]
fn settings_focus_action_toggles_between_navigation_and_content(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| {
        SettingsWindowProbe::with_snapshot(snapshot_with_category(SettingsCategory::General), cx)
    });
    cx.update(|window, cx| {
        probe.update(cx, |probe, cx| {
            probe
                .settings
                .update(cx, |settings, cx| settings.focus(window, cx));
        });
    });
    cx.refresh().expect("render focused settings window");
    assert!(
        cx.debug_bounds("settings-focus-toggle-to-navbar").is_some(),
        "settings starts with content outside the navigation focus group"
    );

    let focus_shortcut = if cfg!(target_os = "macos") {
        "cmd-shift-e"
    } else {
        "ctrl-shift-e"
    };
    cx.simulate_keystrokes(focus_shortcut);
    cx.refresh().expect("focus settings navigation");
    assert!(
        cx.debug_bounds("settings-focus-toggle-to-content")
            .is_some(),
        "the action moves focus into the navigation"
    );

    cx.update(|window, cx| window.dispatch_action(ToggleFocusNav.boxed_clone(), cx));
    cx.refresh().expect("schedule settings content focus");
    cx.refresh().expect("focus settings content");
    cx.refresh().expect("render settings content focus");
    assert!(
        cx.debug_bounds("settings-focus-toggle-to-navbar").is_some(),
        "the same action moves focus back into the content"
    );
}

#[gpui_kit::test]
fn settings_content_exposes_its_accessible_group_identity(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (_, cx) = cx.add_window_view(|_, cx| {
        SettingsWindowProbe::with_snapshot(snapshot_with_category(SettingsCategory::General), cx)
    });

    assert!(
        cx.debug_bounds("settings-content-scroll").is_some(),
        "the accessible settings content owner is rendered"
    );
    if let Some(tree) = cx.update(|window, _| window.debug_a11y_tree_json()) {
        assert!(tree.contains(r#""role": "Group""#));
        assert!(tree.contains(r#""label": "Settings Content""#));
    }
}

#[gpui_kit::test]
fn settings_expose_the_eight_zed_shaped_pages_in_product_order(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (_, cx) = cx.add_window_view(|_, cx| {
        SettingsWindowProbe::with_snapshot(snapshot_with_category(SettingsCategory::General), cx)
    });
    let expected = [
        ("settings-category-general", "General"),
        ("settings-category-appearance", "Appearance"),
        ("settings-category-keymap", "Keymap"),
        ("settings-category-window-and-layout", "Window & Layout"),
        ("settings-category-panels", "Panels"),
        ("settings-category-terminal", "Terminal"),
        ("settings-category-remotes", "Remotes"),
        ("settings-category-advanced", "Advanced"),
    ];
    let mut previous_top = None;
    for (selector, label) in expected {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{label} page is visible"));
        let top: f32 = bounds.origin.y.into();
        if let Some(previous_top) = previous_top {
            assert!(top > previous_top, "{label} keeps the product page order");
        }
        previous_top = Some(top);
    }
    assert!(cx.debug_bounds("settings-category-ui").is_none());
}

#[gpui_kit::test]
fn translated_category_titles_survive_rendering_and_english_sections_use_human_titles(
    cx: &mut TestAppContext,
) {
    init_zed_ui(cx);
    let mut snapshot = snapshot_with_category(SettingsCategory::Remotes);
    for page in &mut snapshot.pages {
        page.title = if page.category == SettingsCategory::Remotes {
            "Connexions".to_owned()
        } else {
            page.title.clone()
        };
        if page.category == SettingsCategory::Remotes {
            for item in &mut page.items {
                if let SettingsPageItem::SectionHeader { title, .. } = item {
                    *title = "SSH PROFILES".to_owned();
                }
            }
        }
    }

    let (_, cx) = cx.add_window_view(|_, cx| SettingsWindowProbe::with_snapshot(snapshot, cx));

    assert!(cx.debug_bounds("settings-page-title=Connexions").is_some());
    assert!(cx.debug_bounds("settings-page-title=Remotes").is_none());
    assert!(
        cx.debug_bounds("settings-section-title=SSH Profiles")
            .is_some()
    );
    assert!(
        cx.debug_bounds("settings-section-title=SSH PROFILES")
            .is_none()
    );
}

#[gpui_kit::test]
fn category_and_section_navigation_selects_content_locally(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| {
        SettingsWindowProbe::with_snapshot(snapshot_with_category(SettingsCategory::General), cx)
    });
    let appearance = cx
        .debug_bounds("settings-category-appearance")
        .expect("Appearance root is visible");
    cx.simulate_click(bounds_center(appearance), Modifiers::none());

    cx.refresh().expect("render Appearance page");
    let colors = cx
        .debug_bounds("settings-section-colors")
        .expect("Colors is a child anchor of Appearance");
    cx.simulate_click(bounds_center(colors), Modifiers::none());
    probe.update(cx, |probe, _| {
        assert!(
            probe.intents.borrow().is_empty(),
            "navigation stays in the settings view"
        );
    });
}

#[gpui_kit::test]
fn section_entry_and_tab_focus_editors_before_row_utilities(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let mut snapshot = snapshot_with_category(SettingsCategory::Appearance);
    snapshot
        .pages
        .iter_mut()
        .find(|page| page.category == SettingsCategory::Appearance)
        .expect("Appearance page")
        .items = section(
        "docks",
        "Docks",
        "dock buttons",
        vec![
            toggle_row("chrome.left-dock-toggle", "Show left dock button"),
            toggle_row("chrome.right-dock-toggle", "Show right dock button"),
        ],
    );
    let (probe, cx) = cx.add_window_view(|_, cx| SettingsWindowProbe::with_snapshot(snapshot, cx));
    let section = cx
        .debug_bounds("settings-section-docks")
        .expect("Docks section");
    cx.simulate_click(bounds_center(section), Modifiers::none());
    for _ in 0..3 {
        cx.update(|window, app| {
            window.simulate_next_frame(app);
        });
        cx.refresh().expect("focus the section's first editor");
    }

    for (key, expected) in [
        (None, "chrome.left-dock-toggle"),
        (Some("tab"), "chrome.right-dock-toggle"),
        (Some("shift-tab"), "chrome.left-dock-toggle"),
    ] {
        if let Some(key) = key {
            cx.simulate_keystrokes(key);
            for _ in 0..3 {
                cx.update(|window, app| {
                    window.simulate_next_frame(app);
                });
                cx.refresh().expect("advance editor focus");
            }
        }
        probe.update(cx, |probe, _| probe.intents.borrow_mut().clear());
        cx.simulate_keystrokes("space");
        cx.simulate_event(gpui_kit::KeyUpEvent {
            keystroke: gpui_kit::Keystroke::parse("space").unwrap(),
        });
        probe.update(cx, |probe, _| {
            assert!(
                matches!(probe.intents.borrow().as_slice(), [SettingsIntent::SetValue { id, .. }] if id == expected),
                "Space must edit {expected}, not activate its link or reset: {:?}",
                probe.intents.borrow()
            );
        });
    }
}

#[gpui_kit::test]
fn settings_content_focus_survives_inserted_rows_and_keeps_tab_order(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let mut initial = snapshot_with_category(SettingsCategory::General);
    let general = initial
        .pages
        .iter_mut()
        .find(|page| page.category == SettingsCategory::General)
        .expect("General page");
    general.items = section(
        "general",
        "General",
        "general focus",
        vec![
            toggle_row("general.first", "First setting"),
            toggle_row("general.second", "Second setting"),
            toggle_row("general.third", "Third setting"),
        ],
    );
    let (probe, cx) = cx.add_window_view(|_, cx| SettingsWindowProbe::with_snapshot(initial, cx));

    let second = cx
        .debug_bounds("settings-toggle-general.second")
        .expect("second setting is rendered");
    cx.simulate_click(bounds_center(second), Modifiers::none());
    let focused_after_click = cx.update(|window, app| window.focused(app).is_some());
    probe.update(cx, |probe, _| probe.intents.borrow_mut().clear());

    let mut updated = snapshot_with_category(SettingsCategory::General);
    let general = updated
        .pages
        .iter_mut()
        .find(|page| page.category == SettingsCategory::General)
        .expect("General page");
    general.items = section(
        "general",
        "General",
        "general focus",
        vec![
            toggle_row("general.first", "First setting"),
            toggle_row("general.inserted", "Inserted setting"),
            toggle_row("general.second", "Second setting"),
            toggle_row("general.third", "Third setting"),
        ],
    );
    probe.update(cx, |probe, cx| {
        probe.settings.update(cx, |settings, cx| {
            settings.set_content(updated.into_content(), cx);
        });
    });

    cx.simulate_keystrokes("tab");
    for frame in 1..=2 {
        cx.update(|window, app| {
            assert!(
                window.simulate_next_frame(app) > 0,
                "the content Tab handler schedules frame {frame}"
            );
        });
    }
    let focused_after_tab = cx.update(|window, app| window.focused(app).is_some());
    cx.simulate_keystrokes("space");
    cx.simulate_event(gpui_kit::KeyUpEvent {
        keystroke: gpui_kit::Keystroke::parse("space").unwrap(),
    });

    probe.update(cx, |probe, _| {
        assert!(
            probe.intents.borrow().iter().any(|intent| matches!(
                intent,
                SettingsIntent::SetValue { id, .. } if id == "general.third"
            )),
            "tab should advance from the retained second-setting focus to the third setting; focused_after_click={focused_after_click}, focused_after_tab={focused_after_tab}, intents={:?}",
            probe.intents.borrow()
        );
    });
}

#[gpui_kit::test]
fn section_navigation_stops_at_the_real_content_end(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let mut snapshot = snapshot_with_category(SettingsCategory::Appearance);
    let appearance = snapshot
        .pages
        .iter_mut()
        .find(|page| page.category == SettingsCategory::Appearance)
        .expect("Appearance page");
    for index in 0..12 {
        appearance.items.insert(
            1,
            SettingsPageItem::Setting(toggle_row(
                &format!("appearance.filler-{index}"),
                &format!("Filler setting {index}"),
            )),
        );
    }
    let (_, cx) = cx.add_window_view(|_, cx| SettingsWindowProbe::with_snapshot(snapshot, cx));

    let colors = cx
        .debug_bounds("settings-section-colors")
        .expect("Colors is a child anchor of Appearance");
    cx.simulate_click(bounds_center(colors), Modifiers::none());
    for _ in 0..3 {
        cx.refresh().expect("settle the measured section scroll");
    }

    let content = cx
        .debug_bounds("settings-content-scroll")
        .expect("settings content is rendered");
    let header = cx
        .debug_bounds("settings-content-section-colors")
        .expect("requested section header is rendered after scrolling");
    let final_row = cx
        .debug_bounds("settings-toggle-appearance.font-smoothing")
        .expect("the final real setting is rendered at the maximum scroll offset");
    let content_top: f32 = content.origin.y.into();
    let content_height: f32 = content.size.height.into();
    let header_top: f32 = header.origin.y.into();
    let final_row_bottom = bounds_bottom(final_row);
    assert!(
        header_top >= content_top && header_top < content_top + content_height,
        "requested section header must remain visible: content={content:?}, header={header:?}"
    );
    assert!(
        final_row_bottom <= content_top + content_height,
        "the list must stop at its final real row: content={content:?}, final_row={final_row:?}"
    );
}

#[gpui_kit::test]
fn selected_root_disclosure_collapses_and_expands_section_children(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (_, cx) = cx.add_window_view(|_, cx| {
        SettingsWindowProbe::with_snapshot(snapshot_with_category(SettingsCategory::General), cx)
    });
    assert!(cx.debug_bounds("settings-section-general").is_some());
    let root = cx
        .debug_bounds("settings-category-general")
        .expect("General root is visible");
    cx.simulate_click(disclosure_center(root), Modifiers::none());
    cx.refresh().expect("collapse General");
    assert!(cx.debug_bounds("settings-section-general").is_none());

    let root = cx
        .debug_bounds("settings-category-general")
        .expect("General root remains visible");
    cx.simulate_click(disclosure_center(root), Modifiers::none());
    cx.refresh().expect("expand General");
    assert!(cx.debug_bounds("settings-section-general").is_some());
}

#[gpui_kit::test]
fn search_indexes_page_section_and_setting_context_together(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| {
        SettingsWindowProbe::with_snapshot(snapshot_with_category(SettingsCategory::Appearance), cx)
    });
    let search = cx
        .debug_bounds("settings-search")
        .expect("Zed search control is visible");
    cx.simulate_click(bounds_center(search), Modifiers::none());
    cx.simulate_input("ssh host");

    cx.refresh().expect("render matching Remotes page");
    assert!(cx.debug_bounds("settings-toggle-remotes.default").is_some());
    assert!(cx.debug_bounds("settings-toggle-appearance.dark").is_none());
    probe.update(cx, |probe, _| {
        assert!(
            probe.intents.borrow().is_empty(),
            "search stays in the settings view"
        );
    });
}

#[gpui_kit::test]
fn settings_keep_zeds_fixed_split_and_two_column_row_alignment(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (_, cx) = cx.add_window_view(|_, cx| {
        SettingsWindowProbe::with_snapshot(snapshot_with_category(SettingsCategory::Appearance), cx)
    });
    let sidebar = cx
        .debug_bounds("settings-sidebar")
        .expect("fixed navigation sidebar is rendered");
    let content = cx
        .debug_bounds("settings-content-scroll")
        .expect("independent content list is rendered");
    let control = cx
        .debug_bounds("settings-toggle-appearance.cursor-blink")
        .expect("row control is rendered");
    let sidebar_width: f32 = sidebar.size.width.into();
    let sidebar_right = bounds_right(sidebar);
    let content_left: f32 = content.origin.x.into();
    let content_width: f32 = content.size.width.into();
    let control_left: f32 = control.origin.x.into();

    assert_close(sidebar_width, SETTINGS_SIDEBAR_WIDTH_PX, 1.0);
    assert_close(content_left, sidebar_right, 1.0);
    assert!(control_left > content_width.mul_add(0.5, content_left));
}

#[gpui_kit::test]
fn settings_sidebar_and_page_header_share_one_top_inset(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (_, cx) = cx.add_window_view(|_, cx| {
        SettingsWindowProbe::with_snapshot(snapshot_with_category(SettingsCategory::Appearance), cx)
    });
    let search = cx
        .debug_bounds("settings-search")
        .expect("settings search is rendered");
    let header = cx
        .debug_bounds("settings-page-header")
        .expect("settings page header is rendered");
    let title = cx
        .debug_bounds("settings-page-title=Appearance")
        .expect("current category title is rendered in the page header");
    let edit = cx
        .debug_bounds("settings-open-config")
        .expect("config action is rendered in the page header");
    let search_top: f32 = search.origin.y.into();
    let header_top: f32 = header.origin.y.into();
    let title_center = bounds_center_y(title);
    let edit_center = bounds_center_y(edit);

    assert_close(header_top, search_top, 1.0);
    assert_close(title_center, edit_center, 1.0);
}

#[gpui_kit::test]
fn settings_focus_hint_is_vertically_centered_in_footer(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (_, cx) = cx.add_window_view(|_, cx| {
        SettingsWindowProbe::with_snapshot(snapshot_with_category(SettingsCategory::General), cx)
    });
    let footer = cx
        .debug_bounds("settings-focus-toggle-to-navbar")
        .expect("settings footer is rendered");
    let hint = cx
        .debug_bounds("settings-focus-hint")
        .expect("focus hint slot is rendered");
    let hint_content = cx
        .debug_bounds("settings-focus-hint-content")
        .expect("focus hint content is rendered");
    let footer_center = bounds_center_y(footer);
    let hint_center = bounds_center_y(hint);
    let hint_height: f32 = hint.size.height.into();
    assert!(hint_height >= 20.0, "hint slot is not clipped");
    assert_close(footer_center, hint_center, 1.0);
    let hint_content_center = bounds_center_x(hint_content);
    let hint_slot_center = bounds_center_x(hint);
    assert_close(hint_content_center, hint_slot_center, 1.0);
}

#[gpui_kit::test]
fn dependent_settings_use_zeds_single_inset_child_group(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let mut snapshot = snapshot_with_category(SettingsCategory::General);
    snapshot.pages[0].items = vec![
        SettingsPageItem::SectionHeader {
            id: "general".to_owned(),
            title: "General".to_owned(),
            search_terms: "general".to_owned(),
        },
        SettingsPageItem::Dependent {
            parent: toggle_row("general.parent", "Parent setting"),
            children: vec![toggle_row("general.child", "Child setting")],
        },
    ];
    let (_, cx) = cx.add_window_view(|_, cx| SettingsWindowProbe::with_snapshot(snapshot, cx));

    let content = cx
        .debug_bounds("settings-content-scroll")
        .expect("settings content is rendered");
    let parent = cx
        .debug_bounds("settings-dependent-parent-general.parent")
        .expect("dependent parent is rendered");
    let child = cx
        .debug_bounds("settings-dependent-child-general.parent-general.child")
        .expect("dependent child is rendered");
    let content_left: f32 = content.origin.x.into();
    let parent_left: f32 = parent.origin.x.into();
    let child_left: f32 = child.origin.x.into();

    assert_close(parent_left, content_left, 1.0);
    assert_close(child_left - parent_left, 32.0, 1.0);
    let parent_width: f32 = parent.size.width.into();
    let child_width: f32 = child.size.width.into();
    assert_close(child_width, parent_width - 64.0, 1.0);
}

#[gpui_kit::test]
fn live_ui_font_scaling_remeasures_virtualized_setting_rows(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let mut snapshot = snapshot_with_category(SettingsCategory::General);
    snapshot.pages[0].items = vec![
        SettingsPageItem::SectionHeader {
            id: "general".to_owned(),
            title: "General".to_owned(),
            search_terms: "general".to_owned(),
        },
        SettingsPageItem::Dependent {
            parent: SettingsRow::Value {
                id: "general.long-parent".to_owned(),
                label: "Restore every previously active workspace and terminal session".to_owned(),
                help: "Reopen all windows, spaces, panes, sidebars, and terminals that were active when Bootty last exited, preserving their prior arrangement and focus.".to_owned(),
                value: ScalarValue::Bool(true),
                control: SettingsControl::Toggle,
                enabled: true,
            },
            children: vec![toggle_row("general.long-child", "Restore terminal scrollback")],
        },
        SettingsPageItem::Dependent {
            parent: toggle_row("general.next-parent", "Open a new window after restoring"),
            children: vec![],
        },
    ];
    bootty_ui::i18n::localize_settings(
        &mut snapshot.pages,
        &bootty_ui::i18n::Localizer::new("en-XA").unwrap(),
    );
    let (_, cx) = cx.add_window_view(|_, cx| SettingsWindowProbe::with_snapshot(snapshot, cx));
    let initial_first = cx
        .debug_bounds("settings-dependent-general.long-parent")
        .expect("first dependent setting is rendered");
    let initial_second = cx
        .debug_bounds("settings-dependent-general.next-parent")
        .expect("following setting is rendered");
    let initial_first_height: f32 = initial_first.size.height.into();
    let initial_second_top: f32 = initial_second.origin.y.into();

    cx.update(|_, cx| update_ui_font(&[], 24.0, cx));
    cx.refresh().expect("render the live UI font size");

    let scaled_first = cx
        .debug_bounds("settings-dependent-general.long-parent")
        .expect("first dependent setting remains rendered");
    let scaled_second = cx
        .debug_bounds("settings-dependent-general.next-parent")
        .expect("following setting remains rendered");
    let scaled_first_height: f32 = scaled_first.size.height.into();
    let scaled_first_bottom = bounds_bottom(scaled_first);
    let scaled_second_top: f32 = scaled_second.origin.y.into();

    assert!(
        scaled_first_height > initial_first_height,
        "the wrapping setting row should grow with the UI font: {initial_first:?} -> {scaled_first:?}"
    );
    assert!(
        scaled_second_top > initial_second_top,
        "the following virtual row should move after remeasurement: {initial_second:?} -> {scaled_second:?}"
    );
    assert!(
        scaled_second_top >= scaled_first_bottom,
        "remeasured virtual rows must not overlap: first={scaled_first:?}, second={scaled_second:?}"
    );
}

#[gpui_kit::test]
fn settings_have_no_legacy_scope_or_nested_page_controls(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (_, cx) = cx.add_window_view(|_, cx| {
        SettingsWindowProbe::with_snapshot(snapshot_with_category(SettingsCategory::General), cx)
    });
    for selector in [
        "settings-scope",
        "settings-scope-user",
        "settings-scope-bootty",
        "settings-category-colors",
        "settings-category-sidebar",
        "settings-category-status",
    ] {
        assert!(cx.debug_bounds(selector).is_none(), "{selector} is absent");
    }
}

#[gpui_kit::test]
fn edit_config_button_requests_the_host_owned_file_editor(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| {
        SettingsWindowProbe::with_snapshot(snapshot_with_category(SettingsCategory::General), cx)
    });
    let edit = cx
        .debug_bounds("settings-open-config")
        .expect("Edit config.toml is visible");

    cx.simulate_click(bounds_center(edit), Modifiers::none());

    probe.update(cx, |probe, _| {
        assert!(
            probe
                .intents
                .borrow()
                .iter()
                .any(|intent| matches!(intent, SettingsIntent::Invoke(id) if id == "config:edit"))
        );
    });
}

fn snapshot_with_category(category: SettingsCategory) -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category,
        pages: vec![
            page(
                SettingsCategory::General,
                vec![section(
                    "general",
                    "General",
                    "general startup restore session profile lifecycle",
                    vec![toggle_row(
                        "general.restore-session",
                        "Restore previous session",
                    )],
                )],
            ),
            page(
                SettingsCategory::Appearance,
                vec![
                    section(
                        "appearance",
                        "Appearance",
                        "appearance cursor pointer",
                        vec![toggle_row("appearance.cursor-blink", "Blink cursor")],
                    ),
                    section(
                        "colors",
                        "Colors",
                        "colors theme background",
                        vec![toggle_row("appearance.colors", "Use terminal colors")],
                    ),
                    section(
                        "text",
                        "Text & Fonts",
                        "text fonts family size",
                        vec![toggle_row("appearance.font-smoothing", "Font smoothing")],
                    ),
                ],
            ),
            page(
                SettingsCategory::Keymap,
                vec![section(
                    "keymap",
                    "Keymap",
                    "keymap shortcuts bindings",
                    vec![action_row("keymap:open", "Open Keymap")],
                )],
            ),
            page(
                SettingsCategory::WindowAndLayout,
                vec![section(
                    "window",
                    "Window",
                    "window tabs splits fullscreen chrome",
                    vec![toggle_row("window.fullscreen", "Fullscreen")],
                )],
            ),
            page(
                SettingsCategory::Panels,
                vec![section(
                    "panels",
                    "Panels",
                    "sidebar status docking widths visibility modules",
                    vec![toggle_row("panels.sidebar", "Show sidebar")],
                )],
            ),
            page(
                SettingsCategory::Terminal,
                vec![section(
                    "terminal",
                    "Terminal",
                    "terminal shell environment",
                    vec![toggle_row("terminal.login-shell", "Login shell")],
                )],
            ),
            page(
                SettingsCategory::Remotes,
                vec![section(
                    "remotes",
                    "Remotes",
                    "remotes ssh host",
                    vec![toggle_row("remotes.default", "Default remote")],
                )],
            ),
            page(
                SettingsCategory::Advanced,
                vec![section(
                    "advanced",
                    "Advanced",
                    "config diagnostics extensions",
                    vec![toggle_row("advanced.trace", "Stability trace")],
                )],
            ),
        ],
        search: String::new(),
        write_error: None,
    }
}

fn page(category: SettingsCategory, sections: Vec<Vec<SettingsPageItem>>) -> SettingsPage {
    SettingsPage {
        category,
        title: category.label().to_owned(),
        search_terms: category.label().to_ascii_lowercase(),
        items: sections.into_iter().flatten().collect(),
    }
}

fn section(
    id: &str,
    title: &str,
    search_terms: &str,
    rows: Vec<SettingsRow>,
) -> Vec<SettingsPageItem> {
    std::iter::once(SettingsPageItem::SectionHeader {
        id: id.to_owned(),
        title: title.to_owned(),
        search_terms: search_terms.to_owned(),
    })
    .chain(rows.into_iter().map(SettingsPageItem::Setting))
    .collect()
}

fn toggle_row(id: &str, label: &str) -> SettingsRow {
    SettingsRow::Value {
        id: id.to_owned(),
        label: label.to_owned(),
        help: format!("Help for {label}."),
        value: ScalarValue::Bool(false),
        control: SettingsControl::Toggle,
        enabled: true,
    }
}

fn action_row(id: &str, label: &str) -> SettingsRow {
    SettingsRow::Action {
        id: id.to_owned(),
        label: label.to_owned(),
        help: "Open the dedicated Zed-shaped keymap editor.".to_owned(),
        button: label.to_owned(),
        enabled: true,
    }
}

fn bounds_center(bounds: Bounds<gpui_kit::Pixels>) -> gpui_kit::Point<gpui_kit::Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(px(width.mul_add(0.5, left)), px(height.mul_add(0.5, top)))
}

fn bounds_right(bounds: Bounds<gpui_kit::Pixels>) -> f32 {
    let left: f32 = bounds.origin.x.into();
    let width: f32 = bounds.size.width.into();
    width.mul_add(1.0, left)
}

fn bounds_bottom(bounds: Bounds<gpui_kit::Pixels>) -> f32 {
    let top: f32 = bounds.origin.y.into();
    let height: f32 = bounds.size.height.into();
    height.mul_add(1.0, top)
}

fn bounds_center_x(bounds: Bounds<gpui_kit::Pixels>) -> f32 {
    let left: f32 = bounds.origin.x.into();
    let width: f32 = bounds.size.width.into();
    width.mul_add(0.5, left)
}

fn bounds_center_y(bounds: Bounds<gpui_kit::Pixels>) -> f32 {
    let top: f32 = bounds.origin.y.into();
    let height: f32 = bounds.size.height.into();
    height.mul_add(0.5, top)
}

fn disclosure_center(bounds: Bounds<gpui_kit::Pixels>) -> gpui_kit::Point<gpui_kit::Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let height: f32 = bounds.size.height.into();
    point(px(left + 15.0), px(height.mul_add(0.5, top)))
}

fn assert_close(actual: f32, expected: f32, tolerance: f32) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "expected {expected}px ± {tolerance}px, got {actual}px"
    );
}
