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

pub fn render(
    context: &InlineTokenContext,
    _: &mut gpui_kit::Window,
    cx: &mut App,
) -> gpui_kit::AnyElement {
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
        .child(if name.to_ascii_lowercase().ends_with(".md") {
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child("M↓")
                .into_any_element()
        } else {
            crate::gpui::sized_icon(
                if context.token().id().starts_with("application:") {
                    "app-window"
                } else if context.token().id().starts_with("skill:") {
                    "sparkles"
                } else {
                    icon(name)
                },
                crate::gpui::IconSize::XSmall,
                cx.theme().muted_foreground,
            )
        })
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
