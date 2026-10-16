//! Mermaid diagrams drawn as SVG. Drawing is CPU work without cancellation: call it off the UI
//! thread.
use std::sync::Mutex;

/// The largest diagram source drawn; larger ones show as their source.
// Diagrams agents write are a few hundred bytes; raise this if real ones are cut off.
pub const MAX_SOURCE_BYTES: usize = 16 * 1024;

/// One diagram draws at a time, so a burst of replies can't take every core.
static DRAWING: Mutex<()> = Mutex::new(());

/// The theme's colors for a diagram, as `#rrggbb`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DiagramColors {
    pub dark: bool,
    pub font: String,
    pub background: String,
    pub node: String,
    pub text: String,
    pub line: String,
    pub border: String,
}

/// The SVG of the Mermaid diagram `source`, in `colors`.
/// # Errors
/// Returns a source past the size limit, or one Mermaid can't draw, with its reason.
pub fn render_mermaid(source: &str, colors: &DiagramColors) -> Result<String, String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err("The diagram is larger than 16 KiB".to_owned());
    }
    let _drawing = DRAWING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut options = mermaid_rs_renderer::RenderOptions::default();
    let mut theme = if colors.dark {
        mermaid_rs_renderer::Theme::dark()
    } else {
        mermaid_rs_renderer::Theme::modern()
    };
    theme.font_family.clone_from(&colors.font);
    theme.background.clone_from(&colors.background);
    theme.primary_color.clone_from(&colors.node);
    theme.secondary_color.clone_from(&colors.node);
    theme.primary_text_color.clone_from(&colors.text);
    theme.text_color.clone_from(&colors.text);
    theme.primary_border_color.clone_from(&colors.border);
    theme.line_color.clone_from(&colors.line);
    theme.edge_label_background.clone_from(&colors.background);
    options.theme = theme;
    // The engine panics on some malformed input; that is a diagram that can't draw.
    std::panic::catch_unwind(|| mermaid_rs_renderer::render_with_options(source, options))
        .map_err(|_| "Mermaid couldn't draw this diagram".to_owned())?
        .map_err(|error| format!("Mermaid couldn't draw this diagram: {error}"))
}
