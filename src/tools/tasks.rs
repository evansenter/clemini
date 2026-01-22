//! Unified task registry for background and ACP tasks.
//!
//! This module consolidates BACKGROUND_TASKS and ACP_TASKS into a single
//! registry with namespaced IDs to prevent collisions.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

// Re-export task types from their modules
pub use super::background::BackgroundTask;
pub use crate::acp_client::AcpTask;

/// Global counter for generating unique task IDs.
static NEXT_TASK_ID: AtomicUsize = AtomicUsize::new(1);

/// Generate a namespaced task ID.
///
/// - Background tasks: "bg-1", "bg-2", etc.
/// - ACP tasks: "acp-1", "acp-2", etc.
pub fn next_task_id(prefix: &str) -> String {
    format!("{}-{}", prefix, NEXT_TASK_ID.fetch_add(1, Ordering::SeqCst))
}

/// Unified task type that can hold either a background shell task or an ACP subagent task.
pub enum Task {
    /// Background bash command.
    Background(BackgroundTask),
    /// ACP subagent task.
    Acp(AcpTask),
}

impl Task {
    /// Check if the task has completed.
    pub fn is_completed(&self) -> bool {
        match self {
            Task::Background(task) => task.is_completed(),
            Task::Acp(task) => task.is_completed(),
        }
    }

    /// Get the task output (stdout for background, output_buffer for ACP).
    pub fn output(&self) -> String {
        match self {
            Task::Background(task) => task.stdout(),
            Task::Acp(task) => task.output(),
        }
    }

    /// Get error output (stderr for background, error for ACP).
    pub fn error(&self) -> Option<String> {
        match self {
            Task::Background(task) => {
                let stderr = task.stderr();
                if stderr.is_empty() {
                    None
                } else {
                    Some(stderr)
                }
            }
            Task::Acp(task) => task.error(),
        }
    }

    /// Get the exit code (only meaningful for completed background tasks).
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            Task::Background(task) => {
                if task.is_completed() {
                    Some(task.exit_code())
                } else {
                    None
                }
            }
            Task::Acp(_) => None, // ACP tasks don't have exit codes
        }
    }

    /// Get task type as a string.
    pub fn task_type(&self) -> &'static str {
        match self {
            Task::Background(_) => "background",
            Task::Acp(_) => "acp",
        }
    }

    /// Update status for background tasks (no-op for ACP).
    pub fn update_status(&mut self) {
        if let Task::Background(task) = self {
            task.update_status();
        }
    }

    /// Get as mutable BackgroundTask if this is a Background variant.
    pub fn as_background_mut(&mut self) -> Option<&mut BackgroundTask> {
        match self {
            Task::Background(task) => Some(task),
            Task::Acp(_) => None,
        }
    }

    /// Get as mutable AcpTask if this is an Acp variant.
    pub fn as_acp_mut(&mut self) -> Option<&mut AcpTask> {
        match self {
            Task::Background(_) => None,
            Task::Acp(task) => Some(task),
        }
    }
}

/// Global registry of all tasks (background and ACP).
pub static TASKS: LazyLock<Mutex<HashMap<String, Task>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Register a background task and return its ID.
pub fn register_background_task(task: BackgroundTask) -> String {
    let id = next_task_id("bg");
    let mut tasks = TASKS.lock().unwrap();
    tasks.insert(id.clone(), Task::Background(task));
    id
}

/// Register an ACP task and return its ID.
pub fn register_acp_task(task: AcpTask) -> String {
    let id = next_task_id("acp");
    let mut tasks = TASKS.lock().unwrap();
    tasks.insert(id.clone(), Task::Acp(task));
    id
}

/// Get a list of all task IDs.
pub fn list_task_ids() -> Vec<String> {
    let tasks = TASKS.lock().unwrap();
    tasks.keys().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tokio::process::Command;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_next_task_id_generates_unique_ids() {
        let id1 = next_task_id("test");
        let id2 = next_task_id("test");
        assert_ne!(id1, id2);
        assert!(id1.starts_with("test-"));
        assert!(id2.starts_with("test-"));
    }

    #[tokio::test]
    async fn test_register_background_task() {
        let child = Command::new("echo")
            .arg("test")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let task = BackgroundTask::new(child);
        let id = register_background_task(task);

        assert!(id.starts_with("bg-"));

        // Verify it's in the registry
        let tasks = TASKS.lock().unwrap();
        assert!(tasks.contains_key(&id));
    }

    #[tokio::test]
    async fn test_task_type_discrimination() {
        let child = Command::new("echo")
            .arg("test")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let bg_task = Task::Background(BackgroundTask::new(child));
        assert_eq!(bg_task.task_type(), "background");

        let (cancel_tx, _) = mpsc::channel(1);
        let acp_child = Command::new("echo")
            .arg("test")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let acp_task = Task::Acp(AcpTask::new(acp_child, cancel_tx));
        assert_eq!(acp_task.task_type(), "acp");
    }
}
