mod claude_terminal;
mod codex_terminal;
mod commands;
mod events;
mod integration;
mod launch;
mod persistence;
mod pi_terminal;
mod provider;
mod service;
mod terminal_accounts;
mod terminal_commands;
mod terminal_observation;
mod terminal_process;
mod terminal_service;

pub use claude_terminal::{ClaudeTerminalObserver, claude_terminal_observation};
pub use codex_terminal::{CodexTerminalObserver, CodexTerminalProtocol};
pub use commands::{AgentCommandExecutor, AgentInvocation, command_descriptors};
pub use events::{AgentEvent, AgentEventPublisher};
pub use integration::{
    AgentIntegration, IntegrationDeclaration, IntegrationFile, IntegrationMerge,
    IntegrationPlacement, IntegrationState, IntegrationStatus, agent_integrations,
    install_integration, integration_declaration, integration_status, uninstall_integration,
};
pub use launch::{AgentLaunch, AgentLaunchContext, LaunchShell};
pub use persistence::flush_all as flush_agent_state;
pub use pi_terminal::PiTerminalObserver;
pub use provider::{
    AgentAttention, AgentEventKind, AgentKind, AgentPaneKey, AgentSource, AgentState, AgentStatus,
};
pub use service::{AgentPaneResolver, AgentService};
pub use terminal_accounts::{
    TerminalAccountStatus, terminal_account_launch, terminal_account_status,
};
pub use terminal_commands::terminal_command_descriptors;
pub use terminal_observation::{
    AgentObservation, ObservationSink, TerminalAgentStatus, terminal_session_id,
};
pub use terminal_service::{
    PreparedTerminalAgent, TerminalAgentActivity, TerminalAgentRecord, TerminalAgentService,
};
