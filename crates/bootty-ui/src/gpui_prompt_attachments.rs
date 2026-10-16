//! File references are atomic editor content; the session owns attachment bytes.
use gpui_kit::component::{
    ActiveTheme as _,
    input::{InlineToken, InlineTokenContext},
};
use gpui_kit::{App, IntoElement, ParentElement as _, Styled as _, div};

pub fn token(id: String, name: &str, bytes: u64) -> InlineToken {
    let name = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    InlineToken::new(id, format!("[{name}]")).with_label(format!("{name} {}", size(bytes)))
}

/// A prompt token for one app window. The id carries the bundle so any composer can draw the
/// app's icon without its mention list; bundle ids never contain `:`.
pub fn application_token(target: &bootty_computer::ComputerTarget) -> InlineToken {
    InlineToken::new(
        format!(
            "application:{}:{}:{}",
            target.process_id, target.window_id, target.bundle_id
        ),
        format!(
            "@{}",
            crate::gpui_composer_completion::application_name(target)
        ),
    )
}

fn application_bundle(token_id: &str) -> Option<&str> {
    token_id.strip_prefix("application:")?.splitn(3, ':').nth(2)
}

/// One look for an app mention in composers and transcripts: the app's icon and name.
pub fn application_chip(bundle_id: &str, name: &str, selected: bool, cx: &App) -> gpui_kit::Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .px_1()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(if selected {
            cx.theme().selection
        } else {
            cx.theme().muted.opacity(0.4)
        })
        .text_color(cx.theme().foreground)
        .child(crate::gpui_application_icons::render(
            bundle_id,
            crate::gpui::IconSize::XSmall,
            cx.theme().muted_foreground,
            cx,
        ))
        .child(
            div()
                .min_w_0()
                .max_w(gpui_kit::rems(18.))
                .text_ellipsis()
                .child(name.trim_start_matches('@').to_owned()),
        )
}

pub fn render_with_provider(
    context: &InlineTokenContext,
    provider: Option<bootty_agents::NativeCompletionKind>,
    cx: &App,
) -> gpui_kit::AnyElement {
    if let Some(bundle_id) = application_bundle(context.token().id()) {
        return application_chip(
            bundle_id,
            context.token().label(),
            context.is_selected(),
            cx,
        )
        .h(context.line_height())
        .max_w(context.available_width())
        .into_any_element();
    }
    let name = context
        .token()
        .text()
        .trim_start_matches('[')
        .trim_end_matches(']');
    div()
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .h(context.line_height())
        .max_w(context.available_width())
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(if context.is_selected() {
            cx.theme().selection
        } else {
            cx.theme().muted.opacity(0.4)
        })
        .text_color(cx.theme().foreground)
        .child(
            if provider.is_none() && name.to_ascii_lowercase().ends_with(".md") {
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("M↓")
                    .into_any_element()
            } else {
                crate::gpui::sized_icon(
                    if provider == Some(bootty_agents::NativeCompletionKind::App) {
                        "app-window"
                    } else if provider == Some(bootty_agents::NativeCompletionKind::Plugin) {
                        "plug"
                    } else if context.token().id().starts_with("directory:") {
                        "folder"
                    } else if context.token().id().starts_with("skill:") {
                        "sparkles"
                    } else {
                        icon(name)
                    },
                    crate::gpui::IconSize::XSmall,
                    cx.theme().muted_foreground,
                )
            },
        )
        .child(
            div()
                .min_w_0()
                .max_w(gpui_kit::rems(18.))
                .text_ellipsis()
                .child(context.token().label().clone()),
        )
        .into_any_element()
}

pub fn icon(name: &str) -> &'static str {
    match std::path::Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "svg" => "image",
        "mp4" | "webm" | "mov" => "film",
        "mp3" | "wav" | "m4a" | "ogg" => "music",
        "rs" | "js" | "ts" | "tsx" | "py" | "json" | "toml" | "yaml" => "file-code",
        "zip" | "tar" | "gz" => "file-archive",
        "md" | "txt" | "pdf" | "docx" => "file-text",
        _ => "file",
    }
}

pub fn size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{} KB", bytes.div_ceil(1024))
    } else {
        let tenths = bytes.saturating_mul(10) / (1024 * 1024);
        format!("{}.{:01} MB", tenths / 10, tenths % 10)
    }
}

/// A folder is referenced in the prompt, rather than uploaded recursively.
pub fn directory_token(path: &std::path::Path) -> InlineToken {
    let path_text = path.to_string_lossy();
    let label =
        bootty_git::project::display_path(&path_text, crate::strings::home_dir().as_deref());
    InlineToken::new(
        format!("directory:{path_text}"),
        format!(
            "{} ",
            serde_json::Value::String(format!("{}/", path_text.trim_end_matches('/')))
        ),
    )
    .with_label(format!("{}/", label.trim_end_matches('/')))
}
