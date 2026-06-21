use crate::backend::SiteBackend;
use crate::components::{Detail, Menu, SiteViewState, TabHit, draw_site, page_tab_hit};
use crate::content::{
    getting_started_text, line_text, section_has_alternative_tabs, section_text, sections,
};
use crate::input::Focus;
use crate::web_frame::{WebColor, WebFrameState, new_egui_context, web_cell, web_frame};
use bootty_surface::selection::{SelectionPoint, TerminalSelection};
use tuirealm::ratatui::buffer::{Buffer, Cell};
use tuirealm::ratatui::layout::{Rect, Size};
use tuirealm::ratatui::style::Color;
use tuirealm::terminal::{TerminalAdapter, TestTerminalAdapter};

#[test]
fn page_backend_starts_on_requested_section() {
    let site = SiteBackend::for_page("renderer");
    let renderer = sections()
        .iter()
        .position(|section| section.slug == "renderer")
        .expect("renderer section exists");

    assert_eq!(site.selected, renderer);
    assert_eq!(site.focus, Focus::Detail);
}

#[test]
fn legacy_docs_routes_resolve_to_docs_section() {
    let docs = sections()
        .iter()
        .position(|section| section.slug == "docs")
        .expect("docs section exists");

    assert_eq!(SiteBackend::for_page("javascript").selected, docs);
    assert_eq!(SiteBackend::for_page("rust").selected, docs);
}

#[test]
fn mouse_events_do_not_drive_html_shell_navigation() {
    let mut site = SiteBackend::for_page("quickstart");
    let quickstart = sections()
        .iter()
        .position(|section| section.slug == "quickstart")
        .expect("quickstart section exists");

    site.handle_mouse("move", 2, 10, 0)
        .expect("mouse move handles");
    site.handle_mouse("down", 2, 10, 0)
        .expect("mouse down handles");

    assert_eq!(site.selected, quickstart);
    assert_eq!(site.hovered_menu, None);
    assert_eq!(site.focus, Focus::Detail);
}

#[test]
fn mouse_drag_updates_rust_selection_state() {
    let mut site = SiteBackend::new();

    site.handle_mouse("down", 4, 3, 0)
        .expect("mouse down handles");
    site.handle_mouse("move", 12, 3, 0)
        .expect("mouse move handles");
    site.handle_mouse("up", 12, 3, 0).expect("mouse up handles");

    assert_eq!(
        site.selection.selection(),
        Some(TerminalSelection::new(
            SelectionPoint::new(4, 3),
            SelectionPoint::new(12, 3),
        ))
    );
}

#[test]
fn detail_wheel_scrolls_without_changing_selected_page() {
    let mut site = SiteBackend::new();

    site.handle_mouse("wheel", 40, 8, 3).expect("wheel handles");

    assert_eq!(site.selected, 0);
    assert_eq!(site.focus, Focus::Detail);
    assert_eq!(site.detail_scroll, 3);
}

#[test]
fn page_frame_uses_full_canvas_without_egui_shell() {
    let mut terminal = TestTerminalAdapter::new(Size::new(96, 32)).expect("test terminal starts");
    let completed = terminal
        .draw(|frame| {
            draw_site(
                frame,
                &mut Menu::default(),
                &mut Detail::default(),
                SiteViewState {
                    selected: 0,
                    active_tab: 0,
                    active_subtab: 0,
                    active_leaf_tab: 0,
                    hovered_tab: None,
                    hovered_subtab: None,
                    hovered_leaf_tab: None,
                    focus: Focus::Detail,
                    detail_scroll: 0,
                },
            );
        })
        .expect("site draws");
    let egui = new_egui_context();
    let frame = web_frame(
        &egui,
        completed.buffer,
        WebFrameState {
            selected: 0,
            hovered_menu: None,
            tick: 1,
            focus: Focus::Detail,
            fps: 0.0,
            selection: None,
        },
    );
    let egui_frame = frame.egui.expect("site frame exports egui field");

    assert!(egui_frame.meshes.is_empty());
    assert!(egui_frame.links.is_empty());
    assert!(
        frame.cells.iter().any(|cell| cell.text == "T"),
        "page content should be rendered inside the canvas"
    );
}

#[test]
fn web_frame_exports_rust_selection() {
    let buffer = Buffer::empty(Rect::new(0, 0, 20, 8));
    let frame = web_frame(
        &new_egui_context(),
        &buffer,
        WebFrameState {
            selected: 0,
            hovered_menu: None,
            tick: 0,
            focus: Focus::Detail,
            fps: 0.0,
            selection: Some(TerminalSelection::new(
                SelectionPoint::new(2, 1),
                SelectionPoint::new(8, 1),
            )),
        },
    );
    let selection = frame.selection.expect("selection exported");

    assert_eq!(selection.anchor.x, 2);
    assert_eq!(selection.anchor.y, 1);
    assert_eq!(selection.focus.x, 8);
    assert_eq!(selection.focus.y, 1);
}

#[test]
fn reset_cell_colors_fall_back_to_frame_defaults() {
    let cell = web_cell(0, 0, &Cell::new("A"), None);

    assert_eq!(cell.fg, None);
    assert_eq!(cell.bg, None);
}

#[test]
fn explicit_cell_colors_are_serialized() {
    let mut source = Cell::new("A");
    source.fg = Color::Green;
    source.bg = Color::Black;
    let cell = web_cell(0, 0, &source, None);

    assert_eq!(
        cell.fg,
        Some(WebColor {
            r: 158,
            g: 206,
            b: 106,
        })
    );
    assert_eq!(
        cell.bg,
        Some(WebColor {
            r: 17,
            g: 18,
            b: 26,
        })
    );
}

#[test]
fn getting_started_markdown_contains_both_hosts_without_fences() {
    let text = getting_started_text();
    let content = text
        .lines
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(content.contains("Native app"));
    assert!(content.contains("Glyph probe"));
    assert!(!content.contains("```"));
}

#[test]
fn getting_started_key_sections_are_present() {
    let text = getting_started_text();
    let content = text
        .lines
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(content.contains("Native app"));
    assert!(content.contains("Glyph probe"));
}

#[test]
fn getting_started_code_lines_are_styled_as_code() {
    let text = getting_started_text();
    let line = text
        .lines
        .iter()
        .find(|line| line_text(line).contains("cargo run -p bootty-app --bin bootty"))
        .expect("quickstart text includes native app command");

    assert!(line_text(line).contains("bootty-app"));
}

#[test]
fn non_alternative_pages_render_as_scrolling_content_not_tabs() {
    for (section_index, section) in sections().iter().enumerate() {
        if section_has_alternative_tabs(*section) {
            continue;
        }
        assert_eq!(
            page_tab_hit(section_index, 0, 0, 5, 6, 120, 40),
            None,
            "{} should not expose fake primary tabs",
            section.slug
        );
    }

    let overview = sections()
        .iter()
        .find(|section| section.slug == "overview")
        .expect("overview exists");
    let content = section_text(*overview, 0, 0, 0)
        .lines
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(content.contains("What ships"));
    assert!(content.contains("Architecture map"));
    assert!(content.contains("Documentation map"));
}

#[test]
fn docs_nested_tab_click_targets_match_rendered_rows() {
    let docs = sections()
        .iter()
        .position(|section| section.slug == "docs")
        .expect("docs section exists");
    let mut terminal = TestTerminalAdapter::new(Size::new(120, 40)).expect("test terminal starts");
    let completed = terminal
        .draw(|frame| {
            draw_site(
                frame,
                &mut Menu::default(),
                &mut Detail::default(),
                SiteViewState {
                    selected: docs,
                    active_tab: 0,
                    active_subtab: 0,
                    active_leaf_tab: 0,
                    hovered_tab: None,
                    hovered_subtab: None,
                    hovered_leaf_tab: None,
                    focus: Focus::Detail,
                    detail_scroll: 0,
                },
            );
        })
        .expect("site draws");
    let browser_node_y = (0..completed.buffer.area.height)
        .find(|&y| {
            let line = (0..completed.buffer.area.width)
                .map(|x| completed.buffer[(x, y)].symbol())
                .collect::<String>();
            line.contains("Browser") && line.contains("Node")
        })
        .expect("Browser/Node tabs render");

    assert_eq!(
        page_tab_hit(docs, 0, 0, 5, browser_node_y, 120, 40),
        Some(TabHit::Tertiary(0))
    );
    assert_eq!(
        page_tab_hit(docs, 0, 0, 17, browser_node_y, 120, 40),
        Some(TabHit::Tertiary(1))
    );
}

#[test]
fn package_docs_include_highlighted_javascript_and_rust_examples() {
    let docs = sections()
        .iter()
        .find(|section| section.slug == "docs")
        .expect("docs section exists");
    let js_tab = docs
        .tabs
        .iter()
        .position(|tab| tab.label == "JavaScript")
        .expect("javascript docs tab exists");
    let rust_tab = docs
        .tabs
        .iter()
        .position(|tab| tab.label == "Rust")
        .expect("rust docs tab exists");
    let js_content = docs.tabs[js_tab]
        .subtabs
        .iter()
        .enumerate()
        .flat_map(|(subtab_index, subtab)| {
            if subtab.subtabs.is_empty() {
                section_text(*docs, js_tab, subtab_index, 0).lines
            } else {
                subtab
                    .subtabs
                    .iter()
                    .enumerate()
                    .flat_map(|(leaf_index, _)| {
                        section_text(*docs, js_tab, subtab_index, leaf_index).lines
                    })
                    .collect::<Vec<_>>()
            }
        })
        .map(|line| line_text(&line))
        .collect::<Vec<_>>()
        .join("\n");
    let rust_content = section_text(*docs, rust_tab, 0, 0)
        .lines
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(js_content.contains("npm install bootty.js"));
    assert!(js_content.contains("pnpm add bootty.js"));
    assert!(js_content.contains("yarn add bootty.js"));
    assert!(js_content.contains("bun add bootty.js"));
    assert!(js_content.contains("mountCanvasTerminal"));
    assert!(js_content.contains("createEmptyFrame"));
    assert!(rust_content.contains("TerminalSession::new_with_repaint_wakeup"));
    assert!(rust_content.contains("RendererFrame::from_terminal"));
    assert!(!js_content.contains("```"));
    assert!(!rust_content.contains("```"));
}

#[test]
fn config_toml_examples_highlight_keys_tables_and_values() {
    let config = sections()
        .iter()
        .find(|section| section.slug == "config")
        .expect("config section exists");
    let text = section_text(*config, 0, 0, 0);
    let theme_line = text
        .lines
        .iter()
        .find(|line| line_text(line).contains("theme = \"Catppuccin Mocha\""))
        .expect("theme line renders");
    let table_line = text
        .lines
        .iter()
        .find(|line| line_text(line).contains("[window]"))
        .expect("window table renders");
    let key_color = theme_line
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "theme")
        .and_then(|span| span.style.fg)
        .expect("theme key has color");
    let value_color = theme_line
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "\"Catppuccin Mocha\"")
        .and_then(|span| span.style.fg)
        .expect("theme value has color");
    let table_color = table_line
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "[window]")
        .and_then(|span| span.style.fg)
        .expect("table heading has color");

    assert_ne!(key_color, value_color);
    assert_ne!(table_color, value_color);
}
