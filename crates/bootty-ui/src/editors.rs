//! Code editors installed on this computer, for opening a local file or project in one.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// An installed editor Bootty can hand a path to.
#[derive(Debug, PartialEq, Eq)]
pub struct Editor {
    /// The stable id `files.open_in` names it by.
    pub id: &'static str,
    pub name: &'static str,
    /// The macOS app bundle, opened with `open -a`; elsewhere, the command-line launcher.
    program: PathBuf,
}

/// Editors Bootty knows to look for: id, name, macOS app bundles, and launcher command.
// Known editors only; add a configured editor when someone needs one outside this list.
const KNOWN: [(&str, &str, &[&str], &str); 7] = [
    ("vscode", "VS Code", &["Visual Studio Code.app"], "code"),
    ("cursor", "Cursor", &["Cursor.app"], "cursor"),
    ("zed", "Zed", &["Zed.app", "Zed Preview.app"], "zed"),
    ("windsurf", "Windsurf", &["Windsurf.app"], "windsurf"),
    ("sublime", "Sublime Text", &["Sublime Text.app"], "subl"),
    (
        "intellij",
        "IntelliJ IDEA",
        &["IntelliJ IDEA.app", "IntelliJ IDEA CE.app"],
        "idea",
    ),
    ("xcode", "Xcode", &["Xcode.app"], "xed"),
];

/// The editors on this computer, found once per run.
// An editor installed while Bootty runs appears after a restart.
pub fn installed() -> &'static [Editor] {
    static INSTALLED: OnceLock<Vec<Editor>> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let path = std::env::var_os("PATH").unwrap_or_default();
        let launchers: Vec<PathBuf> = std::env::split_paths(&path).collect();
        detect(home.as_deref(), &launchers, Path::exists)
    })
}

/// The installed editor called `id`.
pub fn find(id: &str) -> Option<&'static Editor> {
    installed().iter().find(|editor| editor.id == id)
}

fn detect(
    home: Option<&Path>,
    launchers: &[PathBuf],
    exists: impl Fn(&Path) -> bool,
) -> Vec<Editor> {
    KNOWN
        .iter()
        .filter_map(|&(id, name, bundles, command)| {
            let program = if cfg!(target_os = "macos") {
                let folders = std::iter::once(PathBuf::from("/Applications"))
                    .chain(home.map(|home| home.join("Applications")));
                folders
                    .flat_map(|folder| bundles.iter().map(move |bundle| folder.join(bundle)))
                    .find(|bundle| exists(bundle))?
            } else {
                let names = if cfg!(windows) {
                    vec![format!("{command}.cmd"), format!("{command}.exe")]
                } else {
                    vec![command.to_owned()]
                };
                launchers
                    .iter()
                    .flat_map(|folder| names.iter().map(move |name| folder.join(name)))
                    .find(|launcher| exists(launcher))?
            };
            Some(Editor { id, name, program })
        })
        .collect()
}

impl Editor {
    /// Opens `path` in this editor and waits for its launcher to hand it over.
    ///
    /// # Errors
    /// Returns the launcher's failure, or its exit status when it refuses the path.
    pub fn open(&self, path: &Path) -> Result<(), String> {
        let mut command = if cfg!(target_os = "macos") {
            let mut command = std::process::Command::new("open");
            command.arg("-a").arg(&self.program);
            command
        } else {
            std::process::Command::new(&self.program)
        };
        let status = command
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|error| format!("{}: {error}", self.program.display()))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("{} exited with {status}", self.program.display()))
        }
    }
}
