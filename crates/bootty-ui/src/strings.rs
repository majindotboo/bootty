use std::path::PathBuf;

#[must_use]
pub fn expand_home_path(path: &str) -> PathBuf {
    if let Some(rest) = home_relative_path(path)
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

fn home_relative_path(path: &str) -> Option<&str> {
    if let Some(rest) = path.strip_prefix("~/") {
        return Some(rest);
    }
    #[cfg(windows)]
    {
        path.strip_prefix(r"~\")
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    bootty_config::config::default_working_directory()
}

#[must_use]
pub fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}
