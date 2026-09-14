use super::Snapshot;
use std::sync::{Arc, Mutex, mpsc};

pub(super) struct Backend {
    latest: Arc<Mutex<Snapshot>>,
    wake: mpsc::SyncSender<()>,
}
impl Backend {
    pub(super) fn new(snapshot: Snapshot) -> Result<Self, String> {
        let latest = Arc::new(Mutex::new(snapshot));
        let state = latest.clone();
        let (wake, updates) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("agent-tray".to_owned())
            .spawn(move || {
                use ksni::blocking::TrayMethods as _;
                let tray = Sni(state.lock().expect("tray snapshot").clone());
                let handle = match tray.spawn() {
                    Ok(handle) => handle,
                    Err(error) => {
                        eprintln!("Agent tray unavailable: {error}");
                        return;
                    }
                };
                while updates.recv().is_ok() {
                    let snapshot = state.lock().expect("tray snapshot").clone();
                    handle.update(move |tray| tray.0 = snapshot);
                }
                handle.shutdown();
            })
            .map_err(|error| error.to_string())?;
        Ok(Self { latest, wake })
    }
    pub(super) fn update(&mut self, snapshot: Snapshot) {
        *self.latest.lock().expect("tray snapshot") = snapshot;
        let _ = self.wake.try_send(());
    }
}
struct Sni(Snapshot);
impl ksni::Tray for Sni {
    fn id(&self) -> String {
        format!("bootty-agents-{}", std::process::id())
    }
    fn title(&self) -> String {
        self.0.title()
    }
    fn status(&self) -> ksni::Status {
        if self.0.unread > 0 {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let mut data = super::icon(self.0.unread > 0);
        for pixel in data.as_chunks_mut::<4>().0 {
            pixel.rotate_right(1);
        }
        vec![ksni::Icon {
            width: 16,
            height: 16,
            data,
        }]
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        self.0
            .items
            .iter()
            .map(|(id, label)| {
                let id = id.clone();
                ksni::menu::StandardItem {
                    label: label.clone(),
                    activate: Box::new(move |_| {
                        super::dispatch(&id);
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect()
    }
}
