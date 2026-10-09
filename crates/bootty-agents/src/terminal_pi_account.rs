use std::{env, fs::File, io::Read, path::PathBuf};

use serde::Deserialize;

const MAX_SETTINGS: u64 = 1024 * 1024;

/// The exact Pi launch model scope, shared by inspection and its status cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiAccountSelector {
    provider: Option<String>,
    model: Option<String>,
}

impl PiAccountSelector {
    #[must_use]
    pub fn provider(provider: &str) -> Self {
        Self {
            provider: Some(provider.to_owned()),
            model: None,
        }
    }

    /// Parse the installed Pi CLI's separate flags and last-value precedence.
    /// # Errors
    /// Rejects missing/invalid selectors and model-only patterns without a qualified provider.
    pub fn from_arguments(arguments: &[String]) -> Result<Option<Self>, String> {
        let mut selector = Self {
            provider: None,
            model: None,
        };
        let mut arguments = arguments.iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--" => break,
                "--provider" | "--model" => {
                    let value = arguments
                        .next()
                        .ok_or("Pi model selector is missing its value")?;
                    if !valid_selector(value) {
                        return Err("Pi model selector is invalid".to_owned());
                    }
                    if argument == "--provider" {
                        selector.provider = Some(value.clone());
                    } else {
                        selector.model = Some(value.clone());
                    }
                }
                value if value.starts_with("--provider=") || value.starts_with("--model=") => {
                    return Err(
                        "Pi model selectors require separate flag and value arguments".to_owned(),
                    );
                }
                _ => {}
            }
        }
        if selector.provider.is_none() && selector.model.is_none() {
            return Ok(None);
        }
        selector.expected_provider()?;
        Ok(Some(selector))
    }

    fn expected_provider(&self) -> Result<&str, String> {
        if let Some(provider) = &self.provider {
            if valid_selector(provider) {
                return Ok(provider);
            }
            return Err("Pi model provider is invalid".to_owned());
        }
        self.model
            .as_deref()
            .and_then(|model| model.split_once('/'))
            .filter(|(provider, model)| valid_selector(provider) && valid_selector(model))
            .map(|(provider, _)| provider)
            .ok_or_else(|| {
                "Choose an explicit Pi provider for an unqualified model pattern".to_owned()
            })
    }
}

pub struct Scope {
    pub directory: String,
    pub provider: String,
    pub model: Option<String>,
    pub explicit_provider: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    default_provider: Option<String>,
    default_model: Option<String>,
}

fn valid_selector(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.starts_with('-')
        && !value.chars().any(char::is_whitespace)
        && !value.chars().any(char::is_control)
}

fn home() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "Pi account home directory is unavailable".to_owned())
}

pub fn scope(
    selector: Option<&PiAccountSelector>,
    directory: Option<&str>,
) -> Result<Scope, String> {
    let directory = match directory {
        Some(directory) if !directory.is_empty() => PathBuf::from(directory),
        Some(_) => return Err("Pi account directory is empty".to_owned()),
        None => env::var_os("PI_CODING_AGENT_DIR")
            .filter(|directory| !directory.is_empty())
            .map_or_else(
                || home().map(|home| home.join(".pi/agent")),
                |directory| Ok(PathBuf::from(directory)),
            )?,
    };
    let directory = directory
        .to_str()
        .ok_or("Pi account directory is not valid UTF-8")?
        .to_owned();
    if let Some(selector) = selector {
        let provider = selector.expected_provider()?;
        if selector
            .model
            .as_deref()
            .is_some_and(|model| !valid_selector(model))
        {
            return Err("Pi model selector is invalid".to_owned());
        }
        return Ok(Scope {
            directory,
            provider: provider.to_owned(),
            model: selector.model.clone(),
            explicit_provider: selector.provider.is_some(),
        });
    }
    let path = if let Some(relative) = directory.strip_prefix("~/") {
        home()?.join(relative)
    } else {
        PathBuf::from(&directory)
    }
    .join("settings.json");
    let file =
        File::open(path).map_err(|_| "Choose a Pi model provider or save a default model in Pi")?;
    // Only read model-selection metadata. Never inspect Pi's credential store.
    let mut bytes = Vec::new();
    file.take(MAX_SETTINGS + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Pi model settings could not be read")?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_SETTINGS {
        return Err("Pi model settings exceed 1 MiB".to_owned());
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    let settings: Settings =
        serde_json::from_slice(bytes).map_err(|_| "Pi model settings are malformed")?;
    // Pi selects the saved default only as a provider/model pair. Do not reproduce its
    // first-authenticated-provider fallback when the pair is unavailable.
    let provider = settings
        .default_provider
        .filter(|value| valid_selector(value))
        .ok_or("Choose a Pi model provider or save a default model in Pi")?;
    let model = settings
        .default_model
        .filter(|value| valid_selector(value))
        .ok_or("Choose a Pi model provider or save a default model in Pi")?;
    Ok(Scope {
        directory,
        provider,
        model: Some(model),
        explicit_provider: true,
    })
}
