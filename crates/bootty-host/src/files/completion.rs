//! Project indexes and ordinary path completion run on the selected filesystem host.
use super::*;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FileCompletionPage {
    pub root: String,
    pub files: Vec<String>,
    pub omitted: usize,
}

impl FileCompletionPage {
    /// Reuse a complete snapshot only when narrowing preserves its searched directory.
    /// Callers own snapshot expiry; parent-directory changes and partial results require a read.
    #[must_use]
    pub fn narrow(&self, previous: &str, query: &str) -> Option<Self> {
        if self.omitted != 0
            || query.len() > 1024
            || query.chars().any(char::is_control)
            || !query.starts_with(previous)
            || query.contains('/')
            || previous.contains('/')
            || query.starts_with('~')
        {
            return None;
        }
        Some(ranked_page(self.root.clone(), self.files.iter(), query))
    }
}

pub(super) fn complete(base: &str, query: &str) -> Result<FileCompletionPage> {
    let base = require_absolute(base)?;
    if query.len() > 1024 || query.chars().any(char::is_control) {
        bail!("Invalid file query");
    }
    let indexed = if query.starts_with(['/', '~']) {
        None
    } else {
        project_index(base)?
    };
    let (root, paths, filter) = if let Some((root, paths)) = indexed {
        (root, paths, query.to_owned())
    } else {
        directory_paths(base, query)?
    };
    Ok(ranked_page(root, paths.iter(), &filter))
}

fn ranked_page<'a>(
    root: String,
    paths: impl Iterator<Item = &'a String>,
    filter: &str,
) -> FileCompletionPage {
    let mut matches = paths
        .filter_map(|path| {
            crate::fuzzy::fuzzy_match(path, filter).map(|matched| (matched.score, path))
        })
        .collect::<Vec<_>>();
    matches.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    let total = matches.len();
    let mut bytes = 0_usize;
    let files = matches
        .into_iter()
        .take(100)
        .take_while(|(_, path)| {
            bytes = bytes.saturating_add(path.len().saturating_add(8));
            bytes <= 64 * 1024
        })
        .map(|(_, path)| path.clone())
        .collect::<Vec<_>>();
    FileCompletionPage {
        root,
        omitted: total.saturating_sub(files.len()),
        files,
    }
}

fn project_index(base: &Path) -> Result<Option<(String, BTreeSet<String>)>> {
    let args = vec![
        "-C".to_owned(),
        base.to_string_lossy().into_owned(),
        "rev-parse".into(),
        "--show-toplevel".into(),
    ];
    let Ok(output) = SystemCommandRunner.run("git", &args) else {
        return Ok(None);
    };
    if !output.success {
        return Ok(None);
    }
    let root = output.stdout.trim_end_matches('\n').to_owned();
    require_absolute(&root)?;
    let output = SystemCommandRunner.run(
        "git",
        &[
            "-C".into(),
            root.clone(),
            "ls-files".into(),
            "--cached".into(),
            "--others".into(),
            "--exclude-standard".into(),
            "-z".into(),
        ],
    )?;
    if !output.success {
        bail!("Could not index project files: {}", output.stderr.trim());
    }
    if output.stdout.len() > 16 * 1024 * 1024 || output.stdout.contains('\u{fffd}') {
        bail!("Project index is too large or contains unsupported filenames");
    }
    let mut paths = BTreeSet::new();
    for path in output
        .stdout
        .split_terminator('\0')
        .filter(|path| !path.is_empty())
    {
        paths.insert(path.to_owned());
        for (index, _) in path.match_indices('/') {
            paths.insert(
                path.get(..index.saturating_add(1))
                    .context("Invalid file path")?
                    .to_owned(),
            );
        }
    }
    Ok(Some((root, paths)))
}

fn directory_paths(base: &Path, query: &str) -> Result<(String, BTreeSet<String>, String)> {
    // Outside Git, complete the typed directory rather than walking a home tree.
    let typed = if let Some(home) = query.strip_prefix("~/") {
        std::env::var("HOME").map(|root| format!("{root}/{home}"))?
    } else {
        query.to_owned()
    };
    let (parent, filter) = typed
        .rsplit_once('/')
        .map_or(("", typed.as_str()), |(parent, query)| (parent, query));
    let directory = if typed.starts_with('/') {
        Path::new(if parent.is_empty() { "/" } else { parent }).to_path_buf()
    } else {
        base.join(parent)
    };
    let directory = fs::canonicalize(directory)?;
    let mut paths = BTreeSet::new();
    for entry in fs::read_dir(&directory)?.take(MAX_DIRECTORY_ENTRIES.saturating_add(1)) {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("Directory contains unsupported filenames"))?;
        if paths.len() >= MAX_DIRECTORY_ENTRIES {
            bail!("Directory exceeds the completion entry limit");
        }
        paths.insert(if entry.path().is_dir() {
            format!("{name}/")
        } else {
            name
        });
    }
    Ok((
        directory.to_string_lossy().into_owned(),
        paths,
        filter.into(),
    ))
}
