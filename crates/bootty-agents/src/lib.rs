mod accounts;
mod commands;
mod events;
mod integration;
mod launch;
mod native_commands;
mod native_protocol;
mod native_service;
mod native_session;
mod persistence;
mod provider;
mod service;

pub use accounts::agent_account_launch;
pub use commands::{AgentCommandExecutor, AgentInvocation, command_descriptors};
pub use events::{AgentEvent, AgentEventPublisher};
pub use integration::{
    AgentIntegration, IntegrationDeclaration, IntegrationFile, IntegrationMerge,
    IntegrationPlacement, IntegrationState, IntegrationStatus, agent_integrations,
    install_integration, integration_declaration, integration_status, uninstall_integration,
};
pub use launch::{AgentLaunch, AgentLaunchContext, LaunchShell};
pub use native_commands::native_command_descriptors;
pub use native_protocol::{
    NativeAgentRequest, NativeSessionSnapshot, NativeSessionStatus, NativeTranscriptItem,
};
pub use native_service::{NativeAgentService, NativeSessionRecord};
pub use native_session::{NativeAgentSession, NativeChangeHandler, NativeSessionConfig};
pub use persistence::flush_all as flush_agent_state;
pub use provider::{
    AgentAttention, AgentEventKind, AgentKind, AgentPaneKey, AgentSource, AgentState, AgentStatus,
};
pub use service::{AgentPaneResolver, AgentService};
