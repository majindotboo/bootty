//! Application seam for the dedicated GPUI keymap editor.

use bootty_host::text_file::{LoadedTextFile, load_text_file};

use std::str::FromStr as _;

use crate::gpui::{
    KeymapActionSnapshot, KeymapArgumentKind, KeymapArgumentSnapshot, KeymapBindingDraft,
    KeymapBindingSnapshot, KeymapBindingSource, KeymapBindingTarget, KeymapContextSnapshot,
    KeymapEditorIntent, KeymapEditorSnapshot, KeymapTriggerOptions,
};
use anyhow::{Context as _, Result};
use bootty_config::keymap_file::{
    KeymapAction, KeymapBindingKind, KeymapBindingSource as PersistedSource,
    KeymapBindingTarget as PersistedTarget, KeymapContext, KeymapEdit,
};
use bootty_control::{ArgumentSchema, CommandDescriptor, ValueType};
use gpui_kit::KeyBindingContextPredicate;

use crate::{keymap_runtime::KeymapBindingSnapshot as RuntimeBinding, state::AppState};

const CONSUME_ACTION_ID: &str = "ignore";

pub fn editor_snapshot(state: &AppState) -> KeymapEditorSnapshot {
    let runtime = state.keymap_snapshot();
    let prefix = state.config().input.effective_prefix();
    let mut actions = state
        .command_catalog()
        .list()
        .into_iter()
        .map(action_snapshot)
        .collect::<Vec<_>>();
    actions.sort_by(|left, right| {
        left.title
            .to_ascii_lowercase()
            .cmp(&right.title.to_ascii_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut bindings = runtime
        .effective_bindings
        .iter()
        .enumerate()
        .filter_map(|(index, binding)| {
            if binding_is_shadowed(index, &runtime.effective_bindings) {
                return None;
            }
            Some(binding_snapshot(index, binding, prefix.as_deref()))
        })
        .collect::<Vec<_>>();
    let conflict_counts = bindings
        .iter()
        .enumerate()
        .map(|(index, binding)| {
            bindings
                .iter()
                .enumerate()
                .filter(|(other, candidate)| {
                    *other != index && bindings_conflict(binding, candidate)
                })
                .count()
        })
        .collect::<Vec<_>>();
    for (binding, conflict_count) in bindings.iter_mut().zip(conflict_counts) {
        binding.conflict_count = conflict_count;
    }

    KeymapEditorSnapshot {
        path: runtime.path.display().to_string(),
        prefix,
        actions,
        bindings,
        contexts: KeymapContext::ALL
            .iter()
            .map(|context| context_snapshot(context, &runtime.keymap))
            .collect(),
        diagnostic: runtime.diagnostic_summary(),
        revision: runtime.revision,
    }
}

fn binding_is_shadowed(index: usize, bindings: &[RuntimeBinding]) -> bool {
    let Some(binding) = bindings.get(index) else {
        return false;
    };
    let Ok((keystrokes, _)) = crate::keymap_runtime::parse_sequence_with_flags(&binding.keystrokes)
    else {
        return false;
    };
    bindings.iter().skip(index.saturating_add(1)).any(|later| {
        let Ok((later_keystrokes, _)) =
            crate::keymap_runtime::parse_sequence_with_flags(&later.keystrokes)
        else {
            return false;
        };
        if later.context != binding.context || later_keystrokes != keystrokes {
            return false;
        }
        match (binding.kind, later.kind) {
            (KeymapBindingKind::Binding, KeymapBindingKind::Binding) => true,
            (_, KeymapBindingKind::Unbind) | (KeymapBindingKind::Unbind, _) => {
                later.action == binding.action
            }
        }
    })
}

/// Project the keymap path owned by [`AppState`] into the reusable text editor.
///
/// # Errors
/// Returns the file read or UTF-8 decoding error.
pub fn load_keymap_text_file(state: &AppState) -> Result<LoadedTextFile> {
    load_text_file(state.keymap_snapshot().path)
}

/// Republish a keymap revision after the reusable text editor persisted it.
///
/// # Errors
/// Returns the keymap reload error.
pub fn reload_saved_keymap_text(state: &mut AppState) -> Result<()> {
    state.reload_keymap()
}

pub fn persisted_edit(intent: &KeymapEditorIntent) -> Result<Option<KeymapEdit>> {
    match intent {
        KeymapEditorIntent::Add { binding } => Ok(Some(KeymapEdit::add(persisted_draft(binding)?))),
        KeymapEditorIntent::Replace {
            target,
            replacement,
        } => Ok(Some(KeymapEdit::replace(
            persisted_target(target)?,
            persisted_draft(replacement)?,
            persisted_source(target.source),
        ))),
        KeymapEditorIntent::Remove { target } => Ok(Some(KeymapEdit::remove(
            persisted_target(target)?,
            persisted_source(target.source),
        ))),
        KeymapEditorIntent::SetBuiltInDefaults { context, enabled } => Ok(Some(
            KeymapEdit::set_builtin_defaults(KeymapContext::from_str(context)?, *enabled),
        )),
        KeymapEditorIntent::OpenKeymapFile | KeymapEditorIntent::Close => Ok(None),
    }
}

fn action_snapshot(descriptor: CommandDescriptor) -> KeymapActionSnapshot {
    KeymapActionSnapshot {
        id: descriptor.id,
        title: descriptor.title,
        description: descriptor.description,
        arguments: descriptor
            .arguments
            .arguments
            .into_iter()
            .map(argument_snapshot)
            .collect(),
    }
}

fn argument_snapshot(argument: ArgumentSchema) -> KeymapArgumentSnapshot {
    KeymapArgumentSnapshot {
        name: argument.name,
        kind: match argument.value_type {
            ValueType::String => KeymapArgumentKind::String,
            ValueType::Integer => KeymapArgumentKind::Integer,
            ValueType::Number => KeymapArgumentKind::Number,
        },
        required: argument.required,
        choices: argument.choices,
        minimum: argument.minimum,
        maximum: argument.maximum,
    }
}

fn binding_snapshot(
    index: usize,
    binding: &RuntimeBinding,
    prefix: Option<&str>,
) -> KeymapBindingSnapshot {
    let (action, arguments_json) = match &binding.action {
        KeymapAction::Command { name, input } => {
            (name.clone(), input.as_ref().map(ToString::to_string))
        }
        // Null bindings are the durable representation of a consuming binding. Keep them in the
        // editor as the named `ignore` action so the row can be removed and restored later.
        KeymapAction::None => (CONSUME_ACTION_ID.to_owned(), None),
    };
    let (mut trigger_options, keystrokes) = split_trigger_options(&binding.keystrokes);
    trigger_options.side_sensitive = keystrokes
        .split_ascii_whitespace()
        .any(step_has_modifier_side);
    trigger_options.prefixed =
        prefix.is_some_and(|prefix| keystrokes.split_ascii_whitespace().next() == Some(prefix));
    KeymapBindingSnapshot {
        id: format!(
            "{}:{}:{}:{index}",
            source_label(binding.source),
            binding.context,
            binding.keystrokes
        ),
        action,
        arguments_json,
        persisted_keystrokes: binding.keystrokes.clone(),
        keystrokes: keystrokes
            .split_ascii_whitespace()
            .map(str::to_owned)
            .collect(),
        context: binding.context.to_string(),
        source: match binding.source {
            PersistedSource::BuiltIn => KeymapBindingSource::Default,
            PersistedSource::User => KeymapBindingSource::User,
        },
        kind: binding.kind,
        trigger_options,
        conflict_count: 0,
    }
}

fn context_snapshot(
    context: &KeymapContext,
    keymap: &bootty_config::keymap_file::KeymapFile,
) -> KeymapContextSnapshot {
    let description = match context {
        KeymapContext::Global => "Every Bootty surface.",
        KeymapContext::Sidebar => "The focused sidebar.",
        KeymapContext::Command => "The active command palette or picker.",
        KeymapContext::Terminal => "The focused terminal, regardless of backend.",
        KeymapContext::Herdr => "A focused Herdr terminal.",
        KeymapContext::Native => "A focused native terminal.",
        KeymapContext::Rmux => "A focused rmux terminal.",
        KeymapContext::Tmux => "A focused tmux terminal.",
        KeymapContext::Expression(_) => "A custom GPUI key-context expression.",
    };
    KeymapContextSnapshot {
        id: context.to_string(),
        label: context.to_string(),
        description: description.to_owned(),
        use_builtin_defaults: keymap.use_builtin_defaults(context),
    }
}

fn persisted_draft(draft: &KeymapBindingDraft) -> Result<PersistedTarget> {
    Ok(PersistedTarget::binding(
        persisted_context(&draft.context)?,
        join_trigger_options(draft.trigger_options, &draft.keystrokes.join(" ")),
        persisted_action(&draft.action, draft.arguments_json.as_deref())?,
    ))
}

fn persisted_target(target: &KeymapBindingTarget) -> Result<PersistedTarget> {
    Ok(PersistedTarget {
        context: persisted_context(&target.context)?,
        keystrokes: target.persisted_keystrokes.clone(),
        action: persisted_action(&target.action, target.arguments_json.as_deref())?,
        kind: target.kind,
    })
}

fn persisted_context(source: &str) -> Result<KeymapContext> {
    let source = source.trim();
    let source = if source.is_empty() { "Global" } else { source };
    let context = KeymapContext::from_str(source)?;
    if matches!(context, KeymapContext::Expression(_)) {
        KeyBindingContextPredicate::parse(source)
            .map_err(|error| anyhow::anyhow!("invalid keybinding context {source:?}: {error}"))?;
    }
    Ok(context)
}

fn persisted_action(name: &str, arguments_json: Option<&str>) -> Result<KeymapAction> {
    if name == CONSUME_ACTION_ID && arguments_json.map(str::trim).is_none_or(str::is_empty) {
        return Ok(KeymapAction::None);
    }
    match arguments_json
        .map(str::trim)
        .filter(|arguments| !arguments.is_empty())
    {
        Some(arguments) => Ok(KeymapAction::command_with_input(
            name,
            serde_json::from_str(arguments).context("parse keybinding action arguments as JSON")?,
        )),
        None => Ok(KeymapAction::command(name)),
    }
}

const fn persisted_source(source: KeymapBindingSource) -> PersistedSource {
    match source {
        KeymapBindingSource::User => PersistedSource::User,
        KeymapBindingSource::Default => PersistedSource::BuiltIn,
    }
}

const fn source_label(source: PersistedSource) -> &'static str {
    match source {
        PersistedSource::BuiltIn => "built-in",
        PersistedSource::User => "user",
    }
}

fn bindings_conflict(left: &KeymapBindingSnapshot, right: &KeymapBindingSnapshot) -> bool {
    if left.kind == KeymapBindingKind::Unbind || right.kind == KeymapBindingKind::Unbind {
        return false;
    }
    let parsed = |binding: &KeymapBindingSnapshot| {
        crate::keymap_runtime::parse_sequence_with_flags(&binding.persisted_keystrokes)
            .ok()
            .map(|(sequence, _)| sequence)
    };
    match (parsed(left), parsed(right)) {
        (Some(left_keys), Some(right_keys)) => {
            left_keys == right_keys
                && crate::gpui::keybinding_contexts_overlap(&left.context, &right.context)
        }
        _ => false,
    }
}

fn split_trigger_options(source: &str) -> (KeymapTriggerOptions, &str) {
    let mut options = KeymapTriggerOptions::default();
    let mut rest = source;
    while let Some((prefix, tail)) = rest.split_once(':') {
        let Some(value) = (match prefix {
            "performable" => Some(&mut options.performable),
            "global" => Some(&mut options.global),
            "all" => Some(&mut options.all),
            "unconsumed" => Some(&mut options.unconsumed),
            _ => None,
        }) else {
            break;
        };
        if *value {
            break;
        }
        *value = true;
        rest = tail;
    }
    (options, rest)
}

fn join_trigger_options(options: KeymapTriggerOptions, keystrokes: &str) -> String {
    let mut result = String::new();
    for (enabled, name) in [
        (options.performable, "performable"),
        (options.global, "global"),
        (options.all, "all"),
        (options.unconsumed, "unconsumed"),
    ] {
        if enabled {
            result.push_str(name);
            result.push(':');
        }
    }
    result.push_str(keystrokes);
    result
}

fn step_has_modifier_side(step: &str) -> bool {
    step.split(['+', '-'])
        .any(|part| part.starts_with("left_") || part.starts_with("right_"))
}
