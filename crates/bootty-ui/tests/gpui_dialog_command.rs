#![cfg(test)]

use bootty_ui::gpui as bootty_gpui;
use std::{cell::RefCell, rc::Rc};

use bootty_ui::gpui::{
    ActionId, CommandAction, DialogAction, DialogId, DialogIntent, DialogPayload, DialogRole,
    DialogRow, DialogSpec, DialogView, RowId, UiPalette, init_theme, parse_keybinding,
};
use bootty_ui::presentation::dialogs::{SpaceMoveTarget, SpacePickerDialog, SpacePickerEvent};
use gpui_kit::component::{Root, WindowExt as _};
use gpui_kit::{
    AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable as _, IntoElement, Modifiers,
    MouseButton, Render, Subscription, TestAppContext, VisualTestContext, Window, div, point,
    prelude::*, px,
};
use rstest::rstest;

#[rstest]
#[case("cmd+ArrowLeft")]
#[case("arrow_up")]
#[case("ctrl+arrow_down")]
#[case("shift+PageUp")]
#[case("right_ctrl+Backspace")]
#[case("cmd+k > cmd+p")]
#[case("cmd++")]
#[case("+")]
fn durable_keybinding_aliases_parse(#[case] binding: &str) {
    assert!(parse_keybinding(binding).is_some(), "{binding}");
}

struct CommandDialogProbe {
    dialog: Entity<DialogView>,
    background_focus: FocusHandle,
    render_full_surface: bool,
    commands: Vec<String>,
    intents: Rc<RefCell<Vec<DialogIntent>>>,
    _subscription: Subscription,
}

impl CommandDialogProbe {
    fn with_spec(window: &mut Window, cx: &mut Context<Self>, spec: DialogSpec) -> Self {
        let dialog = cx.new(|cx| DialogView::new(window, cx));
        let intents = Rc::new(RefCell::new(Vec::new()));
        let received = intents.clone();
        let subscription = cx.subscribe(&dialog, move |_, _, intent: &DialogIntent, _| {
            received.borrow_mut().push(intent.clone());
        });
        dialog.update(cx, |dialog, cx| {
            dialog.present(Some(spec), window, cx);
        });
        Self {
            dialog,
            background_focus: cx.focus_handle(),
            render_full_surface: false,
            commands: Vec::new(),
            intents,
            _subscription: subscription,
        }
    }
}

fn terminal_find_spec() -> DialogSpec {
    let mut spec = DialogSpec::searchable(
        "find",
        "Find",
        "needle",
        vec![
            DialogRow::action("previous", "Previous", DialogAction::new("previous")),
            DialogRow::action("next", "Next", DialogAction::new("next")),
        ],
    );
    spec.role = bootty_gpui::DialogRole::TerminalFind;
    spec
}

impl Render for CommandDialogProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog = self.dialog.clone();
        let child = if self.render_full_surface {
            // Match the accessibility-active DockSkin and SurfacePanel host nesting.
            Some(
                div()
                    .id("tab-content")
                    .size_full()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .size_full()
                            .min_h_0()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .id("surface-agent-form")
                                    .flex_1()
                                    .min_h_0()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .p_4()
                                    .child(dialog),
                            ),
                    )
                    .into_any_element(),
            )
        } else if self.dialog.read(cx).is_non_modal() {
            Some(dialog.into_any_element())
        } else {
            None
        };
        div()
            .size_full()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .track_focus(&self.background_focus)
            .key_context(bootty_ui::gpui_actions::WORKSPACE_KEY_CONTEXT)
            .on_action(cx.listener(
                |this, action: &bootty_ui::gpui_actions::InvokeCommand, _, cx| {
                    this.commands.push(action.invocation().action_name());
                    cx.stop_propagation();
                },
            ))
            .children(child)
    }
}

impl CommandDialogProbe {
    fn open_root_dialog(&self, dialog_id: DialogId, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.dialog.clone();
        let focus_view = view.clone();
        let root_title = self.dialog.read(cx).root_title();
        let show_root_chrome = root_title.is_some();
        window.open_dialog(cx, move |dialog, _, _| {
            let cancel_view = view.clone();
            let cancel_dialog_id = dialog_id.clone();
            let content_view = view.clone();
            let dialog = dialog
                .close_button(show_root_chrome)
                .when(!show_root_chrome, |dialog| dialog.p_0().gap_0());
            let dialog = match root_title.clone() {
                Some(title) => dialog.title(title),
                None => dialog,
            };
            dialog
                .on_cancel({
                    move |_, _, cx| {
                        cancel_view.update(cx, |_, cx| {
                            cx.emit(DialogIntent::Dismiss {
                                dialog: cancel_dialog_id.clone(),
                            });
                        });
                        true
                    }
                })
                .content(move |content, _, _| content.p_0().child(content_view.clone()))
        });
        // Host code chooses the role-specific DialogView handle before Root renders the modal.
        focus_view.read(cx).focus_handle(cx).focus(window, cx);
    }
}

fn rooted_probe(
    cx: &TestAppContext,
    spec: DialogSpec,
) -> (Entity<CommandDialogProbe>, VisualTestContext) {
    rooted_probe_with_size(cx, spec, 800.0, 600.0)
}

fn rooted_probe_with_size(
    cx: &TestAppContext,
    spec: DialogSpec,
    width: f32,
    height: f32,
) -> (Entity<CommandDialogProbe>, VisualTestContext) {
    rooted_probe_with_viewport(cx, spec, width, height, 16.0)
}

fn rooted_probe_with_viewport(
    cx: &TestAppContext,
    spec: DialogSpec,
    width: f32,
    height: f32,
    rem_size: f32,
) -> (Entity<CommandDialogProbe>, VisualTestContext) {
    mounted_dialog_probe(cx, spec, width, height, rem_size, false)
}

fn full_surface_probe_with_viewport(
    cx: &TestAppContext,
    spec: DialogSpec,
    width: f32,
    height: f32,
    rem_size: f32,
) -> (Entity<CommandDialogProbe>, VisualTestContext) {
    mounted_dialog_probe(cx, spec, width, height, rem_size, true)
}

fn mounted_dialog_probe(
    cx: &TestAppContext,
    spec: DialogSpec,
    width: f32,
    height: f32,
    rem_size: f32,
    render_full_surface: bool,
) -> (Entity<CommandDialogProbe>, VisualTestContext) {
    let dialog_id = spec.id.clone();
    let slot = Rc::new(RefCell::new(None));
    let opened = Rc::clone(&slot);
    let window = cx.update(|cx| {
        init_theme(UiPalette::default(), cx);
        cx.open_window(
            gpui_kit::WindowOptions {
                window_bounds: Some(gpui_kit::WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: gpui_kit::size(px(width), px(height)),
                })),
                ..Default::default()
            },
            move |window, cx| {
                window.set_rem_size(px(rem_size));
                let probe = cx.new(|cx| {
                    let mut probe = CommandDialogProbe::with_spec(window, cx, spec);
                    probe.render_full_surface = render_full_surface;
                    probe
                });
                opened.replace(Some(probe.clone()));
                cx.new(|cx| Root::new(probe, window, cx))
            },
        )
        .expect("open rooted command dialog")
    });
    let probe = slot.borrow_mut().take().expect("capture command dialog");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.refresh().expect("render rooted command dialog");
    visual.update(|window, cx| {
        probe.update(cx, |probe, cx| {
            probe.background_focus.focus(window, cx);
            if probe.render_full_surface || probe.dialog.read(cx).is_non_modal() {
                probe.dialog.read(cx).focus_handle(cx).focus(window, cx);
            } else {
                probe.open_root_dialog(dialog_id, window, cx);
            }
        });
    });
    visual.run_until_parked();
    visual.refresh().expect("render active Root dialog");
    (probe, visual)
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing clickable element {selector}"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

#[gpui_kit::test]
fn searchable_dialog_omits_generic_dialog_chrome(cx: &TestAppContext) {
    let (_, mut cx) = rooted_probe(
        cx,
        DialogSpec::searchable(
            "commands",
            "Commands",
            "",
            vec![DialogRow::action(
                "new-session",
                "New Session",
                DialogAction::new("new_session"),
            )],
        ),
    );

    assert!(
        cx.debug_bounds("dialog-close-commands").is_none(),
        "the generic dialog close button is absent"
    );
    assert!(
        cx.debug_bounds("dialog-content").is_none(),
        "the generic dialog content frame is absent"
    );
}

#[gpui_kit::test]
fn command_confirmation_emits_the_original_dialog_row(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(
        cx,
        DialogSpec::searchable(
            "commands",
            "Commands",
            "",
            vec![DialogRow::action(
                "new-session",
                "New Session",
                DialogAction::new("new_session"),
            )],
        ),
    );
    cx.update(|window, app| {
        probe
            .read(app)
            .dialog
            .read(app)
            .focus_handle(app)
            .focus(window, app);
    });
    cx.simulate_keystrokes("enter");

    probe.update(&mut cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [DialogIntent::Activate {
                dialog: DialogId::new("commands"),
                row: RowId::new("new-session"),
                action: ActionId::new("new_session"),
                payload: DialogPayload::default(),
            }]
        );
    });
}

#[gpui_kit::test]
fn projected_alias_matches_remain_selectable(cx: &TestAppContext) {
    use bootty_mux::{controller::SpaceId, workspace::ScopedSessionTarget};

    let destination = SpaceId::from_persistence(1);
    let session = ScopedSessionTarget::new(SpaceId::from_persistence(2), "session-id");
    for (query, label, expected_space) in [
        ("rocket", "Development", Some(destination)),
        ("unassign", "Nothing", None),
    ] {
        let mut picker = SpacePickerDialog::open(
            session.clone(),
            "Session".to_owned(),
            vec![SpaceMoveTarget {
                id: destination,
                name: "Development".to_owned(),
                icon: "rocket".to_owned(),
                reachable: true,
                current: false,
            }],
        );
        picker.apply(&DialogIntent::TextChanged {
            dialog: picker.spec().id,
            value: query.to_owned(),
        });
        let spec = picker.spec();
        pretty_assertions::assert_eq!(spec.rows.len(), 1);
        pretty_assertions::assert_eq!(spec.rows[0].label, label);
        let (probe, mut visual) = rooted_probe(cx, spec);
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        let events = probe.read_with(&visual, |probe, _| {
            probe
                .intents
                .borrow()
                .iter()
                .filter_map(|intent| picker.apply(intent))
                .collect::<Vec<_>>()
        });
        pretty_assertions::assert_eq!(
            events,
            vec![SpacePickerEvent::Move {
                session: session.clone(),
                space: expected_space,
            }]
        );
    }
}

#[gpui_kit::test]
fn command_control_navigation_uses_component_selection(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(
        cx,
        DialogSpec::searchable(
            "commands",
            "Commands",
            "",
            ["first", "second", "third"]
                .into_iter()
                .map(|id| DialogRow {
                    current: id == "third",
                    ..DialogRow::action(id, id, DialogAction::new(id))
                })
                .collect(),
        ),
    );
    cx.update(|window, app| {
        probe
            .read(app)
            .dialog
            .read(app)
            .focus_handle(app)
            .focus(window, app);
    });
    cx.simulate_keystrokes("ctrl-n ctrl-n ctrl-p enter");
    probe.update(&mut cx, |probe, _| {
        assert!(probe.intents.borrow().contains(&DialogIntent::Activate {
            dialog: DialogId::new("commands"),
            row: RowId::new("second"),
            action: ActionId::new("second"),
            payload: DialogPayload::default(),
        }));
    });
}

#[gpui_kit::test]
fn retired_command_does_not_intercept_background_keys(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(
        cx,
        DialogSpec::searchable(
            "commands",
            "Commands",
            "",
            vec![DialogRow::action("run", "Run", DialogAction::new("run"))],
        ),
    );
    cx.update(|window, app| {
        probe.update(app, |probe, cx| {
            probe
                .dialog
                .update(cx, |dialog, cx| dialog.present(None, window, cx));
            window.close_dialog(cx);
            probe.background_focus.focus(window, cx);
            probe.intents.borrow_mut().clear();
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("x escape");
    probe.read_with(&cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
}

#[gpui_kit::test]
fn application_command_action_uses_component_selection_and_confirmation(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(
        cx,
        DialogSpec::searchable(
            "commands",
            "Commands",
            "",
            vec![
                DialogRow::action("first", "First", DialogAction::new("first")),
                DialogRow::action("second", "Second", DialogAction::new("second")),
            ],
        ),
    );
    cx.update(|window, app| {
        probe.update(app, |probe, cx| {
            probe.dialog.update(cx, |dialog, cx| {
                dialog.perform(CommandAction::Next, window, cx);
            });
        });
    });
    cx.run_until_parked();
    cx.update(|window, app| {
        probe.update(app, |probe, cx| {
            probe.dialog.update(cx, |dialog, cx| {
                dialog.perform(CommandAction::Confirm, window, cx);
            });
        });
    });
    cx.run_until_parked();

    probe.update(&mut cx, |probe, _| {
        assert!(probe.intents.borrow().contains(&DialogIntent::Activate {
            dialog: DialogId::new("commands"),
            row: RowId::new("second"),
            action: ActionId::new("second"),
            payload: DialogPayload::default(),
        }));
    });
}

#[gpui_kit::test]
fn command_container_focus_keeps_typing_and_actions_after_tab(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(
        cx,
        DialogSpec::searchable(
            "commands",
            "Commands",
            "",
            vec![
                DialogRow::action("x-first", "X First", DialogAction::new("x-first")),
                DialogRow::action("x-second", "X Second", DialogAction::new("x-second")),
            ],
        ),
    );
    cx.update(|window, app| {
        probe
            .read(app)
            .dialog
            .read(app)
            .focus_handle(app)
            .focus(window, app);
    });
    cx.simulate_keystrokes("tab x ctrl-n enter");
    cx.run_until_parked();

    probe.update(&mut cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(intents.iter().any(|intent| matches!(
            intent,
            DialogIntent::TextChanged { dialog, value }
                if *dialog == DialogId::new("commands") && value.ends_with('x')
        )));
        assert!(intents.contains(&DialogIntent::Activate {
            dialog: DialogId::new("commands"),
            row: RowId::new("x-second"),
            action: ActionId::new("x-second"),
            payload: DialogPayload::default(),
        }));
    });
}

#[gpui_kit::test]
fn same_dialog_refresh_preserves_selection_by_row_id(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(
        cx,
        DialogSpec::searchable(
            "commands",
            "Commands",
            "",
            vec![
                DialogRow::action("first", "First", DialogAction::new("first")),
                DialogRow::action("second", "Second", DialogAction::new("second")),
            ],
        ),
    );
    cx.update(|window, app| {
        probe
            .read(app)
            .dialog
            .read(app)
            .focus_handle(app)
            .focus(window, app);
    });
    cx.simulate_keystrokes("ctrl-n");
    cx.run_until_parked();
    cx.update(|window, app| {
        probe.update(app, |probe, cx| {
            probe.dialog.update(cx, |dialog, cx| {
                dialog.present(
                    Some(DialogSpec::searchable(
                        "commands",
                        "Commands",
                        "",
                        vec![
                            DialogRow::action("second", "Second", DialogAction::new("second")),
                            DialogRow::action("first", "First", DialogAction::new("first")),
                        ],
                    )),
                    window,
                    cx,
                );
            });
        });
    });
    cx.refresh().expect("render reordered command rows");
    cx.update(|window, app| {
        probe
            .read(app)
            .dialog
            .read(app)
            .focus_handle(app)
            .focus(window, app);
    });
    cx.simulate_keystrokes("enter");

    probe.update(&mut cx, |probe, _| {
        assert!(probe.intents.borrow().contains(&DialogIntent::Activate {
            dialog: DialogId::new("commands"),
            row: RowId::new("second"),
            action: ActionId::new("second"),
            payload: DialogPayload::default(),
        }));
    });
}

#[gpui_kit::test]
fn command_escape_closes_from_dialog_container_focus(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(
        cx,
        DialogSpec::searchable("commands", "Commands", "", vec![]),
    );
    cx.update(|window, app| {
        gpui_kit::base::active_focus_trap(window, app)
            .expect("active dialog focus trap")
            .focus(window, app);
    });
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.update(|window, app| !window.has_active_dialog(app)));
    probe.update(&mut cx, |probe, _| {
        assert_eq!(
            probe
                .intents
                .borrow()
                .iter()
                .filter(|intent| matches!(intent, DialogIntent::Dismiss { .. }))
                .count(),
            1
        );
    });
}

#[gpui_kit::test]
fn new_dialog_identity_starts_at_its_first_enabled_row(cx: &TestAppContext) {
    let first = DialogSpec::searchable(
        "first",
        "First",
        "",
        vec![
            DialogRow {
                enabled: false,
                ..DialogRow::action("disabled", "Disabled", DialogAction::new("disabled"))
            },
            DialogRow::action(
                "first-enabled",
                "First enabled",
                DialogAction::new("first-enabled"),
            ),
            DialogRow::action(
                "second-enabled",
                "Second enabled",
                DialogAction::new("second-enabled"),
            ),
        ],
    );
    let (probe, mut cx) = rooted_probe(cx, first);
    cx.update(|window, app| {
        probe
            .read(app)
            .dialog
            .read(app)
            .focus_handle(app)
            .focus(window, app);
    });
    cx.simulate_keystrokes("down");
    cx.update(|window, app| {
        probe.update(app, |probe, cx| {
            probe.dialog.update(cx, |dialog, cx| {
                dialog.present(
                    Some(DialogSpec::searchable(
                        "second",
                        "Second",
                        "",
                        vec![
                            DialogRow::action(
                                "new-first",
                                "New first",
                                DialogAction::new("new-first"),
                            ),
                            DialogRow::action(
                                "new-second",
                                "New second",
                                DialogAction::new("new-second"),
                            ),
                        ],
                    )),
                    window,
                    cx,
                );
            });
        });
    });
    cx.refresh().expect("render new command identity");
    cx.update(|window, app| {
        probe
            .read(app)
            .dialog
            .read(app)
            .focus_handle(app)
            .focus(window, app);
    });
    cx.simulate_keystrokes("enter");

    probe.update(&mut cx, |probe, _| {
        assert!(probe.intents.borrow().contains(&DialogIntent::Activate {
            dialog: DialogId::new("second"),
            row: RowId::new("new-first"),
            action: ActionId::new("new-first"),
            payload: DialogPayload::default(),
        }));
    });
}

#[gpui_kit::test]
fn theme_picker_starts_at_the_current_theme(cx: &TestAppContext) {
    let mut spec = DialogSpec::searchable(
        "themes",
        "Themes",
        "",
        vec![
            DialogRow::action("light", "Light", DialogAction::new("light")),
            DialogRow {
                current: true,
                ..DialogRow::action("dark", "Dark", DialogAction::new("dark"))
            },
        ],
    );
    spec.role = bootty_gpui::DialogRole::ThemePicker;
    let (probe, mut cx) = rooted_probe(cx, spec);
    cx.update(|window, app| {
        probe
            .read(app)
            .dialog
            .read(app)
            .focus_handle(app)
            .focus(window, app);
    });
    cx.simulate_keystrokes("enter");

    probe.update(&mut cx, |probe, _| {
        assert!(probe.intents.borrow().contains(&DialogIntent::Activate {
            dialog: DialogId::new("themes"),
            row: RowId::new("dark"),
            action: ActionId::new("dark"),
            payload: DialogPayload::default(),
        }));
    });
}

#[gpui_kit::test]
fn terminal_find_keeps_shift_direction_and_free_query(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let window = cx.update(|cx| {
        cx.open_window(gpui_kit::WindowOptions::default(), |window, cx| {
            cx.new(|cx| CommandDialogProbe::with_spec(window, cx, terminal_find_spec()))
        })
        .expect("open find dialog")
    });
    window
        .update(cx, |probe, window, cx| {
            probe.dialog.read(cx).focus_handle(cx).focus(window, cx);
        })
        .expect("focus find dialog");

    cx.simulate_keystrokes(*window, "shift-enter");

    window
        .update(cx, |probe, _, _| {
            let intents = probe.intents.borrow();
            assert_eq!(
                intents
                    .iter()
                    .filter(|intent| matches!(intent, DialogIntent::Find { .. }))
                    .count(),
                1
            );
            assert!(intents.contains(&DialogIntent::Find {
                dialog: DialogId::new("find"),
                query: "needle".to_owned(),
                direction: bootty_gpui::FindDirection::Previous,
            }));
        })
        .expect("read reverse find intent");
}

#[gpui_kit::test]
fn prompt_enter_activates_once(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(
        cx,
        DialogSpec::prompt(
            "prompt",
            "Prompt",
            "value",
            "Value",
            DialogAction::new("submit"),
        ),
    );
    cx.update(|window, app| {
        probe
            .read(app)
            .dialog
            .read(app)
            .focus_handle(app)
            .focus(window, app);
    });
    cx.simulate_keystrokes("enter");

    probe.update(&mut cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert_eq!(
            intents
                .iter()
                .filter(|intent| matches!(intent, DialogIntent::Activate { .. }))
                .count(),
            1
        );
        assert!(intents.contains(&DialogIntent::Activate {
            dialog: DialogId::new("prompt"),
            row: RowId::new("submit"),
            action: ActionId::new("submit"),
            payload: DialogPayload::default(),
        }));
    });
}

#[gpui_kit::test]
fn command_escape_emits_one_dismiss_intent(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(
        cx,
        DialogSpec::searchable(
            "commands",
            "Commands",
            "",
            vec![DialogRow::action(
                "new-session",
                "New Session",
                DialogAction::new("new_session"),
            )],
        ),
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(cx.update(|window, app| !window.has_active_dialog(app)));
    assert!(cx.update(|window, app| { probe.read(app).background_focus.is_focused(window) }));

    probe.update(&mut cx, |probe, _| {
        assert_eq!(
            probe
                .intents
                .borrow()
                .iter()
                .filter(|intent| matches!(intent, DialogIntent::Dismiss { .. }))
                .count(),
            1
        );
    });
}

#[gpui_kit::test]
fn root_backdrop_emits_one_dismiss_and_restores_focus(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(
        cx,
        DialogSpec::prompt(
            "prompt",
            "Prompt",
            "value",
            "Value",
            DialogAction::new("submit"),
        ),
    );
    cx.simulate_mouse_down(
        point(px(790.0), px(590.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.run_until_parked();

    assert!(cx.update(|window, app| !window.has_active_dialog(app)));
    assert!(cx.update(|window, app| { probe.read(app).background_focus.is_focused(window) }));
    probe.update(&mut cx, |probe, _| {
        assert_eq!(
            probe
                .intents
                .borrow()
                .iter()
                .filter(|intent| matches!(intent, DialogIntent::Dismiss { .. }))
                .count(),
            1
        );
    });
}

#[gpui_kit::test]
fn rooted_prompt_accepts_typed_input(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(
        cx,
        DialogSpec::prompt(
            "prompt",
            "Prompt",
            "value",
            "Value",
            DialogAction::new("submit"),
        ),
    );
    cx.update(|window, app| {
        probe
            .read(app)
            .dialog
            .read(app)
            .focus_handle(app)
            .focus(window, app);
    });
    cx.simulate_input("typed");

    probe.update(&mut cx, |probe, _| {
        assert!(probe.intents.borrow().iter().any(|intent| matches!(
            intent,
            DialogIntent::TextChanged { dialog, value }
                if *dialog == DialogId::new("prompt") && value.ends_with("typed")
        )));
    });
}

fn native_agent_form_spec() -> DialogSpec {
    use bootty_config::config::AgentProvidersConfig;
    use bootty_control::{CommandTarget, ResourceKind};
    use bootty_mux::controller::SpaceId;
    use bootty_ui::presentation::new_session_form::{
        NewSessionDraft, NewSessionForm, NewSessionMode, SessionDestination,
    };

    let scope = SpaceId::from_persistence(1);
    let cwd = "/workspace/review-project";
    let destination = SessionDestination {
        scope,
        label: "Current host".to_owned(),
        icon: "folder".to_owned(),
        color: [122, 162, 247],
        cwd: cwd.to_owned(),
        remote: None,
        target: CommandTarget {
            kind: ResourceKind::Binding,
            handle: "test-binding".to_owned(),
            generation: 1,
        },
        worktrees: false,
    };
    let form = NewSessionForm::new(
        NewSessionDraft {
            projectless: false,
            scope,
            cwd: cwd.to_owned(),
            mode: NewSessionMode::Agent,
            prompt: String::new(),
            applications: Vec::new(),
            provider_mentions: Vec::new(),
            goal: String::new(),
            attachments: Vec::new(),
            command: String::new(),
            provider: "codex".to_owned(),
            profiles: std::collections::BTreeMap::default(),
            model_selection: None,
            permissions: bootty_agents::NativePermissionMode::ProviderDefault,
            isolated: false,
            isolation_preference: false,
            branch: String::new(),
            folder: String::new(),
            start_ref: String::new(),
            suffix: "a123".to_owned(),
            identity: "0123456789abcdef0123456789abcdef".to_owned(),
            fan_out: Vec::new(),
            directories: std::collections::HashMap::default(),
        },
        vec![destination],
        AgentProvidersConfig::default(),
    );
    let mut spec = form.spec(false);
    "Agent".clone_into(&mut spec.title);
    spec.fields
        .retain(|field| matches!(field.id.as_str(), "provider" | "profile"));
    spec.rows.retain(|row| row.id.0 != "choose-project");
    spec.footer = Some("review-project".to_owned());
    spec
}

#[gpui_kit::test]
fn agent_composer_shows_discovery_and_retries_without_replacing_the_prompt(cx: &TestAppContext) {
    let mut spec = native_agent_form_spec();
    spec.text = Some("Keep this draft".into());
    spec.models_loading = true;
    let (probe, mut cx) = full_surface_probe_with_viewport(cx, spec.clone(), 1200.0, 900.0, 24.0);
    assert!(cx.debug_bounds("new-session-model-loading").is_some());
    spec.models_loading = false;
    spec.model_error = Some("Provider could not load models".into());
    probe.update_in(&mut cx, |probe, window, cx| {
        probe
            .dialog
            .update(cx, |dialog, cx| dialog.present(Some(spec), window, cx));
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("new-session-model-loading").is_none());
    assert!(cx.debug_bounds("dialog-prompt-input").is_some());
    assert!(cx.debug_bounds("new-session-provider-sign-in").is_none());
    click(&mut cx, "new-session-model-retry");
    probe.update(&mut cx, |probe, _| {
        assert!(probe.intents.borrow().iter().any(|intent| matches!(intent,
            DialogIntent::Activate { action, .. } if action.0 == "reload-models"
        )));
    });
}

#[gpui_kit::test]
fn agent_project_search_keeps_the_composer_and_selects_a_registered_project(cx: &TestAppContext) {
    let mut spec = native_agent_form_spec();
    spec.text = Some("Keep the prompt and attachments".into());
    spec.rows.push(DialogRow::action(
        "choose-project",
        "review-project…",
        DialogAction::new("choose-project"),
    ));
    spec.projects = ["review-project", "agents"]
        .into_iter()
        .map(|name| bootty_git::ProjectPickerEntry {
            path: format!("/workspace/{name}"),
            favorite: false,
        })
        .collect();
    spec.selected_project = Some("/workspace/review-project".into());
    let (probe, mut cx) = full_surface_probe_with_viewport(cx, spec, 1200.0, 900.0, 24.0);
    click(&mut cx, "new-session-header-project");
    assert!(cx.debug_bounds("dialog-prompt-input").is_some());
    cx.simulate_input("agents");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_input(" more");
    cx.run_until_parked();
    probe.update(&mut cx, |probe, _| {
        assert!(probe.intents.borrow().contains(&DialogIntent::FieldChanged {
            dialog: DialogId::new(bootty_ui::presentation::dialogs::NEW_SESSION_ID),
            field: "project".into(), value: "/workspace/agents".into(),
        }));
        assert!(probe.intents.borrow().iter().any(|intent| matches!(intent,
            DialogIntent::TextChanged { value, .. } if value.contains("Keep the prompt and attachments") && value.contains(" more")
        )));
        assert!(!probe.intents.borrow().iter().any(|intent| matches!(intent, DialogIntent::Activate { action, .. } if action.0 == "choose-project")));
    });
}

#[gpui_kit::test]
fn agent_composer_handles_cancel_from_child_controls(cx: &TestAppContext) {
    let mut spec = native_agent_form_spec();
    spec.fields.push(bootty_gpui::DialogField {
        id: "model".into(),
        label: "Model".into(),
        value: "openai/current".into(),
        placeholder: String::new(),
        kind: bootty_gpui::DialogFieldKind::Choice(vec!["openai/current".into()]),
    });
    spec.models.push(bootty_agents::NativeModelOption {
        id: "openai/current".into(),
        display_name: "Current model".into(),
        reasoning_efforts: vec!["high".into()],
        default_reasoning_effort: Some("high".into()),
        is_default: true,
        is_legacy: false,
        is_favorite: false,
        supports_fast_mode: false,
        fast_mode_detail: None,
    });
    let (probe, mut cx) = full_surface_probe_with_viewport(cx, spec, 1200.0, 900.0, 24.0);
    click(&mut cx, "model-picker-trigger");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    probe.update(&mut cx, |probe, _| {
        assert!(
            !probe
                .intents
                .borrow()
                .iter()
                .any(|intent| matches!(intent, DialogIntent::Dismiss { .. }))
        );
    });
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    probe.update(&mut cx, |probe, _| {
        assert_eq!(
            probe
                .intents
                .borrow()
                .iter()
                .filter(|intent| matches!(intent, DialogIntent::Dismiss { .. }))
                .count(),
            1
        );
    });
}

#[gpui_kit::test]
fn agent_composer_centers_in_a_tall_full_surface_and_keeps_prompt_focus(cx: &TestAppContext) {
    let (probe, mut cx) =
        full_surface_probe_with_viewport(cx, native_agent_form_spec(), 1200.0, 900.0, 24.0);

    let composer = cx
        .debug_bounds("dialog-prompt-panel")
        .expect("mounted agent composer");
    assert!(
        composer.size.width > px(0.0) && composer.size.height > px(0.0),
        "composer collapsed in its host: {composer:?}"
    );
    assert!(
        composer.left() >= px(0.0),
        "composer is clipped on the left"
    );
    assert!(
        composer.right() <= px(1200.0),
        "composer is clipped on the right: {composer:?}"
    );
    assert!(
        composer.bottom() <= px(900.0),
        "composer exceeds the window height: {composer:?}"
    );
    assert!(
        composer.size.height < px(900.0),
        "the tall viewport must leave room around the composer: {composer:?}"
    );
    assert!(
        (px(599.0)..=px(601.0)).contains(&composer.center().x),
        "composer is not horizontally centered: {composer:?}"
    );
    assert!(
        (px(449.0)..=px(451.0)).contains(&composer.center().y),
        "composer is not vertically centered: {composer:?}"
    );
    let input = cx
        .debug_bounds("dialog-prompt-input")
        .expect("visible agent prompt input");
    assert!(
        input.size.width > px(0.0) && input.size.height > px(0.0),
        "prompt input collapsed: {input:?}"
    );
    assert!(
        composer.contains(&input.center()),
        "prompt is outside composer"
    );
    let model = cx
        .debug_bounds("model-picker-trigger")
        .expect("visible combined provider and model choice");
    assert!(
        model.size.width > px(0.0) && model.size.height > px(0.0),
        "combined provider and model choice collapsed: {model:?}"
    );
    assert!(
        composer.contains(&model.center()),
        "combined provider and model choice is outside composer"
    );
    cx.simulate_input("run the tests");
    cx.run_until_parked();
    probe.update(&mut cx, |probe, _| {
        assert!(probe.intents.borrow().iter().any(|intent| matches!(
            intent,
            DialogIntent::TextChanged { dialog, value }
                if *dialog == DialogId::new(bootty_ui::presentation::dialogs::NEW_SESSION_ID)
                    && value == "run the tests"
        )));
    });
}

#[gpui_kit::test]
fn prompt_validation_is_visible_and_blocks_submit(cx: &TestAppContext) {
    let mut spec = DialogSpec::prompt("prompt", "Rename", "", "name…", DialogAction::new("submit"));
    spec.rows[0].enabled = false;
    spec.rows[0].detail = Some("A name is required.".to_owned());
    let (probe, mut cx) = rooted_probe(cx, spec);

    assert!(cx.debug_bounds("dialog-prompt-input").is_some());
    assert!(cx.debug_bounds("dialog-prompt-validation").is_some());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("dialog-prompt-input").is_some(),
        "invalid Enter must keep the prompt open"
    );
    click(&mut cx, "dialog-prompt-submit");

    probe.update(&mut cx, |probe, _| {
        assert!(
            !probe
                .intents
                .borrow()
                .iter()
                .any(|intent| matches!(intent, DialogIntent::Activate { .. }))
        );
    });
}

#[gpui_kit::test]
fn prompt_submit_button_emits_supplied_action(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(
        cx,
        DialogSpec::prompt(
            "prompt",
            "Rename",
            "new name",
            "name…",
            DialogAction::new("rename").with_payload("new name"),
        ),
    );
    click(&mut cx, "dialog-prompt-submit");

    probe.update(&mut cx, |probe, _| {
        assert!(probe.intents.borrow().contains(&DialogIntent::Activate {
            dialog: DialogId::new("prompt"),
            row: RowId::new("submit"),
            action: ActionId::new("rename"),
            payload: bootty_gpui::DialogPayload::text("new name"),
        }));
    });
}

#[gpui_kit::test]
fn confirm_shows_details_uses_first_enabled_action_and_closes_once(cx: &TestAppContext) {
    let spec = DialogSpec {
        id: DialogId::new("confirm"),
        role: DialogRole::Confirm,
        title: "Delete session".to_owned(),
        icon: None,
        hint: Some("Enter confirm   Esc cancel".to_owned()),
        footer: None,
        text: None,
        text_label: None,
        fields: Vec::new(),
        spaces: Vec::new(),
        applications: Vec::new(),
        provider_mentions: Vec::new(),
        attachments: Vec::new(),
        completion: None,
        models: Vec::new(),
        available_efforts: Vec::new(),
        models_loading: false,
        model_error: None,
        projects: Vec::new(),
        project_labels: std::collections::BTreeMap::new(),
        selected_project: None,
        selected_model: None,
        busy: false,
        progress: Vec::new(),
        multiline: false,
        text_hint: None,
        rows: vec![
            DialogRow {
                detail: Some("This removes the worktree.".to_owned()),
                destructive: true,
                ..DialogRow::action("delete", "Delete", DialogAction::new("delete"))
            },
            DialogRow::action("cancel-action", "Keep", DialogAction::new("keep")),
        ],
        empty_text: String::new(),
        placement: bootty_gpui::DialogPlacement::Center,
    };
    let (probe, mut cx) = rooted_probe(cx, spec);

    assert!(cx.debug_bounds("dialog-confirm-panel").is_some());
    assert!(cx.debug_bounds("dialog-confirm-detail-delete").is_some());
    cx.simulate_keystrokes("enter");
    probe.update(&mut cx, |probe, _| {
        assert!(probe.intents.borrow().contains(&DialogIntent::Activate {
            dialog: DialogId::new("confirm"),
            row: RowId::new("delete"),
            action: ActionId::new("delete"),
            payload: DialogPayload::default(),
        }));
    });

    // Root remains the sole modal Escape owner, even after an action intent is emitted.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    probe.update(&mut cx, |probe, _| {
        assert_eq!(
            probe
                .intents
                .borrow()
                .iter()
                .filter(|intent| matches!(intent, DialogIntent::Dismiss { .. }))
                .count(),
            1
        );
    });
}

#[gpui_kit::test]
fn prompt_fields_edit_without_resetting_sibling_values(cx: &TestAppContext) {
    let mut spec = DialogSpec::prompt(
        "worktree",
        "New Worktree",
        "topic",
        "Branch",
        DialogAction::new("create"),
    );
    spec.text_label = Some("Branch".to_owned());
    spec.fields = vec![
        bootty_gpui::DialogField {
            kind: bootty_gpui::DialogFieldKind::Text,
            id: "folder".to_owned(),
            label: "Folder".to_owned(),
            value: String::new(),
            placeholder: "Folder".to_owned(),
        },
        bootty_gpui::DialogField {
            kind: bootty_gpui::DialogFieldKind::Text,
            id: "start".to_owned(),
            label: "Start from".to_owned(),
            value: "main".to_owned(),
            placeholder: "HEAD".to_owned(),
        },
    ];
    let (probe, mut cx) = rooted_probe(cx, spec.clone());
    cx.run_until_parked();
    let bounds = cx
        .debug_bounds("dialog-field-input-folder")
        .expect("folder field");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.simulate_input("checkout");
    cx.run_until_parked();
    probe.update(&mut cx, |probe, _| {
        assert!(
            probe
                .intents
                .borrow()
                .contains(&DialogIntent::FieldChanged {
                    dialog: DialogId::new("worktree"),
                    field: "folder".to_owned(),
                    value: "checkout".to_owned(),
                }),
            "{:?}",
            probe.intents.borrow()
        );
    });
    spec.fields[0].value = "checkout".to_owned();
    cx.update(|window, app| {
        let dialog = probe.read(app).dialog.clone();
        dialog.update(app, |dialog, cx| dialog.present(Some(spec), window, cx));
    });
    cx.simulate_input("-two");
    cx.run_until_parked();
    probe.update(&mut cx, |probe, _| {
        assert!(
            probe
                .intents
                .borrow()
                .contains(&DialogIntent::FieldChanged {
                    dialog: DialogId::new("worktree"),
                    field: "folder".to_owned(),
                    value: "checkout-two".to_owned(),
                })
        );
        assert!(!probe.intents.borrow().iter().any(|intent| matches!(intent,
            DialogIntent::FieldChanged { field, .. } if field == "start")));
    });
    cx.simulate_keystrokes("enter");
    probe.update(&mut cx, |probe, _| {
        assert_eq!(
            probe
                .intents
                .borrow()
                .iter()
                .filter(|intent| matches!(intent, DialogIntent::Activate { .. }))
                .count(),
            1
        );
    });
}

#[gpui_kit::test]
fn attachment_pills_edit_delete_and_undo_in_the_prompt(cx: &TestAppContext) {
    use assert_fs::prelude::*;
    use pretty_assertions::assert_eq;
    let file = assert_fs::NamedTempFile::new("notes.md").unwrap();
    file.write_str("attachment contents").unwrap();
    let path = file.path().to_owned();
    let (probe, mut cx) =
        full_surface_probe_with_viewport(cx, native_agent_form_spec(), 1200., 900., 16.);
    cx.simulate_input("Read ");
    click(&mut cx, "dialog-prompt-attach");
    cx.simulate_keystrokes("down down enter");
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
    cx.run_until_parked();
    cx.simulate_input(" please");
    cx.run_until_parked();
    let latest_text = |cx: &mut VisualTestContext| {
        probe.read_with(cx, |probe, _| {
            probe
                .intents
                .borrow()
                .iter()
                .rev()
                .find_map(|intent| match intent {
                    DialogIntent::TextChanged { value, .. } => Some(value.clone()),
                    _ => None,
                })
                .unwrap()
        })
    };
    assert_eq!(latest_text(&mut cx), "Read [notes.md] please");
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "ctrl-a right right right right right delete"
    } else {
        "home right right right right right delete"
    });
    cx.run_until_parked();
    assert_eq!(latest_text(&mut cx), "Read  please");
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    cx.run_until_parked();
    assert_eq!(latest_text(&mut cx), "Read [notes.md] please");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    probe.read_with(&cx, |probe, _| {
        let intents = probe.intents.borrow();
        let attachments = intents
            .iter()
            .rev()
            .find_map(|intent| match intent {
                DialogIntent::AttachmentsChanged { attachments, .. } => Some(attachments),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            attachments
                .iter()
                .map(|attachment| attachment.path.clone())
                .collect::<Vec<_>>(),
            vec![path]
        );
    });
}

#[gpui_kit::test]
fn creation_prompt_keeps_configured_mux_shortcuts_without_typing_them(cx: &TestAppContext) {
    use bootty_ui::{commands::CommandCatalog, gpui_actions, keymap_runtime::KeymapRuntime};
    let file = assert_fs::NamedTempFile::new("config.toml").expect("private configuration");
    std::fs::write(file.path(), "").expect("configuration");
    let mut config = bootty_config::config::load_config_from_path(file.path()).unwrap();
    config.input.keybind = vec!["alt+2=select_tab:2".into(), "cmd+t=new_tab".into()];
    let catalog = std::sync::Arc::new(CommandCatalog::default());
    let keymap = KeymapRuntime::new(&config, catalog.clone());
    let (probe, mut cx) =
        full_surface_probe_with_viewport(cx, native_agent_form_spec(), 1200., 900., 16.);
    cx.update(|_, cx| {
        gpui_actions::replace_workspace_key_bindings(
            gpui_actions::key_bindings_for_native_conversation(
                keymap.snapshot(),
                bootty_config::config::MultiplexerBackendConfig::Native,
                &catalog,
            ),
            cx,
        )
        .expect("composer bindings");
    });
    cx.simulate_keystrokes("alt-2");
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    probe.update(&mut cx, |probe, _| {
        assert_eq!(probe.commands, ["select_tab:2", "new_tab"]);
        assert!(
            !probe
                .intents
                .borrow()
                .iter()
                .any(|intent| matches!(intent, DialogIntent::TextChanged { .. }))
        );
    });
}

#[gpui_kit::test]
fn composer_provider_focus_action_reaches_the_real_picker(cx: &TestAppContext) {
    let (probe, mut cx) =
        full_surface_probe_with_viewport(cx, native_agent_form_spec(), 1200., 900., 16.);
    probe.update_in(&mut cx, |probe, window, cx| {
        probe.dialog.update(cx, |dialog, cx| {
            dialog.perform(
                CommandAction::Focus(bootty_gpui::ComposerControl::Provider),
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("model-picker-content").is_some());
    click(&mut cx, "model-agent-provider:Claude");
    probe.read_with(&cx, |probe, _| {
        assert!(probe.intents.borrow().iter().any(|intent| matches!(intent, DialogIntent::FieldChanged { field, .. } if field == "provider")), "focus action must reach the real menu and select a provider");
    });
    assert!(cx.debug_bounds("model-picker-content").is_some());
}

#[gpui_kit::test]
fn composer_space_focus_reaches_a_single_available_space(cx: &TestAppContext) {
    let mut spec = native_agent_form_spec();
    spec.fields.push(bootty_gpui::DialogField {
        id: "host".into(),
        label: "Space".into(),
        value: "Current Space".into(),
        placeholder: String::new(),
        kind: bootty_gpui::DialogFieldKind::Choice(vec!["Current Space".into()]),
    });
    let (probe, mut cx) = full_surface_probe_with_viewport(cx, spec, 1200., 900., 16.);
    probe.update_in(&mut cx, |probe, window, cx| {
        probe.dialog.update(cx, |dialog, cx| {
            dialog.perform(
                CommandAction::Focus(bootty_gpui::ComposerControl::Space),
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    probe.read_with(&cx, |probe, _| {
        assert!(probe.intents.borrow().iter().any(|intent| matches!(intent,
            DialogIntent::FieldChanged { field, value, .. } if field == "host" && value == "Current Space"
        )), "single-choice controls must remain reachable by the focus action");
    });
}

#[gpui_kit::test]
fn composer_model_focus_action_reaches_the_real_picker(cx: &TestAppContext) {
    let mut spec = native_agent_form_spec();
    spec.fields.push(bootty_gpui::DialogField {
        id: "model".into(),
        label: "Model".into(),
        value: "current".into(),
        placeholder: String::new(),
        kind: bootty_gpui::DialogFieldKind::Choice(vec!["current".into()]),
    });
    spec.models.push(bootty_agents::NativeModelOption {
        id: "current".into(),
        display_name: "Current model".into(),
        reasoning_efforts: Vec::new(),
        default_reasoning_effort: None,
        is_default: true,
        is_legacy: false,
        is_favorite: false,
        supports_fast_mode: false,
        fast_mode_detail: None,
    });
    let (probe, mut cx) = full_surface_probe_with_viewport(cx, spec, 1200., 900., 16.);
    probe.update_in(&mut cx, |probe, window, cx| {
        probe.dialog.update(cx, |dialog, cx| {
            dialog.perform(
                CommandAction::Focus(bootty_gpui::ComposerControl::Model),
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("model-picker-content").is_some());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    probe.read_with(&cx, |probe, _| {
        assert!(probe.intents.borrow().iter().any(|intent| matches!(intent,
            DialogIntent::FieldChanged { field, value, .. } if field == "model" && value == "current"
        )), "focus action must open the model menu and accept its advertised selection");
    });
}

#[gpui_kit::test]
fn composer_control_shortcuts_open_and_select_without_an_extra_activation(cx: &TestAppContext) {
    for (control, field, label) in [
        (
            bootty_gpui::ComposerControl::Effort,
            "reasoning",
            "Reasoning",
        ),
        (
            bootty_gpui::ComposerControl::Permissions,
            "permissions",
            "Permissions",
        ),
    ] {
        let mut spec = native_agent_form_spec();
        spec.fields.push(bootty_gpui::DialogField {
            id: field.into(),
            label: label.into(),
            value: if field == "reasoning" {
                "Medium"
            } else {
                "Supervised"
            }
            .into(),
            placeholder: String::new(),
            kind: bootty_gpui::DialogFieldKind::Choice(if field == "reasoning" {
                vec!["Medium".into(), "High".into()]
            } else {
                vec!["Supervised".into(), "Full access".into()]
            }),
        });
        if field == "reasoning" {
            spec.models.push(bootty_agents::NativeModelOption {
                id: "current".into(),
                display_name: "Current model".into(),
                reasoning_efforts: vec!["medium".into(), "high".into()],
                default_reasoning_effort: Some("medium".into()),
                is_default: true,
                is_favorite: false,
                supports_fast_mode: false,
                fast_mode_detail: None,
                is_legacy: false,
            });
            spec.selected_model = Some("current".into());
        }
        let (probe, mut cx) = full_surface_probe_with_viewport(cx, spec, 1200., 900., 16.);
        probe.update_in(&mut cx, |probe, window, cx| {
            probe.dialog.update(cx, |dialog, cx| {
                dialog.perform(CommandAction::Focus(control), window, cx);
            });
        });
        cx.run_until_parked();
        if field == "reasoning" {
            assert!(cx.debug_bounds("model-picker-content").is_some());
            probe.read_with(&cx, |probe, _| {
                assert!(
                    probe.intents.borrow().is_empty(),
                    "Opening effort must not change it"
                );
            });
            cx.simulate_keystrokes("right enter");
        } else {
            probe.update_in(&mut cx, |_, window, _| {
                assert!(
                    gpui_kit::base::test_support::snapshots(window)
                        .iter()
                        .any(|item| item.path().last()
                            == Some(&gpui_kit::ElementId::from("popup-menu"))
                            && item.visible()),
                    "Permissions shortcut must open its menu"
                );
            });
            cx.simulate_keystrokes("down enter");
        }
        cx.run_until_parked();
        probe.read_with(&cx, |probe, _| {
            assert!(probe.intents.borrow().iter().any(|intent| matches!(intent,
                DialogIntent::FieldChanged { field: changed, value, .. } if changed == field && value == if field == "reasoning" { "high" } else { "Supervised" }
            )), "{field} shortcut must permit selection; intents: {:?}", probe.intents.borrow());
        });
    }
}

#[gpui_kit::test]
fn effort_commands_skip_hidden_levels_and_stop_at_the_model_bounds(cx: &TestAppContext) {
    let mut spec = native_agent_form_spec();
    spec.selected_model = Some("current".into());
    spec.available_efforts = vec!["low".into(), "high".into()];
    spec.fields.push(bootty_gpui::DialogField {
        id: "reasoning".into(),
        label: "Reasoning".into(),
        value: "medium".into(),
        placeholder: String::new(),
        kind: bootty_gpui::DialogFieldKind::Choice(vec!["medium".into()]),
    });
    spec.models.push(bootty_agents::NativeModelOption {
        id: "current".into(),
        display_name: "Current model".into(),
        reasoning_efforts: vec!["low".into(), "medium".into(), "high".into()],
        default_reasoning_effort: Some("medium".into()),
        is_default: true,
        is_legacy: false,
        is_favorite: false,
        supports_fast_mode: false,
        fast_mode_detail: None,
    });
    let (probe, mut cx) = full_surface_probe_with_viewport(cx, spec.clone(), 1200., 900., 16.);
    for (current, action, expected) in [
        ("medium", CommandAction::EffortDown, Some("low")),
        ("low", CommandAction::EffortUp, Some("high")),
        ("high", CommandAction::EffortUp, None),
        ("low", CommandAction::EffortDown, None),
    ] {
        spec.fields
            .iter_mut()
            .find(|field| field.id == "reasoning")
            .unwrap()
            .value = current.into();
        probe.update_in(&mut cx, |probe, window, cx| {
            probe.intents.borrow_mut().clear();
            probe.dialog.update(cx, |dialog, cx| {
                dialog.present(Some(spec.clone()), window, cx);
                dialog.perform(action, window, cx);
            });
        });
        cx.run_until_parked();
        probe.read_with(&cx, |probe, _| {
            let intents = probe.intents.borrow();
            let actual = intents.iter().find_map(|intent| match intent {
                DialogIntent::FieldChanged { field, value, .. } if field == "reasoning" => {
                    Some(value.as_str())
                }
                _ => None,
            });
            pretty_assertions::assert_eq!(actual, expected);
        });
    }
}

/// The threshold counts UTF-8 bytes, and the temporary file retains the complete paste.
#[gpui_kit::test]
fn creation_composer_attaches_large_pastes_and_keeps_small_text_editable(cx: &TestAppContext) {
    use pretty_assertions::assert_eq;
    for text in [
        "x".repeat(32 * 1024 - 1),
        "x".repeat(32 * 1024),
        "🥟".repeat(8192),
    ] {
        let (probe, mut cx) =
            full_surface_probe_with_viewport(cx, native_agent_form_spec(), 1200., 900., 16.);
        cx.simulate_input("Read ");
        cx.update(|_, cx| cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.clone())));
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-v"
        } else {
            "ctrl-v"
        });
        cx.run_until_parked();
        cx.simulate_input(" please");
        cx.run_until_parked();
        probe.read_with(&cx, |probe, _| {
            let intents = probe.intents.borrow();
            let prompt = intents
                .iter()
                .rev()
                .find_map(|intent| match intent {
                    DialogIntent::TextChanged { value, .. } => Some(value),
                    _ => None,
                })
                .unwrap();
            let attachments = intents.iter().rev().find_map(|intent| match intent {
                DialogIntent::AttachmentsChanged { attachments, .. } => Some(attachments),
                _ => None,
            });
            if text.len() < 32 * 1024 {
                assert_eq!(prompt, &format!("Read {text} please"));
                assert!(attachments.is_none_or(Vec::is_empty));
            } else {
                let attachments = attachments.unwrap();
                assert_eq!(attachments.len(), 1);
                let attachment = &attachments[0];
                assert_eq!(std::fs::read_to_string(&attachment.path).unwrap(), text);
                assert_eq!(attachment.path.extension().unwrap(), "txt");
                assert!(prompt.starts_with("Read [pasted-text-"));
                assert!(prompt.ends_with(".txt] please"));
            }
        });
    }
}

#[gpui_kit::test]
fn provider_auth_error_wraps_and_offers_sign_in_without_losing_details(cx: &TestAppContext) {
    let mut spec = native_agent_form_spec();
    spec.model_error = Some(format!(
        "Agent process ended: Unable to verify organization for the current authentication token.\nThis machine requires one of these organizations: {}\nTry again, or run: claude auth login",
        "5312fef9-f7d9-4b02-99ad-75e2e9f050ff, ".repeat(40)
    ));
    let (probe, mut visual) = full_surface_probe_with_viewport(cx, spec, 1000.0, 900.0, 16.0);
    for width in [600., 1000.] {
        visual.simulate_resize(gpui_kit::size(px(width), px(900.)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let summary = visual
            .debug_bounds("new-session-model-error")
            .expect("bounded summary");
        assert!(
            summary.left() >= px(0.) && summary.right() <= px(width),
            "{summary:?}"
        );
        let login = visual
            .debug_bounds("new-session-provider-sign-in")
            .expect("sign-in button");
        assert!(login.left() >= px(0.) && login.right() <= px(width));
    }
    assert!(
        visual
            .debug_bounds("new-session-model-error-details")
            .is_none()
    );
    click(&mut visual, "new-session-model-error-details-toggle");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let details = visual
        .debug_bounds("new-session-model-error-details")
        .expect("original error details");
    assert!(details.size.height > px(0.) && details.size.height <= px(200.));
    assert!(details.right() <= px(1000.));
    click(&mut visual, "new-session-provider-sign-in");
    probe.update(&mut visual, |probe, _| {
        assert!(probe.intents.borrow().iter().any(|intent| matches!(intent,
            DialogIntent::Activate { action, .. } if action == &ActionId::new("sign-in-provider")
        )));
    });
}

#[gpui_kit::test]
fn loading_models_keeps_the_composer_and_picker_in_place(cx: &TestAppContext) {
    for width in [600., 1200.] {
        let mut spec = native_agent_form_spec();
        spec.fields
            .retain(|field| !matches!(field.id.as_str(), "model" | "reasoning"));
        spec.models.clear();
        spec.models_loading = true;
        let (probe, mut visual) =
            full_surface_probe_with_viewport(cx, spec.clone(), width, 900., 16.);
        let before = visual.debug_bounds("model-picker-trigger").unwrap();
        let composer = visual.debug_bounds("agent-session-composer").unwrap();
        spec.models_loading = false;
        spec.models.push(bootty_agents::NativeModelOption {
            id: "current".into(),
            display_name: "Current model".into(),
            reasoning_efforts: vec!["high".into()],
            default_reasoning_effort: Some("high".into()),
            is_default: true,
            is_legacy: false,
            is_favorite: false,
            supports_fast_mode: false,
            fast_mode_detail: None,
        });
        spec.fields.push(bootty_gpui::DialogField {
            id: "model".into(),
            label: "Model".into(),
            value: "Current model".into(),
            placeholder: String::new(),
            kind: bootty_gpui::DialogFieldKind::Choice(vec!["Current model".into()]),
        });
        probe.update_in(&mut visual, |probe, window, cx| {
            probe
                .dialog
                .update(cx, |dialog, cx| dialog.present(Some(spec), window, cx));
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let after = visual.debug_bounds("model-picker-trigger").unwrap();
        assert_eq!(before.origin, after.origin, "picker moved at width {width}");
        assert_eq!(
            composer,
            visual.debug_bounds("agent-session-composer").unwrap()
        );
    }
}
