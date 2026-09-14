//! Parent-directory watches survive atomic document replacement.
use anyhow::{Context as _, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub struct FileWatch {
    _watcher: RecommendedWatcher,
    changed: Arc<AtomicBool>,
}
impl FileWatch {
    /// # Errors
    /// Returns an error if the operating system cannot create or register the file watcher.
    pub fn new(path: &Path) -> Result<Self> {
        let changed = Arc::new(AtomicBool::new(true));
        let signal = changed.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if !matches!(
                    event,
                    Ok(notify::Event {
                        kind: notify::EventKind::Access(_),
                        ..
                    })
                ) {
                    signal.store(true, Ordering::Release);
                }
            })?;
        let parent = path.parent().context("document has no parent directory")?;
        watcher.watch(parent, RecursiveMode::NonRecursive)?;
        // A symlink may point into another directory; edits there also invalidate the snapshot.
        let resolved = std::fs::canonicalize(path)?;
        if let Some(target_parent) = resolved.parent()
            && target_parent != parent
        {
            watcher.watch(target_parent, RecursiveMode::NonRecursive)?;
        }
        Ok(Self {
            _watcher: watcher,
            changed,
        })
    }
    #[must_use]
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }
}
