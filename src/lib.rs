//! Clemini library - exposes core functionality for integration tests.
//!
//! This module re-exports the core types and functions needed for testing.
//! The binary crate (main.rs) uses these same modules.

pub mod agent;
pub mod diff;
pub mod events;
pub mod logging;
pub mod tools;

// Re-export commonly used types
pub use agent::{AgentEvent, InteractionResult, run_interaction};
pub use logging::{OutputSink, log_event, log_event_raw, set_output_sink};
pub use tools::CleminiToolService;
