use std::{collections::BTreeMap, sync::Arc};

use bootty_control::{
    CommandDescriptor, CommandInvocation, CommandOutcome, CompactSchema, MutationClass,
    ResourceKind, ValueType,
};

/// A browser action submitted through Bootty's shared command path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserAction {
    Open(String),
    NewTab(Option<String>),
    CloseTab(u64),
    Address,
    Back,
    Forward,
    Reload,
    Stop,
    Input(bootty_browser::BrowserInput),
    OpenExternal,
    Capture,
    CaptureAnnotation(u64),
    Snapshot(Option<String>),
    ResetSiteData,
}

/// A command completed by the window that owns the browser pages.
#[derive(Clone, Debug)]
pub struct BrowserRequest {
    pub action: BrowserAction,
    pub page: Option<u64>,
    pub(crate) target: Option<bootty_control::CommandTarget>,
    completion: Arc<BrowserCompletion>,
}

#[derive(Debug)]
struct BrowserCompletion {
    execution: Option<(std::time::Instant, bootty_control::CommandCancellation)>,
    response: Option<std::sync::mpsc::Sender<CommandOutcome>>,
    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "Exact-window capture is currently macOS-only")
    )]
    capture: Option<(
        std::num::NonZeroU32,
        std::path::PathBuf,
        bootty_computer::ComputerAccess,
        bootty_control::CommandTarget,
    )>,
}

impl PartialEq for BrowserRequest {
    fn eq(&self, other: &Self) -> bool {
        self.action == other.action
            && self.page == other.page
            && Arc::ptr_eq(&self.completion, &other.completion)
    }
}

impl BrowserRequest {
    pub(crate) fn new(
        action: BrowserAction,
        page: Option<u64>,
        target: Option<bootty_control::CommandTarget>,
        execution: Option<(std::time::Instant, bootty_control::CommandCancellation)>,
        response: Option<std::sync::mpsc::Sender<CommandOutcome>>,
    ) -> Self {
        Self {
            action,
            page,
            target,
            completion: Arc::new(BrowserCompletion {
                execution,
                response,
                capture: None,
            }),
        }
    }

    pub(crate) fn capture(
        page: u64,
        execution: (std::time::Instant, bootty_control::CommandCancellation),
        response: std::sync::mpsc::Sender<CommandOutcome>,
        native_window: std::num::NonZeroU32,
        directory: std::path::PathBuf,
        access: bootty_computer::ComputerAccess,
        target: bootty_control::CommandTarget,
    ) -> Self {
        Self {
            action: BrowserAction::Capture,
            page: Some(page),
            target: Some(target.clone()),
            completion: Arc::new(BrowserCompletion {
                execution: Some(execution),
                response: Some(response),
                capture: Some((native_window, directory, access, target)),
            }),
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn capture_context(
        &self,
    ) -> Option<(
        std::num::NonZeroU32,
        &std::path::Path,
        bootty_computer::ComputerAccess,
        &bootty_control::CommandTarget,
    )> {
        self.completion
            .capture
            .as_ref()
            .map(|(window, directory, access, target)| {
                (*window, directory.as_path(), *access, target)
            })
    }

    pub(crate) fn capture_deadline(&self) -> Option<std::time::Instant> {
        self.completion
            .execution
            .as_ref()
            .map(|(deadline, _)| *deadline)
    }

    pub(crate) fn cancel_capture(&self) {
        if matches!(
            self.action,
            BrowserAction::Capture | BrowserAction::CaptureAnnotation(_)
        ) && let Some((_, cancellation)) = &self.completion.execution
        {
            _ = cancellation.cancel();
        }
    }

    pub(crate) fn capture_cancelled(&self) -> bool {
        self.completion
            .execution
            .as_ref()
            .is_none_or(|(_, cancellation)| cancellation.is_cancelled())
    }

    pub(crate) fn begin(&self) -> Result<(), bootty_mux::controller::MuxCommandError> {
        bootty_mux::executor::begin_synchronous_command(self.completion.execution.clone())
    }

    pub fn complete(self, outcome: CommandOutcome) {
        if let Some(response) = &self.completion.response {
            let _ = response.send(outcome);
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "This fixed command table keeps browser metadata together; splitting the declarations would scatter the registry"
)]
pub(super) fn register_commands(commands: &mut BTreeMap<String, super::RegisteredCommand>) {
    let page_argument = |required| {
        let mut argument = super::argument("page-id", ValueType::Integer);
        argument.required = required;
        argument.minimum = Some(1);
        argument
    };

    for (id, title, description, arguments) in [
        (
            "browser.input",
            "Send Browser Input",
            "Send native input to the exact selected browser page without activating Bootty.",
            vec![
                page_argument(true),
                super::argument("action", ValueType::String),
            ],
        ),
        (
            "browser.open",
            "Open Browser Address",
            "Navigate a browser page to an address.",
            vec![
                super::argument("address", ValueType::String),
                page_argument(false),
            ],
        ),
        (
            "browser.new_tab",
            "Open Browser Tab",
            "Open a browser tab, optionally at an address.",
            vec![optional_string_argument("address")],
        ),
        (
            "browser.close_tab",
            "Close Browser Tab",
            "Close the browser page with this page ID.",
            vec![page_argument(true)],
        ),
        (
            "browser.address",
            "Focus Browser Address",
            "Focus the address field for a browser page.",
            vec![page_argument(false)],
        ),
        (
            "browser.back",
            "Browser Back",
            "Go back in a browser page's history.",
            vec![page_argument(false)],
        ),
        (
            "browser.forward",
            "Browser Forward",
            "Go forward in a browser page's history.",
            vec![page_argument(false)],
        ),
        (
            "browser.reload",
            "Reload Browser Page",
            "Reload a browser page.",
            vec![page_argument(false)],
        ),
        (
            "browser.stop",
            "Stop Browser Page",
            "Stop loading a browser page.",
            vec![page_argument(false)],
        ),
        (
            "browser.open_external",
            "Open Browser Address Externally",
            "Open a browser page's address in the default browser.",
            vec![page_argument(false)],
        ),
        (
            "browser.capture",
            "Capture Browser Page",
            "Save the exact visible browser page after validating its document and geometry.",
            vec![page_argument(true), {
                let mut intent = super::argument("capture-intent", ValueType::Integer);
                intent.required = false;
                intent.minimum = Some(1);
                intent
            }],
        ),
        (
            "browser.snapshot",
            "Read Browser Document",
            "Read bounded visible text from an exact browser page and document without changing focus.",
            vec![page_argument(true), optional_string_argument("document")],
        ),
        (
            "browser.reset_site_data",
            "Reset Browser Site Data",
            "Clear this browser profile's cookies and website storage.",
            vec![page_argument(false)],
        ),
    ] {
        let descriptor = CommandDescriptor {
            id: id.to_owned(),
            title: title.to_owned(),
            description: description.to_owned(),
            mutation: if id == "browser.reset_site_data" {
                MutationClass::Destructive
            } else if id == "browser.snapshot" {
                MutationClass::Read
            } else {
                MutationClass::Write
            },
            arguments: CompactSchema { arguments },
            target: Some(ResourceKind::ApplicationWindow),
            palette: id == "browser.new_tab",
        };
        commands.insert(
            descriptor.id.clone(),
            super::RegisteredCommand {
                descriptor,
                executor: super::CommandExecutorResolver::Browser,
            },
        );
    }
}

pub(super) fn resolve(
    invocation: &CommandInvocation,
) -> Result<(BrowserAction, Option<u64>), CommandOutcome> {
    let invalid_arguments = || CommandOutcome::Failed {
        code: "invalid_arguments".to_owned(),
        message: format!("Invalid arguments for {}", invocation.command),
    };
    let page = |value: Option<&String>| {
        value
            .map(|value| {
                value
                    .parse::<u64>()
                    .ok()
                    .filter(|page| *page != 0)
                    .ok_or_else(invalid_arguments)
            })
            .transpose()
    };
    let (action, page) = match invocation.command.as_str() {
        "browser.snapshot" if (1..=2).contains(&invocation.arguments.len()) => {
            let document = invocation.arguments.get(1);
            if document.is_some_and(|document| !bootty_browser::valid_document_token(document)) {
                return Err(invalid_arguments());
            }
            (
                BrowserAction::Snapshot(document.cloned()),
                Some(page(invocation.arguments.first())?.ok_or_else(invalid_arguments)?),
            )
        }
        "browser.input" if invocation.arguments.len() == 2 => {
            let action: bootty_browser::BrowserInput =
                serde_json::from_str(invocation.arguments.get(1).ok_or_else(invalid_arguments)?)
                    .map_err(|_| invalid_arguments())?;
            action.validate().map_err(|_| invalid_arguments())?;
            (
                BrowserAction::Input(action),
                page(invocation.arguments.first())?,
            )
        }
        "browser.open" => {
            let address = invocation
                .arguments
                .first()
                .ok_or_else(invalid_arguments)?
                .clone();
            (
                BrowserAction::Open(address),
                page(invocation.arguments.get(1))?,
            )
        }
        "browser.new_tab" => (
            BrowserAction::NewTab(invocation.arguments.first().cloned()),
            None,
        ),
        "browser.close_tab" => {
            let page = page(invocation.arguments.first())?.ok_or_else(invalid_arguments)?;
            (BrowserAction::CloseTab(page), Some(page))
        }
        "browser.address" => (BrowserAction::Address, page(invocation.arguments.first())?),
        "browser.back" => (BrowserAction::Back, page(invocation.arguments.first())?),
        "browser.forward" => (BrowserAction::Forward, page(invocation.arguments.first())?),
        "browser.reload" => (BrowserAction::Reload, page(invocation.arguments.first())?),
        "browser.stop" => (BrowserAction::Stop, page(invocation.arguments.first())?),
        "browser.open_external" => (
            BrowserAction::OpenExternal,
            page(invocation.arguments.first())?,
        ),
        "browser.capture" if (1..=2).contains(&invocation.arguments.len()) => (
            page(invocation.arguments.get(1))?
                .map_or(BrowserAction::Capture, BrowserAction::CaptureAnnotation),
            Some(page(invocation.arguments.first())?.ok_or_else(invalid_arguments)?),
        ),
        "browser.reset_site_data" => (
            BrowserAction::ResetSiteData,
            page(invocation.arguments.first())?,
        ),
        _ => return Err(invalid_arguments()),
    };
    Ok((action, page))
}

fn optional_string_argument(name: &str) -> bootty_control::ArgumentSchema {
    let mut argument = super::argument(name, ValueType::String);
    argument.required = false;
    argument
}
