use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use serde_json::{Value, json};

use crate::{
    CommandCatalogSource, ControlEventReceiver, ControlEventSender, event_queue,
    state::{SharedControlState, lock_control_state},
};

#[derive(Clone)]
pub struct ControlPlane {
    pub(crate) state: SharedControlState,
    pub(crate) instance_scope: Arc<Mutex<Option<String>>>,
    events: Arc<ControlEventBus>,
}

struct ControlEventBus {
    sender: ControlEventSender,
    receiver: Mutex<ControlEventReceiver>,
}

impl Default for ControlPlane {
    fn default() -> Self {
        let (sender, receiver) = event_queue();
        Self {
            state: SharedControlState::default(),
            instance_scope: Arc::new(Mutex::new(None)),
            events: Arc::new(ControlEventBus {
                sender,
                receiver: Mutex::new(receiver),
            }),
        }
    }
}

impl ControlPlane {
    pub fn event_sender(&self) -> ControlEventSender {
        self.events.sender.clone()
    }

    pub(crate) fn process_events(&self, catalog: &dyn CommandCatalogSource) {
        let Ok(receiver) = self.events.receiver.lock() else {
            return;
        };
        for _ in 0..32 {
            let Ok(request) = receiver.try_recv() else {
                break;
            };
            let result = if request.cancellation.is_cancelled() {
                Err("control event was cancelled".to_owned())
            } else if Instant::now() >= request.deadline {
                Err("control event deadline expired".to_owned())
            } else {
                self.publish_scoped(
                    catalog,
                    request.identity.as_str(),
                    request.generation,
                    &request.topic,
                    &request.payload,
                )
            };
            let _ = request.response.send(result);
        }
    }

    fn publish_scoped(
        &self,
        catalog: &dyn CommandCatalogSource,
        module: &str,
        generation: u64,
        topic: &str,
        payload: &Value,
    ) -> Result<(), String> {
        let scope = self
            .instance_scope
            .lock()
            .map_err(|_| "control plane scope is unavailable".to_owned())?
            .clone()
            .ok_or_else(|| "control plane is not bound to an instance".to_owned())?;
        let mut publish = || {
            lock_control_state(&self.state).publish_event(
                &scope,
                topic,
                &json!({"extension": module, "generation": generation}),
                &Value::Null,
                payload,
            );
        };
        catalog.with_active_topic(module, generation, topic, &mut publish)
    }
}
