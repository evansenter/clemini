//! Clemini library - exposes core functionality for integration tests.
//!
//! This module re-exports the core types and functions needed for testing.
//! The binary crate (main.rs) uses these same modules.

pub mod acp;
pub mod acp_client;
pub mod agent;
pub mod diff;
pub mod event_bus;
pub mod events;
pub mod format;
pub mod logging;
pub mod plan;
pub mod tools;

// Re-export commonly used types
pub use agent::{AgentEvent, InteractionResult, RetryConfig, run_interaction};
pub use logging::{OutputSink, log_event, set_output_sink};
pub use tools::CleminiToolService;
