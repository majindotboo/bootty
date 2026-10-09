//! Project edits commit through the captured Space's shared command path.
use bootty_control::{Caller, CommandInvocation, CommandTarget};
use bootty_mux::repository::{ProjectSettings, RegisteredProject};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSettingsEditor {
    target: CommandTarget,
    cwd: String,
    pub settings: ProjectSettings,
    pub error: Option<String>,
}

impl ProjectSettingsEditor {
    #[must_use]
    pub fn name(&self) -> &str {
        if self.settings.name.is_empty() {
            self.cwd
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(&self.cwd)
        } else {
            &self.settings.name
        }
    }
    #[must_use]
    pub fn new(target: CommandTarget, project: RegisteredProject) -> Self {
        Self {
            target,
            cwd: project.cwd,
            settings: project.settings,
            error: None,
        }
    }
    /// # Errors
    /// Returns invalid project settings or serialization failures without submitting a mutation.
    pub fn configure(&self) -> Result<CommandInvocation, String> {
        self.settings
            .validate()
            .map_err(|error| error.to_string())?;
        let encoded = serde_json::to_string(&self.settings).map_err(|error| error.to_string())?;
        let mut invocation = CommandInvocation::new(
            "project.configure",
            vec![self.cwd.clone(), encoded],
            Caller::Internal,
        );
        invocation.target = Some(self.target.clone());
        Ok(invocation)
    }
}
