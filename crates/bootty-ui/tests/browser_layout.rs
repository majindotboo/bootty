use bootty_ui::workspace_composition::fixed_panel_layout;
use gpui_kit::{
    component::dock::{DockAreaState, DockPlacement, DockState, PanelInfo, PanelState},
    px,
};
use proptest::prelude::*;

proptest! {
    #[test]
    fn restored_browser_peers_keep_exact_ids_and_selection(
        ids in prop::collection::btree_set(1u64..u64::MAX, 2..32),
        selected in 0usize..32, open in any::<bool>(),
    ) {
        // Equal titles and addresses must not coalesce distinct browser page identities.
        let pages: Vec<_> = ids.into_iter().map(|page| PanelState {
            panel_name: "bootty.browser.page".into(),
            children: Vec::new(),
            info: PanelInfo::panel(serde_json::json!({
                "page": page, "address": "https://example.com/", "title": "Example",
            })),
        }).collect();
        let mut peers = vec![
            PanelState::new("bootty.files"), PanelState::new("bootty.changes"),
            PanelState::new("bootty.diff"),
        ];
        let selected_ix = selected.checked_rem(pages.len()).ok_or_else(|| TestCaseError::fail("no browser pages"))?;
        let active = peers.len().saturating_add(selected_ix);
        let selected_page = pages.get(selected_ix).cloned();
        peers.extend(pages);
        peers.push(PanelState {
            panel_name: "bootty.document".into(), children: Vec::new(),
            info: PanelInfo::panel(serde_json::json!({"path":"/workspace/main.rs"})),
        });
        let layout = fixed_panel_layout(DockAreaState {
            center: PanelState::new("bootty.terminal"),
            right_dock: Some(DockState::new(PanelState {
                panel_name: "TabPanel".into(), children: peers.clone(), info: PanelInfo::tabs(active),
            }, DockPlacement::Right, px(420.), open)),
            ..DockAreaState::default()
        });
        let Some(right) = layout.right_dock.as_ref() else { return Err(TestCaseError::fail("missing right dock")); };
        prop_assert_eq!(&right.panel().children, &peers);
        let restored = right.panel().info.active_index().and_then(|ix| right.panel().children.get(ix));
        prop_assert!(selected_page.is_some());
        prop_assert_eq!(restored, selected_page.as_ref());
        prop_assert_eq!(right.open(), open);
        prop_assert!(right.panel().children.iter().all(|peer| peer.children.is_empty()));
    }
}
