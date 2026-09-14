use super::AppState;
use bootty_mux::{controller::SpaceId, terminal::decode_scoped_pane_id};
use bootty_terminal::{
    clipboard_write::{ClipboardResult, ClipboardWrite, reply},
    terminal_side_effect::{TerminalSideEffect, TerminalSideEffectEvent},
};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Clone, PartialEq, Eq)]
struct Destination {
    scope: SpaceId,
    generation: u64,
    pane: String,
    host: String,
}
struct Pending {
    destination: Destination,
    id: String,
    result: mpsc::Receiver<anyhow::Result<arboard::ImageData<'static>>>,
}
#[derive(Default)]
pub(super) struct ImageClipboard {
    parser: ClipboardWrite,
    owner: Option<(Destination, Instant)>,
    pending: Option<Pending>,
}
impl AppState {
    fn clipboard_host(&self, scope: SpaceId) -> Option<String> {
        let remote = self.workspace.binding(scope)?.multiplexer().remote.as_ref();
        Some(match remote {
            None => "local".into(),
            Some(bootty_config::config::RemoteConfig::Ssh(ssh)) => {
                format!(
                    "ssh:{}:{}",
                    bootty_host::ssh::SshRemote::new(ssh.clone()).destination(),
                    ssh.port.unwrap_or(22)
                )
            }
            Some(bootty_config::config::RemoteConfig::Wsl(wsl)) => {
                format!("wsl:{}", wsl.distribution.as_str())
            }
        })
    }
    fn clipboard_allowed(&self, destination: &Destination) -> bool {
        self.workspace
            .binding(destination.scope)
            .is_some_and(|binding| binding.mux().binding_generation() == destination.generation)
            && self.clipboard_host(destination.scope).as_ref() == Some(&destination.host)
            && self
                .config()
                .session
                .clipboard_write_hosts
                .split_whitespace()
                .any(|host| host == destination.host)
    }
    fn clipboard_reply(&mut self, destination: &Destination, bytes: &[u8]) {
        let Some(binding) = self.workspace.binding_mut(destination.scope) else {
            return;
        };
        if binding.mux().binding_generation() != destination.generation {
            return;
        }
        if let Some(runtime) = binding
            .terminal_mut()
            .focused_terminal_runtime(&destination.pane)
            && let Err(error) = runtime.write_input(bytes)
        {
            self.record_error(error);
        }
    }
    pub(super) fn apply_image_clipboard(
        &mut self,
        scope: SpaceId,
        generation: u64,
        event: &TerminalSideEffectEvent,
        now: Instant,
    ) {
        if !matches!(
            event.effect,
            TerminalSideEffect::ClipboardPacket(_) | TerminalSideEffect::ClipboardReset
        ) {
            return;
        }
        let Some(binding) = self.workspace.binding(scope) else {
            return;
        };
        if binding.mux().binding_generation() != generation {
            return;
        }
        let Some(raw) = event.source_pane_id.as_ref() else {
            return;
        };
        let pane = if let Some((actual, pane)) = decode_scoped_pane_id(raw) {
            if actual != scope {
                return;
            }
            pane
        } else {
            raw.clone()
        };
        let Some(host) = self.clipboard_host(scope) else {
            return;
        };
        let destination = Destination {
            scope,
            generation,
            pane,
            host,
        };
        if event.effect == TerminalSideEffect::ClipboardReset {
            if self
                .image_clipboard
                .owner
                .as_ref()
                .is_some_and(|(owner, _)| owner == &destination)
            {
                self.image_clipboard.parser.reset();
                self.image_clipboard.owner = None;
            }
            if self
                .image_clipboard
                .pending
                .as_ref()
                .is_some_and(|pending| pending.destination == destination)
            {
                self.image_clipboard.pending = None;
            }
            return;
        }
        let TerminalSideEffect::ClipboardPacket(packet) = &event.effect else {
            return;
        };
        // Opaque attachment clients cannot route protocol replies to an inner pane.
        if self
            .workspace
            .binding_mut(scope)
            .and_then(|b| b.terminal_mut().focused_terminal_runtime(&destination.pane))
            .is_none()
        {
            return;
        }
        let start = std::str::from_utf8(packet).is_ok_and(|p| {
            p.split(';')
                .next()
                .unwrap_or("")
                .split(':')
                .any(|f| f == "type=write")
        });
        if self.image_clipboard.pending.is_some()
            || self
                .image_clipboard
                .owner
                .as_ref()
                .is_some_and(|(owner, _)| *owner != destination)
        {
            if start {
                let id = std::str::from_utf8(packet)
                    .unwrap_or("")
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .split(':')
                    .find_map(|s| s.strip_prefix("id="))
                    .unwrap_or("");
                self.clipboard_reply(&destination, &reply("write", id, "EBUSY"));
            }
            return;
        }
        let allowed = self.clipboard_allowed(&destination);
        if start && allowed {
            self.image_clipboard.owner = Some((destination.clone(), now));
        }
        self.feed_image_clipboard(packet, destination, allowed);
    }
    fn feed_image_clipboard(&mut self, packet: &[u8], destination: Destination, allowed: bool) {
        let outcome = self.image_clipboard.parser.feed(packet, allowed);
        if !self.image_clipboard.parser.is_active() {
            self.image_clipboard.owner = None;
        }
        match outcome {
            Some(ClipboardResult::Reply(bytes)) => self.clipboard_reply(&destination, &bytes),
            Some(ClipboardResult::Image(image)) => {
                let (sender, result) = mpsc::channel();
                let id = image.id.clone();
                let repaint = self.repaint.clone();
                std::thread::spawn(move || {
                    let _ = sender.send(crate::platform::decode_clipboard_image(
                        &image.mime,
                        &image.data,
                    ));
                    repaint();
                });
                self.image_clipboard.pending = Some(Pending {
                    destination,
                    id,
                    result,
                });
            }
            None => {}
        }
    }
    pub(super) fn poll_image_clipboard(&mut self, now: Instant) {
        if self
            .image_clipboard
            .owner
            .as_ref()
            .is_some_and(|(destination, started)| {
                now.saturating_duration_since(*started) >= Duration::from_secs(30)
                    || !self.clipboard_allowed(destination)
            })
        {
            self.image_clipboard.parser.reset();
            self.image_clipboard.owner = None;
        }
        let result =
            self.image_clipboard
                .pending
                .as_ref()
                .and_then(|p| match p.result.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(Err(anyhow::anyhow!("clipboard image decoder stopped")))
                    }
                    Err(mpsc::TryRecvError::Empty) => None,
                });
        if let Some(result) = result
            && let Some(pending) = self.image_clipboard.pending.take()
        {
            let attached = self
                .workspace
                .binding_mut(pending.destination.scope)
                .and_then(|b| {
                    b.terminal_mut()
                        .focused_terminal_runtime(&pending.destination.pane)
                })
                .is_some();
            let status = if !attached || !self.clipboard_allowed(&pending.destination) {
                "EPERM"
            } else {
                match result {
                    Err(error) => {
                        self.record_error(error);
                        "EINVAL"
                    }
                    Ok(image) => match arboard::Clipboard::new()
                        .and_then(|mut clipboard| clipboard.set_image(image))
                    {
                        Ok(()) => "DONE",
                        Err(error) => {
                            self.record_error(error);
                            "EIO"
                        }
                    },
                }
            };
            self.clipboard_reply(&pending.destination, &reply("write", &pending.id, status));
        }
    }
}
