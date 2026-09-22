//! Serialized native panel layouts and the shared background save worker.

use gpui_kit::component::dock::DockAreaState;
use gpui_kit::{App, Global};
use std::{collections::BTreeMap, path::PathBuf, sync::mpsc};

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct SavedLayout {
    #[serde(flatten)]
    pub layout: DockAreaState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub always_show_tabs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub always_hide_tabs: Vec<String>,
}

struct LayoutSave {
    path: PathBuf,
    key: String,
    state: SavedLayout,
    result: async_channel::Sender<Result<(), String>>,
}

struct LayoutWriter(mpsc::Sender<LayoutSave>);
impl Global for LayoutWriter {}

impl LayoutWriter {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<LayoutSave>();
        std::thread::spawn(move || {
            while let Ok(first) = receiver.recv() {
                let mut pending = BTreeMap::new();
                for save in std::iter::once(first).chain(receiver.try_iter()) {
                    pending.insert((save.path.clone(), save.key.clone()), save);
                }
                for save in pending.into_values() {
                    let result = save_layout(&save.path, &save.key, save.state)
                        .map_err(|error| error.to_string());
                    // A closed Dock must not stop saves for other windows or Spaces.
                    let _ = save.result.send_blocking(result);
                }
            }
        });
        Self(sender)
    }
}

pub(super) struct LayoutSaveHandle {
    sender: mpsc::Sender<LayoutSave>,
    path: PathBuf,
    key: String,
    result: async_channel::Sender<Result<(), String>>,
}
impl LayoutSaveHandle {
    pub fn new(
        path: PathBuf,
        key: String,
        cx: &mut App,
    ) -> (Self, async_channel::Receiver<Result<(), String>>) {
        if cx.try_global::<LayoutWriter>().is_none() {
            cx.set_global(LayoutWriter::new());
        }
        let (result, receiver) = async_channel::unbounded();
        let handle = Self {
            sender: cx.global::<LayoutWriter>().0.clone(),
            path,
            key,
            result,
        };
        (handle, receiver)
    }

    pub fn send(&self, state: SavedLayout) {
        if self
            .sender
            .send(LayoutSave {
                path: self.path.clone(),
                key: self.key.clone(),
                state,
                result: self.result.clone(),
            })
            .is_err()
        {
            let _ = self
                .result
                .try_send(Err("Layout writer is unavailable".to_owned()));
        }
    }
}

fn save_layout(path: &std::path::Path, key: &str, state: SavedLayout) -> anyhow::Result<()> {
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("layout path has no parent"))?,
    )?;
    let target = bootty_write::WriteTarget::resolve(path)
        .map_err(bootty_write::ResolveTargetError::into_io)?
        .lock()?;
    let mut states: BTreeMap<String, SavedLayout> = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(error) => return Err(error.into()),
    };
    states.insert(key.to_owned(), state);
    target
        .replace(
            &serde_json::to_vec(&states)?,
            bootty_write::NewFileMode::Private,
        )
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    drop(target);
    Ok(())
}

impl SavedLayout {
    pub fn load(path: &std::path::Path, state_key: &str, legacy_key: &str) -> Option<Self> {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<BTreeMap<String, Self>>(&bytes).ok())
            .and_then(|mut states| {
                states
                    .remove(state_key)
                    .or_else(|| states.remove(legacy_key))
                    .or_else(|| {
                        let prefix = format!("{legacy_key}:directory:");
                        states
                            .into_iter()
                            .find(|(key, _)| key.starts_with(&prefix))
                            .map(|(_, state)| state)
                    })
            })
    }
}
