use std::{collections::BTreeSet, sync::Arc};

use crate::CommandDescriptor;

/// Read-only command and event metadata supplied by a feature owner.
pub trait CommandCatalogSource: Send + Sync {
    fn list(&self) -> Vec<CommandDescriptor>;

    fn describe(&self, id: &str) -> Option<CommandDescriptor>;

    fn topics(&self) -> BTreeSet<String>;

    fn with_active_topic(
        &self,
        module: &str,
        generation: u64,
        topic: &str,
        publish: &mut dyn FnMut(),
    ) -> Result<(), String>;
}

/// Read-only command metadata for the local control protocol.
#[derive(Clone)]
pub struct ControlCatalog {
    core: Arc<[CommandDescriptor]>,
    source: Arc<dyn CommandCatalogSource>,
}

impl ControlCatalog {
    pub fn new(core: Vec<CommandDescriptor>, source: Arc<dyn CommandCatalogSource>) -> Self {
        Self {
            core: Arc::from(core),
            source,
        }
    }

    pub fn list(&self) -> Vec<CommandDescriptor> {
        let mut commands = self.core.iter().cloned().collect::<Vec<_>>();
        commands.extend(
            self.source
                .list()
                .into_iter()
                .filter(|candidate| !self.core.iter().any(|core| core.id == candidate.id)),
        );
        commands.sort_by(|left, right| left.id.cmp(&right.id));
        commands
    }

    pub fn describe(&self, id: &str) -> Option<CommandDescriptor> {
        self.core
            .iter()
            .find(|command| command.id == id)
            .cloned()
            .or_else(|| self.source.describe(id))
    }

    pub fn source(&self) -> &dyn CommandCatalogSource {
        &*self.source
    }
}
