#![cfg(test)]

use std::{cell::RefCell, rc::Rc};

use bootty_ui::gpui::{
    GpuiSpaceEditor, OptionalSpaceEditorChoice, RemoteSpaceSnapshot, SpaceEditorChoice,
    SpaceEditorColors, SpaceEditorIcon, SpaceEditorIntent, SpaceEditorSnapshot, UiPalette,
    init_theme,
};
use gpui_kit::component::{Root, WindowExt as _};
use gpui_kit::{
    AppContext as _, Bounds, Context, Entity, IntoElement, Modifiers, Pixels, Render, ScrollDelta,
    ScrollWheelEvent, TestAppContext, Window, div, point, prelude::*, px,
};

struct SpaceEditorProbe {
    editor: Entity<GpuiSpaceEditor>,
    intents: Rc<RefCell<Vec<SpaceEditorIntent>>>,
}

struct SpaceEditorDialogSurface {
    background_focus: gpui_kit::FocusHandle,
}

impl Render for SpaceEditorDialogSurface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.background_focus)
            .children(Root::render_dialog_layer(window, cx))
    }
}

impl SpaceEditorProbe {
    fn new(snapshot: SpaceEditorSnapshot, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| GpuiSpaceEditor::new_with_window(snapshot, window, cx));
        let intents = Rc::new(RefCell::new(Vec::new()));
        let received = Rc::clone(&intents);
        cx.subscribe(&editor, move |_, _, intent: &SpaceEditorIntent, _| {
            received.borrow_mut().push(intent.clone());
        })
        .detach();
        Self { editor, intents }
    }
}

impl Render for SpaceEditorProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.editor.clone())
    }
}

#[gpui_kit::test]
fn editor_inputs_and_choices_emit_domain_intents(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(cx, snapshot());

    click(&mut cx, "space-editor-name");
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    });
    cx.simulate_input("Renamed");
    click(&mut cx, "space-backend-default");
    click(&mut cx, "space-tint-sidebar");

    probe.update(&mut cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(intents.iter().any(|intent| matches!(
            intent,
            SpaceEditorIntent::SetName(name) if name == "Renamed"
        )));
        assert!(
            intents
                .iter()
                .any(|intent| matches!(intent, SpaceEditorIntent::SelectBackend(None)))
        );
        assert!(
            intents
                .iter()
                .any(|intent| matches!(intent, SpaceEditorIntent::SetTintSidebar(true)))
        );
    });
}

#[gpui_kit::test]
fn editor_focus_targets_the_retained_name_input(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(cx, snapshot());

    cx.update(|window, app| {
        probe.update(app, |probe, app| {
            probe
                .editor
                .update(app, |editor, app| editor.focus(window, app));
        });
    });
    cx.refresh().expect("redraw after editor focus");
    let has_focused_input = cx.update(|window, app| window.focused_input(app).is_some());
    assert!(
        has_focused_input,
        "editor focus selects the retained InputState"
    );
}

#[gpui_kit::test]
fn editor_keeps_actions_visible_in_a_short_window(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_space_editor_dialog_probe(cx, snapshot());
    cx.simulate_resize(gpui_kit::size(px(800.0), px(500.0)));
    cx.refresh().expect("render short Space editor");

    let scroll = cx
        .debug_bounds("space-editor-scroll")
        .expect("scrolling form");
    assert!(scroll.size.height > px(0.0));
    assert!(scroll.bottom() <= px(500.0));
    let footer = cx
        .debug_bounds("space-editor-footer")
        .expect("dialog footer");
    let cancel = cx
        .debug_bounds("space-editor-cancel")
        .expect("dialog cancel button");
    let save = cx
        .debug_bounds("space-editor-save")
        .expect("dialog save button");
    assert!(footer.bottom() <= px(500.0));
    assert!(cancel.origin.y >= footer.origin.y);
    assert!(save.origin.y >= footer.origin.y);
    click(&mut cx, "space-editor-save");
    click(&mut cx, "space-editor-cancel");
    probe.update(&mut cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(intents.contains(&SpaceEditorIntent::Save));
        assert!(intents.contains(&SpaceEditorIntent::Close));
    });
}

#[gpui_kit::test]
fn large_icon_catalog_search_reaches_offscreen_choices(cx: &TestAppContext) {
    let mut catalog = snapshot();
    catalog.icons = (0..3200)
        .map(|index| SpaceEditorIcon {
            id: format!("icon-{index}"),
            glyph: "T".to_owned(),
            label: format!("Icon {index}"),
            selected: index == 0,
        })
        .collect();
    let (probe, mut cx) = rooted_probe(cx, catalog);
    assert!(cx.debug_bounds("space-icon-icon-0").is_some());
    assert!(cx.debug_bounds("space-icon-icon-3199").is_none());

    click(&mut cx, "space-icon-search");
    cx.simulate_input("icon-3199");
    cx.refresh().expect("filter icon catalog");
    click(&mut cx, "space-icon-icon-3199");
    probe.update(&mut cx, |probe, _| {
        assert!(
            probe
                .intents
                .borrow()
                .contains(&SpaceEditorIntent::SelectIcon("icon-3199".to_owned()))
        );
    });
}

#[gpui_kit::test]
fn color_editor_uses_the_shared_color_picker(cx: &TestAppContext) {
    let (_, mut cx) = rooted_probe(cx, snapshot());

    assert!(
        cx.debug_bounds("space-color-picker").is_some(),
        "Space color editing is represented by the shared ColorPicker"
    );
    assert!(
        cx.debug_bounds("space-color-0-lower").is_none(),
        "the editor no longer renders one button pair per RGB channel"
    );
}

#[gpui_kit::test]
fn remote_loading_error_and_create_controls_keep_their_intents(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(
        cx,
        remote_snapshot(RemoteSpaceSnapshot::Failed {
            message: "connection failed".to_owned(),
        }),
    );
    scroll_editor(&mut cx);
    click(&mut cx, "space-remote-retry");

    probe.update(&mut cx, |probe, _| {
        assert!(
            probe
                .intents
                .borrow()
                .iter()
                .any(|intent| matches!(intent, SpaceEditorIntent::RetryRemoteSpaces))
        );
    });

    probe.update(&mut cx, |probe, cx| {
        probe.editor.update(cx, |editor, cx| {
            editor.set_snapshot(
                remote_snapshot(RemoteSpaceSnapshot::Ready {
                    spaces: Vec::new(),
                    warning: None,
                    new_name: "Created remotely".to_owned(),
                    create_backends: vec![SpaceEditorChoice {
                        id: "native".to_owned(),
                        label: "Native".to_owned(),
                        detail: None,
                        selected: true,
                        enabled: true,
                    }],
                    can_create: true,
                }),
                cx,
            );
        });
    });
    cx.run_until_parked();
    click(&mut cx, "space-create-remote");

    probe.update(&mut cx, |probe, _| {
        assert!(probe.intents.borrow().iter().any(|intent| matches!(
            intent,
            SpaceEditorIntent::CreateRemoteSpace { name, backend }
                if name == "Created remotely" && backend == "native"
        )));
    });
}

#[gpui_kit::test]
fn disabled_save_and_name_validation_are_visible(cx: &TestAppContext) {
    let mut snapshot = snapshot();
    snapshot.can_save = false;
    snapshot.name_error = Some("A Space name is required.".to_owned());
    let (probe, mut cx) = rooted_space_editor_dialog_probe(cx, snapshot);

    assert!(cx.debug_bounds("space-editor-name-error").is_some());
    assert!(cx.debug_bounds("space-editor-save").is_some());
    click(&mut cx, "space-editor-save");

    probe.update(&mut cx, |probe, _| {
        assert!(
            !probe
                .intents
                .borrow()
                .iter()
                .any(|intent| matches!(intent, SpaceEditorIntent::Save))
        );
    });
}

#[gpui_kit::test]
fn icon_search_filters_and_selects_by_domain_identity(cx: &TestAppContext) {
    let mut snapshot = snapshot();
    snapshot.icons.push(SpaceEditorIcon {
        id: "briefcase".to_owned(),
        glyph: "B".to_owned(),
        label: "Briefcase".to_owned(),
        selected: false,
    });
    let (probe, mut cx) = rooted_probe(cx, snapshot);

    click(&mut cx, "space-icon-search");
    cx.simulate_input("briefcase");
    assert!(cx.debug_bounds("space-icon-briefcase").is_some());
    assert!(cx.debug_bounds("space-icon-terminal").is_none());
    click(&mut cx, "space-icon-briefcase");

    probe.update(&mut cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(intents.iter().any(|intent| matches!(
            intent,
            SpaceEditorIntent::SetIconSearch(value) if value == "briefcase"
        )));
        assert!(intents.iter().any(
            |intent| matches!(intent, SpaceEditorIntent::SelectIcon(id) if id == "briefcase")
        ));
    });
}

#[gpui_kit::test]
fn disabled_space_editor_controls_are_pointer_noops(cx: &TestAppContext) {
    let mut snapshot = snapshot();
    snapshot.backend_enabled = false;
    snapshot
        .backends
        .iter_mut()
        .for_each(|choice| choice.enabled = false);
    snapshot.locations[0].enabled = false;
    snapshot.remote = RemoteSpaceSnapshot::Ready {
        spaces: Vec::new(),
        warning: None,
        new_name: "Remote".to_owned(),
        create_backends: vec![SpaceEditorChoice {
            id: "native".to_owned(),
            label: "Native".to_owned(),
            detail: None,
            selected: true,
            enabled: true,
        }],
        can_create: false,
    };
    let (probe, mut cx) = rooted_probe(cx, snapshot);

    click(&mut cx, "space-backend-default");
    click(&mut cx, "space-location-local");
    scroll_editor(&mut cx);
    click(&mut cx, "space-create-remote");

    probe.update(&mut cx, |probe, _| {
        assert!(probe.intents.borrow().is_empty());
    });
}

#[gpui_kit::test]
fn space_editor_cancel_button_emits_typed_close(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_probe(cx, snapshot());
    probe.update(&mut cx, |probe, cx| {
        probe
            .editor
            .update(cx, |editor, cx| editor.request_close(cx));
    });

    probe.update(&mut cx, |probe, _| {
        assert_eq!(
            probe
                .intents
                .borrow()
                .iter()
                .filter(|intent| matches!(intent, SpaceEditorIntent::Close))
                .count(),
            1
        );
    });
}

fn rooted_probe(
    cx: &TestAppContext,
    snapshot: SpaceEditorSnapshot,
) -> (Entity<SpaceEditorProbe>, gpui_kit::VisualTestContext) {
    let probe_slot = Rc::new(RefCell::new(None));
    let opened_probe = Rc::clone(&probe_slot);
    let window = cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
        cx.open_window(
            gpui_kit::WindowOptions {
                window_bounds: Some(gpui_kit::WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: gpui_kit::size(px(800.0), px(1200.0)),
                })),
                ..Default::default()
            },
            move |window, cx| {
                let probe = cx.new(|cx| SpaceEditorProbe::new(snapshot, window, cx));
                opened_probe.replace(Some(probe.clone()));
                cx.new(|cx| Root::new(probe, window, cx).bordered(false))
            },
        )
        .expect("open rooted Space editor window")
    });
    let probe = probe_slot
        .borrow_mut()
        .take()
        .expect("capture Space editor probe");
    let mut visual = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    visual.simulate_resize(gpui_kit::size(px(800.0), px(1200.0)));
    visual.refresh().expect("render Space editor");
    (probe, visual)
}

fn rooted_space_editor_dialog_probe(
    cx: &TestAppContext,
    snapshot: SpaceEditorSnapshot,
) -> (Entity<SpaceEditorProbe>, gpui_kit::VisualTestContext) {
    let probe_slot = Rc::new(RefCell::new(None));
    let opened_probe = Rc::clone(&probe_slot);
    let window = cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
        cx.open_window(
            gpui_kit::WindowOptions {
                window_bounds: Some(gpui_kit::WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: gpui_kit::size(px(800.0), px(1200.0)),
                })),
                ..Default::default()
            },
            move |window, cx| {
                let probe = cx.new(|cx| SpaceEditorProbe::new(snapshot, window, cx));
                opened_probe.replace(Some(probe));
                let surface = cx.new(|cx| SpaceEditorDialogSurface {
                    background_focus: cx.focus_handle(),
                });
                cx.new(|cx| Root::new(surface, window, cx).bordered(false))
            },
        )
        .expect("open rooted Space editor dialog window")
    });
    let probe = probe_slot
        .borrow_mut()
        .take()
        .expect("capture Space editor dialog probe");
    let mut visual = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    visual.simulate_resize(gpui_kit::size(px(800.0), px(1200.0)));
    visual.refresh().expect("render Space editor dialog");
    visual.update(|window, cx| {
        let editor = probe.read(cx).editor.clone();
        window.open_dialog(cx, move |dialog, window, app| {
            let editor = editor.clone();
            let footer = GpuiSpaceEditor::render_dialog_footer(editor.clone(), app);
            dialog
                .w(gpui_kit::px(f32::from(window.rem_size()) * 37.5))
                .max_w(gpui_kit::px(f32::from(window.rem_size()) * 45.0))
                .title("Edit Space")
                .footer(footer)
                .content(move |content, _, _| content.p_0().child(editor.clone()))
        });
    });
    visual.run_until_parked();
    visual.refresh().expect("render active Space editor dialog");
    (probe, visual)
}

fn click(cx: &mut gpui_kit::VisualTestContext, selector: &'static str) {
    let bounds = cx.debug_bounds(selector).expect("control exists");
    cx.simulate_click(center(bounds), Modifiers::none());
    cx.run_until_parked();
}

fn scroll_editor(cx: &mut gpui_kit::VisualTestContext) {
    let bounds = cx
        .debug_bounds("space-editor-scroll")
        .expect("editor scroll region");
    cx.simulate_event(ScrollWheelEvent {
        position: bounds.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-1000.0))),
        ..Default::default()
    });
    cx.refresh().expect("refresh after editor scroll");
}

fn center(bounds: Bounds<Pixels>) -> gpui_kit::Point<Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(px(left + width / 2.0), px(top + height / 2.0))
}

fn snapshot() -> SpaceEditorSnapshot {
    SpaceEditorSnapshot {
        title: "Edit Space".to_owned(),
        name: "Original".to_owned(),
        name_error: None,
        icon_search: String::new(),
        icons: vec![SpaceEditorIcon {
            id: "terminal".to_owned(),
            glyph: "T".to_owned(),
            label: "Terminal".to_owned(),
            selected: true,
        }],
        color: [32, 64, 128],
        tint_sidebar: false,
        backends: vec![
            OptionalSpaceEditorChoice {
                id: Some("native".to_owned()),
                label: "Native".to_owned(),
                selected: true,
                enabled: true,
            },
            OptionalSpaceEditorChoice {
                id: None,
                label: "Default".to_owned(),
                selected: false,
                enabled: true,
            },
        ],
        backend_enabled: true,
        locations: vec![SpaceEditorChoice {
            id: "local".to_owned(),
            label: "Local".to_owned(),
            detail: None,
            selected: true,
            enabled: true,
        }],
        location_notice: None,
        remote: RemoteSpaceSnapshot::Hidden,
        can_save: true,
        colors: SpaceEditorColors::default(),
    }
}

fn remote_snapshot(remote: RemoteSpaceSnapshot) -> SpaceEditorSnapshot {
    let mut snapshot = snapshot();
    snapshot.remote = remote;
    snapshot
}
