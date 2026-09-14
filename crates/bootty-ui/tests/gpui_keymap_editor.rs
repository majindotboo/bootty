#![cfg(test)]

use bootty_ui::gpui as bootty_gpui;
use gpui_kit::test::TestWindowExt as _;
use std::{cell::RefCell, rc::Rc};

use bootty_config::keymap_file::KeymapBindingKind;
use bootty_ui::gpui::{
    GpuiKeymapEditor, KeymapActionSnapshot, KeymapArgumentKind, KeymapArgumentSnapshot,
    KeymapBindingSnapshot, KeymapBindingSource, KeymapContextSnapshot, KeymapEditorIntent,
    KeymapEditorRow, KeymapEditorSnapshot, KeymapSearchMode, KeymapSourceFilters,
    KeymapTriggerOptions, UiPalette, init_theme,
};
use gpui_kit::component::{Root, WindowExt as _};
use gpui_kit::{
    AppContext as _, Context, Entity, FocusHandle, Half, IntoElement, Modifiers, MouseButton,
    Render, ScrollDelta, ScrollWheelEvent, Subscription, TestAppContext, TouchPhase,
    VisualTestContext, Window, div, point, prelude::*, px,
};
use pretty_assertions::assert_eq;

#[test]
fn keybinding_parser_preserves_literal_greater_than_and_chord_separators() {
    let unmodified_literal =
        bootty_gpui::parse_keybinding(">").expect("unmodified greater-than key");
    assert_eq!(unmodified_literal.len(), 1);
    assert_eq!(unmodified_literal[0].inner().key, ">");
    assert!(!unmodified_literal[0].inner().modifiers.shift);

    let literal = bootty_gpui::parse_keybinding("shift+>").expect("literal greater-than key");
    assert_eq!(literal.len(), 1);
    assert_eq!(literal[0].inner().key, ">");
    assert!(literal[0].inner().modifiers.shift);

    let compact_chord = bootty_gpui::parse_keybinding("ctrl+a>ctrl+b").expect("compact chord");
    assert_eq!(compact_chord.len(), 2);

    let spaced_chord = bootty_gpui::parse_keybinding("cmd+k > cmd+p").expect("spaced chord");
    assert_eq!(spaced_chord.len(), 2);
}

#[gpui_kit::test]
fn consume_and_unbind_rows_stay_visible_with_exact_targets(cx: &mut TestAppContext) {
    initialize(cx);
    let mut projected = snapshot();
    projected.actions.push(KeymapActionSnapshot {
        id: "ignore".to_owned(),
        title: "Ignore".to_owned(),
        description: "Consume the trigger without invoking an action.".to_owned(),
        arguments: Vec::new(),
    });
    projected.bindings.extend([
        KeymapBindingSnapshot {
            id: "user:unbound".to_owned(),
            action: "new_tab".to_owned(),
            arguments_json: None,
            keystrokes: vec!["cmd+n".to_owned()],
            persisted_keystrokes: "cmd+n".to_owned(),
            context: "Global".to_owned(),
            source: KeymapBindingSource::User,
            kind: KeymapBindingKind::Unbind,
            trigger_options: KeymapTriggerOptions::default(),
            conflict_count: 0,
        },
        KeymapBindingSnapshot {
            id: "user:consume".to_owned(),
            action: "ignore".to_owned(),
            arguments_json: None,
            keystrokes: vec!["cmd+escape".to_owned()],
            persisted_keystrokes: "cmd+escape".to_owned(),
            context: "Global".to_owned(),
            source: KeymapBindingSource::User,
            kind: KeymapBindingKind::Binding,
            trigger_options: KeymapTriggerOptions::default(),
            conflict_count: 0,
        },
    ]);
    let editor = cx.update(|cx| cx.new(|cx| GpuiKeymapEditor::new(projected, cx)));

    editor.update(cx, |editor, _| {
        let rows = editor
            .visible_rows()
            .iter()
            .filter_map(KeymapEditorRow::binding)
            .collect::<Vec<_>>();
        let unbound = rows
            .iter()
            .find(|binding| binding.id == "user:unbound")
            .expect("unbind rows remain visible for restoration");
        assert_eq!(unbound.kind, KeymapBindingKind::Unbind);
        assert_eq!(unbound.target().kind, KeymapBindingKind::Unbind);

        let consume = rows
            .iter()
            .find(|binding| binding.id == "user:consume")
            .expect("null bindings remain visible as consume rows");
        assert_eq!(consume.action, "ignore");
        assert_eq!(consume.target().kind, KeymapBindingKind::Binding);
    });
}

struct KeymapProbe {
    editor: Entity<GpuiKeymapEditor>,
    intents: Rc<RefCell<Vec<KeymapEditorIntent>>>,
    _subscription: Subscription,
}

impl KeymapProbe {
    fn with_snapshot(snapshot: KeymapEditorSnapshot, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| GpuiKeymapEditor::new(snapshot, cx));
        let intents = Rc::new(RefCell::new(Vec::new()));
        let received = Rc::clone(&intents);
        let subscription = cx.subscribe(&editor, move |_, _, intent, _| {
            received.borrow_mut().push(intent.clone());
        });
        Self {
            editor,
            intents,
            _subscription: subscription,
        }
    }
}

impl Render for KeymapProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.editor.clone()
    }
}

struct ModalFocusProbe {
    editor: Entity<GpuiKeymapEditor>,
    invoker_focus: FocusHandle,
    background_focus: FocusHandle,
}

impl ModalFocusProbe {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            editor: cx.new(|cx| GpuiKeymapEditor::new(snapshot(), cx)),
            invoker_focus: cx.focus_handle(),
            background_focus: cx.focus_handle(),
        }
    }
}

impl Render for ModalFocusProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let pointer_editor = self.editor.clone();
        let keyboard_editor = self.editor.clone();
        div()
            .size_full()
            .child(
                div()
                    .id("keymap-modal-invoker")
                    .debug_selector(|| "keymap-modal-invoker".to_owned())
                    .track_focus(&self.invoker_focus)
                    .on_click(move |_, window, cx| {
                        pointer_editor.update(cx, |editor, cx| {
                            editor.focus_action("create_space", window, cx);
                        });
                    })
                    .on_key_down(move |event, window, cx| {
                        if event.keystroke.key == "enter" {
                            keyboard_editor.update(cx, |editor, cx| {
                                editor.focus_action("create_space", window, cx);
                            });
                            cx.stop_propagation();
                        }
                    })
                    .child("Change Create Space keybinding"),
            )
            .child(self.editor.clone())
            .child(
                div()
                    .id("keymap-modal-background-focus")
                    .track_focus(&self.background_focus)
                    .child("Background focus target"),
            )
    }
}

struct RootLayers<V> {
    view: Entity<V>,
}

impl<V: Render + 'static> Render for RootLayers<V> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sheet_layer = Root::render_sheet_layer(window, cx);
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);
        div()
            .relative()
            .size_full()
            .child(self.view.clone())
            .children(sheet_layer)
            .children(dialog_layer)
            .children(notification_layer)
    }
}

fn rooted_modal_focus_probe(
    cx: &TestAppContext,
) -> (Entity<ModalFocusProbe>, gpui_kit::VisualTestContext) {
    let probe_slot = Rc::new(RefCell::new(None));
    let opened_probe = Rc::clone(&probe_slot);
    let window = cx.update(|cx| {
        cx.open_window(gpui_kit::WindowOptions::default(), move |window, cx| {
            let probe = cx.new(ModalFocusProbe::new);
            opened_probe.replace(Some(probe.clone()));
            let layers = cx.new(|_| RootLayers { view: probe });
            cx.new(|cx| Root::new(layers, window, cx).bordered(false))
        })
        .expect("open rooted keymap editor")
    });
    let probe = probe_slot
        .borrow_mut()
        .take()
        .expect("capture keymap focus probe");
    let cx = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    (probe, cx)
}

fn rooted_keymap_probe(cx: &mut TestAppContext) -> (Entity<KeymapProbe>, &mut VisualTestContext) {
    rooted_keymap_probe_with_snapshot(cx, snapshot())
}

fn rooted_keymap_probe_with_snapshot(
    cx: &mut TestAppContext,
    snapshot: KeymapEditorSnapshot,
) -> (Entity<KeymapProbe>, &mut VisualTestContext) {
    let probe_slot = Rc::new(RefCell::new(None));
    let opened_probe = Rc::clone(&probe_slot);
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let probe = cx.new(|cx| KeymapProbe::with_snapshot(snapshot, cx));
        opened_probe.replace(Some(probe.clone()));
        let layers = cx.new(|_| RootLayers { view: probe });
        Root::new(layers, window, cx).bordered(false)
    });
    let probe = probe_slot
        .borrow_mut()
        .take()
        .expect("capture keymap probe");
    (probe, cx)
}

fn snapshot() -> KeymapEditorSnapshot {
    KeymapEditorSnapshot {
        path: "/tmp/keymap.json".to_owned(),
        prefix: Some("ctrl+a".to_owned()),
        actions: vec![
            KeymapActionSnapshot {
                id: "new_tab".to_owned(),
                title: "New Tab".to_owned(),
                description: "Open a new tab.".to_owned(),
                arguments: Vec::new(),
            },
            KeymapActionSnapshot {
                id: "create_space".to_owned(),
                title: "Create Space".to_owned(),
                description: "Create a persisted workspace.".to_owned(),
                arguments: Vec::new(),
            },
            KeymapActionSnapshot {
                id: "select_tab".to_owned(),
                title: "Select Tab".to_owned(),
                description: "Select a tab by index.".to_owned(),
                arguments: vec![KeymapArgumentSnapshot {
                    name: "index".to_owned(),
                    kind: KeymapArgumentKind::Integer,
                    required: true,
                    choices: Vec::new(),
                    minimum: Some(1),
                    maximum: Some(9),
                }],
            },
        ],
        bindings: vec![
            KeymapBindingSnapshot {
                id: "default:new-tab".to_owned(),
                action: "new_tab".to_owned(),
                arguments_json: None,
                keystrokes: vec!["cmd+t".to_owned()],
                persisted_keystrokes: "cmd+t".to_owned(),
                context: "Global".to_owned(),
                source: KeymapBindingSource::Default,
                kind: KeymapBindingKind::Binding,
                trigger_options: KeymapTriggerOptions::default(),
                conflict_count: 1,
            },
            KeymapBindingSnapshot {
                id: "user:new-tab".to_owned(),
                action: "new_tab".to_owned(),
                arguments_json: None,
                keystrokes: vec!["cmd+w".to_owned()],
                persisted_keystrokes: "cmd+w".to_owned(),
                context: "Terminal".to_owned(),
                source: KeymapBindingSource::User,
                kind: KeymapBindingKind::Binding,
                trigger_options: KeymapTriggerOptions::default(),
                conflict_count: 1,
            },
        ],
        contexts: vec![
            KeymapContextSnapshot {
                id: "Global".to_owned(),
                label: "Global".to_owned(),
                description: "Every Bootty surface.".to_owned(),
                use_builtin_defaults: true,
            },
            KeymapContextSnapshot {
                id: "Terminal".to_owned(),
                label: "Terminal".to_owned(),
                description: "The focused terminal.".to_owned(),
                use_builtin_defaults: false,
            },
        ],
        diagnostic: None,
        revision: 7,
    }
}

fn action(id: &str, title: &str) -> KeymapActionSnapshot {
    KeymapActionSnapshot {
        id: id.to_owned(),
        title: title.to_owned(),
        description: format!("Run {title}."),
        arguments: Vec::new(),
    }
}

fn initialize(cx: &TestAppContext) {
    cx.update(|cx| {
        // Pointer tests need final dialog geometry, independent of wall-clock animation progress.
        cx.set_reduce_motion(true);
        init_theme(UiPalette::default(), cx);
    });
}

#[gpui_kit::test]
fn combines_effective_bindings_with_every_unmapped_command(cx: &mut TestAppContext) {
    initialize(cx);
    let editor = cx.update(|cx| cx.new(|cx| GpuiKeymapEditor::new(snapshot(), cx)));

    editor.update(cx, |editor, cx| {
        let rows = editor.visible_rows();
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows.iter()
                .filter_map(|row| match row {
                    KeymapEditorRow::Unmapped(action) => Some(action.id.as_str()),
                    KeymapEditorRow::Binding { .. } => None,
                })
                .collect::<Vec<_>>(),
            vec!["create_space", "select_tab"]
        );

        editor.set_search("create_space", cx);
        assert_eq!(editor.visible_rows().len(), 1);
        assert!(matches!(
            editor.visible_rows(),
            [KeymapEditorRow::Unmapped(action)] if action.id == "create_space"
        ));

        editor.set_search_mode(KeymapSearchMode::Keystroke, cx);
        editor.set_search("cmd+t", cx);
        assert_eq!(editor.visible_rows().len(), 1);

        editor.set_source_filters(
            KeymapSourceFilters {
                defaults: false,
                user: true,
                unmapped: false,
            },
            cx,
        );
        editor.set_search("cmd+w", cx);
        assert_eq!(editor.visible_rows().len(), 1);
        assert!(matches!(
            editor.visible_rows(),
            [KeymapEditorRow::Binding { binding, .. }]
                if binding.source == KeymapBindingSource::User
        ));

        editor.set_source_filters(KeymapSourceFilters::default(), cx);
        editor.set_search("", cx);
        editor.set_conflicts_only(true, cx);
        assert_eq!(editor.visible_rows().len(), 2);
        assert!(
            editor
                .visible_rows()
                .iter()
                .all(|row| row.conflict_count() == 1)
        );
    });
}

#[gpui_kit::test]
fn action_search_normalizes_ids_and_ranks_non_contiguous_fuzzy_matches(cx: &mut TestAppContext) {
    initialize(cx);
    let editor = cx.update(|cx| {
        cx.new(|cx| {
            GpuiKeymapEditor::new(
                KeymapEditorSnapshot {
                    actions: vec![
                        action("toggle_sidebar", "Toggle Sidebar"),
                        action("open_sidebar", "Open Sidebar"),
                        action("sidebar_toggle", "Sidebar Toggle"),
                    ],
                    ..KeymapEditorSnapshot::default()
                },
                cx,
            )
        })
    });

    editor.update(cx, |editor, cx| {
        editor.set_search("sbar", cx);
        assert_eq!(
            editor
                .visible_rows()
                .iter()
                .map(|row| row.action().id.as_str())
                .collect::<Vec<_>>(),
            ["sidebar_toggle", "open_sidebar", "toggle_sidebar"],
            "non-contiguous fuzzy matches are ordered by match quality"
        );

        editor.set_search("sidebar_toggle", cx);
        assert_eq!(
            editor
                .visible_rows()
                .iter()
                .map(|row| row.action().id.as_str())
                .collect::<Vec<_>>(),
            ["sidebar_toggle"],
            "Zed-style query normalization maps action IDs to humanized titles"
        );
    });
}

#[gpui_kit::test]
fn large_catalog_render_reads_reuse_the_projected_row_cache(cx: &mut TestAppContext) {
    initialize(cx);
    let action_count = 4_096;
    let actions = (0..action_count)
        .map(|index| KeymapActionSnapshot {
            id: format!("action_{index:04}"),
            title: format!("Action {index:04}"),
            description: format!("Execute catalog action {index}."),
            arguments: Vec::new(),
        })
        .collect::<Vec<_>>();
    let bindings = (0..action_count)
        .map(|index| KeymapBindingSnapshot {
            id: format!("binding_{index:04}"),
            action: format!("action_{index:04}"),
            arguments_json: None,
            keystrokes: vec![format!("ctrl-{}", index % 64)],
            persisted_keystrokes: format!("ctrl-{}", index % 64),
            context: if index % 8 == 0 {
                "Global".to_owned()
            } else {
                format!("Pane{}", index % 4)
            },
            source: KeymapBindingSource::Default,
            kind: KeymapBindingKind::Binding,
            trigger_options: KeymapTriggerOptions::default(),
            conflict_count: 0,
        })
        .collect::<Vec<_>>();
    let editor = cx.update(|cx| {
        cx.new(|cx| {
            GpuiKeymapEditor::new(
                KeymapEditorSnapshot {
                    actions,
                    bindings,
                    ..KeymapEditorSnapshot::default()
                },
                cx,
            )
        })
    });

    editor.update(cx, |editor, cx| {
        let rows = editor.visible_rows();
        assert_eq!(rows.len(), action_count);
        let cached_rows = rows.as_ptr();
        assert!(rows.iter().all(|row| row.conflict_count() > 0));

        for _ in 0..10_000 {
            let render_read = editor.visible_rows();
            assert_eq!(render_read.as_ptr(), cached_rows);
            assert_eq!(render_read.len(), action_count);
        }

        editor.set_search("action_2048", cx);
        assert!(matches!(
            editor.visible_rows(),
            [KeymapEditorRow::Binding { action, .. }] if action.id == "action_2048"
        ));
    });
}

#[gpui_kit::test]
fn renders_compact_toolbar_filters_and_six_column_table(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);

    for selector in [
        "keymap-search-action",
        "keymap-search-keystroke",
        "keymap-filter-menu",
        "keymap-use-built-in-defaults",
        "keymap-edit-json",
        "keymap-create",
        "keymap-table",
        "keymap-column-edit",
        "keymap-column-action",
        "keymap-column-arguments",
        "keymap-column-keystrokes",
        "keymap-column-context",
        "keymap-column-source",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "Keymap editor exposes {selector}"
        );
    }

    let action_search = cx
        .debug_bounds("keymap-search-action")
        .expect("action filter is visible");
    let table = cx
        .debug_bounds("keymap-table")
        .expect("keymap table is visible");
    assert!(
        bounds_bottom(action_search) <= bounds_top(table),
        "the filter toolbar precedes the dense keymap table"
    );
    assert_eq!(
        bounds_left(action_search),
        bounds_left(table),
        "Zed's editor body uses one compact padded column for the toolbar and table"
    );

    let action_column = cx
        .debug_bounds("keymap-column-action")
        .expect("Action header is visible");
    let context_column = cx
        .debug_bounds("keymap-column-context")
        .expect("Context header is visible");
    assert!(
        bounds_width(context_column) > bounds_width(action_column),
        "Zed reserves its widest redistributable column for keybinding contexts"
    );

    let filter = cx
        .debug_bounds("keymap-filter-menu")
        .expect("filter popover trigger is visible");
    cx.simulate_click(bounds_center(filter), Modifiers::none());
    for (index, label) in ["Conflicts", "No Action", "User", "Default"]
        .into_iter()
        .enumerate()
    {
        cx.update(|window, _| {
            assert_eq!(
                window
                    .within("popup-menu")
                    .find(index.checked_add(1).expect("popup item index fits"))
                    .label(),
                Some(label)
            );
        });
    }
    cx.simulate_click(point(px(0.0), px(0.0)), Modifiers::none());

    let edit_json = cx
        .debug_bounds("keymap-edit-json")
        .expect("Edit in JSON action is visible");
    cx.simulate_click(bounds_center(edit_json), Modifiers::none());
    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [KeymapEditorIntent::OpenKeymapFile]
        );
    });
}

#[gpui_kit::test]
fn use_builtin_defaults_menu_toggles_one_context_without_resetting_bindings(
    cx: &mut TestAppContext,
) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let defaults = cx
        .debug_bounds("keymap-use-built-in-defaults")
        .expect("built-in defaults action is visible");
    cx.simulate_click(bounds_center(defaults), Modifiers::none());

    cx.update(|window, cx| {
        let mut menu = window.within("popup-menu");
        assert_eq!(menu.find(2usize).label(), Some("Terminal"));
        menu.click(2usize, cx);
    });
    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [KeymapEditorIntent::SetBuiltInDefaults {
                context: "Terminal".to_owned(),
                enabled: true,
            }]
        );
    });
}

#[gpui_kit::test]
fn keystroke_search_uses_a_second_toolbar_row(cx: &mut TestAppContext) {
    initialize(cx);
    let (_probe, cx) = rooted_keymap_probe(cx);

    assert!(cx.debug_bounds("keymap-keystroke-input").is_none());
    let toggle = cx
        .debug_bounds("keymap-search-keystroke")
        .expect("keystroke search toggle is visible");
    cx.simulate_click(bounds_center(toggle), Modifiers::none());

    let action_search = cx
        .debug_bounds("keymap-search-action")
        .expect("action filter remains in the first toolbar row");
    let keystroke_input = cx
        .debug_bounds("keymap-keystroke-input")
        .expect("keystroke input is shown in a second toolbar row");
    assert!(bounds_bottom(action_search) <= bounds_top(keystroke_input));
    assert!(cx.debug_bounds("keymap-keystroke-record").is_some());
    assert!(cx.debug_bounds("keymap-search-exact").is_some());
}

#[gpui_kit::test]
fn filter_editor_drives_the_cached_action_rows(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let search = cx
        .debug_bounds("keymap-search-action")
        .expect("action filter is visible");
    cx.simulate_click(bounds_center(search), Modifiers::none());
    cx.simulate_keystrokes("c r e a t e");
    cx.run_until_parked();

    probe.update(cx, |probe, cx| {
        assert!(matches!(
            probe.editor.read(cx).visible_rows(),
            [KeymapEditorRow::Unmapped(action)] if action.id == "create_space"
        ));
    });
}

#[gpui_kit::test]
fn clicking_the_root_dialog_overlay_dismisses_without_emitting_an_edit(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let add = cx
        .debug_bounds("keymap-row-action-0")
        .expect("create binding action");
    cx.simulate_click(bounds_center(add), Modifiers::none());
    let outside = cx.update(|window, _| {
        let viewport = window.viewport_size();
        point(px(4.0), viewport.height.half())
    });
    cx.simulate_click(outside, Modifiers::none());

    assert!(cx.debug_bounds("keymap-modal").is_none());
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
}

#[gpui_kit::test]
fn root_dialog_traps_tab_focus_and_restores_its_exact_invoker(cx: &TestAppContext) {
    initialize(cx);
    let (probe, mut cx) = rooted_modal_focus_probe(cx);
    let invoker = cx
        .debug_bounds("keymap-modal-invoker")
        .expect("modal invoker is visible");
    cx.simulate_click(bounds_center(invoker), Modifiers::none());
    cx.run_until_parked();

    assert!(cx.debug_bounds("keymap-modal").is_some());
    cx.update(|window, cx| {
        assert!(window.has_active_dialog(cx));
        let probe = probe.read(cx);
        assert!(!probe.invoker_focus.is_focused(window));
        assert!(!probe.background_focus.is_focused(window));
    });

    for keystroke in std::iter::repeat_n("tab", 16).chain(std::iter::repeat_n("shift-tab", 16)) {
        cx.simulate_keystrokes(keystroke);
        cx.run_until_parked();
        cx.update(|window, cx| {
            let probe = probe.read(cx);
            assert!(
                !probe.invoker_focus.is_focused(window)
                    && !probe.background_focus.is_focused(window),
                "{keystroke} must keep focus inside the modal"
            );
        });
    }

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("keymap-modal").is_none());
    cx.update(|window, cx| {
        assert!(!window.has_active_dialog(cx));
        let probe = probe.read(cx);
        assert!(
            probe.invoker_focus.is_focused(window),
            "Escape restores the exact invoking control"
        );
        assert!(!probe.background_focus.is_focused(window));
    });

    cx.simulate_keystrokes("enter");
    cx.simulate_event(gpui_kit::KeyUpEvent {
        keystroke: gpui_kit::Keystroke::parse("enter").expect("Enter keystroke"),
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("keymap-modal-action").is_some(),
        "the restored invoking control remains keyboard-activatable"
    );

    let outside = cx.update(|window, _| {
        let viewport = window.viewport_size();
        point(px(4.0), viewport.height.half())
    });
    cx.simulate_click(outside, Modifiers::none());
    cx.run_until_parked();
    cx.update(|window, cx| {
        assert!(!window.has_active_dialog(cx));
        let probe = probe.read(cx);
        assert!(
            probe.invoker_focus.is_focused(window),
            "outside-click dismissal restores the same invoking control"
        );
        assert!(!probe.background_focus.is_focused(window));
    });
}

#[gpui_kit::test]
fn repeated_activation_keeps_the_existing_keybinding_dialog(cx: &TestAppContext) {
    initialize(cx);
    let (probe, mut cx) = rooted_modal_focus_probe(cx);
    let invoker = cx
        .debug_bounds("keymap-modal-invoker")
        .expect("modal invoker is visible");
    cx.simulate_click(bounds_center(invoker), Modifiers::none());
    cx.run_until_parked();

    cx.update(|window, cx| {
        let editor = probe.read(cx).editor.clone();
        editor.update(cx, |editor, cx| {
            editor.focus_action("select_tab", window, cx);
        });
    });

    probe.update(&mut cx, |probe, cx| {
        assert_eq!(
            probe
                .editor
                .read(cx)
                .modal_draft()
                .expect("the original modal remains open")
                .action,
            "create_space",
            "a repeated activation must not replace the active modal draft"
        );
    });
    assert!(cx.debug_bounds("keymap-modal").is_some());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("keymap-modal").is_none());
}

#[gpui_kit::test]
fn create_modal_searches_and_selects_stable_action_ids_with_the_combobox(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let create = cx
        .debug_bounds("keymap-create")
        .expect("Create Keybinding action");
    cx.simulate_click(bounds_center(create), Modifiers::none());

    let trigger = cx
        .debug_bounds("keymap-modal-action")
        .expect("create modal action combobox");
    cx.simulate_click(bounds_center(trigger), Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("keymap-action-option-new_tab").is_some());
    assert!(
        cx.debug_bounds("keymap-action-option-create_space")
            .is_some()
    );

    cx.simulate_input("Tab");
    assert!(cx.debug_bounds("keymap-action-option-new_tab").is_some());
    assert!(cx.debug_bounds("keymap-action-option-select_tab").is_some());
    assert!(
        cx.debug_bounds("keymap-action-option-create_space")
            .is_none(),
        "the actual Combobox filters the action catalog"
    );

    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    probe.update(cx, |probe, cx| {
        assert_eq!(
            probe
                .editor
                .read(cx)
                .modal_draft()
                .expect("create binding draft")
                .action,
            "select_tab",
            "selection persists Bootty's stable action ID, not its display title"
        );
    });
    assert!(cx.debug_bounds("keymap-action-option-select_tab").is_none());

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("keymap-action-option-select_tab").is_some(),
        "confirmation restores focus to the trigger, so Enter reopens the combobox"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("keymap-action-option-select_tab").is_none());
    assert!(
        cx.debug_bounds("keymap-modal").is_some(),
        "Escape dismisses the topmost combobox before the editor modal"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("keymap-modal").is_none(),
        "the next Escape dismisses the editor modal after the combobox restores trigger focus"
    );
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
}

#[gpui_kit::test]
fn create_modal_fuzzy_ranking_inserts_the_exact_action_id(cx: &mut TestAppContext) {
    initialize(cx);
    let mut custom_snapshot = snapshot();
    custom_snapshot.actions = vec![
        action("open_sidebar", "Open Sidebar"),
        action("sidebar_toggle", "Sidebar Toggle"),
        action("toggle_sidebar", "Toggle Sidebar"),
    ];
    custom_snapshot.bindings.clear();
    let (probe, cx) = rooted_keymap_probe_with_snapshot(cx, custom_snapshot);

    let create = cx
        .debug_bounds("keymap-create")
        .expect("Create Keybinding action");
    cx.simulate_click(bounds_center(create), Modifiers::none());
    let trigger = cx
        .debug_bounds("keymap-modal-action")
        .expect("create modal action combobox");
    cx.simulate_click(bounds_center(trigger), Modifiers::none());
    cx.run_until_parked();

    cx.simulate_input("sbar");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("keymap-action-option-sidebar_toggle")
            .is_some()
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    probe.update(cx, |probe, cx| {
        assert_eq!(
            probe
                .editor
                .read(cx)
                .modal_draft()
                .expect("create binding draft")
                .action,
            "sidebar_toggle",
            "the highest-ranked non-contiguous match inserts its stable catalog ID"
        );
    });
}

#[gpui_kit::test]
fn create_modal_caps_ranked_action_completions(cx: &mut TestAppContext) {
    initialize(cx);
    let mut custom_snapshot = snapshot();
    custom_snapshot.actions = (0..60)
        .map(|index| {
            action(
                &format!("catalog_action_{index:02}"),
                &format!("Catalog Action {index:02}"),
            )
        })
        .collect();
    custom_snapshot.bindings.clear();
    let (probe, cx) = rooted_keymap_probe_with_snapshot(cx, custom_snapshot);

    let create = cx
        .debug_bounds("keymap-create")
        .expect("Create Keybinding action");
    cx.simulate_click(bounds_center(create), Modifiers::none());
    let trigger = cx
        .debug_bounds("keymap-modal-action")
        .expect("create modal action combobox");
    cx.simulate_click(bounds_center(trigger), Modifiers::none());
    cx.run_until_parked();
    cx.simulate_input("catalog action");
    cx.run_until_parked();

    for _ in 0..50 {
        cx.simulate_keystrokes("down");
    }
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    probe.update(cx, |probe, cx| {
        assert_eq!(
            probe
                .editor
                .read(cx)
                .modal_draft()
                .expect("create binding draft")
                .action,
            "catalog_action_00",
            "fifty Down presses wrap to the first item when completions are capped at 50"
        );
    });
}

#[gpui_kit::test]
fn create_modal_selects_an_action_with_the_combobox_pointer_path(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let create = cx
        .debug_bounds("keymap-create")
        .expect("Create Keybinding action");
    cx.simulate_click(bounds_center(create), Modifiers::none());

    let trigger = cx
        .debug_bounds("keymap-modal-action")
        .expect("create modal action combobox");
    cx.simulate_click(bounds_center(trigger), Modifiers::none());
    cx.run_until_parked();

    let option = cx
        .debug_bounds("keymap-action-option-select_tab")
        .expect("Select Tab action option");
    cx.simulate_click(bounds_center(option), Modifiers::none());
    cx.run_until_parked();

    probe.update(cx, |probe, cx| {
        assert_eq!(
            probe
                .editor
                .read(cx)
                .modal_draft()
                .expect("create binding draft")
                .action,
            "select_tab",
            "pointer selection persists the stable action ID"
        );
    });
    assert!(cx.debug_bounds("keymap-action-option-select_tab").is_none());

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("keymap-action-option-select_tab").is_some(),
        "pointer confirmation restores focus to the trigger"
    );
}

#[gpui_kit::test]
fn modal_context_and_arguments_use_retained_components(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let create = cx
        .debug_bounds("keymap-create")
        .expect("Create Keybinding action");
    cx.simulate_click(bounds_center(create), Modifiers::none());

    let action = cx
        .debug_bounds("keymap-modal-action")
        .expect("create modal action combobox");
    cx.simulate_click(bounds_center(action), Modifiers::none());
    cx.run_until_parked();
    let select_tab = cx
        .debug_bounds("keymap-action-option-select_tab")
        .expect("Select Tab action option");
    cx.simulate_click(bounds_center(select_tab), Modifiers::none());
    cx.run_until_parked();

    assert!(cx.debug_bounds("keymap-modal-arguments").is_some());
    assert!(cx.debug_bounds("keymap-modal-context").is_some());

    let arguments = cx
        .debug_bounds("keymap-modal-arguments")
        .expect("JSON arguments editor");
    cx.simulate_click(bounds_center(arguments), Modifiers::none());
    cx.simulate_input(r#"{"index":2}"#);
    cx.run_until_parked();
    probe.update(cx, |probe, cx| {
        assert_eq!(
            probe
                .editor
                .read(cx)
                .modal_draft()
                .expect("create binding draft")
                .arguments_json
                .as_deref(),
            Some(r#"{"index":2}"#),
            "the retained JSON editor updates the typed draft"
        );
    });

    let context = cx
        .debug_bounds("keymap-modal-context")
        .expect("keybinding context editor");
    cx.simulate_click(bounds_center(context), Modifiers::none());
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    });
    cx.simulate_input("Terminal && backend == rmux");
    cx.run_until_parked();

    probe.update(cx, |probe, cx| {
        assert_eq!(
            probe
                .editor
                .read(cx)
                .modal_draft()
                .expect("create binding draft")
                .context,
            "Terminal && backend == rmux",
            "the retained editor accepts Zed-compatible context expressions"
        );
    });

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("keymap-modal").is_none());
}

#[gpui_kit::test]
fn arguments_editor_enter_inserts_json_newline_without_saving_modal(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let create = cx
        .debug_bounds("keymap-create")
        .expect("Create Keybinding action");
    cx.simulate_click(bounds_center(create), Modifiers::none());

    let action = cx
        .debug_bounds("keymap-modal-action")
        .expect("create modal action combobox");
    cx.simulate_click(bounds_center(action), Modifiers::none());
    cx.run_until_parked();
    let select_tab = cx
        .debug_bounds("keymap-action-option-select_tab")
        .expect("Select Tab action option");
    cx.simulate_click(bounds_center(select_tab), Modifiers::none());
    cx.run_until_parked();

    let arguments = cx
        .debug_bounds("keymap-modal-arguments")
        .expect("JSON arguments editor");
    cx.simulate_click(bounds_center(arguments), Modifiers::none());
    cx.simulate_input("1");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("keymap-modal").is_some(),
        "Enter in the multi-line arguments editor must not submit the modal"
    );
    probe.update(cx, |probe, cx| {
        assert_eq!(
            probe
                .editor
                .read(cx)
                .modal_draft()
                .expect("create binding draft")
                .arguments_json
                .as_deref(),
            Some("1\n"),
            "plain Enter inserts a newline in the JSON editor"
        );
        assert!(
            probe.intents.borrow().is_empty(),
            "editing arguments does not emit a save intent"
        );
    });

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("keymap-modal").is_none());
}

#[gpui_kit::test]
fn action_combobox_click_away_closes_only_the_popup(cx: &mut TestAppContext) {
    initialize(cx);
    let (_probe, cx) = rooted_keymap_probe(cx);
    let create = cx
        .debug_bounds("keymap-create")
        .expect("Create Keybinding action");
    cx.simulate_click(bounds_center(create), Modifiers::none());
    let trigger = cx
        .debug_bounds("keymap-modal-action")
        .expect("create modal action combobox");
    cx.simulate_click(bounds_center(trigger), Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("keymap-action-option-new_tab").is_some());

    let keystrokes = cx
        .debug_bounds("keymap-modal-keystrokes")
        .expect("keystroke field inside the modal");
    cx.simulate_click(bounds_center(keystrokes), Modifiers::none());
    cx.run_until_parked();

    assert!(cx.debug_bounds("keymap-action-option-new_tab").is_none());
    assert!(
        cx.debug_bounds("keymap-modal").is_some(),
        "click-away dismisses the anchored popup without dismissing its owner modal"
    );
}

#[gpui_kit::test]
fn modal_keystroke_button_focuses_the_recording_surface(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let add = cx
        .debug_bounds("keymap-row-action-0")
        .expect("create binding action");
    cx.simulate_click(bounds_center(add), Modifiers::none());
    let record = cx
        .debug_bounds("keymap-modal-record")
        .expect("record trigger action");
    cx.simulate_click(bounds_center(record), Modifiers::none());
    cx.simulate_keystrokes("cmd-k");

    probe.update(cx, |probe, cx| {
        assert_eq!(
            probe
                .editor
                .read(cx)
                .modal_draft()
                .expect("binding draft")
                .keystrokes,
            ["cmd+k"]
        );
    });
}

#[gpui_kit::test]
fn editing_then_recording_replaces_the_existing_keystroke(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let edit = cx
        .debug_bounds("keymap-row-action-1")
        .expect("the New Tab binding has an Edit action");
    cx.simulate_click(bounds_center(edit), Modifiers::none());

    let record = cx
        .debug_bounds("keymap-modal-record")
        .expect("record trigger action");
    cx.simulate_click(bounds_center(record), Modifiers::none());
    cx.simulate_keystrokes("cmd-k");

    probe.update(cx, |probe, cx| {
        assert_eq!(
            probe
                .editor
                .read(cx)
                .modal_draft()
                .expect("edit binding draft")
                .keystrokes,
            ["cmd+k"],
            "the first newly recorded stroke replaces the binding being edited"
        );
    });
}

#[gpui_kit::test]
fn binding_modal_uses_dialog_inset_and_segmented_trigger_options(cx: &mut TestAppContext) {
    initialize(cx);
    let (_probe, cx) = rooted_keymap_probe(cx);
    let edit = cx
        .debug_bounds("keymap-row-action-1")
        .expect("the New Tab binding has an Edit action");
    cx.simulate_click(bounds_center(edit), Modifiers::none());

    let dialog_title = cx
        .debug_bounds("keymap-modal-dialog-title")
        .expect("dialog title");
    let modal = cx.debug_bounds("keymap-modal").expect("modal body");
    let keystrokes = cx
        .debug_bounds("keymap-modal-keystrokes")
        .expect("keystroke field inside the dialog");
    let options = cx
        .debug_bounds("keymap-modal-trigger-options")
        .expect("segmented trigger options");
    assert_eq!(
        bounds_left(modal),
        bounds_left(dialog_title),
        "form content stays aligned with the padded dialog header"
    );
    assert_eq!(
        bounds_left(keystrokes),
        bounds_left(options),
        "form controls and trigger options share one leading alignment spine"
    );
}

#[gpui_kit::test]
fn escape_dismisses_the_editor_modal_before_closing_the_editor(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let add = cx
        .debug_bounds("keymap-row-action-0")
        .expect("create binding action");
    cx.simulate_click(bounds_center(add), Modifiers::none());
    assert!(cx.debug_bounds("keymap-modal").is_some());

    cx.simulate_keystrokes("escape");
    assert!(
        cx.debug_bounds("keymap-modal").is_none(),
        "Escape follows Zed's modal-first dismissal order"
    );
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));

    cx.simulate_keystrokes("escape");
    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [KeymapEditorIntent::Close]
        );
    });
}

#[gpui_kit::test]
fn modal_requires_second_confirmation_before_add(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let add = cx
        .debug_bounds("keymap-row-action-0")
        .expect("the first sorted row is the unmapped Create Space action");
    cx.simulate_click(bounds_center(add), Modifiers::none());

    for selector in [
        "keymap-modal",
        "keymap-modal-action",
        "keymap-modal-keystrokes",
        "keymap-modal-context",
        "keymap-modal-save",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "create modal exposes {selector}"
        );
    }

    probe.update(cx, |probe, cx| {
        probe.editor.update(cx, |editor, cx| {
            editor.record_modal_keystroke("cmd+t", cx);
            assert_eq!(
                editor
                    .modal_draft()
                    .expect("create binding draft")
                    .keystrokes,
                ["cmd+t"]
            );
            editor.submit_modal(cx);
            assert_eq!(
                editor.modal_error(),
                Some("This keybinding conflicts with 1 existing binding. Save again to confirm.")
            );
            editor.submit_modal(cx);
        });
    });
    cx.run_until_parked();
    probe.update(cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(
            matches!(
                intents.as_slice(),
                [KeymapEditorIntent::Add { binding }]
                    if binding.action == "create_space"
                        && binding.keystrokes == ["cmd+t"]
                        && binding.context == "Global"
            ),
            "unexpected add intents: {intents:#?}"
        );
    });
}

#[gpui_kit::test]
fn modal_records_prefixed_sided_chords_with_typed_legacy_options(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let add = cx
        .debug_bounds("keymap-row-action-0")
        .expect("create binding action");
    cx.simulate_click(bounds_center(add), Modifiers::none());

    probe.update(cx, |probe, cx| {
        probe.editor.update(cx, |editor, cx| {
            editor.set_modal_trigger_options(
                KeymapTriggerOptions {
                    performable: true,
                    unconsumed: true,
                    side_sensitive: true,
                    prefixed: true,
                    ..KeymapTriggerOptions::default()
                },
                cx,
            );
            editor.record_modal_keystroke("ctrl-k", cx);
            assert_eq!(
                editor.modal_draft().expect("binding draft").keystrokes,
                ["ctrl+a", "left_ctrl+k"]
            );
            editor.submit_modal(cx);
        });
    });
    cx.run_until_parked();

    probe.update(cx, |probe, _| {
        assert!(matches!(
            probe.intents.borrow().as_slice(),
            [KeymapEditorIntent::Add { binding }]
                if binding.trigger_options.performable
                    && binding.trigger_options.unconsumed
                    && binding.trigger_options.side_sensitive
                    && binding.trigger_options.prefixed
        ));
    });
}

#[gpui_kit::test]
fn modal_rejects_function_keys_outside_the_durable_grammar(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let add = cx
        .debug_bounds("keymap-row-action-0")
        .expect("create binding action");
    cx.simulate_click(bounds_center(add), Modifiers::none());

    probe.update(cx, |probe, cx| {
        probe.editor.update(cx, |editor, cx| {
            editor.record_modal_keystroke("f13", cx);
            editor.submit_modal(cx);
            assert_eq!(
                editor.modal_error(),
                Some(
                    "Function keys above F12 are not supported by Bootty keybindings. Record F1–F12 or another key.",
                )
            );
        });
        assert!(probe.intents.borrow().is_empty());
    });
}

#[gpui_kit::test]
fn modal_rejects_fn_arrow_keystrokes(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let add = cx
        .debug_bounds("keymap-row-action-0")
        .expect("create binding action");
    cx.simulate_click(bounds_center(add), Modifiers::none());

    probe.update(cx, |probe, cx| {
        probe.editor.update(cx, |editor, cx| {
            editor.record_modal_keystroke("fn+ArrowUp", cx);
            editor.submit_modal(cx);
            assert_eq!(
                editor.modal_error(),
                Some(
                    "The Fn modifier is not supported by Bootty keybindings. Record a shortcut without Fn.",
                )
            );
        });
        assert!(probe.intents.borrow().is_empty());
    });
}

#[gpui_kit::test]
fn recording_modal_captures_wheel_as_a_trigger_step(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let add = cx
        .debug_bounds("keymap-row-action-0")
        .expect("create binding action");
    cx.simulate_click(bounds_center(add), Modifiers::none());
    let record = cx
        .debug_bounds("keymap-modal-record")
        .expect("record trigger action");
    cx.simulate_click(bounds_center(record), Modifiers::none());
    let modal = cx.debug_bounds("keymap-modal").expect("keymap modal");
    cx.simulate_event(ScrollWheelEvent {
        position: bounds_center(modal),
        delta: ScrollDelta::Lines(point(0.0, 1.0)),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });

    probe.update(cx, |probe, cx| {
        assert_eq!(
            probe
                .editor
                .read(cx)
                .modal_draft()
                .expect("binding draft")
                .keystrokes,
            ["scroll_up"]
        );
    });
}

#[gpui_kit::test]
fn recording_modal_preserves_unmodified_greater_than(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);
    let add = cx
        .debug_bounds("keymap-row-action-0")
        .expect("create binding action");
    cx.simulate_click(bounds_center(add), Modifiers::none());
    let record = cx
        .debug_bounds("keymap-modal-record")
        .expect("record trigger action");
    cx.simulate_click(bounds_center(record), Modifiers::none());
    cx.simulate_keystrokes(">");

    probe.update(cx, |probe, cx| {
        assert_eq!(
            probe
                .editor
                .read(cx)
                .modal_draft()
                .expect("binding draft")
                .keystrokes,
            [">"]
        );
    });
}

#[gpui_kit::test]
fn edit_modal_and_row_context_menu_emit_exact_typed_targets(cx: &mut TestAppContext) {
    initialize(cx);
    let (probe, cx) = rooted_keymap_probe(cx);

    let edit_default = cx
        .debug_bounds("keymap-row-action-1")
        .expect("the first New Tab binding has an Edit action");
    cx.simulate_click(bounds_center(edit_default), Modifiers::none());
    let save = cx
        .debug_bounds("keymap-modal-save")
        .expect("edit dialog save action");
    cx.simulate_click(bounds_center(save), Modifiers::none());
    cx.run_until_parked();
    probe.update(cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(
            matches!(
                intents.as_slice(),
                [KeymapEditorIntent::Replace { target, replacement }]
                    if target.id == "default:new-tab"
                        && target.source == KeymapBindingSource::Default
                        && replacement.action == "new_tab"
            ),
            "unexpected replace intents: {intents:#?}"
        );
        drop(intents);
        probe.intents.borrow_mut().clear();
    });

    let row = cx
        .debug_bounds("keymap-row-1")
        .expect("the edited binding remains in the dense table");
    cx.simulate_mouse_down(bounds_center(row), MouseButton::Right, Modifiers::none());
    cx.update(|window, cx| {
        let mut menu = window.within("popup-menu");
        assert_eq!(menu.find(1usize).label(), Some("Delete"));
        menu.click(1usize, cx);
    });
    probe.update(cx, |probe, _| {
        assert!(matches!(
            probe.intents.borrow().as_slice(),
            [KeymapEditorIntent::Remove { target }]
                if target.id == "default:new-tab"
                    && target.source == KeymapBindingSource::Default
        ));
    });
}

fn bounds_center(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> gpui_kit::Point<gpui_kit::Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(px(left + width / 2.0), px(top + height / 2.0))
}

fn bounds_top(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> f32 {
    bounds.origin.y.into()
}

fn bounds_bottom(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> f32 {
    let top: f32 = bounds.origin.y.into();
    let height: f32 = bounds.size.height.into();
    top + height
}

const fn bounds_left(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> gpui_kit::Pixels {
    bounds.origin.x
}

fn bounds_width(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> f32 {
    bounds.size.width.into()
}
