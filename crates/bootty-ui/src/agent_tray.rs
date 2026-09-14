//! One process-wide attention tray. Menu routes retain the originating window and pane target.
use crate::state::agent_attention::AgentOverview;
use bootty_control::{BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation};
use gpui_kit::{App, EntityId, Global};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[path = "agent_tray/native.rs"]
mod platform;
#[cfg(target_os = "linux")]
#[path = "agent_tray/linux.rs"]
mod platform;

type Route = (BoundAppCommandSender, CommandInvocation);
static ROUTES: OnceLock<Mutex<HashMap<String, Route>>> = OnceLock::new();

/// Native menu callbacks only enqueue commands; they never touch GPUI state.
pub fn dispatch(id: &str) -> bool {
    if !id.starts_with("bootty.agent-tray.") {
        return false;
    }
    let route = ROUTES.get().and_then(|routes| {
        routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
    });
    if let Some((sender, command)) = route
        && let Err(error) = sender.submit(
            command,
            {
                let now = Instant::now();
                now.checked_add(Duration::from_secs(10)).unwrap_or(now)
            },
            CommandCancellation::new(),
        )
    {
        eprintln!("Agent tray command unavailable: {error:?}");
    }
    true
}

#[derive(Clone, Default)]
struct Snapshot {
    unread: usize,
    total: usize,
    items: Vec<(String, String)>,
}
impl Snapshot {
    fn title(&self) -> String {
        format!("Bootty · {} unread · {} agents", self.unread, self.total)
    }
}

#[derive(Default)]
struct AgentTray {
    windows: HashMap<EntityId, (Vec<AgentOverview>, BoundAppCommandSender)>,
    revision: u64,
    backend: Option<platform::Backend>,
    failed: bool,
}
impl Global for AgentTray {}

pub fn update(
    id: EntityId,
    entries: Vec<AgentOverview>,
    sender: BoundAppCommandSender,
    cx: &mut App,
) {
    if cx.try_global::<AgentTray>().is_none() {
        cx.set_global(AgentTray::default());
    }
    let tray = cx.global_mut::<AgentTray>();
    if tray
        .windows
        .get(&id)
        .is_some_and(|(current, _)| *current == entries)
    {
        return;
    }
    tray.windows.insert(id, (entries, sender));
    tray.refresh();
}
pub fn remove(id: EntityId, cx: &mut App) {
    if cx.try_global::<AgentTray>().is_some() {
        let tray = cx.global_mut::<AgentTray>();
        tray.windows.remove(&id);
        tray.refresh();
    }
}
impl AgentTray {
    fn refresh(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        let mut snapshot = Snapshot::default();
        let mut routes = HashMap::new();
        let mut entries = self
            .windows
            .values()
            .flat_map(|(entries, sender)| entries.iter().map(move |entry| (entry, sender)))
            .collect::<Vec<_>>();
        entries.sort_by(|(a, _), (b, _)| {
            b.unread
                .cmp(&a.unread)
                .then_with(|| a.host.cmp(&b.host))
                .then_with(|| a.title.cmp(&b.title))
        });
        snapshot.total = entries.len();
        snapshot.unread = entries.iter().filter(|(entry, _)| entry.unread).count();
        for (index, (entry, sender)) in entries.into_iter().take(128).enumerate() {
            let id = format!("bootty.agent-tray.{}.{index}", self.revision);
            let mut command = CommandInvocation::from_action("agents.focus", Caller::Internal);
            command.target = Some(entry.target.clone());
            routes.insert(id.clone(), (sender.clone(), command));
            snapshot.items.push((
                id,
                format!(
                    "{}{} · {} · {} · {}",
                    if entry.unread { "● " } else { "" },
                    entry.provider,
                    entry.host,
                    entry.title,
                    entry.status
                ),
            ));
        }
        // Keep the native menu bounded; the Dock panel always contains the full list.
        for (index, (_, sender)) in self.windows.values().enumerate() {
            let id = format!("bootty.agent-tray.{}.window.{index}", self.revision);
            routes.insert(
                id.clone(),
                (
                    sender.clone(),
                    CommandInvocation::from_action("show_agents", Caller::Internal),
                ),
            );
            snapshot.items.push((
                id,
                format!("Open Agents · window {}", index.saturating_add(1)),
            ));
        }
        *ROUTES
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = routes;
        if snapshot.total == 0 {
            self.backend = None;
            self.failed = false;
            return;
        }
        if self.backend.is_none() && !self.failed {
            match platform::Backend::new(snapshot.clone()) {
                Ok(backend) => self.backend = Some(backend),
                Err(error) => {
                    self.failed = true;
                    eprintln!("Agent tray unavailable: {error}");
                }
            }
        } else if let Some(backend) = &mut self.backend {
            backend.update(snapshot);
        }
    }
}

fn icon(unread: bool) -> Vec<u8> {
    let mut bytes = vec![0; 16 * 16 * 4];
    for (y, row) in bytes.as_chunks_mut::<{ 16 * 4 }>().0.iter_mut().enumerate() {
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let chevron = (3..=11).contains(&y) && x == 7_usize.saturating_sub(y.abs_diff(7));
            let underline = y == 11 && (9..=13).contains(&x);
            let dot = unread && x >= 11 && y <= 4;
            if chevron || underline || dot {
                pixel.copy_from_slice(if dot {
                    &[255, 140, 0, 255]
                } else {
                    &[128, 128, 128, 255]
                });
            }
        }
    }
    bytes
}
