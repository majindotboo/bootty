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
fn fixed_homes_retain_active_document_and_closed_tool_region(#[case] open: bool) {
    use bootty_ui::workspace_composition::fixed_panel_layout;
    use gpui_kit::component::dock::{DockAreaState, DockState};
    let document = document_panel();
    let state = DockAreaState {
        right_dock: Some(DockState::new(
            PanelState {
                panel_name: "TabPanel".into(),
                children: vec![PanelState::new("bootty.files"), document.clone()],
                info: PanelInfo::tabs(1),
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
