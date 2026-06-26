use std::sync::OnceLock;

#[cfg(target_os = "macos")]
use std::path::PathBuf;

pub fn system_font_database() -> &'static fontdb::Database {
    static SYSTEM_FONT_DATABASE: OnceLock<fontdb::Database> = OnceLock::new();
    SYSTEM_FONT_DATABASE.get_or_init(|| {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        configure_windows_fonts(&mut database);
        load_macos_fonts(&mut database);
        database
    })
}

#[cfg(windows)]
fn configure_windows_fonts(database: &mut fontdb::Database) {
    for family in ["Cascadia Mono", "Consolas"] {
        if database
            .query(&fontdb::Query {
                families: &[fontdb::Family::Name(family)],
                ..fontdb::Query::default()
            })
            .is_some()
        {
            database.set_monospace_family(family);
            break;
        }
    }
}

#[cfg(not(windows))]
fn configure_windows_fonts(_database: &mut fontdb::Database) {}

#[cfg(target_os = "macos")]
fn load_macos_fonts(database: &mut fontdb::Database) {
    for dir in macos_font_dirs() {
        database.load_fonts_dir(dir);
    }
}

#[cfg(not(target_os = "macos"))]
fn load_macos_fonts(_database: &mut fontdb::Database) {}

#[cfg(target_os = "macos")]
fn macos_font_dirs() -> Vec<PathBuf> {
    macos_font_dirs_from(std::env::var_os("HOME"), std::env::var("USER").ok())
}

#[cfg(target_os = "macos")]
fn macos_font_dirs_from(home: Option<std::ffi::OsString>, user: Option<String>) -> Vec<PathBuf> {
    let mut dirs = macos_user_font_dirs_from(home, user);
    dirs.extend([
        PathBuf::from("/opt/zerobrew/share/fonts"),
        PathBuf::from("/opt/homebrew/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ]);
    dirs.sort();
    dirs.dedup();
    dirs
}

#[cfg(target_os = "macos")]
fn macos_user_font_dirs_from(
    home: Option<std::ffi::OsString>,
    user: Option<String>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home {
        dirs.push(PathBuf::from(home).join("Library/Fonts"));
    }
    if let Some(user) = user {
        let user = user.trim();
        if !user.is_empty() && !user.contains('/') {
            dirs.push(PathBuf::from("/Users").join(user).join("Library/Fonts"));
        }
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;

    #[test]
    fn macos_user_font_dirs_include_home_library_fonts() {
        let dirs =
            macos_user_font_dirs_from(Some("/Users/example".into()), Some("example".to_owned()));

        assert!(dirs.contains(&PathBuf::from("/Users/example/Library/Fonts")));
    }

    #[test]
    fn macos_font_dirs_include_package_manager_font_roots() {
        let dirs = macos_font_dirs_from(Some("/Users/example".into()), Some("example".to_owned()));

        assert!(dirs.contains(&PathBuf::from("/Users/example/Library/Fonts")));
        assert!(dirs.contains(&PathBuf::from("/opt/zerobrew/share/fonts")));
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/share/fonts")));
        assert!(dirs.contains(&PathBuf::from("/usr/local/share/fonts")));
    }
}
