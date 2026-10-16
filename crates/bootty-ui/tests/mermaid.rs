use bootty_ui::presentation::mermaid::{DiagramColors, MAX_SOURCE_BYTES, render_mermaid};
use rstest::rstest;

fn colors() -> DiagramColors {
    DiagramColors {
        dark: true,
        font: "Inter".to_owned(),
        background: "#101010".to_owned(),
        node: "#202020".to_owned(),
        text: "#f0f0f0".to_owned(),
        line: "#808080".to_owned(),
        border: "#404040".to_owned(),
    }
}

/// A diagram draws as SVG with its labels and the theme's colors.
#[rstest]
fn diagrams_draw_with_their_labels_in_the_theme() {
    let svg = render_mermaid("flowchart LR\n  Plan --> Build", &colors()).unwrap();
    assert!(svg.starts_with("<svg") || svg.contains("<svg"), "{svg}");
    assert!(svg.contains("Plan") && svg.contains("Build"), "{svg}");
    assert!(svg.contains("#f0f0f0"), "the theme's text color: {svg}");
}

/// A diagram too large to draw, or one Mermaid can't read, says why.
#[rstest]
#[case::too_large("flowchart LR\n".to_owned() + &"  A --> B\n".repeat(MAX_SOURCE_BYTES / 8), "The diagram is larger than 16 KiB")]
#[case::unreadable("not a diagram at all".to_owned(), "Mermaid couldn't draw this diagram")]
fn diagrams_that_cant_draw_say_why(#[case] source: String, #[case] reason: &str) {
    let error = render_mermaid(&source, &colors()).unwrap_err();
    assert!(error.starts_with(reason), "{error}");
}
