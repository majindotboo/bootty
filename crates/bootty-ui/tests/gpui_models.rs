#![cfg(test)]

use std::sync::Arc;

use bootty_terminal::geometry::{
    CellMetrics, SurfacePoint, TerminalPadding, TerminalSurface, ViewTransform,
};
use bootty_terminal::terminal_frame::{CursorSnapshot, RenderCell, RenderFrame};
use bootty_ui::{
    gpui::{GpuiTerminalAdapter, Rgba, UiPalette, has_icon},
    product_dialogs::{SearchableEntry, SearchableIntent, SearchableList},
    terminal_render::{FillRole, TerminalRenderCommand},
};
use bootty_ui::{
    paint_plan::CursorBlinkPhase,
    terminal_text::{NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract},
};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
fn searchable_list_matches_metadata_and_skips_disabled_rows() {
    let mut disabled = SearchableEntry::new("disabled", "Remote server");
    disabled.secondary = Some("Unavailable".to_owned());
    disabled.enabled = false;
    let mut local = SearchableEntry::new("local", "Local shell");
    local.keywords = vec!["native".to_owned()];
    let mut list = SearchableList::new(vec![disabled, local]);

    assert_eq!(list.selected_value(), Some(&"local"));
    list.apply(SearchableIntent::SetFilter("native".to_owned()));

    let rows = list.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].value, &"local");
    assert!(rows[0].selected);
}

#[rstest]
fn filtering_reselects_the_first_visible_enabled_row() {
    let mut list = SearchableList::new(vec![
        SearchableEntry::new("right", "Split Right"),
        SearchableEntry::new("down", "Split Down"),
    ]);

    list.apply(SearchableIntent::MoveNext);
    assert_eq!(list.selected_value(), Some(&"down"));

    list.apply(SearchableIntent::SetFilter("split".to_owned()));

    assert_eq!(list.selected(), 0);
    assert_eq!(list.selected_value(), Some(&"right"));
    assert!(list.rows()[0].selected);
}

#[rstest]
fn theme_scope_marks_the_current_theme_for_component_selection() {
    use bootty_ui::{gpui::DialogIntent, presentation::dialogs::ThemePickerDialog};
    let mut dialog = ThemePickerDialog::open(
        vec!["Paper Light".to_owned(), "Midnight".to_owned()],
        Some("Midnight".to_owned()),
        "Built in".to_owned(),
    );
    for _ in 0..2 {
        dialog.apply(&DialogIntent::CycleScope {
            dialog: dialog.spec().id,
        });
    }
    let spec = dialog.spec();
    assert!(
        spec.rows
            .iter()
            .any(|row| row.label == "Midnight" && row.current)
    );
    assert!(!spec.rows.iter().any(|row| row.label == "Paper Light"));
}

#[rstest]
fn terminal_colors_drive_the_shared_ui_palette() {
    let background = Rgba::rgb(0x10, 0x20, 0x30);
    let foreground = Rgba::rgb(0xf0, 0xe0, 0xd0);
    let accent = Rgba::rgb(0x11, 0x22, 0xee);
    let mut terminal = [None; 16];
    terminal[4] = Some(accent);

    let palette = UiPalette::from_terminal_colors(Some(background), Some(foreground), terminal);

    assert_eq!(palette.base, background);
    assert_eq!(palette.text, foreground);
    assert_eq!(palette.accent, accent);
    assert_ne!(palette.surface, background);
}

#[rstest]
#[case("settings", true)]
#[case("lucide:terminal", true)]
#[case("lucide:grip-vertical", true)]
#[case("phosphor:acorn", true)]
#[case("phosphor:acorn-duotone", true)]
#[case("phosphor:asclepius-duotone-caduceus-duotone", true)]
#[case("phosphor:address-book", true)]
#[case("not-a-real-bootty-icon", false)]
fn icon_inventory_is_renderer_owned(#[case] slug: &str, #[case] expected: bool) {
    assert_eq!(has_icon(slug), expected);
}

#[rstest]
fn blank_terminal_cells_have_no_plaintext_hyperlink() {
    let frame = Arc::new(RenderFrame {
        cols: 1,
        rows: 1,
        row_dirty: vec![true],
        row_wraps: vec![false],
        cells: vec![RenderCell {
            x: 0,
            y: 0,
            text_start: 0,
            text_len: 0,
            fg: None,
            bg: None,
            style: bootty_terminal::terminal_frame::CellStyle::default(),
            hyperlink: None,
        }],
        ..Default::default()
    });
    let surface = TerminalSurface::for_logical_size(
        10.0,
        20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());
    let mut adapter = GpuiTerminalAdapter::default();

    let element = adapter.element(
        surface,
        &frame,
        14.0,
        20.0,
        1.0,
        &contract,
        CursorBlinkPhase::visible(),
        true,
        "",
    );

    assert_eq!(
        element
            .interaction()
            .expect("terminal interaction")
            .hyperlink_at(SurfacePoint { x: 1.0, y: 1.0 }),
        None
    );
}

#[rstest]
fn terminal_adapter_reuses_the_prepared_scene_for_the_same_published_frame() {
    let frame = Arc::new(RenderFrame::default());
    let surface = TerminalSurface::for_logical_size(
        800.0,
        600.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());
    let mut adapter = GpuiTerminalAdapter::default();

    let first = adapter.element(
        surface,
        &frame,
        14.0,
        20.0,
        1.0,
        &contract,
        CursorBlinkPhase::visible(),
        true,
        "",
    );
    let second = adapter.element(
        surface,
        &frame,
        14.0,
        20.0,
        1.0,
        &contract,
        CursorBlinkPhase::visible(),
        true,
        "",
    );

    assert!(std::ptr::eq(first.frame(), second.frame()));
}

fn text_frame(text: &str) -> Arc<RenderFrame> {
    Arc::new(RenderFrame {
        cols: u16::try_from(text.chars().count()).expect("test text width"),
        rows: 1,
        row_dirty: vec![true],
        row_wraps: vec![false],
        cells: text
            .chars()
            .enumerate()
            .map(|(x, _)| RenderCell {
                x: u16::try_from(x).expect("test column"),
                y: 0,
                text_start: x,
                text_len: 1,
                fg: None,
                bg: None,
                style: bootty_terminal::terminal_frame::CellStyle::default(),
                hyperlink: None,
            })
            .collect(),
        text: text.chars().collect(),
        ..Default::default()
    })
}

#[rstest]
fn terminal_glyph_cache_reuses_images_across_new_published_frames() {
    let surface = TerminalSurface::for_logical_size(
        40.0,
        20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());
    let mut adapter = GpuiTerminalAdapter::default();

    let _ = adapter.element(
        surface,
        &text_frame("ABCD"),
        14.0,
        20.0,
        2.0,
        &contract,
        CursorBlinkPhase::visible(),
        true,
        "",
    );
    let cold = adapter.glyph_cache_metrics();
    let _ = adapter.element(
        surface,
        &text_frame("ABCD"),
        14.0,
        20.0,
        2.0,
        &contract,
        CursorBlinkPhase::visible(),
        true,
        "",
    );
    let warm = adapter.glyph_cache_metrics();

    assert!(cold.image_creations > 0);
    assert_eq!(warm.image_creations, cold.image_creations);
    assert!(warm.hits > cold.hits);
}

#[rstest]
fn terminal_glyph_cache_is_byte_bounded_and_device_aligned() {
    let surface = TerminalSurface::for_logical_size(
        80.0,
        20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());
    let mut probe = GpuiTerminalAdapter::with_glyph_cache_byte_budget(usize::MAX);
    let _ = probe.element(
        surface,
        &text_frame("A"),
        14.0,
        20.0,
        2.0,
        &contract,
        CursorBlinkPhase::visible(),
        true,
        "",
    );
    let byte_budget = probe
        .glyph_cache_metrics()
        .bytes
        .checked_mul(2)
        .expect("test glyph cache budget fits");
    assert!(byte_budget > 0);
    let mut adapter = GpuiTerminalAdapter::with_glyph_cache_byte_budget(byte_budget);
    let element = adapter.element(
        surface,
        &text_frame("ABCDEFGH"),
        14.0,
        20.0,
        2.0,
        &contract,
        CursorBlinkPhase::visible(),
        true,
        "",
    );

    let metrics = adapter.glyph_cache_metrics();
    assert!(metrics.bytes <= byte_budget);
    assert!(metrics.evictions > 0);
    for rect in element.glyph_sprite_rects() {
        for logical in [rect.min_x, rect.min_y, rect.width(), rect.height()] {
            let physical = logical * 2.0;
            assert_eq!(gpui_kit::px(physical), gpui_kit::px(physical.round()));
        }
        assert!(rect.min_x >= 0.0 && rect.max_x <= 80.0);
        assert!(rect.min_y >= 0.0 && rect.max_y <= 20.0);
    }
    // Magnified glyph edges must interpolate against transparent pixels, not the neighboring
    // allocation in GPUI's texture atlas. The guard must not enlarge the logical glyph bounds.
    let images = adapter.take_render_images();
    assert_ne!(images, Vec::<std::sync::Arc<gpui_kit::RenderImage>>::new());
    for image in images {
        let size = image.size(0);
        let width = usize::try_from(size.width.0).expect("image width fits usize");
        let height = usize::try_from(size.height.0).expect("image height fits usize");
        assert!(width > 2 && height > 2);
        let last_x = width.checked_sub(1).expect("image width is non-zero");
        let last_y = height.checked_sub(1).expect("image height is non-zero");
        let pixels = image.as_bytes(0).expect("glyph pixels");
        for y in 0..height {
            for x in 0..width {
                if x == 0 || y == 0 || x == last_x || y == last_y {
                    let pixel_index = y
                        .checked_mul(width)
                        .and_then(|row| row.checked_add(x))
                        .and_then(|pixel| pixel.checked_mul(4))
                        .and_then(|pixel| pixel.checked_add(3))
                        .expect("pixel index fits the image buffer");
                    assert_eq!(pixels[pixel_index], 0);
                }
            }
        }
    }
}

#[rstest]
fn terminal_adapter_reuses_the_stable_scene_when_cursor_blink_changes() {
    let frame = Arc::new(RenderFrame {
        cols: 1,
        rows: 1,
        cursor: Some(CursorSnapshot {
            x: 0,
            y: 0,
            at_wide_tail: false,
            style: libghostty_vt::render::CursorVisualStyle::Block,
            blinking: true,
            color: None,
        }),
        ..Default::default()
    });
    let surface = TerminalSurface::for_logical_size(
        800.0,
        600.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());
    let mut adapter = GpuiTerminalAdapter::default();

    let visible = adapter.element(
        surface,
        &frame,
        14.0,
        20.0,
        1.0,
        &contract,
        CursorBlinkPhase::visible(),
        true,
        "",
    );
    let hidden = adapter.element(
        surface,
        &frame,
        14.0,
        20.0,
        1.0,
        &contract,
        CursorBlinkPhase::hidden(),
        true,
        "",
    );

    assert!(std::ptr::eq(visible.frame(), hidden.frame()));
}

#[rstest]
fn terminal_interaction_retains_wide_cursor_bounds_for_ime_placement() {
    let frame = Arc::new(RenderFrame {
        cols: 10,
        rows: 4,
        cursor: Some(CursorSnapshot {
            x: 4,
            y: 2,
            at_wide_tail: true,
            style: libghostty_vt::render::CursorVisualStyle::Block,
            blinking: false,
            color: None,
        }),
        ..Default::default()
    });
    let surface = TerminalSurface::for_logical_size(
        100.0,
        80.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());
    let mut adapter = GpuiTerminalAdapter::default();

    let element = adapter.element(
        surface,
        &frame,
        14.0,
        20.0,
        1.0,
        &contract,
        CursorBlinkPhase::visible(),
        true,
        "",
    );

    assert_eq!(
        element
            .interaction()
            .expect("terminal interaction")
            .cursor_bounds(),
        Some(bootty_terminal::geometry::SurfaceRect::from_min_size(
            30.0, 40.0, 20.0, 20.0,
        ))
    );
}

#[rstest]
fn terminal_view_transform_projects_interaction_geometry_without_rebuilding_scene() {
    let mut frame = (*text_frame("http://x")).clone();
    frame.cursor = Some(CursorSnapshot {
        x: 4,
        y: 0,
        at_wide_tail: true,
        style: libghostty_vt::render::CursorVisualStyle::Block,
        blinking: false,
        color: None,
    });
    for cell in &mut frame.cells {
        cell.hyperlink = Some("https://example.com".to_owned());
    }
    let frame = Arc::new(frame);
    let surface = TerminalSurface::for_logical_size(
        100.0,
        80.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());
    let mut adapter = GpuiTerminalAdapter::default();
    let first = adapter
        .element(
            surface,
            &frame,
            14.0,
            20.0,
            1.0,
            &contract,
            CursorBlinkPhase::visible(),
            true,
            "",
        )
        .with_view_transform(ViewTransform::IDENTITY);
    let initial_cache = adapter.glyph_cache_metrics();
    let view = ViewTransform {
        zoom: 2.0,
        pan_x: 5.0,
        pan_y: -3.0,
    };
    let second = adapter
        .element(
            surface,
            &frame,
            14.0,
            20.0,
            1.0,
            &contract,
            CursorBlinkPhase::visible(),
            true,
            "",
        )
        .with_view_transform(view);
    let second_cache = adapter.glyph_cache_metrics();
    let other_view = ViewTransform {
        zoom: 3.0,
        pan_x: -20.0,
        pan_y: 7.0,
    };
    let third = adapter
        .element(
            surface,
            &frame,
            14.0,
            20.0,
            1.0,
            &contract,
            CursorBlinkPhase::visible(),
            true,
            "",
        )
        .with_view_transform(other_view);
    let final_cache = adapter.glyph_cache_metrics();

    assert!(std::ptr::eq(first.frame(), second.frame()));
    assert!(std::ptr::eq(second.frame(), third.frame()));
    assert_eq!(second_cache, initial_cache);
    assert_eq!(final_cache, initial_cache);

    let identity = first.interaction().expect("identity interaction");
    assert_eq!(identity.view_transform(), ViewTransform::IDENTITY);
    assert_eq!(
        identity.cursor_bounds(),
        Some(bootty_terminal::geometry::SurfaceRect::from_min_size(
            30.0, 0.0, 20.0, 20.0,
        ))
    );
    let transformed = second.interaction().expect("transformed interaction");
    assert_eq!(transformed.view_transform(), view);
    assert_eq!(
        transformed.cursor_bounds(),
        Some(bootty_terminal::geometry::SurfaceRect::from_min_size(
            65.0, -3.0, 40.0, 40.0,
        ))
    );
    let hyperlink = transformed
        .hyperlink_at(SurfacePoint { x: 15.0, y: 17.0 })
        .expect("hyperlink under transformed point");
    assert_eq!(hyperlink.url, "https://example.com");
    assert_eq!(
        hyperlink.rect,
        bootty_terminal::geometry::SurfaceRect::from_min_size(5.0, -3.0, 160.0, 40.0)
    );
    assert!(
        transformed
            .hyperlink_at(SurfacePoint { x: -1.0, y: 17.0 })
            .is_none()
    );

    let other = third.interaction().expect("other transformed interaction");
    assert_eq!(other.view_transform(), other_view);
    assert_eq!(
        other.cursor_bounds(),
        Some(bootty_terminal::geometry::SurfaceRect::from_min_size(
            70.0, 7.0, 60.0, 60.0,
        ))
    );
    assert!(
        other
            .hyperlink_at(SurfacePoint { x: 101.0, y: 20.0 })
            .is_none()
    );
}

#[rstest]
fn terminal_preedit_overlays_the_cursor_cell_and_hides_the_terminal_cursor() {
    let frame = Arc::new(RenderFrame {
        cols: 10,
        rows: 4,
        cursor: Some(CursorSnapshot {
            x: 4,
            y: 2,
            at_wide_tail: false,
            style: libghostty_vt::render::CursorVisualStyle::Bar,
            blinking: true,
            color: None,
        }),
        ..Default::default()
    });
    let surface = TerminalSurface::for_logical_size(
        100.0,
        80.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());
    let mut adapter = GpuiTerminalAdapter::default();

    let element = adapter.element(
        surface,
        &frame,
        14.0,
        20.0,
        1.0,
        &contract,
        CursorBlinkPhase::hidden(),
        true,
        "文",
    );
    let commands = &element.frame().commands;
    let preedit_rect =
        bootty_terminal::geometry::SurfaceRect::from_min_size(40.0, 40.0, 20.0, 20.0);
    let background = commands
        .iter()
        .rposition(|command| {
            matches!(
                command,
                TerminalRenderCommand::FillRect(fill)
                    if fill.role == FillRole::SurfaceBackground && fill.rect == preedit_rect
            )
        })
        .expect("preedit background");
    let underline = commands
        .iter()
        .rposition(|command| {
            matches!(
                command,
                TerminalRenderCommand::Decoration(line)
                    if line.style == bootty_ui::paint_plan::DecorationStyle::Single
            )
        })
        .expect("preedit underline");
    let text = commands
        .iter()
        .rposition(
            |command| matches!(command, TerminalRenderCommand::Text(text) if text.text == "文"),
        )
        .expect("preedit text");

    assert!(background < underline && underline < text);
    assert!(
        commands
            .iter()
            .all(|command| !matches!(command, TerminalRenderCommand::Cursor(_)))
    );
}
