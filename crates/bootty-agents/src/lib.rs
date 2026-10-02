mod accounts;
mod commands;
mod events;
mod integration;
mod launch;
mod orchestration;
mod persistence;
mod provider;
mod service;
mod terminal_commands;
mod terminal_history;
mod terminal_service;
mod terminal_tools;

pub use accounts::{agent_account_launch, terminal_account_status};
pub use commands::{AgentCommandExecutor, AgentInvocation, command_descriptors};
pub use events::{AgentEvent, AgentEventPublisher};
pub use integration::{
    AgentIntegration, IntegrationDeclaration, IntegrationFile, IntegrationMerge,
    IntegrationPlacement, IntegrationState, IntegrationStatus, agent_integrations,
    install_integration, integration_declaration, integration_status, uninstall_integration,
};
pub use launch::{AgentLaunch, AgentLaunchContext, LaunchShell};
pub use orchestration::{
    OrchestrationMessage, OrchestrationMessageState, OrchestrationRun, OrchestrationService,
    OrchestrationTask, OrchestrationTaskState, OrchestrationWorker,
    orchestration_command_descriptors,
};
pub use persistence::flush_all as flush_agent_state;
pub use provider::{
    AgentAttention, AgentEventKind, AgentKind, AgentPaneKey, AgentSource, AgentState, AgentStatus,
};
pub use service::{AgentPaneResolver, AgentService};

pub use terminal_commands::terminal_command_descriptors;
pub use terminal_service::{TerminalAgentRecord, TerminalAgentService};
pub use terminal_tools::{
    TerminalToolRequest, attach_terminal_tool_arguments, terminal_tools_supported,
};

pub use terminal_history::{
    TerminalSessionHistory, TerminalSessionUsage, discover_terminal_history, terminal_history_root,
};
