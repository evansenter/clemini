//! Plan mode for structured task planning with user approval.
//!
//! Plan mode allows clemini to propose an implementation approach before executing,
//! getting user approval first. This prevents wasted effort when the approach
//! doesn't match user expectations.
//!
//! # Architecture
//!
//! When in plan mode:
//! - Read-only tools available (Glob, Grep, Read, WebFetch, WebSearch)
//! - Write tools disabled (Edit, Write, Bash with side effects)
//! - Plan written to a temporary file for user review
//! - User must approve before implementation proceeds
//!
//! # ACP Integration
//!
//! Plans are sent via ACP `SessionUpdate::Plan` when connected to an ACP client.
//! This enables Toad and other frontends to display plan progress natively.

use agent_client_protocol as acp;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

/// Global counter for generating unique plan IDs.
static NEXT_PLAN_ID: AtomicU64 = AtomicU64::new(1);

/// Manages plan state for a session.
#[derive(Debug)]
pub struct PlanManager {
    /// Current plan, if any.
    plan: Option<acp::Plan>,

    /// Whether we're currently in plan mode.
    in_plan_mode: bool,

    /// Path to the plan file.
    plan_file_path: Option<PathBuf>,

    /// Channel to send plan updates to ACP.
    acp_update_tx: Option<mpsc::UnboundedSender<acp::SessionNotification>>,

    /// Session ID for ACP notifications.
    session_id: Option<String>,
}

impl Default for PlanManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanManager {
    /// Create a new plan manager.
    pub fn new() -> Self {
        Self {
            plan: None,
            in_plan_mode: false,
            plan_file_path: None,
            acp_update_tx: None,
            session_id: None,
        }
    }

    /// Set the ACP update channel for sending plan updates.
    pub fn set_acp_channel(
        &mut self,
        tx: mpsc::UnboundedSender<acp::SessionNotification>,
        session_id: String,
    ) {
        self.acp_update_tx = Some(tx);
        self.session_id = Some(session_id);
    }

    /// Enter plan mode, optionally specifying a plan file path.
    pub fn enter_plan_mode(&mut self, plan_file_path: Option<PathBuf>) -> Result<(), String> {
        if self.in_plan_mode {
            return Err("Already in plan mode".to_string());
        }

        self.in_plan_mode = true;
        self.plan_file_path = plan_file_path.or_else(|| {
            // Default to ~/.clemini/plans/<plan-id>.md
            let plan_id = NEXT_PLAN_ID.fetch_add(1, Ordering::SeqCst);
            dirs::home_dir().map(|h| {
                h.join(".clemini")
                    .join("plans")
                    .join(format!("{}.md", plan_id))
            })
        });

        // Ensure plan directory exists
        if let Some(ref path) = self.plan_file_path
            && let Some(parent) = path.parent()
        {
            let _ = std::fs::create_dir_all(parent);
        }

        Ok(())
    }

    /// Exit plan mode and return whether we were in plan mode.
    pub fn exit_plan_mode(&mut self) -> bool {
        let was_in_plan_mode = self.in_plan_mode;
        self.in_plan_mode = false;
        was_in_plan_mode
    }

    /// Check if we're currently in plan mode.
    pub fn is_in_plan_mode(&self) -> bool {
        self.in_plan_mode
    }

    /// Get the current plan file path.
    pub fn plan_file_path(&self) -> Option<&PathBuf> {
        self.plan_file_path.as_ref()
    }

    /// Create a new plan with the given entries.
    pub fn create_plan(&mut self, entries: Vec<PlanEntryInput>) -> acp::Plan {
        let plan = acp::Plan::new(
            entries
                .into_iter()
                .map(|e| {
                    acp::PlanEntry::new(e.content, e.priority.into(), acp::PlanEntryStatus::Pending)
                })
                .collect(),
        );

        self.plan = Some(plan.clone());
        self.send_plan_update();
        plan
    }

    /// Update an entry's status by index.
    pub fn update_entry_status(
        &mut self,
        index: usize,
        status: PlanEntryStatus,
    ) -> Result<(), String> {
        let plan = self.plan.as_mut().ok_or("No active plan")?;

        if index >= plan.entries.len() {
            return Err(format!("Entry index {} out of range", index));
        }

        plan.entries[index].status = status.into();
        self.send_plan_update();
        Ok(())
    }

    /// Get the current plan.
    pub fn current_plan(&self) -> Option<&acp::Plan> {
        self.plan.as_ref()
    }

    /// Send plan update via ACP if connected.
    fn send_plan_update(&self) {
        if let (Some(tx), Some(session_id), Some(plan)) =
            (&self.acp_update_tx, &self.session_id, &self.plan)
        {
            let notification = acp::SessionNotification::new(
                session_id.clone(),
                acp::SessionUpdate::Plan(plan.clone()),
            );
            let _ = tx.send(notification);
        }
    }
}

/// Input for creating a plan entry.
#[derive(Debug, Clone)]
pub struct PlanEntryInput {
    /// Human-readable description of the task.
    pub content: String,
    /// Priority level.
    pub priority: PlanEntryPriority,
}

/// Priority levels for plan entries.
#[derive(Debug, Clone, Copy, Default)]
pub enum PlanEntryPriority {
    High,
    #[default]
    Medium,
    Low,
}

impl From<PlanEntryPriority> for acp::PlanEntryPriority {
    fn from(p: PlanEntryPriority) -> Self {
        match p {
            PlanEntryPriority::High => acp::PlanEntryPriority::High,
            PlanEntryPriority::Medium => acp::PlanEntryPriority::Medium,
            PlanEntryPriority::Low => acp::PlanEntryPriority::Low,
        }
    }
}

/// Status of a plan entry.
#[derive(Debug, Clone, Copy)]
pub enum PlanEntryStatus {
    Pending,
    InProgress,
    Completed,
}

impl From<PlanEntryStatus> for acp::PlanEntryStatus {
    fn from(s: PlanEntryStatus) -> Self {
        match s {
            PlanEntryStatus::Pending => acp::PlanEntryStatus::Pending,
            PlanEntryStatus::InProgress => acp::PlanEntryStatus::InProgress,
            PlanEntryStatus::Completed => acp::PlanEntryStatus::Completed,
        }
    }
}

/// Allowed prompt for implementation phase permissions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AllowedPrompt {
    /// The tool name (e.g., "Bash").
    pub tool: String,
    /// Semantic description of allowed action (e.g., "run tests").
    pub prompt: String,
}

/// Global plan manager instance.
/// Uses RwLock for interior mutability since tools() creates tools once
/// but plan state needs to be modified during execution.
pub static PLAN_MANAGER: std::sync::LazyLock<Arc<RwLock<PlanManager>>> =
    std::sync::LazyLock::new(|| Arc::new(RwLock::new(PlanManager::new())));

/// Check if the given tool is allowed in plan mode.
///
/// In plan mode, only read-only tools are allowed. This delegates to
/// `tool_is_read_only()` which is the source of truth for tool categorization.
///
/// # Read-only tools (allowed)
/// - read, glob, grep, web_fetch, web_search, ask_user, todo_write
/// - enter_plan_mode, exit_plan_mode
/// - event_bus_list_*, event_bus_get_events, task_output
///
/// # Write tools (blocked)
/// - edit, write, bash, kill_shell, task
/// - event_bus_register, event_bus_publish, event_bus_unregister
pub fn is_tool_allowed_in_plan_mode(tool_name: &str) -> bool {
    crate::tools::tool_is_read_only(tool_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_manager_enter_exit() {
        let mut manager = PlanManager::new();

        assert!(!manager.is_in_plan_mode());

        manager.enter_plan_mode(None).unwrap();
        assert!(manager.is_in_plan_mode());
        assert!(manager.plan_file_path().is_some());

        // Can't enter plan mode twice
        assert!(manager.enter_plan_mode(None).is_err());

        assert!(manager.exit_plan_mode());
        assert!(!manager.is_in_plan_mode());
    }

    #[test]
    fn test_plan_manager_create_plan() {
        let mut manager = PlanManager::new();
        manager.enter_plan_mode(None).unwrap();

        let plan = manager.create_plan(vec![
            PlanEntryInput {
                content: "Step 1".to_string(),
                priority: PlanEntryPriority::High,
            },
            PlanEntryInput {
                content: "Step 2".to_string(),
                priority: PlanEntryPriority::Medium,
            },
        ]);

        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].content, "Step 1");
        assert_eq!(plan.entries[0].priority, acp::PlanEntryPriority::High);
        assert_eq!(plan.entries[0].status, acp::PlanEntryStatus::Pending);
    }

    #[test]
    fn test_plan_manager_update_status() {
        let mut manager = PlanManager::new();
        manager.enter_plan_mode(None).unwrap();

        manager.create_plan(vec![PlanEntryInput {
            content: "Step 1".to_string(),
            priority: PlanEntryPriority::High,
        }]);

        manager
            .update_entry_status(0, PlanEntryStatus::InProgress)
            .unwrap();

        let plan = manager.current_plan().unwrap();
        assert_eq!(plan.entries[0].status, acp::PlanEntryStatus::InProgress);

        // Out of range
        assert!(
            manager
                .update_entry_status(5, PlanEntryStatus::Completed)
                .is_err()
        );
    }

    #[test]
    fn test_is_tool_allowed_in_plan_mode() {
        // Allowed tools (read-only)
        assert!(is_tool_allowed_in_plan_mode("read"));
        assert!(is_tool_allowed_in_plan_mode("glob"));
        assert!(is_tool_allowed_in_plan_mode("grep"));
        assert!(is_tool_allowed_in_plan_mode("web_fetch"));
        assert!(is_tool_allowed_in_plan_mode("web_search"));
        assert!(is_tool_allowed_in_plan_mode("ask_user"));
        assert!(is_tool_allowed_in_plan_mode("todo_write"));
        assert!(is_tool_allowed_in_plan_mode("task_output"));
        assert!(is_tool_allowed_in_plan_mode("event_bus_list_sessions"));
        assert!(is_tool_allowed_in_plan_mode("event_bus_list_channels"));
        assert!(is_tool_allowed_in_plan_mode("event_bus_get_events"));

        // Blocked tools (write/side effects)
        assert!(!is_tool_allowed_in_plan_mode("edit"));
        assert!(!is_tool_allowed_in_plan_mode("write"));
        assert!(!is_tool_allowed_in_plan_mode("bash"));
        assert!(!is_tool_allowed_in_plan_mode("kill_shell"));
        assert!(!is_tool_allowed_in_plan_mode("task"));
        assert!(!is_tool_allowed_in_plan_mode("event_bus_register"));
        assert!(!is_tool_allowed_in_plan_mode("event_bus_publish"));
        assert!(!is_tool_allowed_in_plan_mode("event_bus_unregister"));
    }
}
