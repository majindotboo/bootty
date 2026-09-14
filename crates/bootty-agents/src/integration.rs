//! Static vendor adapters and the small, transactional file surface used to install them.
//!
//! The service never evaluates user supplied source.  It only writes the bundled Pi extension or
//! shell hook and merges the provider's declared JSON entries additively.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use serde_json::{Map, Value, json};

use crate::provider::AgentKind;

const ENTRY_LIMIT: usize = 16;
const FILE_SIZE_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentIntegration {
    pub provider: AgentKind,
    pub declaration: IntegrationDeclaration,
}

impl AgentIntegration {
    #[must_use]
    pub fn for_provider(provider: AgentKind, integration_dir: &Path) -> Self {
        Self {
            provider,
            declaration: declaration(provider, integration_dir),
        }
    }

    #[must_use]
    pub const fn declaration(&self) -> &IntegrationDeclaration {
        &self.declaration
    }
}

#[must_use]
pub fn agent_integrations(integration_dir: &Path) -> Vec<AgentIntegration> {
    AgentKind::ALL
        .into_iter()
        .map(|provider| AgentIntegration::for_provider(provider, integration_dir))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationDeclaration {
    pub module: String,
    pub id: String,
    pub title: String,
    pub summary: String,
    pub files: Vec<IntegrationFile>,
    pub merge: Vec<IntegrationMerge>,
    pub place: Vec<IntegrationPlacement>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationFile {
    pub path: String,
    pub contents: String,
    pub executable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationPlacement {
    pub path: String,
    pub file: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationMerge {
    pub path: String,
    pub value: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntegrationStatus {
    Missing,
    Partial,
    Installed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationState {
    pub declaration: IntegrationDeclaration,
    pub status: IntegrationStatus,
}

#[must_use]
pub fn integration_declaration(
    provider: AgentKind,
    integration_dir: &Path,
) -> IntegrationDeclaration {
    declaration(provider, integration_dir)
}

fn declaration(provider: AgentKind, integration_dir: &Path) -> IntegrationDeclaration {
    // `integration_dir` is the owner directory itself. The declaration id is metadata only;
    // retaining the vendor subpaths here keeps the existing installed paths byte-for-byte stable.
    let path = integration_dir.to_owned();
    match provider {
        AgentKind::Pi => IntegrationDeclaration {
            module: provider.module().to_owned(),
            id: provider.integration_id().to_owned(),
            title: "Pi extension".to_owned(),
            summary: "Reports Pi activity to Bootty.".to_owned(),
            files: vec![IntegrationFile {
                path: "pi/bootty.ts".to_owned(),
                contents: include_str!("assets/pi-bootty.ts").to_owned(),
                executable: false,
            }],
            merge: Vec::new(),
            place: vec![IntegrationPlacement {
                path: "~/.pi/agent/extensions/bootty.ts".to_owned(),
                file: "pi/bootty.ts".to_owned(),
            }],
        },
        AgentKind::Codex => IntegrationDeclaration {
            module: provider.module().to_owned(),
            id: provider.integration_id().to_owned(),
            title: "Codex hooks".to_owned(),
            summary: "Reports Codex activity to Bootty.".to_owned(),
            files: vec![hook_file(HookProvider::Codex)],
            merge: vec![IntegrationMerge {
                path: "~/.codex/hooks.json".to_owned(),
                value: hook_config(HookProvider::Codex, &path),
            }],
            place: Vec::new(),
        },
        AgentKind::Claude => IntegrationDeclaration {
            module: provider.module().to_owned(),
            id: provider.integration_id().to_owned(),
            title: "Claude Code hooks".to_owned(),
            summary: "Reports Claude Code activity to Bootty.".to_owned(),
            files: vec![hook_file(HookProvider::Claude)],
            merge: vec![IntegrationMerge {
                path: "~/.claude/settings.json".to_owned(),
                value: hook_config(HookProvider::Claude, &path),
            }],
            place: Vec::new(),
        },
    }
}

#[derive(Clone, Copy)]
enum HookProvider {
    Codex,
    Claude,
}

fn hook_file(provider: HookProvider) -> IntegrationFile {
    let (path, contents) = match provider {
        HookProvider::Codex => (
            "codex/bootty-hook.sh",
            include_str!("assets/codex-bootty-hook.sh"),
        ),
        HookProvider::Claude => (
            "claude/bootty-hook.sh",
            include_str!("assets/claude-bootty-hook.sh"),
        ),
    };
    IntegrationFile {
        path: path.to_owned(),
        contents: contents.to_owned(),
        executable: true,
    }
}

fn hook_config(provider: HookProvider, integration_dir: &Path) -> Value {
    let script = integration_dir
        .join(match provider {
            HookProvider::Codex => "codex/bootty-hook.sh",
            HookProvider::Claude => "claude/bootty-hook.sh",
        })
        .display()
        .to_string();
    // Preserve existing hook entries for ordinary paths; quote paths the hook shell would split
    // or expand, including macOS application-support directories and apostrophes in home names.
    let script = if script
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"/_-.".contains(&byte))
    {
        script
    } else {
        crate::launch::quote_posix(&script)
    };
    let hook =
        |timeout| json!([{ "type": "command", "command": script.clone(), "timeout": timeout }]);
    let hooks = match provider {
        HookProvider::Codex => json!({
            "SessionStart": [{ "matcher": "startup|resume|clear|compact", "hooks": hook(2) }],
            "UserPromptSubmit": [{ "hooks": hook(2) }],
            "PreToolUse": [{ "matcher": ".*", "hooks": hook(2) }],
            "PermissionRequest": [{ "matcher": ".*", "hooks": hook(2) }],
            "PostToolUse": [{ "matcher": ".*", "hooks": hook(2) }],
            "Interrupt": [{ "hooks": hook(1) }],
            "Stop": [{ "hooks": hook(2) }],
            "SessionEnd": [{ "hooks": hook(1) }],
        }),
        HookProvider::Claude => json!({
            "SessionStart": [{ "matcher": "startup|resume|clear|compact", "hooks": hook(2) }],
            "UserPromptSubmit": [{ "hooks": hook(2) }],
            "PreToolUse": [{ "matcher": "*", "hooks": hook(2) }],
            "PostToolUse": [{ "matcher": "*", "hooks": hook(2) }],
            "Notification": [{ "hooks": hook(2) }],
            "Stop": [{ "hooks": hook(2) }],
            "SessionEnd": [{ "hooks": hook(1) }],
        }),
    };
    json!({"hooks": hooks})
}

#[must_use]
pub fn integration_status(
    integration_dir: &Path,
    home: Option<&Path>,
    declaration: &IntegrationDeclaration,
) -> IntegrationStatus {
    let total = declaration
        .files
        .len()
        .saturating_add(declaration.merge.len())
        .saturating_add(declaration.place.len());
    let mut applied = 0_usize;
    for file in &declaration.files {
        if file_installed(integration_dir, file) {
            applied = applied.saturating_add(1);
        }
    }
    for entry in &declaration.merge {
        if resolve_path(home, &entry.path)
            .ok()
            .and_then(|path| read_json(&path).ok())
            .is_some_and(|existing| contains(&existing, &entry.value))
        {
            applied = applied.saturating_add(1);
        }
    }
    for placement in &declaration.place {
        if placed_file(home, declaration, placement).is_some_and(|(path, contents)| {
            fs::read_to_string(path).is_ok_and(|existing| existing == contents)
        }) {
            applied = applied.saturating_add(1);
        }
    }
    if applied == total {
        IntegrationStatus::Installed
    } else if applied == 0 {
        IntegrationStatus::Missing
    } else {
        IntegrationStatus::Partial
    }
}

/// Install a declaration after validating all merge targets. JSON files are committed atomically;
/// a conflicting scalar or malformed target rejects the operation before adapter files are written.
/// # Errors
/// Returns an error for an invalid declaration, unsafe path, conflicting merge, or a failed filesystem operation.
pub fn install_integration(
    integration_dir: &Path,
    home: Option<&Path>,
    declaration: &IntegrationDeclaration,
) -> Result<(), String> {
    validate_declaration(declaration)?;
    if declaration.files.len() > ENTRY_LIMIT
        || declaration.merge.len() > ENTRY_LIMIT
        || declaration.place.len() > ENTRY_LIMIT
    {
        return Err(format!(
            "integration entry count exceeds the limit of {ENTRY_LIMIT}"
        ));
    }
    let mut merged = Vec::with_capacity(declaration.merge.len());
    for entry in &declaration.merge {
        let path = resolve_path(home, &entry.path)?;
        merged_json(&path, &entry.value)?;
        merged.push((path, &entry.value));
    }
    // Resolve every placement before creating or replacing any adapter file. An unresolved home
    // path must leave the integration directory untouched rather than creating a partial install.
    for placement in &declaration.place {
        if placed_file(home, declaration, placement).is_none() {
            return Err(format!(
                "integration placement `{}` is unresolved",
                placement.path
            ));
        }
    }
    fs::create_dir_all(integration_dir)
        .map_err(|error| format!("{}: {error}", integration_dir.display()))?;
    for file in &declaration.files {
        let relative = relative_path(&file.path)?;
        if let Some(parent) = relative.parent() {
            fs::create_dir_all(integration_dir.join(parent))
                .map_err(|error| format!("{}: {error}", integration_dir.display()))?;
        }
        let path = safe_file_path(integration_dir, &file.path)?;
        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent", path.display()))?;
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        write_bytes(&path, file.contents.as_bytes())?;
        if file.executable {
            set_executable(&path)?;
        }
    }
    for placement in &declaration.place {
        let (path, contents) = placed_file(home, declaration, placement)
            .ok_or_else(|| format!("integration placement `{}` is unresolved", placement.path))?;
        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent", path.display()))?;
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        write_bytes(&path, contents.as_bytes())?;
    }
    for (path, addition) in merged {
        let target = lock_target(&path)?;
        // Preflight does not own a revision. Merge the latest bytes under the writer's lease.
        let value = merged_json(target.path(), addition)?;
        write_json(&target, &value)?;
        drop(target);
    }
    Ok(())
}

/// Uninstall only exact files and exact JSON values this declaration owns. User edits survive.
/// # Errors
/// Returns an error for an invalid declaration, unsafe path, or a failed read, removal, or atomic JSON update.
pub fn uninstall_integration(
    integration_dir: &Path,
    home: Option<&Path>,
    declaration: &IntegrationDeclaration,
) -> Result<(), String> {
    validate_declaration(declaration)?;
    for file in &declaration.files {
        let path = safe_file_path(integration_dir, &file.path)?;
        if fs::read_to_string(&path).is_ok_and(|existing| existing == file.contents) {
            remove_if_present(&path)?;
        }
    }
    for placement in &declaration.place {
        let Some((path, contents)) = placed_file(home, declaration, placement) else {
            continue;
        };
        if fs::read_to_string(&path).is_ok_and(|existing| existing == contents) {
            remove_if_present(&path)?;
        }
    }
    for entry in &declaration.merge {
        let path = resolve_path(home, &entry.path)?;
        if !path.exists() {
            continue;
        }
        let target = lock_target(&path)?;
        let mut value = read_json(target.path())?;
        if unmerge_value(&mut value, &entry.value) {
            write_json(&target, &value)?;
        }
        drop(target);
    }
    Ok(())
}

fn validate_declaration(declaration: &IntegrationDeclaration) -> Result<(), String> {
    if declaration.id.is_empty()
        || !declaration
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "an integration id must be non-empty and use only letters, digits, - or _".to_owned(),
        );
    }
    for file in &declaration.files {
        relative_path(&file.path)?;
        if file.contents.len() > FILE_SIZE_LIMIT {
            return Err(format!(
                "integration file `{}` exceeds the limit of {FILE_SIZE_LIMIT} bytes",
                file.path
            ));
        }
    }
    for placement in &declaration.place {
        if !declaration
            .files
            .iter()
            .any(|file| file.path == placement.file)
        {
            return Err(format!(
                "integration placement names `{}`, which this integration does not declare",
                placement.file
            ));
        }
    }
    Ok(())
}

fn placed_file<'a>(
    home: Option<&Path>,
    declaration: &'a IntegrationDeclaration,
    placement: &IntegrationPlacement,
) -> Option<(PathBuf, &'a str)> {
    let path = resolve_path(home, &placement.path).ok()?;
    let contents = declaration
        .files
        .iter()
        .find(|file| file.path == placement.file)
        .map(|file| file.contents.as_str())?;
    Some((path, contents))
}

fn file_installed(dir: &Path, file: &IntegrationFile) -> bool {
    safe_file_path(dir, &file.path)
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .is_some_and(|contents| contents == file.contents)
        && (!file.executable
            || safe_file_path(dir, &file.path).is_ok_and(|path| is_executable(&path)))
}

fn relative_path(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "integration file path `{value}` must stay inside the integration directory"
        ));
    }
    Ok(path.to_owned())
}

fn safe_file_path(dir: &Path, value: &str) -> Result<PathBuf, String> {
    let relative = relative_path(value)?;
    if !dir.exists() {
        return Ok(dir.join(relative));
    }
    let canonical_dir =
        fs::canonicalize(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let requested = dir.join(relative);
    // `WriteTarget` requires an existing parent. Find the deepest existing parent first so
    // uninstalling a partial install remains a no-op while existing symlink parents are checked.
    let mut existing_parent = requested.parent();
    while let Some(parent) = existing_parent {
        if parent.exists() {
            break;
        }
        existing_parent = parent.parent();
    }
    let Some(existing_parent) = existing_parent else {
        return Ok(requested);
    };
    let canonical_parent = fs::canonicalize(existing_parent)
        .map_err(|error| format!("{}: {error}", existing_parent.display()))?;
    if !canonical_parent.starts_with(&canonical_dir) {
        return Err(format!(
            "integration file `{value}` escapes the integration directory"
        ));
    }
    if fs::symlink_metadata(&requested).is_err() {
        return Ok(requested);
    }
    let resolved = bootty_write::WriteTarget::resolve(&requested)
        .map_err(|error| format!("{}: {error:?}", requested.display()))?
        .path()
        .to_owned();
    if !resolved.starts_with(&canonical_dir) {
        return Err(format!(
            "integration file `{value}` escapes the integration directory"
        ));
    }
    Ok(resolved)
}

fn resolve_path(home: Option<&Path>, value: &str) -> Result<PathBuf, String> {
    let path = match value.strip_prefix("~/") {
        Some(rest) => home
            .ok_or_else(|| format!("integration path `{value}` needs a home directory"))?
            .join(rest),
        None => PathBuf::from(value),
    };
    if !path.is_absolute() {
        return Err(format!(
            "integration path `{value}` must be absolute or start with `~/`"
        ));
    }
    Ok(path)
}

fn read_json(path: &Path) -> Result<Value, String> {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Value::Object(Map::new())),
        Ok(text) => serde_json::from_str(&text)
            .map_err(|error| format!("{} is not valid JSON: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn merged_json(path: &Path, addition: &Value) -> Result<Value, String> {
    let mut value = read_json(path)?;
    merge_value(&mut value, addition);
    if !contains(&value, addition) {
        return Err(format!(
            "{} already has a different value where this integration writes one",
            path.display()
        ));
    }
    Ok(value)
}

fn write_json(target: &bootty_write::LockedWriteTarget, value: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    replace_bytes(target, &bytes)
}

fn lock_target(path: &Path) -> Result<bootty_write::LockedWriteTarget, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    bootty_write::WriteTarget::resolve(path)
        .map_err(|error| format!("{}: {error:?}", path.display()))?
        .lock()
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    replace_bytes(&lock_target(path)?, bytes)
}

fn replace_bytes(target: &bootty_write::LockedWriteTarget, bytes: &[u8]) -> Result<(), String> {
    target
        .replace(bytes, bootty_write::NewFileMode::UmaskWritable)
        .map(|_| ())
        .map_err(|error| format!("{}: {}", target.path().display(), error.into_io()))
}

fn remove_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn merge_value(target: &mut Value, addition: &Value) {
    match (target, addition) {
        (Value::Object(target), Value::Object(addition)) => {
            for (key, value) in addition {
                match target.get_mut(key) {
                    Some(existing) => merge_value(existing, value),
                    None => {
                        target.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (Value::Array(target), Value::Array(addition)) => {
            for value in addition {
                if !target.contains(value) {
                    target.push(value.clone());
                }
            }
        }
        _ => {}
    }
}

fn contains(target: &Value, addition: &Value) -> bool {
    match (target, addition) {
        (Value::Object(target), Value::Object(addition)) => addition
            .iter()
            .all(|(key, value)| target.get(key).is_some_and(|held| contains(held, value))),
        (Value::Array(target), Value::Array(addition)) => {
            addition.iter().all(|value| target.contains(value))
        }
        _ => target == addition,
    }
}

fn unmerge_value(target: &mut Value, addition: &Value) -> bool {
    match (target, addition) {
        (Value::Object(target), Value::Object(addition)) => {
            let mut changed = false;
            for (key, value) in addition {
                let Some(held) = target.get_mut(key) else {
                    continue;
                };
                if held == value {
                    target.remove(key);
                    changed = true;
                    continue;
                }
                changed |= unmerge_value(held, value);
                if matches!(target.get(key), Some(Value::Object(held)) if held.is_empty())
                    || matches!(target.get(key), Some(Value::Array(held)) if held.is_empty())
                {
                    target.remove(key);
                }
            }
            changed
        }
        (Value::Array(target), Value::Array(addition)) => {
            let before = target.len();
            target.retain(|value| !addition.contains(value));
            before != target.len()
        }
        _ => false,
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(path, permissions).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}
