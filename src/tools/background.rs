use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize};
use std::sync::{Arc, LazyLock, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::task::JoinHandle;

/// Global counter for generating unique task IDs.
pub static NEXT_TASK_ID: AtomicUsize = AtomicUsize::new(1);

/// Represents a running or completed background task.
pub struct BackgroundTask {
    /// The child process (if still running).
    /// Optional because we might want to take it out to kill it or wait on it.
    pub child: Option<Child>,

    /// Captured stdout buffer.
    pub stdout_buffer: Arc<Mutex<String>>,

    /// Captured stderr buffer.
    pub stderr_buffer: Arc<Mutex<String>>,

    /// Whether the task has completed.
    pub completed: Arc<AtomicBool>,

    /// Exit code (only valid if completed is true).
    pub exit_code: Arc<AtomicI32>,

    /// Handle for the output collection task (stdout).
    pub stdout_task: Option<JoinHandle<()>>,

    /// Handle for the output collection task (stderr).
    pub stderr_task: Option<JoinHandle<()>>,
}

/// Global registry of background tasks.
pub static BACKGROUND_TASKS: LazyLock<Mutex<HashMap<String, BackgroundTask>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

impl BackgroundTask {
    /// Create a new background task from a spawned child process.
    /// Starts background tasks to collect stdout and stderr.
    pub fn new(mut child: Child) -> Self {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let stdout_buffer = Arc::new(Mutex::new(String::new()));
        let stderr_buffer = Arc::new(Mutex::new(String::new()));
        let completed = Arc::new(AtomicBool::new(false));
        let exit_code = Arc::new(AtomicI32::new(0));

        let stdout_task = stdout.map(|s| spawn_output_collector(s, stdout_buffer.clone()));
        let stderr_task = stderr.map(|s| spawn_output_collector(s, stderr_buffer.clone()));

        // Status is checked lazily via update_status() when TaskOutput is called.

        Self {
            child: Some(child),
            stdout_buffer,
            stderr_buffer,
            completed,
            exit_code,
            stdout_task,
            stderr_task,
        }
    }

    /// Check if the process has exited and update status fields.
    ///
    /// Exit code conventions:
    /// - Normal exit: the process's actual exit code
    /// - Killed by signal (no code): exit_code unchanged (0)
    /// - Error checking status: -1
    pub fn update_status(&mut self) {
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.completed
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    if let Some(code) = status.code() {
                        self.exit_code
                            .store(code, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                Ok(None) => {} // Still running
                Err(e) => {
                    tracing::warn!("Error checking process status: {}", e);
                    self.completed
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    self.exit_code
                        .store(-1, std::sync::atomic::Ordering::SeqCst);
                }
            }
        } else {
            // Child was taken (killed) - mark as completed
            self.completed
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// Helper to spawn a task that reads a stream into a buffer.
fn spawn_output_collector<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    stream: R,
    buffer: Arc<Mutex<String>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stream).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let mut buf = buffer.lock().unwrap();
            buf.push_str(&line);
            buf.push('\n');
            // Limit buffer size to prevent memory exhaustion
            if buf.len() > 1_000_000 {
                let len = buf.len();
                buf.truncate(1_000_000);
                buf.push_str(&format!("\n... [truncated, {} bytes total]", len));
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::sync::atomic::Ordering;
    use tokio::process::Command;
    use tokio::time::{Duration, sleep};

    #[tokio::test]
    async fn test_background_task_new_initial_state() {
        let child = Command::new("echo")
            .arg("test")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let task = BackgroundTask::new(child);

        // Initial state: not completed, exit code 0, empty buffers
        assert!(!task.completed.load(Ordering::SeqCst));
        assert_eq!(task.exit_code.load(Ordering::SeqCst), 0);
        assert!(task.child.is_some());
        assert!(task.stdout_task.is_some());
        assert!(task.stderr_task.is_some());
    }

    #[tokio::test]
    async fn test_background_task_captures_stdout() {
        let child = Command::new("echo")
            .arg("hello_stdout")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let task = BackgroundTask::new(child);

        // Wait for output collection
        sleep(Duration::from_millis(100)).await;

        let stdout = task.stdout_buffer.lock().unwrap();
        assert!(stdout.contains("hello_stdout"));
    }

    #[tokio::test]
    async fn test_background_task_captures_stderr() {
        let child = Command::new("sh")
            .arg("-c")
            .arg("echo hello_stderr >&2")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let task = BackgroundTask::new(child);

        // Wait for output collection
        sleep(Duration::from_millis(100)).await;

        let stderr = task.stderr_buffer.lock().unwrap();
        assert!(stderr.contains("hello_stderr"));
    }

    #[tokio::test]
    async fn test_update_status_detects_completion() {
        let child = Command::new("echo")
            .arg("done")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let mut task = BackgroundTask::new(child);

        // Wait for process to complete
        sleep(Duration::from_millis(100)).await;

        task.update_status();

        assert!(task.completed.load(Ordering::SeqCst));
        assert_eq!(task.exit_code.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_update_status_captures_nonzero_exit_code() {
        let child = Command::new("sh")
            .arg("-c")
            .arg("exit 42")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let mut task = BackgroundTask::new(child);

        // Wait for process to complete
        sleep(Duration::from_millis(100)).await;

        task.update_status();

        assert!(task.completed.load(Ordering::SeqCst));
        assert_eq!(task.exit_code.load(Ordering::SeqCst), 42);
    }

    #[tokio::test]
    async fn test_update_status_running_task() {
        let child = Command::new("sleep")
            .arg("10")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let mut task = BackgroundTask::new(child);

        // Check immediately - should still be running
        task.update_status();
        assert!(!task.completed.load(Ordering::SeqCst));

        // Clean up
        if let Some(mut child) = task.child.take() {
            let _ = child.kill().await;
        }
    }

    #[tokio::test]
    async fn test_update_status_after_child_taken() {
        let child = Command::new("echo")
            .arg("test")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let mut task = BackgroundTask::new(child);

        // Take the child (simulating kill_shell behavior)
        let _ = task.child.take();

        // update_status should mark as completed when child is None
        task.update_status();
        assert!(task.completed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_next_task_id_increments() {
        let id1 = NEXT_TASK_ID.fetch_add(1, Ordering::SeqCst);
        let id2 = NEXT_TASK_ID.fetch_add(1, Ordering::SeqCst);
        assert_eq!(id2, id1 + 1);
    }
}
