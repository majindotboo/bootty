#![cfg(test)]

use bootty_mux::{
    controller::SpaceId,
    snapshot::{MuxPaneAnchor, MuxSession, MuxSessionTag, MuxWindow},
    workspace::ScopedWindowId,
};
use bootty_ui::workspace_composition::{EmptyTerminalState, has_selected_terminal};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case(false, None, EmptyTerminalState::Loading)]
#[case(true, None, EmptyTerminalState::Ready)]
#[case(false, Some("Connecting to host"), EmptyTerminalState::Unavailable("Connecting to host".into()))]
#[case(true, Some("Connection lost"), EmptyTerminalState::Unavailable("Connection lost".into()))]
fn empty_terminal_requires_an_available_snapshot(
    #[case] has_snapshot: bool,
    #[case] unavailable: Option<&str>,
    #[case] expected: EmptyTerminalState,
) {
    assert_eq!(
        EmptyTerminalState::from_snapshot(has_snapshot, unavailable),
        expected,
    );
}

#[rstest]
#[case("empty", "", false)]
#[case("populated", "live", true)]
#[case("populated", "closed", false)]
#[case("missing", "", false)]
fn terminal_presence_follows_selection(
    #[case] session: &str,
    #[case] window: &str,
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
    assert_eq!(has_selected_terminal(&sessions, &selected), expected);
}

use bootty_mux::{
    pane_layout::{PaneLayout, SplitDirection},
    snapshot::{MuxPaneLayout, MuxPaneSplitDirection},
};
use bootty_ui::{
    gpui::{DraggedPane, pane_invocation},
    workspace_composition::{
        fixed_panel_layout, mux_pane_layout, pane_layout_matches, pane_layout_state,
    },
    workspace_composition::{pane_drop_command, pane_resize_command},
};
use gpui_kit::{
    component::{
        Placement,
        dock::{
            DockAreaState, DockPlacement, DockState, PaneTree, PanelBuilder, PanelId, PanelInfo,
            PanelSource, PanelState, RootKind,
        },
    },
    px, size,
};
use proptest::prelude::*;

#[derive(Default)]
struct Panels(Vec<PanelState>);
impl PanelBuilder for Panels {
    fn build(&mut self, state: &PanelState, _: &PanelInfo) -> PanelId {
        self.0.push(state.clone());
        PanelId::from_u64(u64::try_from(self.0.len()).unwrap())
    }
}
impl PanelSource for Panels {
    fn panel_name(&self, _: PanelId) -> &'static str {
        "bootty.terminal-pane"
    }
    fn is_visible(&self, _: PanelId) -> bool {
        true
    }
    fn dump(&self, id: PanelId) -> PanelState {
        self.0[usize::try_from(id.as_u64())
            .unwrap()
            .checked_sub(1)
            .unwrap()]
        .clone()
    }
}
fn target() -> bootty_control::CommandTarget {
    bootty_control::CommandTarget {
        kind: bootty_control::ResourceKind::Session,
        handle: "test-session".into(),
        generation: 1,
    }
}
fn assign_weights(layout: &mut MuxPaneLayout, seed: &mut impl Iterator<Item = u16>) {
    if let MuxPaneLayout::Split {
        weights, children, ..
    } = layout
    {
        for weight in weights {
            *weight = f32::from(seed.next().expect("cycling weight seeds"));
        }
        for child in children {
            assign_weights(child, seed);
        }
    }
}

proptest! {
    #[test]
    fn dock_normalization_preserves_server_structure_and_weights(splits in prop::collection::vec((any::<bool>(),any::<u8>()), 0..24), weight_seed in prop::collection::vec(1u16..u16::MAX, 1..32), width in 400u16..2000, height in 300u16..1600) {
        let mut layout = PaneLayout::single("0".into());
        for (ix,(vertical, focus)) in splits.into_iter().enumerate() {
            let panes=layout.panes(); layout.set_focus(&panes[usize::from(focus).checked_rem(panes.len()).unwrap()]);
            layout.split_focused((ix.saturating_add(1)).to_string(),if vertical {SplitDirection::Down}else{SplitDirection::Right});
        }
        let mut weighted=layout.snapshot();
        assign_weights(&mut weighted, &mut weight_seed.into_iter().cycle());
        let authoritative=PaneLayout::from_mux_layout(&weighted).unwrap().snapshot();
        let state=pane_layout_state(&authoritative,size(px(f32::from(width)),px(f32::from(height))),&str::to_owned);
        let mut panels=Panels::default();
        let tree=PaneTree::from_state(&state,RootKind::Split,&mut panels);
        let roundtrip=mux_pane_layout(&tree.to_state(&panels)).unwrap();
        prop_assert!(pane_layout_matches(&authoritative,&roundtrip), "{authoritative:?} != {roundtrip:?}");
        prop_assert!(pane_resize_command("window", &authoritative,&roundtrip,target()).is_none());
    }
    #[test]
    fn a_finished_resize_is_one_weights_command(first in 0.05f32..0.95) {
        let authoritative=MuxPaneLayout::Split {direction:MuxPaneSplitDirection::Right,weights:vec![0.5,0.5],children:vec![MuxPaneLayout::Pane("a".into()),MuxPaneLayout::Pane("b".into())]};
        let proposed=MuxPaneLayout::Split {direction:MuxPaneSplitDirection::Right,weights:vec![first,1.-first],children:vec![MuxPaneLayout::Pane("a".into()),MuxPaneLayout::Pane("b".into())]};
        let invocation=pane_resize_command("window",&authoritative,&proposed,target());
        if (first-0.5).abs()>0.000_01 {
            let invocation=invocation.unwrap(); prop_assert_eq!(invocation.command,"pane.resize"); prop_assert_eq!(invocation.arguments.len(),2);
            let decoded: MuxPaneLayout=serde_json::from_str(&invocation.arguments[1]).unwrap(); prop_assert!(pane_layout_matches(&decoded,&proposed));
            prop_assert!(pane_resize_command("window",&decoded,&proposed,target()).is_none());
        }
        prop_assert!(pane_resize_command("window",&authoritative,&MuxPaneLayout::Pane("a".into()),target()).is_none());
    }
}
#[rstest]
#[case(Some(Placement::Left), "pane.move", Some("left"))]
#[case(Some(Placement::Right), "pane.move", Some("right"))]
#[case(Some(Placement::Top), "pane.move", Some("up"))]
#[case(Some(Placement::Bottom), "pane.move", Some("down"))]
#[case(None, "pane.swap", None)]
fn dock_drop_uses_mux_commands(
    #[case] placement: Option<Placement>,
    #[case] command: &str,
    #[case] direction: Option<&str>,
) {
    let drag = DraggedPane {
        pane: "source".into(),
        target: target(),
    };
    let invocation = pane_drop_command(&drag, "destination", placement).unwrap();
    assert_eq!(invocation.command, command);
    let mut args = vec!["source".to_owned(), "destination".to_owned()];
    if let Some(direction) = direction {
        args.push(direction.into());
    }
    assert_eq!(invocation.arguments, args);
    assert_eq!(invocation.target, Some(target()));
    assert!(pane_drop_command(&drag, "source", placement).is_none());
    let focus = pane_invocation(
        "pane.focus",
        vec!["window".into(), "destination".into()],
        target(),
    );
    let command = bootty_ui::commands::PaneAction::Focus.descriptor();
    assert_eq!(command.id, focus.command);
    assert_eq!(command.target, Some(bootty_control::ResourceKind::Session));
}
#[rstest]
fn native_panel_saves_contain_only_sidebar_preferences() {
    let layout = MuxPaneLayout::Split {
        direction: MuxPaneSplitDirection::Right,
        weights: vec![0.25, 0.75],
        children: vec![
            MuxPaneLayout::Pane("a".into()),
            MuxPaneLayout::Pane("b".into()),
        ],
    };
    let state = DockAreaState {
        version: Some(8),
        center: pane_layout_state(&layout, size(px(800.), px(600.)), &str::to_owned),
        left_dock: Some(DockState::new(
            PanelState {
                panel_name: "TabPanel".into(),
                children: vec![PanelState::new("bootty.sessions")],
                info: PanelInfo::tabs(0),
            },
            DockPlacement::Left,
            px(317.),
            false,
        )),
        right_dock: None,
        bottom_dock: None,
    };
    let saved = fixed_panel_layout(state);
    assert_eq!(saved.center.children, Vec::<PanelState>::new());
    assert_eq!(saved.left_dock.as_ref().unwrap().size(), px(317.));
    assert!(!saved.left_dock.as_ref().unwrap().open());
    assert_eq!(fixed_panel_layout(saved.clone()), saved);
}
