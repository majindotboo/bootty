#![cfg(test)]

use bootty_ui::gpui::{
    ModifierRemap, ModifierRemapField, ScalarValue, SettingsCategory, SettingsChoice,
    SettingsControl, SettingsIntent, SettingsPage, SettingsPageItem, SettingsRow, UiPalette,
    init_theme, update_ui_font,
};
use settings_support::GpuiSettingsSnapshot;

fn init_zed_ui(cx: &TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
}
use gpui_kit::{Context, Modifiers, TestAppContext, point, px};

#[path = "support/settings.rs"]
mod settings_support;
use settings_support::SettingsProbe as ChoiceProbe;

impl ChoiceProbe {
    fn new(cx: &mut Context<Self>) -> Self {
        Self::with_snapshot(choice_snapshot(), cx)
    }
}

fn choice_snapshot() -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category: SettingsCategory::Appearance,
        pages: vec![SettingsPage {
            category: SettingsCategory::Appearance,
            title: "Appearance".to_owned(),
            search_terms: "appearance|colors|text|window|sidebar|status".to_owned(),
            items: vec![
                SettingsPageItem::SectionHeader {
                    id: "appearance".to_owned(),
                    title: "Appearance".to_owned(),
                    search_terms:
                        "cursor|blink|inactive pane|mouse pointer|hide while typing|fullscreen notch"
                            .to_owned(),
                },
                SettingsPageItem::Setting(SettingsRow::Value {
                    id: "cursor.style".to_owned(),
                    label: "Cursor style".to_owned(),
                    help: "Choose the cursor shape.".to_owned(),
                    value: ScalarValue::Token("block".to_owned()),
                    control: SettingsControl::Choice(vec![
                        SettingsChoice {
                            token: "block".to_owned(),
                            label: "Block".to_owned(),
                            description: Some("A solid block cursor.".to_owned()),
                        },
                        SettingsChoice {
                            token: "bar".to_owned(),
                            label: "Bar".to_owned(),
                            description: Some(
                                "A thin vertical cursor that remains readable at small cell sizes."
                                    .to_owned(),
                            ),
                        },
                    ]),
                    enabled: true,
                }),
            ],
        }],
        search: String::new(),
        write_error: None,
    }
}

fn modifier_remap_snapshot() -> GpuiSettingsSnapshot {
    let choices = [
        ("ctrl", "Control"),
        ("alt", "Alt"),
        ("shift", "Shift"),
        ("super", "Command / Super"),
        ("left_ctrl", "Left Control"),
        ("left_alt", "Left Alt"),
        ("left_shift", "Left Shift"),
        ("left_super", "Left Command / Super"),
        ("right_ctrl", "Right Control"),
        ("right_alt", "Right Alt"),
        ("right_shift", "Right Shift"),
        ("right_super", "Right Command / Super"),
    ]
    .into_iter()
    .map(|(token, label)| SettingsChoice {
        token: token.to_owned(),
        label: label.to_owned(),
        description: None,
    })
    .collect();
    GpuiSettingsSnapshot {
        category: SettingsCategory::Keymap,
        pages: vec![SettingsPage {
            category: SettingsCategory::Keymap,
            title: "Keymap".to_owned(),
            search_terms: "keymap|input".to_owned(),
            items: vec![SettingsPageItem::Setting(SettingsRow::ModifierRemaps {
                id: "input.modifier-remap".to_owned(),
                label: "Modifier remapping".to_owned(),
                help: "Remap physical modifiers before shortcuts.".to_owned(),
                mappings: vec![
                    ModifierRemap {
                        source: "right_alt".to_owned(),
                        target: "left_ctrl".to_owned(),
                    },
                    ModifierRemap {
                        source: "right_shift".to_owned(),
                        target: "left_shift".to_owned(),
                    },
                ],
                choices,
                enabled: true,
            })],
        }],
        search: String::new(),
        write_error: None,
    }
}

#[gpui_kit::test]
fn short_choice_uses_component_select_and_emits_the_selected_token(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| ChoiceProbe::new(cx));
    let trigger = cx
        .debug_bounds("settings-choice-cursor.style")
        .expect("choice is rendered as a dropdown trigger");
    let trigger_width: f32 = trigger.size.width.into();
    let trigger_height: f32 = trigger.size.height.into();
    assert!(
        (trigger_width - 210.0).abs() <= 1.0,
        "choice controls should use Zed's 210px picker width, got {trigger_width}px"
    );
    assert!(
        (trigger_height - 32.0).abs() <= 1.0,
        "gpui-component medium Select triggers are 32px tall, got {trigger_height}px"
    );
    cx.simulate_click(bounds_center(trigger), Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("settings-picker-option-bar").is_some());
    assert!(
        cx.debug_bounds("settings-picker-option-description-bar")
            .is_some(),
        "choice descriptions are rendered in the picker option"
    );
    assert!(
        cx.debug_bounds("settings-select-settings-choice-cursor.style")
            .is_some(),
        "short enums use gpui-component Select"
    );
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));

    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();

    probe.update(cx, |probe, _| {
        assert!(probe.intents.borrow().iter().any(|intent| matches!(
            intent,
            SettingsIntent::SetValue {
                id,
                value: ScalarValue::Token(value),
            } if id == "cursor.style" && value == "bar"
        )));
    });
}

#[gpui_kit::test]
fn modifier_remaps_use_two_stable_selects_and_emit_typed_edits(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) =
        cx.add_window_view(|_, cx| ChoiceProbe::with_snapshot(modifier_remap_snapshot(), cx));

    let source = cx
        .debug_bounds("settings-modifier-remap-0-source")
        .expect("source modifier Select is rendered");
    let source_width: f32 = source.size.width.into();
    assert!(
        (source_width - 118.0).abs() <= 1.0,
        "modifier Selects use the compact 118px width, got {source_width}px"
    );
    assert!(
        cx.debug_bounds("settings-select-settings-modifier-remap-0-source")
            .is_some(),
        "modifier sources use gpui-component Select"
    );
    assert!(
        cx.debug_bounds("settings-select-settings-modifier-remap-0-target")
            .is_some(),
        "modifier targets use gpui-component Select"
    );

    cx.simulate_click(bounds_center(source), Modifiers::none());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("settings-picker-option-right_shift")
            .is_some()
    );
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();

    probe.update(cx, |probe, _| {
        assert!(probe.intents.borrow().iter().any(|intent| matches!(
            intent,
            SettingsIntent::SetModifierRemap {
                index: 0,
                field: ModifierRemapField::Source,
                value,
            } if value == "right_shift"
        )));
    });
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("settings-picker-option-right_shift")
            .is_some(),
        "confirming a modifier restores focus to its Select trigger"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    let target = cx
        .debug_bounds("settings-modifier-remap-0-target")
        .expect("target trigger remains available");
    cx.simulate_click(bounds_center(target), Modifiers::none());
    cx.run_until_parked();
    cx.simulate_click(point(px(0.0), px(0.0)), Modifiers::none());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("settings-picker-option-right_shift")
            .is_none(),
        "clicking outside dismisses the modifier Select"
    );
}

#[gpui_kit::test]
fn modifier_remap_list_actions_are_typed(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) =
        cx.add_window_view(|_, cx| ChoiceProbe::with_snapshot(modifier_remap_snapshot(), cx));

    for selector in [
        "settings-modifier-remap-0-drag-handle",
        "settings-modifier-remap-0-remove",
        "settings-modifier-remap-add",
    ] {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is rendered"));
        cx.simulate_click(bounds_center(bounds), Modifiers::none());
        cx.run_until_parked();
    }

    probe.update(cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(
            intents
                .iter()
                .all(|intent| !matches!(intent, SettingsIntent::MoveModifierRemap { .. }))
        );
        assert!(
            intents
                .iter()
                .any(|intent| matches!(intent, SettingsIntent::RemoveModifierRemap(0)))
        );
        assert!(
            intents
                .iter()
                .any(|intent| matches!(intent, SettingsIntent::AddModifierRemap))
        );
    });
}

#[gpui_kit::test]
fn long_choices_use_the_searchable_picker_and_confirm_the_filtered_value(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) =
        cx.add_window_view(|_, cx| ChoiceProbe::with_snapshot(long_choice_snapshot(), cx));
    let trigger = cx
        .debug_bounds("settings-choice-terminal.catalog")
        .expect("long choice is rendered");
    cx.simulate_click(bounds_center(trigger), Modifiers::none());
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("settings-picker-option-cascadia-code")
            .is_some(),
        "long catalogs open the searchable settings picker"
    );
    assert!(
        cx.debug_bounds("settings-select-settings-choice-terminal.catalog")
            .is_some(),
        "large catalogs use the shared accessible Select"
    );
    cx.simulate_input("JetBrains");
    assert!(
        cx.debug_bounds("settings-picker-option-jetbrains-mono")
            .is_some(),
        "filtering keeps matching catalog entries"
    );
    assert!(
        cx.debug_bounds("settings-picker-option-cascadia-code")
            .is_none(),
        "filtering removes non-matching catalog entries"
    );
    cx.simulate_keystrokes("down enter");

    probe.update(cx, |probe, _| {
        let matches = probe
            .intents
            .borrow()
            .iter()
            .filter(|intent| {
                matches!(
                    intent,
                    SettingsIntent::SetValue {
                        id,
                        value: ScalarValue::Token(value),
                    } if id == "terminal.catalog" && value == "jetbrains-mono"
                )
            })
            .count();
        assert_eq!(matches, 1, "selection emits exactly one typed intent");
    });
}

#[gpui_kit::test]
fn font_weight_pickers_submit_the_selected_face(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let mut snapshot = font_stack_snapshot();
    for item in &mut snapshot.pages[0].items {
        if let SettingsPageItem::Setting(SettingsRow::StringList { items, .. }) = item {
            *items = vec!["Lilex-Regular".to_owned()];
        }
    }
    let (probe, cx) = cx.add_window_view(|_, cx| ChoiceProbe::with_snapshot(snapshot, cx));
    for (id, selector) in [
        ("font.family", "settings-font-weight-font.family-0"),
        ("font.ui-family", "settings-font-weight-font.ui-family-0"),
    ] {
        let trigger = cx.debug_bounds(selector).expect("font weight trigger");
        cx.simulate_click(bounds_center(trigger), Modifiers::none());
        cx.run_until_parked();
        let bold = cx
            .debug_bounds("settings-picker-option-Lilex-Bold")
            .expect("bold face option");
        cx.simulate_click(bounds_center(bold), Modifiers::none());
        cx.run_until_parked();
        probe.update(cx, |probe, _| {
            assert!(probe.intents.borrow().iter().any(|intent| matches!(
                intent,
                SettingsIntent::SetStringListItem { id: actual, index: 0, value }
                    if actual == id && value == "Lilex-Bold"
            )));
        });
    }
}

#[gpui_kit::test]
fn terminal_and_ui_font_stacks_use_the_searchable_picker_for_large_font_catalogs(
    cx: &mut TestAppContext,
) {
    init_zed_ui(cx);
    let (_, cx) = cx.add_window_view(|_, cx| ChoiceProbe::with_snapshot(font_stack_snapshot(), cx));

    let terminal = cx
        .debug_bounds("settings-string-list-font.family-0")
        .expect("terminal font entry is rendered");
    cx.simulate_click(bounds_center(terminal), Modifiers::none());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("settings-picker-option-Cascadia Code")
            .is_some(),
        "terminal font catalogs are searchable"
    );
    cx.simulate_click(point(px(0.0), px(0.0)), Modifiers::none());
    cx.run_until_parked();

    let ui = cx
        .debug_bounds("settings-string-list-font.ui-family-0")
        .expect("UI font entry is rendered");
    cx.simulate_click(bounds_center(ui), Modifiers::none());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("settings-picker-option-JetBrains Mono")
            .is_some(),
        "UI font catalogs are searchable"
    );
}

#[gpui_kit::test]
fn empty_font_fallback_uses_a_real_searchable_trigger(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let mut snapshot = font_stack_snapshot();
    let SettingsPageItem::Setting(SettingsRow::StringList { items, .. }) =
        &mut snapshot.pages[0].items[1]
    else {
        panic!("font stack snapshot should contain a string list");
    };
    items.push(String::new());

    let (_, cx) = cx.add_window_view(|_, cx| ChoiceProbe::with_snapshot(snapshot, cx));
    let trigger = cx
        .debug_bounds("settings-string-list-font.family-1")
        .expect("empty font fallback still renders a searchable trigger");
    cx.simulate_click(bounds_center(trigger), Modifiers::none());
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("settings-picker-option-Cascadia Code")
            .is_some(),
        "an empty fallback opens the font catalog instead of leaking a component key"
    );
}

#[gpui_kit::test]
fn choice_menu_dismisses_without_changing_the_setting(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| ChoiceProbe::new(cx));
    let trigger = cx
        .debug_bounds("settings-choice-cursor.style")
        .expect("choice trigger is rendered");
    cx.simulate_click(bounds_center(trigger), Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("settings-picker-option-bar").is_some());
    cx.simulate_click(point(px(0.0), px(0.0)), Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("settings-picker-option-bar").is_none());
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
}

#[gpui_kit::test]
fn choice_menu_dismisses_on_escape_without_changing_the_setting(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| ChoiceProbe::new(cx));
    let trigger = cx
        .debug_bounds("settings-choice-cursor.style")
        .expect("choice trigger is rendered");
    cx.simulate_click(bounds_center(trigger), Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("settings-picker-option-bar").is_some());

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(cx.debug_bounds("settings-picker-option-bar").is_none());
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
}

#[gpui_kit::test]
fn theme_picker_filters_navigates_and_confirms(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| ChoiceProbe::with_snapshot(theme_snapshot(), cx));
    let trigger = cx
        .debug_bounds("settings-choice-theme")
        .expect("theme picker trigger is rendered");
    let trigger_height: f32 = trigger.size.height.into();
    assert!(
        (trigger_height - 32.0).abs() <= 1.0,
        "gpui-component medium Select triggers are 32px tall, got {trigger_height}px"
    );
    cx.simulate_click(bounds_center(trigger), Modifiers::none());
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("settings-picker-option-one-light")
            .is_some(),
        "theme picker renders the current theme"
    );

    cx.simulate_input("solarized light");
    assert!(
        cx.debug_bounds("settings-picker-option-solarized-light")
            .is_some(),
        "typing filters the theme list"
    );
    assert!(
        cx.debug_bounds("settings-picker-option-one-light")
            .is_none(),
        "non-matching themes leave the filtered list"
    );
    let option = cx
        .debug_bounds("settings-picker-option-solarized-light")
        .expect("filtered theme option remains pointer-reachable");
    cx.simulate_click(bounds_center(option), Modifiers::none());
    cx.run_until_parked();

    probe.update(cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(
            intents.iter().any(|intent| matches!(
                intent,
                SettingsIntent::SetValue {
                    id,
                    value: ScalarValue::Token(value),
                } if id == "theme" && value == "solarized-light"
            )),
            "unexpected intents: {intents:#?}"
        );
    });
}

#[gpui_kit::test]
fn theme_picker_width_tracks_the_window_rem_size(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    cx.update(|cx| update_ui_font(&[], 20.0, cx));
    let (_, cx) = cx.add_window_view(|_, cx| ChoiceProbe::with_snapshot(theme_snapshot(), cx));

    let trigger = cx
        .debug_bounds("settings-choice-theme")
        .expect("theme picker trigger is rendered");
    let trigger_width: f32 = trigger.size.width.into();
    assert!(
        (trigger_width - 262.5).abs() <= 1.0,
        "13.125rem select width should scale with a 20px rem, got {trigger_width}px"
    );
}

#[gpui_kit::test]
fn theme_picker_dismissal_restores_the_accepted_value_and_clears_search(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| ChoiceProbe::with_snapshot(theme_snapshot(), cx));
    for (query, click_away) in [
        ("ayu", false),
        ("ayu", true),
        ("ayu light", false),
        ("ayu light", true),
        ("no matching theme", false),
        ("no matching theme", true),
    ] {
        let trigger = cx
            .debug_bounds("settings-choice-theme")
            .expect("theme picker trigger is rendered");
        cx.simulate_click(bounds_center(trigger), Modifiers::none());
        assert!(
            cx.debug_bounds("settings-picker-option-one-light")
                .is_some()
        );
        cx.simulate_input(query);
        assert!(
            cx.debug_bounds("settings-picker-option-one-light")
                .is_none()
        );

        if click_away {
            cx.simulate_click(point(px(0.0), px(0.0)), Modifiers::none());
        } else {
            cx.simulate_keystrokes("escape");
        }
        assert!(
            cx.debug_bounds("settings-picker-option-ayu-light")
                .is_none()
        );
        probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));

        cx.simulate_keystrokes("enter");
        assert!(
            cx.debug_bounds("settings-picker-option-one-light")
                .is_some(),
            "dismissal restores trigger focus and reopening shows the unfiltered catalog"
        );
        cx.simulate_keystrokes("enter");
        probe.update(cx, |probe, _| {
            let intents = probe.intents.borrow();
            assert!(
                matches!(
                    intents.as_slice(),
                    [SettingsIntent::SetValue { id, value: ScalarValue::Token(value) }]
                        if id == "theme" && value == "one-light"
                ),
                "dismissal must restore the accepted selection, query={query:?}, click_away={click_away}, intents={intents:#?}"
            );
            drop(intents);
            probe.intents.borrow_mut().clear();
        });
    }

    let trigger = cx
        .debug_bounds("settings-choice-theme")
        .expect("theme trigger remains after query cancellation");
    cx.simulate_click(bounds_center(trigger), Modifiers::none());
    cx.simulate_input("ayu light");
    cx.simulate_keystrokes("enter");
    probe.update(cx, |probe, _| {
        assert!(matches!(
            probe.intents.borrow().as_slice(),
            [SettingsIntent::SetValue { id, value: ScalarValue::Token(value) }]
                if id == "theme" && value == "ayu-light"
        ));
    });
}

fn theme_snapshot() -> GpuiSettingsSnapshot {
    let choices = [
        ("one-dark", "One Dark"),
        ("one-light", "One Light"),
        ("solarized-dark", "Solarized Dark"),
        ("solarized-light", "Solarized Light"),
        ("ayu-dark", "Ayu Dark"),
        ("ayu-light", "Ayu Light"),
        ("catppuccin-mocha", "Catppuccin Mocha"),
        ("catppuccin-latte", "Catppuccin Latte"),
        ("dracula", "Dracula"),
        ("gruvbox-dark", "Gruvbox Dark"),
        ("gruvbox-light", "Gruvbox Light"),
        ("nord", "Nord"),
        ("rose-pine", "Rosé Pine"),
        ("tokyo-night", "Tokyo Night"),
        ("everforest", "Everforest"),
        ("monokai", "Monokai"),
    ]
    .into_iter()
    .map(|(token, label)| SettingsChoice {
        token: token.to_owned(),
        label: label.to_owned(),
        description: Some(format!("{label} terminal and application palette.")),
    })
    .collect();

    GpuiSettingsSnapshot {
        category: SettingsCategory::Appearance,
        pages: vec![SettingsPage {
            category: SettingsCategory::Appearance,
            title: "Appearance".to_owned(),
            search_terms: "appearance theme colors".to_owned(),
            items: vec![
                SettingsPageItem::SectionHeader {
                    id: "appearance".to_owned(),
                    title: "Appearance".to_owned(),
                    search_terms: "theme colors".to_owned(),
                },
                SettingsPageItem::Setting(SettingsRow::Value {
                    id: "theme".to_owned(),
                    label: "Theme".to_owned(),
                    help: "Choose the application theme.".to_owned(),
                    value: ScalarValue::Token("one-light".to_owned()),
                    control: SettingsControl::Theme(choices),
                    enabled: true,
                }),
            ],
        }],
        search: String::new(),
        write_error: None,
    }
}

fn long_choice_snapshot() -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category: SettingsCategory::Terminal,
        pages: vec![SettingsPage {
            category: SettingsCategory::Terminal,
            title: "Terminal".to_owned(),
            search_terms: "terminal catalog".to_owned(),
            items: vec![
                SettingsPageItem::SectionHeader {
                    id: "terminal".to_owned(),
                    title: "Terminal".to_owned(),
                    search_terms: "terminal catalog".to_owned(),
                },
                SettingsPageItem::Setting(SettingsRow::Value {
                    id: "terminal.catalog".to_owned(),
                    label: "Terminal catalog".to_owned(),
                    help: "Choose a terminal catalog entry.".to_owned(),
                    value: ScalarValue::Token("cascadia-code".to_owned()),
                    control: SettingsControl::Choice(long_catalog()),
                    enabled: true,
                }),
            ],
        }],
        search: String::new(),
        write_error: None,
    }
}

fn font_stack_snapshot() -> GpuiSettingsSnapshot {
    let options = long_catalog()
        .into_iter()
        .map(|choice| choice.label)
        .collect::<Vec<_>>();
    GpuiSettingsSnapshot {
        category: SettingsCategory::Appearance,
        pages: vec![SettingsPage {
            category: SettingsCategory::Appearance,
            title: "Appearance".to_owned(),
            search_terms: "appearance fonts".to_owned(),
            items: vec![
                SettingsPageItem::SectionHeader {
                    id: "fonts".to_owned(),
                    title: "Fonts".to_owned(),
                    search_terms: "fonts".to_owned(),
                },
                SettingsPageItem::Setting(SettingsRow::StringList {
                    id: "font.family".to_owned(),
                    label: "Terminal font stack".to_owned(),
                    help: "Terminal fonts in fallback order.".to_owned(),
                    items: vec!["Cascadia Code".to_owned()],
                    options: options.clone(),
                    add_label: "+ Add terminal fallback".to_owned(),
                    enabled: true,
                }),
                SettingsPageItem::Setting(SettingsRow::StringList {
                    id: "font.ui-family".to_owned(),
                    label: "UI font stack".to_owned(),
                    help: "UI fonts in fallback order.".to_owned(),
                    items: vec!["JetBrains Mono".to_owned()],
                    options,
                    add_label: "+ Add UI fallback".to_owned(),
                    enabled: true,
                }),
            ],
        }],
        search: String::new(),
        write_error: None,
    }
}

fn long_catalog() -> Vec<SettingsChoice> {
    [
        ("atkinson-hyperlegible-mono", "Atkinson Hyperlegible Mono"),
        ("cascadia-code", "Cascadia Code"),
        ("commit-mono", "Commit Mono"),
        ("courier-prime", "Courier Prime"),
        ("fira-code", "Fira Code"),
        ("geist-mono", "Geist Mono"),
        ("hack", "Hack"),
        ("ibm-plex-mono", "IBM Plex Mono"),
        ("inconsolata", "Inconsolata"),
        ("iosevka", "Iosevka"),
        ("jetbrains-mono", "JetBrains Mono"),
        ("menlo", "Menlo"),
        ("monaspace", "Monaspace"),
        ("operator-mono", "Operator Mono"),
        ("source-code-pro", "Source Code Pro"),
        ("victor-mono", "Victor Mono"),
    ]
    .into_iter()
    .map(|(token, label)| SettingsChoice {
        token: token.to_owned(),
        label: label.to_owned(),
        description: None,
    })
    .collect()
}

fn bounds_center(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> gpui_kit::Point<gpui_kit::Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(px(left + width / 2.0), px(top + height / 2.0))
}
