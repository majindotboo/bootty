//! Workspace window placement, saved independently of launch preferences.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context as _, Result};
use gpui_kit::{App, Bounds, Pixels, Window, WindowBounds, WindowOptions, px};
use serde::{Deserialize, Serialize};

use bootty_config::config::BoottyConfig;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowState {
    pub bounds: Bounds<Pixels>,
    pub fullscreen: bool,
    pub maximized: bool,
    pub display: Option<String>,
}

impl WindowState {
    pub(crate) fn capture(window: &Window, non_native_fullscreen: bool, cx: &App) -> Self {
        let bounds = window.window_bounds();
        Self {
            bounds: bounds.get_bounds(),
            fullscreen: window.is_fullscreen()
                || window.is_simple_fullscreen()
                || non_native_fullscreen,
            maximized: matches!(bounds, WindowBounds::Maximized(_)),
            display: window
                .display(cx)
                .and_then(|display| display.uuid().ok())
                .map(|uuid| uuid.to_string()),
        }
    }

    /// Read one workspace window's state beside its identity-scoped configuration.
    /// # Errors
    /// Returns unreadable, malformed, or invalid saved state.
    pub fn load(config_path: &Path, key: &str) -> Result<Option<Self>> {
        let states = read_states(&config_path.with_file_name("windows.json"))?;
        let state = states.get(key).cloned();
        if let Some(state) = &state {
            state.validate()?;
        }
        Ok(state)
    }

    /// Atomically save this window without replacing other windows' state.
    /// # Errors
    /// Returns invalid bounds, unreadable existing state, or an atomic write failure.
    pub fn save(&self, config_path: &Path, key: &str) -> Result<()> {
        self.validate()?;
        let path = config_path.with_file_name("windows.json");
        std::fs::create_dir_all(path.parent().context("window state has no parent")?)?;
        let target = bootty_write::WriteTarget::resolve(&path)
            .map_err(bootty_write::ResolveTargetError::into_io)?
            .lock()?;
        let mut states = read_states(&path)?;
        states.insert(key.to_owned(), self.clone());
        target
            .replace(
                &serde_json::to_vec(&states)?,
                bootty_write::NewFileMode::Private,
            )
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        drop(target);
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        let bounds = self.bounds;
        anyhow::ensure!(
            [
                bounds.origin.x,
                bounds.origin.y,
                bounds.size.width,
                bounds.size.height
            ]
            .into_iter()
            .all(|value| f32::from(value).is_finite())
                && bounds.size.width > px(0.0)
                && bounds.size.height > px(0.0),
            "invalid saved window bounds"
        );
        Ok(())
    }

    /// Apply saved placement and fullscreen state to a new workspace window.
    pub fn restore(&self, config: &BoottyConfig, options: &mut WindowOptions, cx: &App) {
        let displays = cx.displays();
        let display = displays
            .iter()
            .find(|display| {
                self.display
                    .as_ref()
                    .is_some_and(|uuid| display.uuid().is_ok_and(|id| id.to_string() == *uuid))
            })
            .or_else(|| {
                displays
                    .iter()
                    .find(|display| display.bounds().intersects(&self.bounds))
            })
            .cloned()
            .or_else(|| cx.primary_display());
        let bounds = display.as_ref().map_or(self.bounds, |display| {
            options.display_id = Some(display.id());
            self.visible_bounds(display.visible_bounds())
        });
        options.window_bounds = Some(if self.fullscreen {
            if config.window.uses_non_native_fullscreen_style() {
                if crate::platform::macos_handles_non_native_fullscreen_frame(&config.window) {
                    WindowBounds::Windowed(bounds)
                } else {
                    WindowBounds::Maximized(bounds)
                }
            } else {
                WindowBounds::Fullscreen(bounds)
            }
        } else if self.maximized {
            WindowBounds::Maximized(bounds)
        } else {
            WindowBounds::Windowed(bounds)
        });
    }

    /// Keep the restored window reachable after a monitor or resolution change.
    #[must_use]
    pub fn visible_bounds(&self, display: Bounds<Pixels>) -> Bounds<Pixels> {
        let mut bounds = self.bounds;
        bounds.size.width = bounds.size.width.min(display.size.width);
        bounds.size.height = bounds.size.height.min(display.size.height);
        bounds.origin.x = bounds.origin.x.clamp(
            display.left(),
            px(f32::from(display.right()) - f32::from(bounds.size.width)),
        );
        bounds.origin.y = bounds.origin.y.clamp(
            display.top(),
            px(f32::from(display.bottom()) - f32::from(bounds.size.height)),
        );
        bounds
    }
}

fn read_states(path: &Path) -> Result<BTreeMap<String, WindowState>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error.into()),
    }
}
