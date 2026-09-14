//! Platform-aware rendering and parsing for Bootty's durable keybinding spelling.

use gpui_kit::component::{h_flex, kbd::Kbd};
use gpui_kit::{
    AnyElement, AsKeystroke as _, IntoElement, KeybindingKeystroke, Keystroke, ParentElement as _,
    Styled as _, div,
};

/// Recognize conflicting built-in scopes and predicate subsets with GPUI's own semantics.
/// Arbitrary intersecting predicates that are not subsets require a satisfiability solver.
#[must_use]
pub fn keybinding_contexts_overlap(left: &str, right: &str) -> bool {
    fn predicate(source: &str) -> Option<gpui_kit::KeyBindingContextPredicate> {
        let source = source.trim();
        let alias = match source.to_ascii_lowercase().as_str() {
            "" | "global" | "workspace" => return None,
            "terminal" => "Terminal",
            "sidebar" => "Sidebar",
            "command" => "Command",
            "herdr" => "Terminal && backend == herdr",
            "native" => "Terminal && backend == native",
            "rmux" => "Terminal && backend == rmux",
            "tmux" => "Terminal && backend == tmux",
            _ => source,
        };
        gpui_kit::KeyBindingContextPredicate::parse(alias).ok()
    }
    match (predicate(left), predicate(right)) {
        (Some(left), Some(right)) => left.is_superset(&right) || right.is_superset(&left),
        _ => true,
    }
}

/// Parse Bootty's `cmd+shift+p` (and multi-step `cmd+k > cmd+p`) spelling into GPUI strokes.
/// GPUI owns the platform-specific key syntax; [`Kbd`] owns its presentation.
#[must_use]
pub fn parse_keybinding(value: &str) -> Option<Vec<KeybindingKeystroke>> {
    let mut strokes = Vec::new();
    for step in split_keybinding_steps(value) {
        let key = Keystroke::parse(&gpui_keystroke_text(step)).ok()?;
        strokes.push(KeybindingKeystroke::from_keystroke(key));
    }
    (!strokes.is_empty()).then_some(strokes)
}

/// Split the editor's display chord separators without treating a literal `>` key after a
/// modifier as a separator. Durable bindings use whitespace between steps; `>` is display syntax.
fn split_keybinding_steps(value: &str) -> impl Iterator<Item = &str> {
    let mut steps = Vec::new();
    let mut start = 0;
    for (index, character) in value.char_indices() {
        if character == '>'
            && index > start
            && value
                .get(..index)
                .is_some_and(|prefix| !prefix.ends_with('+'))
        {
            if let Some(step) = value.get(start..index) {
                steps.push(step.trim());
            }
            start = index.saturating_add(character.len_utf8());
        }
    }
    if let Some(step) = value.get(start..) {
        steps.push(step.trim());
    }
    steps.into_iter().filter(|step| !step.is_empty())
}

/// Render a durable binding using modifier symbols on macOS and names elsewhere.
#[must_use]
pub fn keybinding_element_from_text(value: &str) -> AnyElement {
    let Some(keystrokes) = parse_keybinding(value) else {
        return div().into_any_element();
    };

    let stroke_count = keystrokes.len();
    h_flex()
        .gap_1()
        .children(
            keystrokes
                .into_iter()
                .enumerate()
                .flat_map(move |(index, keystroke)| {
                    let key = keybinding_element(keystroke.as_keystroke());
                    std::iter::once(key).chain(
                        (index.saturating_add(1) < stroke_count).then(|| ">".into_any_element()),
                    )
                }),
        )
        .into_any_element()
}

/// Render one already-parsed GPUI keystroke with the same platform treatment.
#[must_use]
pub fn keybinding_element(key: &Keystroke) -> AnyElement {
    raised_kbd(Kbd::new(key.clone())).into_any_element()
}

/// Keep keycap depth consistent without replacing Kbd's themed presentation.
pub(super) fn raised_kbd(key: Kbd) -> Kbd {
    key.outline().border_b_2().shadow_sm()
}

/// Translate Bootty's durable modifier and special-key aliases into GPUI syntax.
#[must_use]
pub fn gpui_keystroke_text(step: &str) -> String {
    let mut parts = step.split('+').collect::<Vec<_>>();
    let Some(mut key) = parts.pop() else {
        return step.to_owned();
    };
    if step == "+" || step.ends_with("++") {
        parts.pop();
        key = "+";
    }
    let key = match key {
        "ArrowUp" | "arrow_up" => "up",
        "ArrowDown" | "arrow_down" => "down",
        "ArrowLeft" | "arrow_left" => "left",
        "ArrowRight" | "arrow_right" => "right",
        "PageUp" => "pageup",
        "PageDown" => "pagedown",
        "Period" | "period" => ".",
        "Backspace" => "backspace",
        "Delete" => "delete",
        "Escape" => "escape",
        "Enter" => "enter",
        "Tab" => "tab",
        "Space" => "space",
        key => key,
    };
    let modifiers = parts.into_iter().map(|modifier| match modifier {
        "left_cmd" | "right_cmd" => "cmd",
        "left_ctrl" | "right_ctrl" => "ctrl",
        "left_alt" | "right_alt" => "alt",
        "left_shift" | "right_shift" => "shift",
        modifier => modifier,
    });
    modifiers.chain([key]).collect::<Vec<_>>().join("-")
}
