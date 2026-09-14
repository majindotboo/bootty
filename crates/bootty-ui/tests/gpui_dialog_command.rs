#![cfg(test)]

use bootty_ui::gpui as bootty_gpui;
use std::{cell::RefCell, rc::Rc};

use bootty_ui::gpui::{
    ActionId, CommandAction, DialogAction, DialogId, DialogIntent, DialogPayload, DialogRole,
    DialogRow, DialogSpec, DialogView, RowId, UiPalette, init_theme, parse_keybinding,
};
use bootty_ui::presentation::dialogs::{
    DitchAction, DitchSessionDialog, DitchSessionEvent, SpaceMoveTarget, SpacePickerDialog,
    SpacePickerEvent,
};
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layer = Root::render_dialog_layer(window, cx);
        div()
            .size_full()
            .track_focus(&self.background_focus)
            .children(
                self.dialog
                    .read(cx)
                    .is_non_modal()
                    .then(|| self.dialog.clone()),
            )
            .children(layer)
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
    let dialog_id = spec.id.clone();
    let slot = Rc::new(RefCell::new(None));
    let opened = Rc::clone(&slot);
    let window = cx.update(|cx| {
        init_theme(UiPalette::default(), cx);
        cx.open_window(
            gpui_kit::WindowOptions {
                window_bounds: Some(gpui_kit::WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: gpui_kit::size(px(800.0), px(600.0)),
                })),
                ..Default::default()
            },
            move |window, cx| {
                let probe = cx.new(|cx| CommandDialogProbe::with_spec(window, cx, spec));
                opened.replace(Some(probe.clone()));
                cx.new(|cx| Root::new(probe, window, cx).bordered(false))
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
            if probe.dialog.read(cx).is_non_modal() {
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
fn ditch_uses_command_focus_navigation_and_cancellation(cx: &TestAppContext) {
    let mut ditch = DitchSessionDialog::open_remote("qa-session".to_owned(), None);
    let (probe, mut cx) = rooted_probe(cx, ditch.spec());
    cx.simulate_keystrokes("tab ctrl-n ctrl-p enter");
    let events = probe.read_with(&cx, |probe, _| {
        probe
            .intents
            .borrow()
            .iter()
            .filter_map(|intent| ditch.apply(intent))
            .collect::<Vec<_>>()
    });
    pretty_assertions::assert_eq!(
        events,
        vec![DitchSessionEvent::Ditch {
            session_id: "qa-session".to_owned(),
            cwd: None,
            action: DitchAction::KillOnly,
        }]
    );
    cx.simulate_keystrokes("escape");
    let events = probe.read_with(&cx, |probe, _| {
        probe
            .intents
            .borrow()
            .iter()
            .filter_map(|intent| ditch.apply(intent))
            .collect::<Vec<_>>()
    });
    assert_eq!(events.last(), Some(&DitchSessionEvent::Close));
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
        title: "Ditch session".to_owned(),
        icon: None,
        hint: Some("Enter confirm   Esc cancel".to_owned()),
        footer: None,
        text: None,
        text_label: None,
        fields: Vec::new(),
        busy: false,
        text_hint: None,
        rows: vec![
            DialogRow {
                detail: Some("This removes the worktree.".to_owned()),
                destructive: true,
                ..DialogRow::action("ditch", "Ditch", DialogAction::new("ditch"))
            },
            DialogRow::action("cancel-action", "Keep", DialogAction::new("keep")),
        ],
        empty_text: String::new(),
        placement: bootty_gpui::DialogPlacement::Center,
    };
    let (probe, mut cx) = rooted_probe(cx, spec);

    assert!(cx.debug_bounds("dialog-confirm-panel").is_some());
    assert!(cx.debug_bounds("dialog-confirm-detail-ditch").is_some());
    cx.simulate_keystrokes("enter");
    probe.update(&mut cx, |probe, _| {
        assert!(probe.intents.borrow().contains(&DialogIntent::Activate {
            dialog: DialogId::new("confirm"),
            row: RowId::new("ditch"),
            action: ActionId::new("ditch"),
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
