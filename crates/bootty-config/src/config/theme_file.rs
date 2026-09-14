use std::{
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use bootty_write::{NewFileMode, WriteTarget};
use num_traits::ToPrimitive as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{ColorConfig, ResolvedTheme, ThemeInfo, parse_theme_source, resolve_theme};
use crate::color::Color;

const MAX_THEME_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ThemeFile {
    pub name: String,
    pub source: String,
    /// Built-in themes have no writable revision; save a copy to author one.
    pub revision: Option<String>,
}

/// Serialize a resolved theme to editable TOML.
///
/// # Errors
/// Returns the TOML serialization error when the theme cannot be encoded.
pub fn encode_theme(theme: &ResolvedTheme) -> Result<String, String> {
    #[derive(Serialize)]
    struct Document<'a> {
        metadata: &'a ThemeInfo,
        colors: &'a ColorConfig,
    }
    toml_edit::ser::to_string_pretty(&Document {
        metadata: &theme.info,
        colors: &theme.colors,
    })
    .map_err(|error| error.to_string())
}

fn revision(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        })
}

fn theme_path(config_dir: &Path, name: &str) -> Result<PathBuf, String> {
    if name.trim().is_empty()
        || name.len() > 128
        || name.contains(['/', '\\', ':'])
        || name.ends_with([' ', '.'])
        || name.chars().any(char::is_control)
    {
        return Err("Theme name must be one filename, at most 128 bytes".to_owned());
    }
    let legacy = config_dir.join("themes").join(name);
    Ok(if legacy.exists() {
        legacy
    } else {
        config_dir.join("themes").join(format!("{name}.toml"))
    })
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    if !fs::metadata(path)
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("Theme source must be a regular file".to_owned());
    }
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_THEME_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_THEME_BYTES {
        return Err("Theme file exceeds 64 KiB".to_owned());
    }
    Ok(bytes)
}

/// Read a bounded user theme or an editable copy of a built-in theme.
///
/// # Errors
/// Rejects invalid names, unreadable or oversized files, and invalid theme data.
pub fn read_theme(config_dir: &Path, name: &str) -> Result<ThemeFile, String> {
    let path = theme_path(config_dir, name)?;
    if path.exists() {
        let bytes = read_bounded(&path)?;
        let source = String::from_utf8(bytes.clone()).map_err(|error| error.to_string())?;
        parse_theme_source(&source, name).map_err(|error| error.to_string())?;
        return Ok(ThemeFile {
            name: name.to_owned(),
            source,
            revision: Some(revision(&bytes)),
        });
    }
    let theme = resolve_theme(name, config_dir).map_err(|error| error.to_string())?;
    Ok(ThemeFile {
        name: name.to_owned(),
        source: encode_theme(&theme)?,
        revision: None,
    })
}

/// Save a theme against its expected revision, or create a new theme exclusively.
///
/// # Errors
/// Rejects invalid names or theme data, changed revisions, existing copy targets,
/// and filesystem failures before the theme can be committed.
pub fn save_theme(
    config_dir: &Path,
    name: &str,
    source: &str,
    expected_revision: Option<&str>,
) -> Result<ThemeFile, String> {
    let path = theme_path(config_dir, name)?;
    if u64::try_from(source.len()).unwrap_or(u64::MAX) > MAX_THEME_BYTES {
        return Err("Theme file exceeds 64 KiB".to_owned());
    }
    let mut theme = parse_theme_source(source, name).map_err(|error| error.to_string())?;
    name.clone_into(&mut theme.info.name);
    let source = encode_theme(&theme)?;
    let directory = path.parent().ok_or("Theme path has no parent directory")?;
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let target = WriteTarget::resolve(&path)
        .map_err(|error| error.into_io().to_string())?
        .lock()
        .map_err(|error| error.to_string())?;
    if let Some(expected) = expected_revision {
        let current = read_bounded(target.path())?;
        if revision(&current) != expected {
            return Err("Theme changed on disk; reload it or save with a new name".to_owned());
        }
        target
            .replace(source.as_bytes(), NewFileMode::Private)
            .map_err(|error| error.into_io().to_string())?;
    } else {
        let mut temporary =
            tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
        temporary
            .write_all(source.as_bytes())
            .map_err(|error| error.to_string())?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| error.to_string())?;
        temporary
            .persist_noclobber(&path)
            .map_err(|error| format!("Save theme: {}", error.error))?;
    }
    drop(target);
    Ok(ThemeFile {
        name: name.to_owned(),
        revision: Some(revision(source.as_bytes())),
        source,
    })
}

/// Import a TOML or iTerm theme from a bounded local file.
///
/// # Errors
/// Rejects relative paths, invalid filenames, unreadable or oversized files,
/// and malformed themes or color components.
pub fn import_theme(path: &Path) -> Result<ThemeFile, String> {
    if !path.is_absolute() {
        return Err("Import needs an absolute local file path".to_owned());
    }
    let bytes = read_bounded(path)?;
    let name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or("Theme filename is not UTF-8")?
        .to_owned();
    let theme = if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
    {
        parse_theme_source(
            std::str::from_utf8(&bytes).map_err(|error| error.to_string())?,
            &name,
        )
        .map_err(|error| error.to_string())?
    } else {
        import_iterm(&bytes, &name)?
    };
    Ok(ThemeFile {
        name,
        source: encode_theme(&theme)?,
        revision: None,
    })
}

fn import_iterm(bytes: &[u8], name: &str) -> Result<ResolvedTheme, String> {
    let value = plist::Value::from_reader(std::io::Cursor::new(bytes))
        .map_err(|error| error.to_string())?;
    let dictionary = value
        .as_dictionary()
        .ok_or("iTerm color theme must be a dictionary")?;
    let color = |key: &str| -> Result<Option<Color>, String> {
        let Some(value) = dictionary.get(key) else {
            return Ok(None);
        };
        let channels = value
            .as_dictionary()
            .ok_or_else(|| format!("{key} must contain color components"))?;
        let component = |name: &str, default: Option<f64>| -> Result<u8, String> {
            let value = match channels.get(name) {
                Some(value) => value
                    .as_real()
                    .ok_or_else(|| format!("{name} in {key} must be a real number"))?,
                None => default.ok_or_else(|| format!("Missing {name} in {key}"))?,
            };
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(format!("{name} in {key} must be between 0 and 1"));
            }
            (value * 255.0)
                .round()
                .to_u8()
                .ok_or_else(|| format!("{name} in {key} cannot be represented as a color channel"))
        };
        Ok(Some(Color {
            r: component("Red Component", None)?,
            g: component("Green Component", None)?,
            b: component("Blue Component", None)?,
            a: component("Alpha Component", Some(1.0))?,
        }))
    };
    let required = |key: &str| color(key)?.ok_or_else(|| format!("Missing {key}"));
    let palette = (0..16)
        .map(|index| required(&format!("Ansi {index} Color")))
        .collect::<Result<_, _>>()?;
    Ok(ResolvedTheme {
        info: ThemeInfo {
            name: name.to_owned(),
            source: "iTerm2 color scheme".to_owned(),
            license: String::new(),
        },
        colors: ColorConfig {
            background: Some(required("Background Color")?),
            foreground: Some(required("Foreground Color")?),
            cursor: color("Cursor Color")?,
            cursor_text: color("Cursor Text Color")?,
            selection_background: color("Selection Color")?,
            selection_foreground: color("Selected Text Color")?,
            palette,
            ..ColorConfig::default()
        },
    })
}
