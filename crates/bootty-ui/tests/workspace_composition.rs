#![cfg(test)]

use bootty_mux::{
    controller::SpaceId,
    snapshot::{MuxPaneAnchor, MuxSession, MuxSessionTag, MuxWindow},
    workspace::ScopedWindowId,
};
use bootty_ui::workspace_composition::{
    EmptyTerminalState, center_eviction_home, has_selected_terminal, replace_terminal_region,
    terminal_leaf_state, terminal_panel_state,
};
use gpui_kit::component::dock::{
    DockPlacement, PaneTree, PanelBuilder, PanelId, PanelInfo, PanelState, RootKind,
};
use gpui_kit::px;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case(false, None, true, EmptyTerminalState::Loading)]
#[case(true, None, true, EmptyTerminalState::Ready { can_create: true })]
#[case(true, None, false, EmptyTerminalState::Ready { can_create: false })]
#[case(false, Some("Connecting to host"), true, EmptyTerminalState::Unavailable("Connecting to host".into()))]
#[case(true, Some("Connection lost"), true, EmptyTerminalState::Unavailable("Connection lost".into()))]
fn empty_terminal_requires_an_available_snapshot(
    #[case] has_snapshot: bool,
    #[case] unavailable: Option<&str>,
    #[case] can_create: bool,
    #[case] expected: EmptyTerminalState,
) {
    assert_eq!(
        EmptyTerminalState::from_snapshot(has_snapshot, unavailable, can_create),
        expected,
    );
}

#[rstest]
#[case("empty", "", true, false)]
#[case("populated", "live", true, true)]
#[case("populated", "closed", true, false)]
#[case("missing", "", true, false)]
#[case("empty", "", false, true)]
#[case("missing", "", false, false)]
fn terminal_presence_follows_selection(
    #[case] session: &str,
    #[case] window: &str,
    #[case] native_layout: bool,
    #[case] expected: bool,
) {
    let sessions = ["empty", "populated"].map(|id| MuxSession {
        id: id.to_owned(),
        name: id.to_owned(),
        active: id == session,
        anchor: MuxPaneAnchor::default(),
        active_window_id: (id == "populated").then(|| "live".to_owned()),
        tag: MuxSessionTag::default(),
        windows: if id == "populated" {
            vec![MuxWindow {
                id: "live".to_owned(),
                index: 0,
                name: "shell".to_owned(),
                active: true,
                anchor: MuxPaneAnchor::default(),
                panes: vec![MuxPaneAnchor::default()],
                layout: None,
                progress: None,
            }]
        } else {
            Vec::new()
        },
    });
    let selected = ScopedWindowId::new(
        SpaceId::from_persistence(1),
        session.to_owned(),
        window.to_owned(),
    );
    assert_eq!(
        has_selected_terminal(&sessions, &selected, native_layout),
        expected
    );
}

fn window_id() -> ScopedWindowId {
    ScopedWindowId::new(
        SpaceId::from_persistence(1),
        "session".to_owned(),
        "window".to_owned(),
    )
}

fn document_panel() -> PanelState {
    PanelState {
        panel_name: "bootty.document".to_owned(),
        children: Vec::new(),
        info: PanelInfo::panel(serde_json::json!({"path":"README.md"})),
    }
}

fn terminal_leaves(state: &PanelState, leaves: &mut Vec<PanelState>) {
    if state.panel_name == "bootty.terminal" {
        leaves.push(state.clone());
    }
    for child in &state.children {
        terminal_leaves(child, leaves);
    }
}

fn single_terminal_leaf(state: &PanelState) -> PanelState {
    let mut leaves = Vec::new();
    terminal_leaves(state, &mut leaves);
    assert_eq!(leaves.len(), 1, "expected one terminal leaf");
    leaves.remove(0)
}

#[rstest]
fn terminal_leaf_carries_the_mux_window_identity() {
    let leaf = terminal_leaf_state(&window_id(), "shell");

    assert_eq!(leaf.panel_name, "bootty.terminal");
    assert_eq!(leaf.children, Vec::<PanelState>::new());
    assert_eq!(
        leaf.info,
        PanelInfo::panel(serde_json::json!({
            "session": "session", "window": "window", "title": "shell",
        }))
    );
}

#[rstest]
fn replacing_terminal_projection_preserves_native_siblings() {
    let id = window_id();
    let current = PanelState {
        panel_name: "StackPanel".to_owned(),
        children: vec![
            terminal_panel_state(&id, "shell"),
            terminal_panel_state(&id, "shell"),
            document_panel(),
        ],
        info: PanelInfo::stack(vec![gpui_kit::px(1.0); 3], gpui_kit::Axis::Horizontal),
    };

    let reconciled = replace_terminal_region(current, terminal_leaf_state(&id, "renamed"));

    // The duplicated stale leaves collapse to one; the document sibling stays.
    let leaf = single_terminal_leaf(&reconciled);
    assert_eq!(
        leaf.info,
        PanelInfo::panel(serde_json::json!({
            "session": "session", "window": "window", "title": "renamed",
        }))
    );
}

#[derive(Default)]
struct RestoredPanels(Vec<PanelState>);

impl PanelBuilder for RestoredPanels {
    fn build(&mut self, state: &PanelState, _: &PanelInfo) -> PanelId {
        self.0.push(state.clone());
        PanelId::from_u64(u64::try_from(self.0.len()).unwrap())
    }
}

fn restored_panels(state: &PanelState) -> Vec<PanelState> {
    let mut panels = RestoredPanels::default();
    let tree = PaneTree::from_state(state, RootKind::Split, &mut panels);
    assert_eq!(tree.panels().count(), panels.0.len());
    panels.0
}

#[rstest]
#[case("", PanelInfo::panel(serde_json::Value::Null))]
#[case("StackPanel", PanelInfo::stack(Vec::new(), gpui_kit::Axis::Horizontal))]
#[case("StackPanel", PanelInfo::panel(serde_json::Value::Null))]
#[case("TabPanel", PanelInfo::tabs(0))]
#[case("TabPanel", PanelInfo::panel(serde_json::Value::Null))]
fn empty_center_restores_only_the_terminal_panel(#[case] name: &str, #[case] info: PanelInfo) {
    let leaf = terminal_leaf_state(&window_id(), "shell");
    let reconciled = replace_terminal_region(
        PanelState {
            panel_name: name.to_owned(),
            children: Vec::new(),
            info,
        },
        leaf.clone(),
    );

    // Exercise Kit's real decoder: a container nested inside Tabs is otherwise
    // dispatched to PanelRegistry as if it were an application panel.
    assert_eq!(restored_panels(&reconciled), vec![leaf]);
}

#[rstest]
#[case("StackPanel", PanelInfo::stack(vec![gpui_kit::px(240.)], gpui_kit::Axis::Vertical))]
#[case("StackPanel", PanelInfo::stack(vec![gpui_kit::px(240.)], gpui_kit::Axis::Horizontal))]
fn terminal_adoption_restores_existing_container_siblings(
    #[case] name: &str,
    #[case] info: PanelInfo,
) {
    let document = document_panel();
    let current = PanelState {
        panel_name: name.to_owned(),
        children: vec![document.clone()],
        info,
    };
    let leaf = terminal_leaf_state(&window_id(), "shell");
    let reconciled = replace_terminal_region(current.clone(), leaf.clone());

    assert_eq!(reconciled.children[1], current);
    assert_eq!(restored_panels(&reconciled), vec![leaf, document]);
}

#[rstest]
fn native_only_center_keeps_its_panels_beside_the_terminal() {
    let document = document_panel();
    let reconciled = replace_terminal_region(
        PanelState {
            panel_name: "TabPanel".to_owned(),
            children: vec![document.clone()],
            info: PanelInfo::tabs(0),
        },
        terminal_leaf_state(&window_id(), "shell"),
    );

    assert_eq!(reconciled.children.len(), 2);
    assert_eq!(reconciled.children[0], document);
    single_terminal_leaf(&reconciled);
}

proptest! {
    #[test]
    fn repairing_duplicate_terminal_tabs_keeps_the_selected_document(
        duplicates in 1usize..16,
        before in 0usize..8,
        after in 1usize..8,
    ) {
        let leaf = terminal_leaf_state(&window_id(), "shell");
        let document = |index: usize| PanelState {
            info: PanelInfo::panel(serde_json::json!({"path":format!("file-{index}")})),
            ..document_panel()
        };
        let children = (0..before).map(document)
            .chain(std::iter::repeat_n(leaf.clone(), duplicates))
            .chain((before..before.checked_add(after).expect("document count fits")).map(document))
            .collect::<Vec<_>>();
        let selected = children.last().unwrap().clone();
        let repaired = replace_terminal_region(PanelState {
            panel_name: "TabPanel".to_owned(),
            info: PanelInfo::tabs(children.len().checked_sub(1).expect("at least one child")),
            children,
        }, leaf.clone());

        assert_eq!(repaired.children.len(), before.checked_add(1).and_then(|count| count.checked_add(after)).expect("repaired child count fits"));
        assert_eq!(repaired.children[repaired.info.active_index().unwrap()], selected);
        assert_eq!(replace_terminal_region(repaired.clone(), leaf), repaired);
    }
}

#[rstest]
#[case(PanelInfo::stack(vec![gpui_kit::px(100.), gpui_kit::px(240.), gpui_kit::px(180.)], gpui_kit::Axis::Vertical),
       PanelInfo::stack(vec![gpui_kit::px(100.), gpui_kit::px(180.)], gpui_kit::Axis::Vertical))]
#[case(PanelInfo::stack(vec![gpui_kit::px(100.), gpui_kit::px(240.), gpui_kit::px(180.)], gpui_kit::Axis::Horizontal),
       PanelInfo::stack(vec![gpui_kit::px(100.), gpui_kit::px(180.)], gpui_kit::Axis::Horizontal))]
fn repairing_duplicate_terminal_leaves_keeps_surviving_geometry(
    #[case] original: PanelInfo,
    #[case] expected: PanelInfo,
) {
    let leaf = terminal_leaf_state(&window_id(), "shell");
    let repaired = replace_terminal_region(
        PanelState {
            panel_name: "StackPanel".to_owned(),
            children: vec![leaf.clone(), leaf.clone(), document_panel()],
            info: original,
        },
        leaf.clone(),
    );

    assert_eq!(repaired.info, expected);
    assert_eq!(repaired.children, vec![leaf, document_panel()]);
}

#[rstest]
#[case("bootty.sessions", DockPlacement::Left)]
#[case("bootty.codexbar", DockPlacement::Right)]
#[case("bootty.spaces", DockPlacement::Left)]
#[case("bootty.attachment", DockPlacement::Right)]
#[case("bootty.changes", DockPlacement::Right)]
#[case("bootty.diff", DockPlacement::Right)]
#[case("bootty.files", DockPlacement::Right)]
#[case("bootty.agents", DockPlacement::Right)]
#[case("bootty.jobs", DockPlacement::Right)]
#[case("bootty.transfers", DockPlacement::Right)]
#[case("bootty.recovery", DockPlacement::Right)]
#[case("bootty.shell", DockPlacement::Right)]
#[case("bootty.document", DockPlacement::Right)]
#[case("bootty.renamed-panel", DockPlacement::Right)]
fn stranded_center_panels_go_home(#[case] name: &str, #[case] home: DockPlacement) {
    assert_eq!(center_eviction_home(name, DockPlacement::Left), home);
}

#[rstest]
fn stranded_sidebar_chrome_follows_the_sidebar() {
    for name in ["bootty.sessions", "bootty.spaces"] {
        assert_eq!(
            center_eviction_home(name, DockPlacement::Right),
            DockPlacement::Right
        );
    }
}

proptest! {
    #[test]
    fn fixed_homes_preserve_documents_and_are_stable(
        paths in prop::collection::vec("[a-z]{1,24}\\.rs", 0..12),
        left_width in 160u16..450, right_width in 200u16..650,
        left_open in any::<bool>(), right_open in any::<bool>(),
    ) {
        use bootty_ui::workspace_composition::fixed_panel_layout;
        use gpui_kit::component::dock::{DockAreaState, DockState};
        let documents: Vec<_> = paths.into_iter().enumerate().map(|(ix, path)| PanelState {
            panel_name: "bootty.document".into(), children: vec![],
            info: PanelInfo::panel(serde_json::json!({"host":"remote-host", "path":path, "line":ix.saturating_add(1)})),
        }).collect();
        let selected = documents.last().cloned();
        let nested = PanelState { panel_name: "StackPanel".into(), children: documents.clone(), info: PanelInfo::stack(vec![], gpui_kit::Axis::Vertical) };
        let legacy = DockAreaState {
            version: Some(8), center: terminal_panel_state(&window_id(), "shell"),
            left_dock: Some(DockState::new(nested, DockPlacement::Left, px(f32::from(left_width)), true)),
            right_dock: Some(DockState::new(PanelState::new("bootty.sessions"), DockPlacement::Right, px(f32::from(right_width)), left_open)),
            bottom_dock: selected.map(|panel| DockState::new(panel, DockPlacement::Bottom, px(190.), right_open)),
        };
        let fixed = fixed_panel_layout(legacy);
        let left = fixed.left_dock.as_ref().unwrap();
        let right = fixed.right_dock.as_ref().unwrap();
        prop_assert_eq!(left.panel().children[0].panel_name.as_str(), "bootty.sessions");
        prop_assert_eq!(left.size(), px(f32::from(right_width)));
        prop_assert_eq!(left.open(), left_open);
        prop_assert_eq!(&right.panel().children, &documents);
        prop_assert!(fixed.bottom_dock.is_none());
        prop_assert_eq!(single_terminal_leaf(&fixed.center), terminal_leaf_state(&window_id(), "shell"));
        prop_assert_eq!(fixed_panel_layout(fixed.clone()), fixed);
    }
}

#[rstest]
#[case(true)]
#[case(false)]
fn fixed_homes_retain_active_document_and_closed_tool_region(
    #[case] open: bool,
    #[values("bootty.agents", "bootty.runs")] retired: &str,
) {
    use bootty_ui::workspace_composition::fixed_panel_layout;
    use gpui_kit::component::dock::{DockAreaState, DockState};
    let document = document_panel();
    let state = DockAreaState {
        right_dock: Some(DockState::new(
            PanelState {
                panel_name: "TabPanel".into(),
                children: vec![
                    PanelState::new("bootty.files"),
                    PanelState::new(retired),
                    document.clone(),
                ],
                info: PanelInfo::tabs(2),
            },
            DockPlacement::Right,
            px(350.),
            open,
        )),
        ..DockAreaState::default()
    };
    let fixed = fixed_panel_layout(state);
    let right = fixed.right_dock.unwrap();
    assert_eq!(right.panel().children[1], document);
    assert_eq!(right.panel().info.active_index(), Some(1));
    assert_eq!(right.size(), px(350.));
    assert_eq!(right.open(), open);
}

#[rstest]
fn fixed_center_keeps_backend_windows_and_retires_legacy_agent_panels() {
    use bootty_ui::workspace_composition::fixed_panel_layout;
    use gpui_kit::component::dock::DockAreaState;
    let agent = PanelState {
        panel_name: "bootty.conversation".into(),
        children: Vec::new(),
        info: PanelInfo::panel(
            serde_json::json!({"target":{"kind":"session","handle":"conversation-id","generation":3},"task":"exact-task"}),
        ),
    };
    let terminal = PanelState {
        panel_name: "bootty.terminal-window".into(),
        children: Vec::new(),
        info: PanelInfo::panel(
            serde_json::json!({"binding_id":"space-id","task_identity":"exact-task","window_key":"stable-window"}),
        ),
    };
    let state = DockAreaState {
        center: PanelState {
            panel_name: "TabPanel".into(),
            children: vec![
                agent,
                PanelState::new("bootty.surface-chooser"),
                terminal.clone(),
            ],
            info: PanelInfo::tabs(2),
        },
        ..DockAreaState::default()
    };
    let fixed = fixed_panel_layout(state);
    assert_eq!(fixed.center.children, [terminal]);
    assert_eq!(fixed.center.info.active_index(), Some(0));
    assert_eq!(fixed_panel_layout(fixed.clone()), fixed);
}

#[rstest]
fn admitted_terminal_window_is_retired_only_after_an_authoritative_closed_snapshot() {
    use bootty_config::config::{AppearanceVariant, BoottyConfig, MultiplexerBackendConfig};
    use bootty_mux::{
        backend::MuxBackend as _,
        command::MuxCommand,
        native::NativeBackend,
        repository::WorkspaceRepository,
        session_membership::{SessionMembership, SessionState, WorkspaceSession},
        workspace::WorkspaceRuntime,
    };
    use bootty_ui::workspace_composition::terminal_window_is_closed;
    use std::{sync::Arc, time::Duration};

    let directory = assert_fs::TempDir::new().expect("isolated native workspace");
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    config.multiplexer.backend = MultiplexerBackendConfig::Native;
    let (mut repository, stored) = WorkspaceRepository::open(&config.config_path).unwrap();
    let space = stored.spaces().first().unwrap();
    let scope = space.id();
    let saved = WorkspaceSession {
        identity: "retained-window-task".to_owned(),
        backend_name: "retained-window".to_owned(),
        display_name: "Work".to_owned(),
        explicit: true,
        cwd: directory.path().to_string_lossy().into_owned(),
        state: SessionState::default(),
        terminal_snapshot: None,
    };
    repository
        .commit_binding_state(
            scope,
            &SessionMembership::from_sessions(vec![saved.clone()]),
        )
        .unwrap();
    let mut backend = NativeBackend::for_workspace(&config.config_path);
    backend
        .execute(MuxCommand::CreateProjectSession {
            session_id: saved.backend_name.clone(),
            cwd: saved.cwd.clone(),
            tag: MuxSessionTag {
                identity: Some(saved.identity.clone()),
                space: Some(space.remote_id().to_owned()),
            },
            argv: Some(vec!["/bin/cat".to_owned()]),
        })
        .unwrap();
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    let mut workspace = WorkspaceRuntime::open(
        &config,
        "main",
        Arc::new(bootty_mux::provider::MuxBackendRegistry::desktop().unwrap()),
        AppearanceVariant::Light,
        Arc::clone(&repaint),
    )
    .unwrap();
    let binding = workspace.binding_mut(scope).unwrap();
    binding
        .mux_mut()
        .refresh_sessions(&repaint, &config.multiplexer, Duration::ZERO);
    let session = binding.session_attachment(&saved.identity).unwrap();
    let window = binding.window_id(session.id.clone(), session.windows[0].id.clone());
    assert!(!terminal_window_is_closed(
        binding,
        &saved.identity,
        &window
    ));
    backend
        .execute(MuxCommand::DitchSession {
            session_id: session.id.clone(),
        })
        .unwrap();
    // Missing current publication during refresh is not evidence of closure.
    binding.mux_mut().refresh_on_next_frame();
    assert!(!terminal_window_is_closed(
        binding,
        &saved.identity,
        &window
    ));
    binding
        .mux_mut()
        .refresh_sessions(&repaint, &config.multiplexer, Duration::ZERO);
    assert!(binding.mux().has_session_snapshot());
    assert!(terminal_window_is_closed(binding, &saved.identity, &window));
    assert!(!terminal_window_is_closed(binding, "foreign-task", &window));
    assert!(
        binding.sessions().get(&saved.identity).is_some(),
        "retiring presentation preserves saved membership"
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn legacy_outer_agent_tabs_are_adopted_once_but_actual_splits_keep_their_geometry(
    #[case] split: bool,
) {
    use bootty_ui::workspace_composition::{
        center_contains_conversation, surface_center_key, take_legacy_surface_tabs,
    };
    let agent = PanelState {
        panel_name: "bootty.conversation".to_owned(),
        children: Vec::new(),
        info: PanelInfo::panel(serde_json::json!({
            "target": {"kind":"session", "handle":"exact-agent", "generation":3},
            "task":"exact-task"
        })),
    };
    let terminal = terminal_leaf_state(&window_id(), "own terminal");
    let mut center = PanelState {
        panel_name: if split { "StackPanel" } else { "TabPanel" }.to_owned(),
        children: vec![terminal.clone(), agent.clone()],
        info: if split {
            PanelInfo::stack(vec![px(420.0), px(180.0)], gpui_kit::Axis::Horizontal)
        } else {
            PanelInfo::tabs(1)
        },
    };
    let previous = center.clone();
    let moved = take_legacy_surface_tabs(&mut center, "binding", "exact-task");
    if split {
        assert_eq!(center, previous, "an ambiguous saved split remains intact");
        assert_eq!(moved, Vec::new());
        assert!(center_contains_conversation(&center, "exact-agent"));
    } else {
        assert_eq!(center.children, [terminal]);
        assert_eq!(moved.len(), 1);
        assert_eq!(
            moved[0].0,
            surface_center_key("binding", "exact-task", "agent", "exact-agent")
        );
        assert_eq!(moved[0].1.children, [agent]);
        assert!(!center_contains_conversation(&center, "exact-agent"));
        let key = moved[0].0.clone();
        let saved = std::collections::BTreeMap::from([(key.clone(), moved[0].1.clone())]);
        let restored: std::collections::BTreeMap<String, PanelState> =
            serde_json::from_value(serde_json::to_value(&saved).unwrap()).unwrap();
        assert_eq!(restored, saved);
        assert!(center_contains_conversation(&restored[&key], "exact-agent"));
    }
    assert!(
        take_legacy_surface_tabs(&mut center, "binding", "exact-task").is_empty(),
        "adoption is not duplicated"
    );
}

#[rstest]
fn saved_terminal_tabs_keep_separate_owners_and_closed_split_leaves_do_not_reappear() {
    use bootty_ui::workspace_composition::{
        TerminalSurfaceOrigin, center_contains_window, center_primary_window_key,
        remove_terminal_surface, surface_center_key, take_legacy_surface_tabs,
    };
    let origin = TerminalSurfaceOrigin {
        binding_id: "own-binding".to_owned(),
        task_identity: "own-task".to_owned(),
        window_key: "saved-window-two".to_owned(),
    };
    let other = TerminalSurfaceOrigin {
        binding_id: "other-binding".to_owned(),
        ..origin.clone()
    };
    let leaf = |origin: &TerminalSurfaceOrigin| PanelState {
        panel_name: "bootty.terminal-window".to_owned(),
        children: Vec::new(),
        info: PanelInfo::panel(serde_json::to_value(origin).unwrap()),
    };
    let singleton = terminal_leaf_state(&window_id(), "original owner");
    let mut tabs = PanelState {
        panel_name: "TabPanel".to_owned(),
        children: vec![singleton.clone(), leaf(&origin)],
        info: PanelInfo::tabs(1),
    };
    assert_eq!(
        center_primary_window_key(&tabs).as_deref(),
        Some(window_id().window_id())
    );
    let adopted = take_legacy_surface_tabs(&mut tabs, "own-binding", "own-task");
    assert_eq!(tabs.children.as_slice(), std::slice::from_ref(&singleton));
    assert_eq!(adopted.len(), 1);
    assert_eq!(
        adopted[0].0,
        surface_center_key("own-binding", "own-task", "window", "saved-window-two")
    );
    assert!(center_contains_window(&adopted[0].1, &origin));
    assert!(!center_contains_window(&tabs, &origin));
    let split = PanelState {
        panel_name: "StackPanel".to_owned(),
        children: vec![leaf(&origin), singleton.clone(), leaf(&other)],
        info: PanelInfo::stack(
            vec![px(120.0), px(300.0), px(180.0)],
            gpui_kit::Axis::Horizontal,
        ),
    };
    let surviving =
        remove_terminal_surface(split, &origin).expect("surviving owner and distinct binding");
    assert_eq!(surviving.children, [singleton, leaf(&other)]);
    assert_eq!(
        surviving.info,
        PanelInfo::stack(vec![px(300.0), px(180.0)], gpui_kit::Axis::Horizontal)
    );
    assert!(!center_contains_window(&surviving, &origin));
    assert!(center_contains_window(&surviving, &other));
}

#[rstest]
#[case(false, false)]
#[case(true, false)]
#[case(false, true)]
#[case(true, true)]
fn cold_layout_admission_keeps_saved_window_metadata_and_preserves_nontrivial_destinations(
    #[case] empty_window_source: bool,
    #[case] populated_destination: bool,
) {
    use bootty_ui::workspace_composition::{
        adopt_saved_window_center, center_contains_conversation, center_primary_window_key,
        surface_center_key,
    };
    let task = "exact-task";
    let id = window_id();
    let source = if empty_window_source {
        surface_center_key("binding", task, "window", "")
    } else {
        serde_json::json!(["binding", task]).to_string()
    };
    let owner = surface_center_key("binding", task, "window", id.window_id());
    let agent = PanelState {
        panel_name: "bootty.conversation".to_owned(),
        children: Vec::new(),
        info: PanelInfo::panel(serde_json::json!({
            "target":{"kind":"session","handle":"exact-pi","generation":6}, "task":task
        })),
    };
    let retained = PanelState {
        panel_name: "StackPanel".to_owned(),
        children: vec![
            terminal_panel_state(&id, "owner"),
            PanelState {
                panel_name: "TabPanel".to_owned(),
                children: vec![agent],
                info: PanelInfo::tabs(0),
            },
        ],
        info: PanelInfo::stack(vec![px(420.0), px(180.0)], gpui_kit::Axis::Horizontal),
    };
    let blank_live_window = ScopedWindowId::new(
        SpaceId::from_persistence(1),
        id.session_id().to_owned(),
        String::new(),
    );
    let transient = terminal_panel_state(&blank_live_window, "starting");
    assert_eq!(center_primary_window_key(&transient).as_deref(), Some(""));
    assert_eq!(
        center_primary_window_key(&retained).as_deref(),
        Some(id.window_id())
    );
    let destination = if populated_destination {
        retained.clone()
    } else {
        terminal_panel_state(&id, "owner")
    };
    let mut centers = std::collections::BTreeMap::from([
        (source.clone(), retained.clone()),
        (owner.clone(), destination),
    ]);
    let previous = centers.clone();
    assert_eq!(
        adopt_saved_window_center(&mut centers, &source, &owner),
        !populated_destination
    );
    if populated_destination {
        assert_eq!(
            centers, previous,
            "a meaningful existing layout is retained"
        );
    } else {
        assert!(!centers.contains_key(&source));
        assert_eq!(centers[&owner], retained);
        assert!(center_contains_conversation(&centers[&owner], "exact-pi"));
        assert_eq!(
            center_primary_window_key(&centers[&owner]).as_deref(),
            Some(id.window_id())
        );
        assert!(!adopt_saved_window_center(&mut centers, &source, &owner));
    }
    let wrong_binding = surface_center_key("other-binding", task, "window", id.window_id());
    assert!(!adopt_saved_window_center(
        &mut centers,
        &source,
        &wrong_binding
    ));
}

#[rstest]
#[case("1", "task", "agent", "native:codex:8", true)]
#[case("1", "task", "window", "tab-1", false)]
#[case("1", "task", "chooser", "41", false)]
#[case("", "task", "agent", "native:codex:8", false)]
#[case("1", "", "agent", "native:codex:8", false)]
#[case("1", "task", "agent", "", false)]
fn persisted_active_center_retains_only_exact_agent_identity(
    #[case] binding: &str,
    #[case] task: &str,
    #[case] kind: &str,
    #[case] id: &str,
    #[case] valid: bool,
) {
    use bootty_ui::workspace_composition::{restored_agent_center_destination, surface_center_key};
    let key = surface_center_key(binding, task, kind, id);
    let saved: String = serde_json::from_value(serde_json::to_value(&key).unwrap()).unwrap();
    let destination = restored_agent_center_destination(&saved);
    assert_eq!(
        destination,
        valid.then(|| (binding.to_owned(), task.to_owned(), id.to_owned()))
    );
    if valid {
        assert_eq!(surface_center_key(binding, task, "agent", id), saved);
    }
    assert!(
        restored_agent_center_destination(&serde_json::json!([binding, task]).to_string())
            .is_none()
    );
}

#[rstest::fixture]
fn agent_terminal_split() -> (
    PanelState,
    bootty_ui::workspace_composition::TerminalSurfaceOrigin,
) {
    use bootty_ui::workspace_composition::TerminalSurfaceOrigin;
    let origin = TerminalSurfaceOrigin {
        binding_id: "binding".to_owned(),
        task_identity: "task".to_owned(),
        window_key: "window-four".to_owned(),
    };
    let terminal = PanelState {
        panel_name: "bootty.terminal-window".to_owned(),
        children: Vec::new(),
        info: PanelInfo::panel(serde_json::to_value(&origin).unwrap()),
    };
    let agent = PanelState {
        panel_name: "bootty.conversation".to_owned(),
        children: Vec::new(),
        info: PanelInfo::panel(serde_json::json!({
            "target":{"kind":"session","handle":"native:codex:8","generation":8},
            "task":"task"
        })),
    };
    (
        PanelState {
            panel_name: "StackPanel".to_owned(),
            children: vec![agent, terminal],
            info: PanelInfo::stack(vec![px(420.0), px(180.0)], gpui_kit::Axis::Horizontal),
        },
        origin,
    )
}

#[rstest]
#[case(true, true)]
#[case(false, true)]
#[case(true, false)]
fn closing_saved_agent_preserves_its_terminal_and_persists_exact_closed_presentation(
    agent_terminal_split: (
        PanelState,
        bootty_ui::workspace_composition::TerminalSurfaceOrigin,
    ),
    #[case] agent_owned: bool,
    #[case] terminal_sibling: bool,
) {
    use bootty_ui::workspace_composition::{
        center_contains_conversation, center_contains_window, close_saved_conversation_centers,
        conversation_center_is_closed, surface_center_key,
    };
    let (split, origin) = agent_terminal_split;
    let agent = surface_center_key("binding", "task", "agent", "native:codex:8");
    let terminal = surface_center_key("binding", "task", "window", &origin.window_key);
    let owner = if agent_owned { &agent } else { &terminal };
    let other = surface_center_key("other-binding", "task", "agent", "native:codex:8");
    let owned = if terminal_sibling {
        split.clone()
    } else {
        split.children[0].clone()
    };
    let mut centers =
        std::collections::BTreeMap::from([(owner.clone(), owned), (other.clone(), split.clone())]);
    let transitions =
        close_saved_conversation_centers(&mut centers, "binding", "task", "native:codex:8")
            .unwrap();
    assert_eq!(
        transitions,
        [(owner.clone(), terminal_sibling.then(|| terminal.clone()))]
    );
    let reopened: std::collections::BTreeMap<String, PanelState> =
        serde_json::from_slice(&serde_json::to_vec(&centers).unwrap()).unwrap();
    assert!(conversation_center_is_closed(&reopened[&agent]));
    if terminal_sibling {
        assert!(center_contains_window(&reopened[&terminal], &origin));
        assert!(!center_contains_conversation(
            &reopened[&terminal],
            "native:codex:8"
        ));
        assert_eq!(reopened[&terminal], split.children[1]);
    } else {
        assert!(
            !reopened.contains_key(&terminal),
            "sole close has no surviving owner"
        );
    }
    assert_eq!(reopened[&other], split, "another binding is unchanged");
    assert!(
        close_saved_conversation_centers(&mut centers, "binding", "task", "native:codex:8",)
            .unwrap()
            .is_empty(),
        "a repeated close cannot discard the surviving pane"
    );
    assert_eq!(centers, reopened);
}

#[rstest]
fn closing_agent_refuses_to_overwrite_a_populated_surviving_tab(
    agent_terminal_split: (
        PanelState,
        bootty_ui::workspace_composition::TerminalSurfaceOrigin,
    ),
) {
    use bootty_ui::workspace_composition::{close_saved_conversation_centers, surface_center_key};
    let (split, origin) = agent_terminal_split;
    let agent = surface_center_key("binding", "task", "agent", "native:codex:8");
    let terminal = surface_center_key("binding", "task", "window", &origin.window_key);
    let mut populated = split.clone();
    if let PanelInfo::Panel(info) = &mut populated.children[0].info {
        info["target"]["handle"] = serde_json::json!("another-agent");
    }
    let mut centers = std::collections::BTreeMap::from([(agent, split), (terminal, populated)]);
    let retained = centers.clone();
    assert!(
        close_saved_conversation_centers(&mut centers, "binding", "task", "native:codex:8",)
            .is_err()
    );
    assert_eq!(
        centers, retained,
        "failed close preserves both complete layouts"
    );
}
