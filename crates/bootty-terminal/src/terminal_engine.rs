use crate::terminal_search::{SearchPattern, TerminalSearchOptions};
use num_traits::ToPrimitive as _;
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Once,
    time::{Duration, Instant},
};

use memchr::{memchr, memmem::find};

use crate::{
    geometry::{
        CellMetrics, GridPoint, SurfacePoint, TerminalGeometry, TerminalPadding, TerminalSurface,
    },
    terminal_image::{
        KittyImageDataCache, KittyImageFrame, KittyImagePlacement, KittyVirtualCell,
        append_virtual_image_placements, collect_kitty_image_frame,
    },
    terminal_png_decoder::BoottyPngDecoder,
};

mod capture;
mod copy_mode;
mod logical_search;
pub(crate) mod write_ingress;

pub use copy_mode::{
    TerminalCopyModeAction, TerminalCopyModeMotion, TerminalCopyModeOutcome,
    TerminalCopyModeSearchOutcome, TerminalSearchDirection,
};
use logical_search::frame_search_matches;
use write_ingress::{
    CURSOR_HOME, SanitizedKittyGraphics, complete_streaming_control_prefix_len,
    contains_tracked_streaming_control, find_osc_terminator, repeated_cursor_home_prefix_len,
    sanitize_kitty_graphics_commands, split_osc_payload, terminal_write_features,
    unwrap_tmux_passthrough_commands,
};

use crate::terminal_frame::{
    CellStyle, CursorSnapshot, FrameColors, FrameScrollbar, FrameSelection, FrameStats, RenderCell,
    RenderFrame,
};
use crate::terminal_input_model::{
    KeyInput, MacosOptionAsAlt, MouseAction, MouseEncoderSize, MouseInput,
};
use crate::terminal_palette::generate_256_palette;
use crate::terminal_side_effect::{TerminalHostAction, TerminalSideEffectCollector};
pub use crate::terminal_side_effect::{TerminalSideEffect, TerminalSideEffectEvent};
use anyhow::{Context as _, Result};
use base64::{Engine as _, engine::general_purpose};
use libghostty_vt::{
    Terminal,
    fmt::Format,
    focus, key,
    kitty::graphics::set_png_decoder,
    mouse, paste,
    render::{CellIterator, CursorVisualStyle, Dirty, RenderState, RowIteration, RowIterator},
    selection::{FormatOptions as SelectionFormatOptions, gesture},
    style::{Palette as GhosttyPalette, RgbColor, StyleColor},
    terminal::{
        ColorScheme, ConformanceLevel, CursorStyle, DeviceAttributeFeature, DeviceAttributes,
        DeviceType, Mode, Point, PointCoordinate, PrimaryDeviceAttributes, ScrollViewport,
        SecondaryDeviceAttributes, SizeReportSize, TertiaryDeviceAttributes,
    },
};

pub const DEFAULT_MAX_SCROLLBACK: usize = 0;
const SELECTION_REPEAT_INTERVAL: Duration = Duration::from_millis(500);
/// Ceiling on button reports emitted for one wheel event. Well above a full-screen page scroll.
const MAX_WHEEL_REPORTS: usize = 1024;
const NATIVE_SCROLLBACK_TARGET_ROWS: usize = 1_000_000;
pub const NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE: usize = 320;
pub const NATIVE_MAX_SCROLLBACK: usize =
    NATIVE_SCROLLBACK_TARGET_ROWS * NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE;
pub const TERMINAL_TERM: &str = "xterm-bootty";
pub const TERMINAL_PROGRAM: &str = "ghostty";
pub const TERMINAL_PROGRAM_VERSION: &str = concat!("Bootty ", env!("CARGO_PKG_VERSION"));
const TERMINAL_XTVERSION: &str = concat!("ghostty (Bootty ", env!("CARGO_PKG_VERSION"), ")");
pub const TERMINAL_BACKGROUND: (u8, u8, u8) = (0x1a, 0x1b, 0x25);
pub const TERMINAL_FOREGROUND: (u8, u8, u8) = (0xc0, 0xca, 0xf5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalColorConfig {
    pub background: RgbColor,
    pub foreground: RgbColor,
    pub cursor: Option<RgbColor>,
    pub cursor_text: Option<RgbColor>,
    pub pointer_foreground: Option<RgbColor>,
    pub pointer_background: Option<RgbColor>,
    pub tektronix_foreground: Option<RgbColor>,
    pub tektronix_background: Option<RgbColor>,
    pub highlight_background: Option<RgbColor>,
    pub tektronix_cursor: Option<RgbColor>,
    pub highlight_foreground: Option<RgbColor>,
    pub selection_background: Option<RgbColor>,
    pub selection_foreground: Option<RgbColor>,
    pub palette: Vec<RgbColor>,
    pub palette_generate: bool,
    pub palette_harmonious: bool,
}

impl Default for TerminalColorConfig {
    fn default() -> Self {
        Self {
            background: rgb(TERMINAL_BACKGROUND),
            foreground: rgb(TERMINAL_FOREGROUND),
            cursor: None,
            cursor_text: None,
            pointer_foreground: None,
            pointer_background: None,
            tektronix_foreground: None,
            tektronix_background: None,
            highlight_background: None,
            tektronix_cursor: None,
            highlight_foreground: None,
            selection_background: None,
            selection_foreground: None,
            palette: default_palette16().into(),
            palette_generate: false,
            palette_harmonious: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalCursorConfig {
    pub style: Option<TerminalCursorStyle>,
    pub blink: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalCursorStyle {
    Bar,
    Block,
    Underline,
    HollowBlock,
}

impl TerminalCursorStyle {
    const fn into_ghostty(self) -> CursorStyle {
        match self {
            Self::Bar => CursorStyle::Bar,
            Self::Block => CursorStyle::Block,
            Self::Underline => CursorStyle::Underline,
            Self::HollowBlock => CursorStyle::BlockHollow,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalFeatureConfig {
    pub glyph_protocol: bool,
}

impl Default for TerminalFeatureConfig {
    fn default() -> Self {
        Self {
            glyph_protocol: true,
        }
    }
}

/// Live terminal settings that can change without restarting the terminal session.
///
/// Callers apply this value as one unit. The terminal engine applies colors first, then the
/// cursor, then terminal features.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalLiveConfig {
    pub colors: TerminalColorConfig,
    pub cursor: TerminalCursorConfig,
    pub features: TerminalFeatureConfig,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalSelectionEvent {
    pub surface: TerminalSurface,
    pub position: SurfacePoint,
    pub rectangle: bool,
}

impl TerminalSelectionEvent {
    fn grid_point(self) -> GridPoint {
        let x = ((self.position.x - self.surface.padding.left).max(0.0) / self.surface.cell.width)
            .floor();
        let y = ((self.position.y - self.surface.padding.top).max(0.0) / self.surface.cell.height)
            .floor();
        GridPoint {
            x: x.max(0.0).to_u16().unwrap_or(u16::MAX),
            y: y.max(0.0).to_u16().unwrap_or(u16::MAX),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalSelectionFormat {
    PlainText,
    Vt,
    Html,
}

impl TerminalSelectionFormat {
    const fn emit_format(self) -> Format {
        match self {
            Self::PlainText => Format::Plain,
            Self::Vt => Format::Vt,
            Self::Html => Format::Html,
        }
    }
}

fn selection_geometry(surface: TerminalSurface) -> gesture::Geometry {
    let metrics = surface.mouse_metrics();
    let grid = surface.raw_grid_size();
    gesture::Geometry {
        columns: u32::from(grid.cols),
        cell_width: metrics.cell_width,
        padding_left: metrics.padding.left,
        screen_height: metrics.screen_height,
    }
}

fn selection_point(
    event: TerminalSelectionEvent,
    geometry: TerminalGeometry,
) -> Option<PointCoordinate> {
    let grid = event.grid_point();
    (grid.x < geometry.cols && grid.y < geometry.rows).then_some(PointCoordinate {
        x: grid.x,
        y: u32::from(grid.y),
    })
}

#[derive(Default)]
struct CachedRenderRow {
    cells: Vec<RenderCell>,
    text: Vec<char>,
    virtual_cells: Vec<KittyVirtualCell>,
    selection: Option<FrameSelection>,
    wrapped: bool,
    wrap_continuation: bool,
}

impl CachedRenderRow {
    fn clear(&mut self) {
        self.cells.clear();
        self.text.clear();
        self.virtual_cells.clear();
        self.selection = None;
        self.wrapped = false;
        self.wrap_continuation = false;
    }
}

#[derive(Clone, Copy)]
struct SizeReportState {
    geometry: TerminalGeometry,
    display_scale: f32,
    render_cell: CellMetrics,
}

impl SizeReportState {
    fn size(self) -> SizeReportSize {
        let (cell_width, cell_height) = self.render_cell.physical_size(self.display_scale);
        SizeReportSize {
            rows: self.geometry.rows,
            columns: self.geometry.cols,
            cell_width,
            cell_height,
        }
    }
}

fn positive_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

fn scaled_mouse_encoder_size(size: MouseEncoderSize, display_scale: f32) -> MouseEncoderSize {
    MouseEncoderSize {
        screen_width: scaled_mouse_extent(size.screen_width, display_scale).max(1),
        screen_height: scaled_mouse_extent(size.screen_height, display_scale).max(1),
        cell_width: scaled_mouse_extent(size.cell_width, display_scale).max(1),
        cell_height: scaled_mouse_extent(size.cell_height, display_scale).max(1),
        padding_top: scaled_mouse_extent(size.padding_top, display_scale),
        padding_bottom: scaled_mouse_extent(size.padding_bottom, display_scale),
        padding_right: scaled_mouse_extent(size.padding_right, display_scale),
        padding_left: scaled_mouse_extent(size.padding_left, display_scale),
    }
}

fn scaled_mouse_extent(value: u32, display_scale: f32) -> u32 {
    (value.to_f32().unwrap_or(f32::MAX) * display_scale)
        .round()
        .max(0.0)
        .to_u32()
        .unwrap_or(u32::MAX)
}

fn extract_render_row(
    terminal: &Terminal<'static, 'static>,
    cell_iterator: &mut CellIterator<'static>,
    grapheme_scratch: &mut Vec<char>,
    hyperlink_scratch: &mut Vec<u8>,
    row: &RowIteration<'static, '_>,
    row_index: u16,
    out: &mut CachedRenderRow,
) -> Result<()> {
    out.clear();
    let raw_row = row.raw_row()?;
    out.wrapped = raw_row.is_wrapped().unwrap_or(false);
    out.wrap_continuation = raw_row.is_wrap_continuation().unwrap_or(false);
    let row_has_hyperlink = raw_row.has_hyperlink().unwrap_or(false);
    out.selection = row.selection()?.map(|selection| FrameSelection {
        row: row_index,
        start_col: selection.start_x,
        end_col: selection.end_x,
    });

    let mut cell_iter = cell_iterator.update(row)?;
    let mut col_index = 0_u16;
    while let Some(cell) = cell_iter.next() {
        // Every field read here is its own call across the FFI boundary, and a terminal is mostly
        // unstyled text. A cell that reports no styling has a default style and no foreground of
        // its own, so asking for either only repeats what this one answer already said. Its
        // background still has to be read: a blank painted with a background colour carries that
        // colour and nothing else.
        let style = cell.has_styling()?.then(|| cell.style()).transpose()?;
        let grapheme_len = cell.graphemes_len()?;
        grapheme_scratch.resize(grapheme_len, '\0');
        if grapheme_len > 0 {
            cell.graphemes_buf(grapheme_scratch)?;
        }

        let is_virtual_placeholder = grapheme_scratch.first() == Some(&'\u{10EEEE}');
        if is_virtual_placeholder {
            out.virtual_cells.push(KittyVirtualCell {
                x: col_index,
                y: row_index,
                grapheme: grapheme_scratch.clone(),
                foreground: style.map_or(StyleColor::None, |style| style.fg_color),
                underline_color: style.map_or(StyleColor::None, |style| style.underline_color),
            });
        }

        let text_start = out.text.len();
        let text_len = if is_virtual_placeholder {
            0
        } else {
            out.text.extend_from_slice(grapheme_scratch);
            grapheme_len
        };

        let hyperlink = if row_has_hyperlink {
            hyperlink_uri_at(terminal, col_index, row_index, hyperlink_scratch)
        } else {
            None
        };

        out.cells.push(RenderCell {
            x: col_index,
            y: row_index,
            text_start,
            text_len,
            fg: match style {
                Some(_) => cell.fg_color()?,
                None => None,
            },
            bg: cell.bg_color()?,
            style: style.map_or_else(CellStyle::default, |style| CellStyle {
                bold: style.bold,
                italic: style.italic,
                faint: style.faint,
                blink: style.blink,
                inverse: style.inverse,
                invisible: style.invisible,
                strikethrough: style.strikethrough,
                overline: style.overline,
                underline: style.underline,
            }),
            hyperlink,
        });

        col_index = col_index
            .checked_add(1)
            .context("terminal row exceeds column limit")?;
    }

    Ok(())
}

type PtyWriteCallback =
    Rc<RefCell<Option<Box<dyn libghostty_vt::terminal::PtyWriteFn<'static, 'static>>>>>;

#[derive(Clone, Debug, Default)]
struct XtermColorOverrides([Option<RgbColor>; 7]);

impl XtermColorOverrides {
    fn slot(&mut self, code: u8) -> Option<&mut Option<RgbColor>> {
        self.0.get_mut(usize::from(code.checked_sub(13)?))
    }

    fn get(&self, code: u8) -> Option<RgbColor> {
        self.0
            .get(usize::from(code.checked_sub(13)?))
            .copied()
            .flatten()
    }

    fn set(&mut self, code: u8, color: RgbColor) -> bool {
        let Some(slot) = self.slot(code) else {
            return false;
        };
        if *slot == Some(color) {
            false
        } else {
            *slot = Some(color);
            true
        }
    }

    fn reset(&mut self, code: u8) -> bool {
        self.slot(code).and_then(Option::take).is_some()
    }
}
#[allow(
    clippy::struct_excessive_bools,
    reason = "VT protocol modes and cache invalidation flags vary independently."
)]
pub struct TerminalEngine {
    terminal: Terminal<'static, 'static>,
    base_color_palette: crate::terminal_palette::Palette,
    render_state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    image_placements: libghostty_vt::kitty::graphics::PlacementIterator<'static>,
    image_data_cache: KittyImageDataCache,
    frame: RenderFrame,
    row_cache: Vec<CachedRenderRow>,
    grapheme_scratch: Vec<char>,
    key_encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    macos_option_as_alt: MacosOptionAsAlt,
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    selection_gesture: gesture::Gesture<'static>,
    selection_press_event: gesture::PressEvent<'static>,
    selection_drag_event: gesture::DragEvent<'static>,
    selection_release_event: gesture::ReleaseEvent<'static>,
    selection_clock_started: Instant,
    mouse_any_button_pressed: bool,
    mouse_encoder_options_dirty: bool,
    mouse_encoder_size: Option<MouseEncoderSize>,
    geometry: TerminalGeometry,
    size_report_state: Rc<Cell<SizeReportState>>,
    current_working_directory_state: Rc<RefCell<String>>,
    side_effects: TerminalSideEffectCollector,
    terminal_write_pending: Vec<u8>,
    synchronized_output_prefix_len: usize,
    synchronized_output_observed: bool,
    cursor_home_pending_len: usize,
    pty_write_callback: PtyWriteCallback,
    pty_write_suppressed: Rc<Cell<bool>>,
    current_working_directory: String,
    colors: TerminalColorConfig,
    xterm_color_overrides: XtermColorOverrides,
    color_scheme: Rc<Cell<ColorScheme>>,
    search_query: String,
    search_options: TerminalSearchOptions,
    search_pattern: Option<SearchPattern>,
    search_groups: Vec<Vec<FrameSelection>>,
    search_active_index: usize,
    search_pulse: u64,
    copy_mode: Option<copy_mode::CopyModeState>,
    content_epoch: u64,
    extracted_content_epoch: u64,
    kitty_graphics_touched: bool,
    display_scale: f32,
    render_cell: CellMetrics,
    render_cell_explicit: bool,
}

fn configure_default_colors(
    terminal: &mut Terminal<'static, 'static>,
    base_color_palette: &crate::terminal_palette::Palette,
    config: &TerminalColorConfig,
) -> Result<()> {
    let mut palette = *base_color_palette;
    let mut explicit = [false; 256];
    for ((entry, explicit), color) in palette.iter_mut().zip(&mut explicit).zip(&config.palette) {
        *entry = *color;
        *explicit = true;
    }
    if config.palette_generate {
        palette = generate_256_palette(
            &palette,
            &explicit,
            config.background,
            config.foreground,
            config.palette_harmonious,
        );
    }
    terminal.set_default_color_palette(Some(GhosttyPalette(palette)))?;
    terminal
        .set_default_bg_color(Some(config.background))?
        .set_default_fg_color(Some(config.foreground))?
        .set_default_cursor_color(config.cursor)?;
    Ok(())
}

fn configure_default_cursor(
    terminal: &mut Terminal<'static, 'static>,
    config: TerminalCursorConfig,
) -> Result<()> {
    terminal
        .set_default_cursor_style(config.style.map(TerminalCursorStyle::into_ghostty))?
        .set_default_cursor_blink(config.blink)?;
    Ok(())
}

fn configure_terminal_features(
    terminal: &mut Terminal<'static, 'static>,
    config: TerminalFeatureConfig,
) -> Result<()> {
    terminal.set_glyph_protocol_enabled(config.glyph_protocol)?;
    Ok(())
}

struct BoottyGhosttyLogger;

impl libghostty_vt::log::Logger for BoottyGhosttyLogger {
    fn log(&self, level: libghostty_vt::log::Level, scope: &str, message: &str) {
        if !libghostty_log_enabled(level) {
            return;
        }
        let scope = if scope.is_empty() {
            "libghostty-vt"
        } else {
            scope
        };
        eprintln!("[libghostty-vt {level:?}] {scope}: {message}");
    }
}

fn install_libghostty_logger() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = libghostty_vt::set_logger(Some(Box::new(BoottyGhosttyLogger)));
    });
}

fn libghostty_log_enabled(level: libghostty_vt::log::Level) -> bool {
    let minimum = std::env::var("BOOTTY_LIBGHOSTTY_LOG")
        .ok()
        .map_or(Some(libghostty_vt::log::Level::Warning), |value| {
            parse_libghostty_log_level(&value)
        });
    minimum.is_some_and(|minimum| log_level_rank(level) <= log_level_rank(minimum))
}

fn parse_libghostty_log_level(value: &str) -> Option<libghostty_vt::log::Level> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" | "false" | "0" => None,
        "error" => Some(libghostty_vt::log::Level::Error),
        "info" => Some(libghostty_vt::log::Level::Info),
        "debug" | "trace" | "all" => Some(libghostty_vt::log::Level::Debug),
        _ => Some(libghostty_vt::log::Level::Warning),
    }
}

const fn log_level_rank(level: libghostty_vt::log::Level) -> u8 {
    match level {
        libghostty_vt::log::Level::Error => 0,
        libghostty_vt::log::Level::Warning => 1,
        libghostty_vt::log::Level::Info => 2,
        _ => 3,
    }
}

const fn rgb((r, g, b): (u8, u8, u8)) -> RgbColor {
    RgbColor { r, g, b }
}
fn color_scheme_for_background(color: RgbColor) -> ColorScheme {
    let luma = u32::from(color.r)
        .saturating_mul(299)
        .saturating_add(u32::from(color.g).saturating_mul(587))
        .saturating_add(u32::from(color.b).saturating_mul(114));
    if luma < 128_000 {
        ColorScheme::Dark
    } else {
        ColorScheme::Light
    }
}

fn default_palette16() -> [RgbColor; 16] {
    crate::terminal_palette::default_base16()
}

const fn default_device_attributes() -> DeviceAttributes {
    DeviceAttributes {
        primary: PrimaryDeviceAttributes::new(
            ConformanceLevel::VT220,
            &[
                DeviceAttributeFeature::ANSI_COLOR,
                DeviceAttributeFeature::CLIPBOARD,
            ],
        ),
        secondary: SecondaryDeviceAttributes {
            device_type: DeviceType::VT220,
            firmware_version: 0,
            rom_cartridge: 0,
        },
        tertiary: TertiaryDeviceAttributes { unit_id: 0 },
    }
}

fn parse_osc_number(bytes: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(bytes).ok()?;
    text.parse().ok()
}

fn parse_color_channel(s: &str) -> Option<u8> {
    let value = u16::from_str_radix(s, 16).ok()?;
    Some(match s.len() {
        1 => u8::try_from(value).ok()?.checked_mul(0x11)?,
        2 => u8::try_from(value).ok()?,
        _ => u8::try_from(value >> 8).ok()?,
    })
}

fn parse_rgb_color_spec(bytes: &[u8]) -> Option<RgbColor> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    if let Some(rgb) = text.strip_prefix("rgb:") {
        let mut parts = rgb.split('/');
        let r = parse_color_channel(parts.next()?)?;
        let g = parse_color_channel(parts.next()?)?;
        let b = parse_color_channel(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        return Some(RgbColor { r, g, b });
    }
    let hex = text.strip_prefix('#')?;
    if hex.len() % 3 != 0 {
        return None;
    }
    let channel_len = hex.len() / 3;
    if !(1..=4).contains(&channel_len) {
        return None;
    }
    let (red, remaining) = hex.split_at_checked(channel_len)?;
    let (green, blue) = remaining.split_at_checked(channel_len)?;
    Some(RgbColor {
        r: parse_color_channel(red)?,
        g: parse_color_channel(green)?,
        b: parse_color_channel(blue)?,
    })
}

fn rgb_spec(color: RgbColor) -> String {
    format!(
        "rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}",
        color.r, color.r, color.g, color.g, color.b, color.b
    )
}

#[must_use]
pub fn encode_iterm2_report_cell_size(cell_width: f32, cell_height: f32, scale: f32) -> Vec<u8> {
    format!(
        "\x1b]1337;ReportCellSize={:.4};{:.4};{:.4}\x1b\\",
        cell_height.max(1.0),
        cell_width.max(1.0),
        scale.max(1.0)
    )
    .into_bytes()
}

#[must_use]
pub fn encode_iterm2_report_variable(value: &str) -> Vec<u8> {
    format!(
        "\x1b]1337;ReportVariable={}\x1b\\",
        general_purpose::STANDARD.encode(value.as_bytes())
    )
    .into_bytes()
}

#[must_use]
pub fn encode_osc52_response(selection: &str, text: &str) -> Vec<u8> {
    format!(
        "\x1b]52;{};{}\x1b\\",
        selection,
        general_purpose::STANDARD.encode(text.as_bytes())
    )
    .into_bytes()
}

fn hyperlink_uri_at(
    terminal: &Terminal<'static, 'static>,
    x: u16,
    y: u16,
    scratch: &mut Vec<u8>,
) -> Option<String> {
    let grid_ref = terminal
        .grid_ref(Point::Viewport(PointCoordinate { x, y: u32::from(y) }))
        .ok()?;
    scratch.resize(256, 0);
    loop {
        match grid_ref.hyperlink_uri(scratch) {
            Ok(0) => return None,
            Ok(len) => return String::from_utf8(scratch.get(..len)?.to_vec()).ok(),
            Err(libghostty_vt::Error::OutOfSpace { required }) => scratch.resize(required, 0),
            Err(_) => return None,
        }
    }
}

fn placement_rows_overlap_content(
    placement: &KittyImagePlacement,
    surface: TerminalSurface,
    rows: &[CachedRenderRow],
) -> bool {
    let origin = surface.content_origin();
    let min_y = placement.destination.min_y - origin.y;
    let max_y = placement.destination.max_y - origin.y;
    let min_x = placement.destination.min_x - origin.x;
    let max_x = placement.destination.max_x - origin.x;
    if max_y <= 0.0
        || min_y >= rows.len().to_f32().unwrap_or(f32::MAX) * surface.cell.height
        || max_x <= 0.0
        || surface.cell.width <= 0.0
        || surface.cell.height <= 0.0
    {
        return false;
    }

    let start = (min_y.max(0.0) / surface.cell.height)
        .floor()
        .max(0.0)
        .to_usize()
        .unwrap_or(usize::MAX);
    let end = (max_y.max(0.0) / surface.cell.height)
        .ceil()
        .max(0.0)
        .to_usize()
        .unwrap_or(usize::MAX);
    let end = end.saturating_sub(1).min(rows.len().saturating_sub(1));
    let start_col = (min_x.max(0.0) / surface.cell.width)
        .floor()
        .max(0.0)
        .to_u16()
        .unwrap_or(u16::MAX);
    let end_col = (max_x.max(0.0) / surface.cell.width)
        .ceil()
        .max(1.0)
        .max(0.0)
        .to_u16()
        .unwrap_or(u16::MAX);
    let end_col = end_col.saturating_sub(1);
    (start..=end).any(|index| {
        rows.get(index).is_some_and(|row| {
            row.cells.iter().any(|cell| {
                cell.x >= start_col
                    && cell.x <= end_col
                    && row
                        .text
                        .get(cell.text_start..cell.text_start.saturating_add(cell.text_len))
                        .is_some_and(|text| text.iter().any(|ch| !ch.is_whitespace()))
            })
        })
    })
}

impl TerminalEngine {
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot create or configure the terminal and its input or rendering state.
    pub fn new(geometry: TerminalGeometry) -> Result<Self> {
        Self::new_with_scrollback(
            geometry,
            TerminalColorConfig::default(),
            DEFAULT_MAX_SCROLLBACK,
        )
    }

    ///
    /// # Errors
    /// Returns an error if Ghostty cannot create or configure the terminal and its input or rendering state.
    pub fn new_with_terminal_options(
        geometry: TerminalGeometry,
        colors: TerminalColorConfig,
        cursor: TerminalCursorConfig,
        features: TerminalFeatureConfig,
        max_scrollback: usize,
        macos_option_as_alt: MacosOptionAsAlt,
    ) -> Result<Self> {
        Self::new_inner(
            geometry,
            colors,
            cursor,
            features,
            max_scrollback,
            macos_option_as_alt,
        )
    }

    ///
    /// # Errors
    /// Returns an error if Ghostty cannot create or configure the terminal and its input or rendering state.
    pub fn new_with_scrollback(
        geometry: TerminalGeometry,
        colors: TerminalColorConfig,
        max_scrollback: usize,
    ) -> Result<Self> {
        Self::new_inner(
            geometry,
            colors,
            TerminalCursorConfig::default(),
            TerminalFeatureConfig::default(),
            max_scrollback,
            MacosOptionAsAlt::default(),
        )
    }

    fn new_inner(
        geometry: TerminalGeometry,
        colors: TerminalColorConfig,
        cursor: TerminalCursorConfig,
        features: TerminalFeatureConfig,
        max_scrollback: usize,
        macos_option_as_alt: MacosOptionAsAlt,
    ) -> Result<Self> {
        install_libghostty_logger();
        let mut terminal = Terminal::new(geometry.cols, geometry.rows)?;
        terminal.set_scrollback_max_bytes(Some(max_scrollback))?;
        let base_color_palette = terminal.default_color_palette()?.0;
        configure_default_colors(&mut terminal, &base_color_palette, &colors)?;
        configure_default_cursor(&mut terminal, cursor)?;
        configure_terminal_features(&mut terminal, features)?;
        // Measure grapheme clusters by their display width (DEC mode 2027), matching Ghostty's
        // default. Without it the grid uses legacy per-codepoint wcwidth, so a VS16 emoji
        // presentation sequence (⚠️ ❤️) lands in a single cell — too narrow to render the color
        // glyph and inconsistent with how bootty measures the run.
        terminal.set_mode(Mode::GRAPHEME_CLUSTER, true)?;
        let size_report_state = Rc::new(Cell::new(SizeReportState {
            geometry,
            display_scale: 1.0,
            render_cell: CellMetrics::new(
                geometry.cell_width.to_f32().unwrap_or(f32::MAX),
                geometry.cell_height.to_f32().unwrap_or(f32::MAX),
            ),
        }));
        let color_scheme = Rc::new(Cell::new(color_scheme_for_background(colors.background)));
        let side_effects = TerminalSideEffectCollector::new();
        let pwd_state = Rc::new(RefCell::new(String::new()));
        let pty_write_callback: PtyWriteCallback = Rc::new(RefCell::new(None));
        let pty_write_suppressed = Rc::new(Cell::new(false));
        terminal.resize(
            geometry.cols,
            geometry.rows,
            geometry.cell_width,
            geometry.cell_height,
        )?;
        terminal.set_kitty_image_from_file_allowed(true)?;
        let temp_dir = std::env::temp_dir();
        #[cfg(unix)]
        let temp_dir = temp_dir.canonicalize().unwrap_or(temp_dir);
        terminal.set_kitty_image_temp_file_dir(Some(&temp_dir))?;
        terminal.set_kitty_image_from_shared_mem_allowed(true)?;
        set_png_decoder(Some(Box::new(BoottyPngDecoder)))?;

        let selection_gesture = gesture::Gesture::new()?;
        let selection_press_event = gesture::PressEvent::new()?;
        let selection_drag_event = gesture::DragEvent::new()?;
        let selection_release_event = gesture::ReleaseEvent::new()?;
        let mut engine = Self {
            terminal,
            base_color_palette,
            render_state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            image_placements: libghostty_vt::kitty::graphics::PlacementIterator::new()?,
            image_data_cache: KittyImageDataCache::default(),
            frame: RenderFrame::default(),
            row_cache: Vec::new(),
            grapheme_scratch: Vec::new(),
            key_encoder: key::Encoder::new()?,
            key_event: key::Event::new()?,
            macos_option_as_alt,
            mouse_encoder: mouse::Encoder::new()?,
            mouse_event: mouse::Event::new()?,
            mouse_any_button_pressed: false,
            selection_gesture,
            selection_press_event,
            selection_drag_event,
            selection_release_event,
            selection_clock_started: Instant::now(),
            mouse_encoder_options_dirty: true,
            mouse_encoder_size: None,
            geometry,
            size_report_state,
            side_effects,
            terminal_write_pending: Vec::new(),
            synchronized_output_prefix_len: 0,
            synchronized_output_observed: false,
            cursor_home_pending_len: 0,
            pty_write_callback,
            pty_write_suppressed,
            current_working_directory: String::new(),
            current_working_directory_state: pwd_state,
            color_scheme,
            colors,
            xterm_color_overrides: XtermColorOverrides::default(),
            search_query: String::new(),
            search_options: TerminalSearchOptions::default(),
            search_pattern: None,
            search_groups: Vec::new(),
            search_active_index: 0,
            search_pulse: 0,
            copy_mode: None,
            content_epoch: 0,
            extracted_content_epoch: u64::MAX,
            kitty_graphics_touched: false,
            display_scale: 1.0,
            render_cell: CellMetrics::new(
                geometry.cell_width.to_f32().unwrap_or(f32::MAX),
                geometry.cell_height.to_f32().unwrap_or(f32::MAX),
            ),
            render_cell_explicit: false,
        };
        engine.register_terminal_callbacks()?;
        engine.set_kitty_image_storage_limit(64 * 1024 * 1024)?;
        Ok(engine)
    }

    fn register_terminal_callbacks(&mut self) -> Result<()> {
        let report_state = self.size_report_state.clone();
        self.terminal
            .on_size(move |_terminal| Some(report_state.get().size()))?;
        self.terminal
            .on_device_attributes(|_terminal| Some(default_device_attributes()))?;
        let report_color_scheme = self.color_scheme.clone();
        self.terminal
            .on_color_scheme(move |_terminal| Some(report_color_scheme.get()))?;
        self.terminal
            .on_xtversion(|_terminal| Some(TERMINAL_XTVERSION))?;
        let callback_side_effects = self.side_effects.callback_effects();
        let title_side_effects = callback_side_effects.clone();
        self.terminal.on_title_changed(move |terminal| {
            if let Ok(title) = terminal.title() {
                title_side_effects
                    .borrow_mut()
                    .push(TerminalSideEffect::WindowTitle(title.to_owned()));
            }
        })?;
        let callback_pwd_state = self.current_working_directory_state.clone();
        self.terminal.on_pwd_changed(move |terminal| {
            if let Ok(pwd) = terminal.pwd() {
                let mut current = callback_pwd_state.borrow_mut();
                current.replace_range(.., pwd);
            }
        })?;
        let bell_side_effects = callback_side_effects;
        self.terminal.on_bell(move |_terminal| {
            bell_side_effects
                .borrow_mut()
                .push(TerminalSideEffect::Bell);
        })?;
        let terminal_pty_write_callback = self.pty_write_callback.clone();
        let terminal_pty_write_suppressed = self.pty_write_suppressed.clone();
        self.terminal.on_pty_write(move |terminal, bytes| {
            if terminal_pty_write_suppressed.get() {
                return;
            }
            if let Some(callback) = terminal_pty_write_callback.borrow_mut().as_deref_mut() {
                callback(terminal, bytes);
            }
        })?;
        Ok(())
    }

    const fn mark_content_changed(&mut self) {
        self.content_epoch = self.content_epoch.wrapping_add(1);
    }

    fn observe_synchronized_output_start(&mut self, bytes: &[u8]) {
        const START: &[u8; 8] = b"\x1b[?2026h";
        let mut remaining = bytes;
        while let Some((&byte, rest)) = remaining.split_first() {
            if self.synchronized_output_prefix_len == 0 {
                let Some(escape) = memchr(START[0], remaining) else {
                    break;
                };
                self.synchronized_output_prefix_len = 1;
                remaining = remaining
                    .split_at(escape.saturating_add(1).min(remaining.len()))
                    .1;
                continue;
            }
            remaining = rest;
            self.synchronized_output_prefix_len =
                if Some(&byte) == START.get(self.synchronized_output_prefix_len) {
                    self.synchronized_output_prefix_len.saturating_add(1)
                } else {
                    usize::from(byte == START[0])
                };
            if self.synchronized_output_prefix_len == START.len() {
                self.synchronized_output_observed = true;
                self.synchronized_output_prefix_len = 0;
            }
        }
    }

    pub fn take_synchronized_output_observed(&mut self) -> bool {
        std::mem::take(&mut self.synchronized_output_observed)
    }

    ///
    /// # Errors
    /// Returns an error if Ghostty cannot apply the selection gesture or read its terminal position.
    pub fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        let Some(point) = selection_point(event, self.geometry) else {
            self.clear_selection()?;
            return Ok(());
        };

        {
            let terminal = &self.terminal;
            let grid_ref = terminal.grid_ref(Point::Viewport(point))?;
            self.selection_press_event
                .set_position(f64::from(event.position.x), f64::from(event.position.y))?
                .set_repeat_distance(4.0)?
                .set_repeat_interval(SELECTION_REPEAT_INTERVAL)?
                .set_time(self.selection_clock_started.elapsed())?;
            let selection = self.selection_press_event.apply(
                &mut self.selection_gesture,
                terminal,
                grid_ref,
            )?;
            terminal.set_selection(selection.as_ref())?;
        }
        self.mark_content_changed();
        Ok(())
    }

    ///
    /// # Errors
    /// Returns an error if Ghostty cannot apply the selection gesture or read its terminal position.
    pub fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        let Some(point) = selection_point(event, self.geometry) else {
            return Ok(());
        };

        {
            let terminal = &self.terminal;
            let grid_ref = terminal.grid_ref(Point::Viewport(point))?;
            self.selection_drag_event
                .set_position(f64::from(event.position.x), f64::from(event.position.y))?
                .set_rectangle(event.rectangle)?;
            let selection = self.selection_drag_event.apply(
                &mut self.selection_gesture,
                terminal,
                grid_ref,
                selection_geometry(event.surface),
            )?;
            terminal.set_selection(selection.as_ref())?;
        }
        self.mark_content_changed();
        Ok(())
    }

    ///
    /// # Errors
    /// Returns an error if Ghostty cannot apply the selection gesture or read its terminal position.
    pub fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        let was_dragged = self
            .selection_gesture
            .dragged(&self.terminal)
            .unwrap_or(false);
        let behavior = self
            .selection_gesture
            .behavior(&self.terminal)
            .unwrap_or(gesture::Behavior::Cell);
        let point = event.and_then(|event| selection_point(event, self.geometry));

        {
            let terminal = &self.terminal;
            let grid_ref = point
                .map(|point| terminal.grid_ref(Point::Viewport(point)))
                .transpose()?;
            self.selection_release_event
                .apply(&mut self.selection_gesture, terminal, grid_ref)?;
        }

        if !was_dragged && behavior == gesture::Behavior::Cell {
            self.terminal.set_selection(None)?;
        }
        if !was_dragged
            && behavior == gesture::Behavior::Word
            && let Some(point) = point
        {
            let range = crate::terminal_links::semantic_selection_at(
                self.extract_frame()?,
                GridPoint {
                    x: point.x,
                    y: u16::try_from(point.y).unwrap_or(u16::MAX),
                },
            );
            if let Some(range) = range {
                let start = self.terminal.grid_ref(Point::Viewport(PointCoordinate {
                    x: range.anchor.x,
                    y: u32::from(range.anchor.y),
                }))?;
                let end = self.terminal.grid_ref(Point::Viewport(PointCoordinate {
                    x: range.focus.x,
                    y: u32::from(range.focus.y),
                }))?;
                let selection = libghostty_vt::selection::Selection::new(start, end, false);
                self.terminal.set_selection(Some(&selection))?;
            }
        }
        self.mark_content_changed();
        Ok(())
    }

    fn clear_selection(&mut self) -> Result<()> {
        self.terminal.set_selection(None)?;
        self.selection_gesture.reset(&self.terminal);
        self.mark_content_changed();
        Ok(())
    }

    ///
    /// # Errors
    /// Returns an error if Ghostty cannot read or format the active selection.
    pub fn format_selection(&self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        let options = SelectionFormatOptions::new()
            .with_emit_format(format.emit_format())
            .with_unwrap(true)
            .with_trim(true);
        Ok(self
            .terminal
            .format_selection_alloc(None, options)?
            .map(|bytes| bytes.as_ref().to_vec()))
    }

    fn set_cursor_config(&mut self, cursor: TerminalCursorConfig) -> Result<()> {
        configure_default_cursor(&mut self.terminal, cursor)?;
        self.mark_content_changed();
        Ok(())
    }

    fn set_feature_config(&mut self, features: TerminalFeatureConfig) -> Result<()> {
        configure_terminal_features(&mut self.terminal, features)?;
        self.mark_content_changed();
        Ok(())
    }

    ///
    /// # Errors
    /// Returns an error if Ghostty rejects a color, cursor, or feature update.
    pub fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        self.set_colors(config.colors)?;
        self.set_cursor_config(config.cursor)?;
        self.set_feature_config(config.features)
    }

    fn set_kitty_image_storage_limit(&mut self, limit: u64) -> Result<()> {
        self.terminal.set_kitty_image_storage_limit(limit)?;
        self.mark_content_changed();
        Ok(())
    }

    ///
    /// # Errors
    /// Returns an error if the PTY callback cannot be registered.
    pub fn on_pty_write(
        &mut self,
        f: impl libghostty_vt::terminal::PtyWriteFn<'static, 'static>,
    ) -> Result<()> {
        *self.pty_write_callback.borrow_mut() = Some(Box::new(f));
        Ok(())
    }

    fn write_pty_response(&self, bytes: &[u8]) {
        if self.pty_write_suppressed.get() {
            return;
        }
        if let Some(callback) = self.pty_write_callback.borrow_mut().as_deref_mut() {
            callback(&self.terminal, bytes);
        }
    }

    fn write_vt_with_ordered_osc_color_state(&mut self, data: &[u8]) {
        let mut remaining = data;
        while let Some(start) = find(remaining, b"\x1b]") {
            let Some((prefix, packet)) = remaining.split_at_checked(start) else {
                break;
            };
            let Some(payload) = packet.strip_prefix(b"\x1b]") else {
                break;
            };
            let Some((payload_len, terminator_len)) = find_osc_terminator(payload) else {
                break;
            };
            let Some((payload, terminated)) = payload.split_at_checked(payload_len) else {
                break;
            };
            let Some((terminator, rest)) = terminated.split_at_checked(terminator_len) else {
                break;
            };
            let standard_query_response = self.standard_color_query_response(payload, terminator);
            self.terminal.vt_write(prefix);
            if standard_query_response.is_none() {
                let consumed = packet.len().saturating_sub(rest.len());
                let (packet, _) = packet.split_at(consumed);
                self.terminal.vt_write(packet);
            }
            self.apply_osc_color_state(payload);
            if let Some(response) = standard_query_response
                .or_else(|| self.extended_color_query_response(payload, terminator))
            {
                self.write_pty_response(&response);
            }
            remaining = rest;
        }
        self.terminal.vt_write(remaining);
    }

    fn apply_osc_color_state(&mut self, payload: &[u8]) {
        let Some((command, rest)) = split_osc_payload(payload) else {
            if let Some(reset_code) = parse_osc_number(payload)
                && (113..=119).contains(&reset_code)
                && let Some(code) = reset_code
                    .checked_sub(100)
                    .and_then(|code| u8::try_from(code).ok())
                && self.xterm_color_overrides.reset(code)
            {
                self.mark_content_changed();
            }
            return;
        };
        let Some(start_code) = parse_osc_number(command) else {
            return;
        };
        if !(13..=19).contains(&start_code) {
            return;
        }
        let mut changed = false;
        for (code, spec) in (start_code..=19).zip(rest.split(|byte| *byte == b';')) {
            if spec == b"?" {
                break;
            }
            if let Some(color) = parse_rgb_color_spec(spec) {
                changed |= self
                    .xterm_color_overrides
                    .set(u8::try_from(code).unwrap_or(u8::MAX), color);
            }
        }
        if changed {
            self.mark_content_changed();
        }
    }

    fn extended_dynamic_color(&self, code: u8) -> Option<RgbColor> {
        match code {
            13 => self
                .xterm_color_overrides
                .get(13)
                .or(self.colors.pointer_foreground)
                .or(Some(self.colors.foreground)),
            14 => self
                .xterm_color_overrides
                .get(14)
                .or(self.colors.pointer_background)
                .or(Some(self.colors.background)),
            15 => self
                .xterm_color_overrides
                .get(15)
                .or(self.colors.tektronix_foreground)
                .or(Some(self.colors.foreground)),
            16 => self
                .xterm_color_overrides
                .get(16)
                .or(self.colors.tektronix_background)
                .or(Some(self.colors.background)),
            17 => self
                .xterm_color_overrides
                .get(17)
                .or(self.colors.highlight_background)
                .or(self.colors.selection_background)
                .or(Some(self.colors.foreground)),
            18 => self
                .xterm_color_overrides
                .get(18)
                .or(self.colors.tektronix_cursor)
                .or(self.colors.cursor)
                .or(Some(self.colors.foreground)),
            19 => self
                .xterm_color_overrides
                .get(19)
                .or(self.colors.highlight_foreground)
                .or(self.colors.selection_foreground)
                .or(Some(self.colors.background)),
            _ => None,
        }
    }

    fn standard_color_query_response(&self, payload: &[u8], terminator: &[u8]) -> Option<Vec<u8>> {
        let (command, rest) = split_osc_payload(payload)?;
        let mut response = Vec::new();
        match command {
            b"10" | b"11" | b"12" => {
                let start_code = u8::try_from(parse_osc_number(command)?).ok()?;
                for (offset, operation) in rest.split(|byte| *byte == b';').enumerate() {
                    let code = start_code.checked_add(u8::try_from(offset).ok()?)?;
                    if code > 12 || operation != b"?" {
                        return None;
                    }
                    let color = match code {
                        10 => self.terminal.fg_color().ok()?,
                        11 => self.terminal.bg_color().ok()?,
                        12 => self.terminal.cursor_color().ok()?,
                        _ => return None,
                    }?;
                    response
                        .extend_from_slice(format!("\x1b]{code};{}", rgb_spec(color)).as_bytes());
                    response.extend_from_slice(terminator);
                }
            }
            b"4" => {
                let palette = self.terminal.color_palette().ok()?;
                let mut operations = rest.split(|byte| *byte == b';');
                while let Some(index) = operations.next() {
                    let operation = operations.next()?;
                    if operation != b"?" {
                        return None;
                    }
                    let index = parse_osc_number(index)?;
                    let color = *palette.0.get(usize::from(index))?;
                    response.extend_from_slice(
                        format!("\x1b]4;{index};{}", rgb_spec(color)).as_bytes(),
                    );
                    response.extend_from_slice(terminator);
                }
            }
            _ => return None,
        }
        (!response.is_empty()).then_some(response)
    }

    fn extended_color_query_response(&self, payload: &[u8], terminator: &[u8]) -> Option<Vec<u8>> {
        let (command, rest) = split_osc_payload(payload)?;
        let start_code = u8::try_from(parse_osc_number(command)?).ok()?;
        if !(13..=19).contains(&start_code) {
            return None;
        }
        let mut response = Vec::new();
        for (code, operation) in (start_code..=19).zip(rest.split(|byte| *byte == b';')) {
            if operation != b"?" {
                return None;
            }
            let color = self.extended_dynamic_color(code)?;
            response.extend_from_slice(format!("\x1b]{code};{}", rgb_spec(color)).as_bytes());
            response.extend_from_slice(terminator);
        }
        (!response.is_empty()).then_some(response)
    }

    #[must_use]
    pub const fn grid_size(&self) -> (u16, u16) {
        (self.geometry.cols, self.geometry.rows)
    }

    #[must_use]
    pub const fn geometry(&self) -> TerminalGeometry {
        self.geometry
    }

    fn set_colors(&mut self, colors: TerminalColorConfig) -> Result<()> {
        configure_default_colors(&mut self.terminal, &self.base_color_palette, &colors)?;
        self.color_scheme
            .set(color_scheme_for_background(colors.background));
        self.colors = colors;
        self.mark_content_changed();
        Ok(())
    }

    ///
    /// # Errors
    /// Returns an error if Ghostty rejects the new terminal dimensions.
    pub fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        if geometry == self.geometry {
            return Ok(());
        }

        let previous = self.geometry;
        self.geometry = geometry;
        let mut report_state = self.size_report_state.get();
        report_state.geometry = geometry;
        if !self.render_cell_explicit {
            self.render_cell = CellMetrics::new(
                geometry.cell_width.to_f32().unwrap_or(f32::MAX),
                geometry.cell_height.to_f32().unwrap_or(f32::MAX),
            );
            report_state.render_cell = self.render_cell;
        }
        self.size_report_state.set(report_state);

        if geometry.cols != previous.cols && geometry.rows < previous.rows {
            self.terminal.resize(
                geometry.cols,
                previous.rows,
                geometry.cell_width,
                geometry.cell_height,
            )?;
        }
        self.terminal.resize(
            geometry.cols,
            geometry.rows,
            geometry.cell_width,
            geometry.cell_height,
        )?;
        self.mark_content_changed();

        Ok(())
    }

    pub fn set_display_scale(&mut self, display_scale: f32) {
        let display_scale = positive_or(display_scale, 1.0);
        if (self.display_scale - display_scale).abs() > f32::EPSILON {
            self.display_scale = display_scale;
            let mut report_state = self.size_report_state.get();
            report_state.display_scale = display_scale;
            self.size_report_state.set(report_state);
            if self.kitty_graphics_touched {
                self.mark_content_changed();
            }
        }
    }

    pub fn set_render_cell_metrics(&mut self, cell: CellMetrics) {
        let width = positive_or(
            cell.width,
            self.geometry.cell_width.to_f32().unwrap_or(f32::MAX),
        );
        let height = positive_or(
            cell.height,
            self.geometry.cell_height.to_f32().unwrap_or(f32::MAX),
        );
        let cell = CellMetrics::new(width, height);
        if self.render_cell != cell {
            self.render_cell = cell;
            let mut report_state = self.size_report_state.get();
            report_state.render_cell = cell;
            self.size_report_state.set(report_state);
            if self.kitty_graphics_touched {
                self.mark_content_changed();
            }
        }
        self.render_cell_explicit = true;
    }

    fn complete_streaming_terminal_write<'a>(&mut self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        if self.terminal_write_pending.is_empty() {
            let complete_len = complete_streaming_control_prefix_len(bytes);
            if complete_len == bytes.len() {
                return Cow::Borrowed(bytes);
            }
            let (complete, pending) = bytes.split_at(complete_len.min(bytes.len()));
            self.terminal_write_pending.extend_from_slice(pending);
            return Cow::Borrowed(complete);
        }

        let mut joined = Vec::with_capacity(
            self.terminal_write_pending
                .len()
                .saturating_add(bytes.len()),
        );
        joined.extend_from_slice(&self.terminal_write_pending);
        joined.extend_from_slice(bytes);
        self.terminal_write_pending.clear();

        let complete_len = complete_streaming_control_prefix_len(&joined);
        if complete_len < joined.len() {
            self.terminal_write_pending = joined.split_off(complete_len);
        }
        Cow::Owned(joined)
    }

    fn try_write_repeated_cursor_home(&mut self, bytes: &[u8]) -> bool {
        let Some((complete, pending_len)) =
            repeated_cursor_home_prefix_len(bytes, self.cursor_home_pending_len)
        else {
            if self.cursor_home_pending_len > 0 {
                self.terminal.vt_write(
                    CURSOR_HOME
                        .split_at(self.cursor_home_pending_len.min(CURSOR_HOME.len()))
                        .0,
                );
                self.cursor_home_pending_len = 0;
            }
            return false;
        };

        if complete > 0 {
            self.terminal.vt_write(CURSOR_HOME);
        }
        self.cursor_home_pending_len = pending_len;
        true
    }

    pub fn write_vt(&mut self, bytes: &[u8]) {
        self.observe_synchronized_output_start(bytes);
        let can_fast_write = self.terminal_write_pending.is_empty()
            && !self.side_effects.needs_input()
            && !contains_tracked_streaming_control(bytes);

        if can_fast_write && self.try_write_repeated_cursor_home(bytes) {
            self.mouse_encoder_options_dirty = true;
            self.mark_content_changed();
            return;
        }

        if self.cursor_home_pending_len > 0 {
            self.terminal.vt_write(
                CURSOR_HOME
                    .split_at(self.cursor_home_pending_len.min(CURSOR_HOME.len()))
                    .0,
            );
            self.cursor_home_pending_len = 0;
        }

        if can_fast_write {
            self.terminal.vt_write(bytes);
            self.sync_current_working_directory();
            self.mouse_encoder_options_dirty = true;
            self.mark_content_changed();
            return;
        }

        let write_bytes = self.complete_streaming_terminal_write(bytes);
        if write_bytes.is_empty() {
            return;
        }

        let mut features = terminal_write_features(write_bytes.as_ref());
        if self.side_effects.needs_input() {
            features.osc_side_effect = true;
        }
        if !features.needs_sanitizing() {
            self.terminal.vt_write(write_bytes.as_ref());
            self.sync_current_working_directory();
            self.mouse_encoder_options_dirty = true;
            self.mark_content_changed();
            return;
        }

        let bytes = if features.tmux_passthrough {
            let unwrapped = unwrap_tmux_passthrough_commands(write_bytes.as_ref());
            features = terminal_write_features(unwrapped.as_ref());
            if self.side_effects.needs_input() {
                features.osc_side_effect = true;
            }
            unwrapped
        } else {
            write_bytes
        };

        let sanitized = if features.kitty_graphics {
            sanitize_kitty_graphics_commands(bytes.as_ref())
        } else {
            SanitizedKittyGraphics {
                bytes,
                touched: false,
            }
        };
        if sanitized.touched {
            self.kitty_graphics_touched = true;
        }
        if features.osc_color {
            self.write_vt_with_ordered_osc_color_state(sanitized.bytes.as_ref());
        } else {
            self.terminal.vt_write(sanitized.bytes.as_ref());
        }
        self.sync_current_working_directory();
        if features.osc_side_effect {
            for action in self.side_effects.collect(sanitized.bytes.as_ref()) {
                let TerminalHostAction::WriteVt(bytes) = action;
                self.terminal.vt_write(bytes);
                self.mark_content_changed();
            }
        }
        self.mouse_encoder_options_dirty = true;
        self.mark_content_changed();
    }

    /// Replays historical pane output without answering the live child process
    /// or repeating what the bytes already did when they first arrived.
    /// Recovery keyframes can carry old terminal queries, a bell, or a
    /// notification the user has already seen. State the keyframe restores,
    /// such as the window title, is kept.
    pub fn write_vt_without_pty_responses(&mut self, bytes: &[u8]) {
        // A keyframe is a complete epoch, so the streaming buffers are emptied on
        // both sides of it. Whatever they held from the previous epoch would
        // otherwise be spliced onto the front of the keyframe, and whatever the
        // keyframe leaves behind would be flushed by the next live write with
        // responses and side effects back on.
        self.clear_streaming_writes();
        let previously_suppressed = self.pty_write_suppressed.replace(true);
        let side_effects = self.side_effects.mark();
        self.write_vt(bytes);
        self.pty_write_suppressed.set(previously_suppressed);
        self.side_effects.drop_replayed(side_effects);
        self.clear_streaming_writes();
        self.side_effects.reset_clipboard_epoch();
    }

    fn clear_streaming_writes(&mut self) {
        self.terminal_write_pending.clear();
        self.side_effects.clear_pending();
    }

    fn sync_current_working_directory(&mut self) {
        let current = self.current_working_directory_state.borrow();
        if self.current_working_directory.as_str() != current.as_str() {
            self.current_working_directory.clear();
            self.current_working_directory.push_str(&current);
        }
    }

    #[must_use]
    pub fn current_working_directory(&self) -> &str {
        &self.current_working_directory
    }

    pub fn drain_side_effects(&mut self) -> Vec<TerminalSideEffect> {
        self.side_effects.drain()
    }

    #[must_use]
    pub fn allows_prompt_editing(&self) -> bool {
        self.copy_mode.is_none()
            && self.search_query.is_empty()
            && self
                .terminal
                .active_screen()
                .is_ok_and(|screen| screen == libghostty_vt::screen::Screen::Primary)
            && self.terminal.mode(Mode::BRACKETED_PASTE).unwrap_or(false)
    }

    ///
    /// # Errors
    /// Returns an error if Ghostty cannot configure or encode this input event.
    pub fn encode_paste_to_vec(&mut self, text: &str, out: &mut Vec<u8>) -> Result<()> {
        out.clear();
        let bracketed = self.terminal.mode(Mode::BRACKETED_PASTE)?;
        let mut data = text.as_bytes().to_vec();
        let mut capacity = data.len().saturating_add(64).max(64);

        loop {
            out.resize(capacity, 0);
            match paste::encode(&mut data, bracketed, out) {
                Ok(written) => {
                    out.truncate(written);
                    return Ok(());
                }
                Err(libghostty_vt::Error::OutOfSpace { required }) => {
                    capacity = required.max(capacity.saturating_mul(2));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    ///
    /// # Errors
    /// Returns an error if Ghostty cannot configure or encode this input event.
    pub fn encode_key_to_vec(&mut self, input: KeyInput, out: &mut Vec<u8>) -> Result<()> {
        out.clear();
        self.key_encoder
            .set_options_from_terminal(&self.terminal)
            .set_alt_esc_prefix(true)
            .set_macos_option_as_alt(self.macos_option_as_alt.into());
        self.key_event
            .set_action(if input.repeat {
                key::Action::Repeat
            } else {
                key::Action::Press
            })
            .set_key(input.key.into())
            .set_mods(input.mods.into())
            .set_utf8(input.utf8);
        self.key_event
            .set_unshifted_codepoint(input.unshifted.unwrap_or('\0'));
        self.key_encoder.encode_to_vec(&self.key_event, out)?;
        Ok(())
    }

    ///
    /// # Errors
    /// Returns an error if Ghostty cannot configure or encode this input event.
    pub fn encode_focus_to_vec(&mut self, gained: bool, out: &mut Vec<u8>) -> Result<()> {
        out.clear();
        if !self.terminal.mode(Mode::FOCUS_EVENT)? {
            return Ok(());
        }

        let event = if gained {
            focus::Event::Gained
        } else {
            focus::Event::Lost
        };
        out.resize(16, 0);
        let written = event.encode(out)?;
        out.truncate(written);
        Ok(())
    }

    ///
    /// # Errors
    /// Returns an error if Ghostty cannot configure or encode this input event.
    pub fn encode_mouse_to_vec(&mut self, input: MouseInput, out: &mut Vec<u8>) -> Result<()> {
        out.clear();
        if !self.terminal.is_mouse_tracking()? {
            return Ok(());
        }

        if self.mouse_encoder_options_dirty {
            self.mouse_encoder
                .set_options_from_terminal(&self.terminal)
                .set_track_last_cell(true);
            self.mouse_encoder_options_dirty = false;
            self.mouse_encoder_size = None;
        }
        let display_scale = self.display_scale;
        let encoder_size = scaled_mouse_encoder_size(input.size, display_scale);
        if self.mouse_encoder_size != Some(encoder_size) {
            self.mouse_encoder.set_size(encoder_size.into());
            self.mouse_encoder_size = Some(encoder_size);
        }
        self.mouse_encoder
            .set_any_button_pressed(self.mouse_any_button_pressed);
        let position = if self.terminal.mode(Mode::SGR_PIXELS_MOUSE)? {
            (input.pixel_x, input.pixel_y)
        } else {
            (input.x, input.y)
        };
        self.mouse_event
            .set_action(input.action.into())
            .set_button(input.button.map(Into::into))
            .set_mods(input.mods.into())
            .set_position(mouse::Position {
                x: position.0 * display_scale,
                y: position.1 * display_scale,
            });
        if out.capacity() < 64 {
            out.reserve(64_usize.saturating_sub(out.capacity()));
        }
        if let Err(error) = self.mouse_encoder.encode_to_vec(&self.mouse_event, out) {
            match error {
                libghostty_vt::Error::OutOfSpace { required } if required > out.capacity() => {
                    out.clear();
                    out.reserve(required.saturating_sub(out.capacity()));
                    self.mouse_encoder.encode_to_vec(&self.mouse_event, out)?;
                }
                error => return Err(error.into()),
            }
        }

        match input.action {
            MouseAction::Press => self.mouse_any_button_pressed = true,
            MouseAction::Release => self.mouse_any_button_pressed = false,
            MouseAction::Motion => {}
        }

        Ok(())
    }

    /// Encode a wheel scroll of `notches` rows for a mouse-tracking application, which expects one
    /// button report per row. A wheel event carries however many rows the scroll accumulator
    /// resolved, so reporting it once makes fast scrolling crawl a line at a time.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot configure or encode this input event.
    pub fn encode_mouse_wheel_to_vec(
        &mut self,
        input: MouseInput,
        notches: usize,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        self.encode_mouse_to_vec(input, out)?;
        let notch = out.len();
        if notch == 0 {
            return Ok(());
        }
        // The row count reaches us from a saturating f32 cast of an OS scroll delta, so a
        // non-finite delta arrives as `isize::MAX`. Nothing observed produces one, but the cost of
        // being wrong is an unbounded write to the PTY, and no real scroll exceeds a screenful.
        let notches = notches.min(MAX_WHEEL_REPORTS);
        out.reserve(notch.saturating_mul(notches.saturating_sub(1)));
        for _ in 1..notches {
            out.extend_from_within(..notch);
        }
        Ok(())
    }

    pub fn scroll_viewport_delta(&mut self, delta: isize) {
        self.terminal.scroll_viewport(ScrollViewport::Delta(delta));
        self.mark_content_changed();
    }

    /// Scroll to an absolute row offset from the top of the scrollback.
    pub fn scroll_viewport_to(&mut self, offset: usize) {
        self.terminal.scroll_viewport(ScrollViewport::Row(offset));
        self.mark_content_changed();
    }

    pub fn scroll_viewport_bottom(&mut self) {
        self.terminal.scroll_viewport(ScrollViewport::Bottom);
        self.mark_content_changed();
    }

    ///
    /// # Errors
    /// Returns an error for an invalid search pattern or a failure to read or move the terminal viewport.
    pub fn search_viewport(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> Result<bool> {
        self.search_viewport_with_options(query, direction, TerminalSearchOptions::default())
    }

    fn set_search_query(&mut self, query: &str, options: TerminalSearchOptions) -> Result<()> {
        if self.search_query != query || self.search_options != options {
            // Validate before replacing the last successful search or moving the viewport.
            let pattern = if query.is_empty() {
                None
            } else {
                Some(SearchPattern::new(query, options)?)
            };
            query.clone_into(&mut self.search_query);
            self.search_options = options;
            self.search_pattern = pattern;
            self.search_active_index = 0;
            self.mark_content_changed();
        }
        Ok(())
    }

    ///
    /// # Errors
    /// Returns an error for an invalid search pattern or a failure to read or move the terminal viewport.
    pub fn search_viewport_with_options(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
        options: TerminalSearchOptions,
    ) -> Result<bool> {
        self.set_search_query(query, options)?;
        if query.is_empty() {
            let _ = self.extract_frame()?;
            return Ok(false);
        }

        let frame = self.extract_frame()?;
        let initial_offset = frame.scrollbar.map_or(0, |scrollbar| scrollbar.offset);
        let visible_count = frame.search_match_count;
        if visible_count > 0 {
            match direction {
                TerminalSearchDirection::Current => return Ok(true),
                TerminalSearchDirection::Next
                    if self.search_active_index.saturating_add(1) < visible_count =>
                {
                    return self
                        .select_search_match(self.search_active_index.saturating_add(1), true);
                }
                TerminalSearchDirection::Previous if self.search_active_index > 0 => {
                    return self
                        .select_search_match(self.search_active_index.saturating_sub(1), true);
                }
                TerminalSearchDirection::Next | TerminalSearchDirection::Previous => {}
            }
        }

        let delta = match direction {
            TerminalSearchDirection::Current | TerminalSearchDirection::Previous => {
                isize::try_from(self.geometry.rows.max(1))
                    .unwrap_or(isize::MAX)
                    .saturating_neg()
            }
            TerminalSearchDirection::Next => {
                isize::try_from(self.geometry.rows.max(1)).unwrap_or(isize::MAX)
            }
        };

        let mut current_offset = initial_offset;
        loop {
            let before = current_offset;
            self.scroll_viewport_delta(delta);
            let frame = self.extract_frame()?;
            current_offset = frame.scrollbar.map_or(before, |scrollbar| scrollbar.offset);
            if current_offset == before {
                break;
            }
            let found_count = frame.search_match_count;
            if found_count > 0 {
                let index = match direction {
                    TerminalSearchDirection::Previous => found_count.saturating_sub(1),
                    TerminalSearchDirection::Current | TerminalSearchDirection::Next => 0,
                };
                return self
                    .select_search_match(index, direction != TerminalSearchDirection::Current);
            }
        }

        let restore_delta = i128::from(initial_offset).saturating_sub(i128::from(current_offset));
        let found = if restore_delta == 0 {
            visible_count
        } else {
            self.scroll_viewport_delta(isize::try_from(restore_delta).unwrap_or(
                if restore_delta < 0 {
                    isize::MIN
                } else {
                    isize::MAX
                },
            ));
            self.extract_frame()?.search_match_count
        };
        if found > 0 {
            return self.select_search_match(
                self.search_active_index.min(found.saturating_sub(1)),
                direction != TerminalSearchDirection::Current,
            );
        }
        Ok(false)
    }

    fn select_search_match(&mut self, index: usize, pulse: bool) -> Result<bool> {
        self.search_active_index = index;
        if pulse {
            self.bump_search_pulse();
            let _ = self.extract_frame()?;
        }
        Ok(true)
    }

    const fn bump_search_pulse(&mut self) {
        self.search_pulse = self.search_pulse.wrapping_add(1);
        self.mark_content_changed();
    }

    ///
    /// # Errors
    /// Returns an error if Ghostty cannot read the terminal mode.
    pub fn is_mouse_tracking(&self) -> Result<bool> {
        self.terminal.is_mouse_tracking().map_err(Into::into)
    }

    ///
    /// # Errors
    /// Returns an error if Ghostty cannot read the terminal mode.
    pub fn is_synchronized_output(&self) -> Result<bool> {
        self.terminal.mode(Mode::SYNC_OUTPUT).map_err(Into::into)
    }

    fn assemble_cached_frame(
        &mut self,
        extract_start: Instant,
        render_state_update_us: u64,
        row_dirty: Vec<bool>,
    ) -> Result<&RenderFrame> {
        self.frame.row_dirty = row_dirty;
        self.frame.row_wraps.clear();
        self.frame.cells.clear();
        self.frame.text.clear();
        self.frame.images = KittyImageFrame::default();
        self.frame.selections.clear();
        self.frame.search_matches.clear();
        self.frame.active_search_match = None;
        self.frame.active_search_segments.clear();
        self.frame.active_search_match_index = None;
        self.frame.search_match_count = 0;
        self.frame.search_pulse = self.search_pulse;
        self.frame.mouse_tracking = self.terminal.is_mouse_tracking()?;
        self.frame.stats = FrameStats {
            render_state_update_us,
            ..FrameStats::default()
        };

        let mut virtual_cells = Vec::new();
        for row in &self.row_cache {
            if let Some(selection) = row.selection {
                self.frame.selections.push(selection);
            }
            self.frame.row_wraps.push(row.wrapped);
            virtual_cells.extend(row.virtual_cells.iter().cloned());
            let text_offset = self.frame.text.len();
            self.frame.text.extend_from_slice(&row.text);
            self.frame.stats.chars = self.frame.stats.chars.saturating_add(row.text.len());
            self.frame.stats.cells = self.frame.stats.cells.saturating_add(row.cells.len());
            self.frame
                .cells
                .extend(row.cells.iter().cloned().map(|mut cell| {
                    cell.text_start = cell.text_start.saturating_add(text_offset);
                    cell
                }));
        }
        self.search_groups.clear();
        if let Some(pattern) = &self.search_pattern {
            self.search_groups = frame_search_matches(&self.frame, pattern);
            self.frame.search_matches = self.search_groups.iter().flatten().copied().collect();
            self.frame.search_match_count = self.search_groups.len();
            if self.frame.search_match_count > 0 {
                self.search_active_index = self
                    .search_active_index
                    .min(self.frame.search_match_count.saturating_sub(1));
                self.frame.active_search_match_index =
                    Some(self.search_active_index.saturating_add(1));
                if let Some(group) = self.search_groups.get(self.search_active_index) {
                    self.frame.active_search_segments.clone_from(group);
                    self.frame.active_search_match = group.first().copied();
                }
            }
        }
        self.frame.stats.dirty_rows = self.frame.row_dirty.iter().filter(|dirty| **dirty).count();

        if self.kitty_graphics_touched || !virtual_cells.is_empty() {
            let surface = TerminalSurface::for_logical_size(
                f32::from(self.geometry.cols) * self.render_cell.width,
                f32::from(self.geometry.rows) * self.render_cell.height,
                self.render_cell,
                TerminalPadding::default(),
            );
            let mut images = collect_kitty_image_frame(
                &self.terminal,
                surface,
                self.display_scale,
                &mut self.image_placements,
                &mut self.image_data_cache,
            )
            .unwrap_or_default();
            images.placements.retain(|placement| {
                !placement_rows_overlap_content(placement, surface, &self.row_cache)
            });
            images.virtual_placeholder_rows = append_virtual_image_placements(
                &self.terminal,
                surface,
                self.display_scale,
                &mut images,
                &virtual_cells,
                &mut self.image_data_cache,
            )?;
            self.image_data_cache.retain_frame(&images);
            self.frame.images = images;
        }

        self.apply_copy_mode_frame(self.viewport_top_screen_row()?);
        self.frame.stats.extraction_us =
            u64::try_from(extract_start.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.extracted_content_epoch = self.content_epoch;
        Ok(&self.frame)
    }

    fn apply_copy_mode_frame(&mut self, viewport_top: u32) {
        self.frame.copy_mode = self.copy_mode.as_ref().map(Self::copy_mode_frame_state);
        Self::apply_copy_mode_frame_cursor(&mut self.frame, self.copy_mode.as_ref(), viewport_top);
    }

    fn reuse_clean_frame(
        &mut self,
        viewport_top: u32,
        extract_start: Instant,
        render_state_update_us: u64,
    ) {
        self.apply_copy_mode_frame(viewport_top);
        self.frame.row_dirty.clear();
        self.frame
            .row_dirty
            .resize(usize::from(self.frame.rows), false);
        self.frame.stats = FrameStats {
            render_state_update_us,
            extraction_us: u64::try_from(extract_start.elapsed().as_micros()).unwrap_or(u64::MAX),
            cells: self.frame.cells.len(),
            chars: self.frame.text.len(),
            dirty_rows: 0,
        };
    }

    ///
    /// # Errors
    /// Returns an error if terminal state cannot be read or its row and grapheme bounds are inconsistent.
    pub fn extract_frame(&mut self) -> Result<&RenderFrame> {
        let extract_start = Instant::now();
        let update_start = Instant::now();
        let snapshot = self.render_state.update(&self.terminal)?;
        let render_state_update_us =
            u64::try_from(update_start.elapsed().as_micros()).unwrap_or(u64::MAX);
        let colors = snapshot.colors()?;
        let cols = snapshot.cols()?;
        let rows = snapshot.rows()?;
        let dirty = snapshot.dirty()?;
        let cache_matches_frame = self.frame.cols == cols
            && self.frame.rows == rows
            && self.row_cache.len() == usize::from(rows);
        let can_reuse_clean_frame = self.content_epoch == self.extracted_content_epoch
            && cache_matches_frame
            && !self.frame.cells.is_empty();

        self.frame.lineage.advance();
        self.frame.cols = cols;
        self.frame.rows = rows;
        self.frame.dirty = if can_reuse_clean_frame {
            Dirty::Clean
        } else {
            dirty
        };
        self.frame.colors =
            resolve_frame_colors(&colors, &self.colors, &self.xterm_color_overrides);
        self.frame.cursor = if snapshot.cursor_visible()? {
            snapshot.cursor_viewport()?.map(|cursor| CursorSnapshot {
                x: cursor.x,
                y: cursor.y,
                at_wide_tail: cursor.at_wide_tail,
                style: snapshot
                    .cursor_visual_style()
                    .unwrap_or(CursorVisualStyle::Block),
                blinking: snapshot.cursor_blinking().unwrap_or(false),
                color: snapshot.cursor_color().ok().flatten().or(colors.cursor),
            })
        } else {
            None
        };
        let scrollbar = self.terminal.scrollbar()?;
        self.frame.scrollbar = Some(FrameScrollbar {
            total: scrollbar.total,
            offset: scrollbar.offset,
            len: scrollbar.len,
        });
        if can_reuse_clean_frame {
            self.reuse_clean_frame(
                u32::try_from(scrollbar.offset).unwrap_or(u32::MAX),
                extract_start,
                render_state_update_us,
            );
            return Ok(&self.frame);
        }

        // Extract through the row cache for any non-clean frame. A cold cache (first
        // frame, resize, or a full redraw) re-extracts every row once and leaves the
        // cache warm, so the *next* localized edit extracts incrementally instead of
        // paying a full re-extraction (the §5.4 cold-cache cliff). A warm cache touches
        // only the rows that changed. `extract_render_row` is the row-decomposed form of
        // the former inline full-frame loop, reassembled by `assemble_cached_frame`.
        let full = dirty == Dirty::Full;
        let mut row_dirty = Vec::with_capacity(usize::from(rows));
        {
            let mut row_iter = self.rows.update(&snapshot)?;
            while let Some(row) = row_iter.next() {
                row_dirty.push(full || row.dirty()?);
            }
        }

        self.row_cache
            .resize_with(usize::from(rows), CachedRenderRow::default);
        // A cold cache can't trust per-row dirty flags against stale/absent rows, so
        // re-extract everything; a warm cache extracts only the rows reported dirty.
        let update_all_rows = full || !cache_matches_frame;
        let mut row_iter = self.rows.update(&snapshot)?;
        let mut row_index = 0_u16;
        let mut hyperlink_scratch = Vec::new();
        while let Some(row) = row_iter.next() {
            let index = usize::from(row_index);
            if update_all_rows || row_dirty.get(index).copied().unwrap_or(false) {
                extract_render_row(
                    &self.terminal,
                    &mut self.cells,
                    &mut self.grapheme_scratch,
                    &mut hyperlink_scratch,
                    row,
                    row_index,
                    self.row_cache
                        .get_mut(index)
                        .context("render row exceeds frame geometry")?,
                )?;
            }
            // Clear the render-state row dirty flag so the next update reports only
            // newly-changed rows. libghostty's update does not unset dirty state.
            row.set_dirty(false)?;
            row_index = row_index
                .checked_add(1)
                .context("render row exceeds terminal limit")?;
        }
        snapshot.set_dirty(Dirty::Clean)?;
        self.assemble_cached_frame(extract_start, render_state_update_us, row_dirty)
    }
}

fn resolve_frame_colors(
    colors: &libghostty_vt::render::Colors,
    configured: &TerminalColorConfig,
    overrides: &XtermColorOverrides,
) -> FrameColors {
    FrameColors {
        background: colors.background,
        foreground: colors.foreground,
        cursor: colors.cursor,
        cursor_text: configured.cursor_text,
        selection_background: overrides
            .get(17)
            .or(configured.highlight_background)
            .or(configured.selection_background),
        selection_foreground: overrides
            .get(19)
            .or(configured.highlight_foreground)
            .or(configured.selection_foreground),
    }
}
