use std::path::PathBuf;

use anyhow::{Context, Result};

use bootty_config::config::{BoottyConfig, MacosTitlebarStyle, WindowConfig};

pub(crate) enum ClipboardContent {
    Text(String),
    Image(arboard::ImageData<'static>),
}

pub(crate) fn read_clipboard_content() -> Result<Option<ClipboardContent>> {
    if let Some(paths) = crate::file_paths::read_clipboard_file_paths()
        && let Some(text) =
            crate::file_paths::format_file_paths_for_paste(paths.iter().map(PathBuf::as_path))
    {
        return Ok(Some(ClipboardContent::Text(text)));
    }
    let mut clipboard = arboard::Clipboard::new()?;
    match clipboard.get_text() {
        Ok(text) if !text.is_empty() => return Ok(Some(ClipboardContent::Text(text))),
        Ok(_) | Err(arboard::Error::ContentNotAvailable) => {}
        Err(error) => return Err(error.into()),
    }
    match clipboard.get_image() {
        Ok(image) => Ok(Some(ClipboardContent::Image(arboard::ImageData {
            width: image.width,
            height: image.height,
            bytes: image.bytes.into_owned().into(),
        }))),
        Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Read clipboard text or file paths formatted for shell input.
///
/// # Errors
/// Returns an error when the system clipboard cannot be accessed or read.
pub fn read_clipboard_text() -> Result<Option<String>> {
    // OSC 52 text queries must not receive a synthetic local image path.
    Ok(match read_clipboard_content()? {
        Some(ClipboardContent::Text(text)) => Some(text),
        Some(ClipboardContent::Image(_)) | None => None,
    })
}

/// Runs on the command worker. Never substitute a local image path after a remote failure.
pub(crate) fn prepare_clipboard_paste(
    content: ClipboardContent,
    remote: Option<&bootty_host::remote::RemoteHost>,
    runner: &impl bootty_host::CommandRunner,
) -> Result<String> {
    let image = match content {
        ClipboardContent::Text(text) => return Ok(text),
        ClipboardContent::Image(image) => image,
    };
    let staged = write_clipboard_image_png(&image)?;
    let path = if let Some(remote) = remote {
        bootty_host::clipboard_image::upload_clipboard_image(remote, staged.path(), runner)?
    } else {
        let (_, path) = staged.keep().context("retain clipboard image")?;
        path.to_string_lossy().into_owned()
    };
    crate::file_paths::format_file_paths_for_paste([std::path::Path::new(&path)])
        .context("format clipboard image path")
}

fn write_clipboard_image_png(image: &arboard::ImageData<'_>) -> Result<tempfile::NamedTempFile> {
    let width = u32::try_from(image.width).context("clipboard image width")?;
    let height = u32::try_from(image.height).context("clipboard image height")?;
    let mut file = tempfile::Builder::new()
        .prefix("bootty-clipboard-")
        .suffix(".png")
        .tempfile()?;
    {
        let mut encoder = png::Encoder::new(&mut file, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .context("write clipboard image header")?
            .write_image_data(&image.bytes)
            .context("write clipboard image data")?;
    }
    Ok(file)
}

/// Replace the system clipboard with text.
///
/// # Errors
/// Returns an error when the clipboard cannot be accessed or written.
pub fn write_clipboard_text(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_text(text.to_owned())?;
    Ok(())
}

/// Replace the clipboard with HTML and optional plain text.
///
/// # Errors
/// Returns an error when the clipboard cannot be accessed or written.
pub fn write_clipboard_html(html: &str, plain_text: Option<&str>) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_html(html.to_owned(), plain_text.map(str::to_owned))?;
    Ok(())
}

/// Delivery runs on a worker. OS notification permissions remain authoritative.
///
/// # Errors
/// Returns an error when notification initialization or delivery fails.
pub fn show_desktop_notification(title: &str, body: &str) -> Result<()> {
    let identity = bootty_config::ApplicationIdentity::current();
    #[cfg(target_os = "macos")]
    {
        static APPLICATION: std::sync::OnceLock<std::result::Result<(), String>> =
            std::sync::OnceLock::new();
        if let Err(error) = APPLICATION.get_or_init(|| {
            notify_rust::set_application(identity.bundle_identifier())
                .map_err(|error| error.to_string())
        }) {
            anyhow::bail!("initialize notifications: {error}");
        }
    }
    let mut notification = notify_rust::Notification::new();
    notification
        .appname(identity.display_name())
        .summary(title)
        .body(body);
    #[cfg(windows)]
    notification.app_id(identity.bundle_identifier());
    notification
        .show()
        .context("deliver desktop notification")?;
    Ok(())
}

#[must_use]
pub const fn macos_handles_non_native_fullscreen_frame(window: &WindowConfig) -> bool {
    window.hides_macos_menu_bar_in_non_native_fullscreen()
        && crate::window::handles_macos_non_native_fullscreen_frame()
}

pub fn native_options_for_config(
    config: &BoottyConfig,
    cx: &gpui_kit::App,
) -> gpui_kit::WindowOptions {
    use gpui_kit::{
        Bounds, TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions, px, size,
    };

    let bounds = Bounds::centered(
        None,
        size(px(config.window.width), px(config.window.height)),
        cx,
    );
    let window_bounds = if config.window.native_fullscreen_enabled() {
        WindowBounds::Fullscreen(bounds)
    } else if config.window.non_native_fullscreen_enabled()
        && !macos_handles_non_native_fullscreen_frame(&config.window)
    {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    };

    let titlebar = match config.window.macos_titlebar_style {
        MacosTitlebarStyle::Native if config.window.decorations_enabled() => {
            Some(TitlebarOptions {
                title: Some(config.window.title.clone().into()),
                ..Default::default()
            })
        }
        MacosTitlebarStyle::Transparent if config.window.decorations_enabled() => {
            Some(TitlebarOptions {
                title: Some(config.window.title.clone().into()),
                appears_transparent: true,
                ..Default::default()
            })
        }
        MacosTitlebarStyle::Hidden
        | MacosTitlebarStyle::Native
        | MacosTitlebarStyle::Transparent => None,
    };

    WindowOptions {
        window_background: crate::gpui_background::material(&config.window),
        window_bounds: Some(window_bounds),
        titlebar,
        // Bootty draws and hit-tests its own titlebar. Match Zed's ownership model so AppKit
        // neither moves the window from tab drags nor delays clicks in custom chrome.
        app_owns_titlebar_drag: true,
        app_id: Some(
            bootty_config::ApplicationIdentity::current()
                .bundle_identifier()
                .to_owned(),
        ),
        // GPUI uses client-side decorations to expose a borderless surface on Linux. On macOS
        // and Windows the titlebar option above is authoritative.
        window_decorations: (!config.window.decorations_enabled())
            .then_some(WindowDecorations::Client),
        ..Default::default()
    }
}

#[cfg(target_os = "macos")]
#[must_use]
pub const fn new_tab_shortcut_trigger() -> &'static str {
    "cmd+t"
}

#[cfg(not(target_os = "macos"))]
pub fn new_tab_shortcut_trigger() -> &'static str {
    "ctrl+shift+t"
}

/// Decode one still image off the UI thread, with independent encoded and decoded bounds.
///
/// # Errors
/// Rejects unsupported or mismatched MIME types, invalid images, and images exceeding
/// the encoded size, dimensions, pixel count, or decode allocation limits.
pub fn decode_clipboard_image(mime: &str, bytes: &[u8]) -> Result<arboard::ImageData<'static>> {
    anyhow::ensure!(
        bytes.len() <= bootty_terminal::clipboard_write::MAX_IMAGE,
        "clipboard image exceeds 16 MiB"
    );
    let format = match mime {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" | "image/jpg" => image::ImageFormat::Jpeg,
        "image/gif" => image::ImageFormat::Gif,
        "image/webp" => image::ImageFormat::WebP,
        _ => anyhow::bail!("unsupported clipboard image type"),
    };
    anyhow::ensure!(
        image::guess_format(bytes)? == format,
        "clipboard MIME does not match image data"
    );
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?;
    anyhow::ensure!(
        u64::from(image.width()).saturating_mul(u64::from(image.height())) <= 16 * 1024 * 1024,
        "clipboard image exceeds 16 megapixels"
    );
    let image = image.into_rgba8();
    Ok(arboard::ImageData {
        width: usize::try_from(image.width()).context("clipboard image width")?,
        height: usize::try_from(image.height()).context("clipboard image height")?,
        bytes: image.into_raw().into(),
    })
}
