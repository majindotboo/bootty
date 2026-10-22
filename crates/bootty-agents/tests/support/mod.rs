//! What a native agent catalog holds on disk, as one value, and making its writes fail.
#![allow(dead_code, reason = "each test target uses some of these helpers")]
use std::{error::Error, fs, path::Path};

use serde_json::Value;

/// Host-absolute metadata for fixtures that never create their captured project or account.
pub fn synthetic_path(relative: &str) -> std::path::PathBuf {
    std::env::temp_dir()
        .join("bootty-agent-fixtures")
        .join(relative)
}

/// The catalog at `path` with its conversations inline under `records`, the way older catalogs
/// kept them. Writing this back is a catalog the service opens.
pub fn stored_catalog(path: &Path) -> Result<Value, Box<dyn Error>> {
    let mut catalog: Value = serde_json::from_slice(&fs::read(path)?)?;
    if catalog.get("records").is_none() {
        let ids = catalog
            .get("record_ids")
            .and_then(Value::as_array)
            .ok_or("the index names no conversations")?;
        let records = ids
            .iter()
            .map(|id| -> Result<Value, Box<dyn Error>> {
                let id = id.as_str().ok_or("a conversation id isn't text")?;
                Ok(serde_json::from_slice(&fs::read(conversation_file(
                    path, id,
                ))?)?)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(catalog) = catalog.as_object_mut() {
            catalog.insert("records".to_owned(), Value::Array(records));
        }
    }
    Ok(catalog)
}

/// Where the catalog at `path` keeps the conversation `id`.
pub fn conversation_file(path: &Path, id: &str) -> std::path::PathBuf {
    let name = id
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02x}")
            }
        })
        .collect::<String>();
    path.with_extension("d").join(format!("{name}.json"))
}

/// Makes every write to the catalog at `path` fail, deterministically and without changing
/// permissions: its index and its conversations' directory become a directory and a file.
pub fn break_catalog(path: &Path) -> std::io::Result<()> {
    let conversations = path.with_extension("d");
    if conversations.is_dir() {
        fs::remove_dir_all(&conversations)?;
    }
    fs::write(&conversations, "")?;
    fs::remove_file(path)?;
    fs::create_dir(path)
}

/// Undoes `break_catalog`, so the catalog at `path` takes writes again.
pub fn repair_catalog(path: &Path) -> std::io::Result<()> {
    fs::remove_file(path.with_extension("d"))?;
    fs::remove_dir(path)
}
