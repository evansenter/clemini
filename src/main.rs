use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use genai_rs::Client;
use serde::Deserialize;
use std::env;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

mod mcp;

use clemini::agent::{self, AgentEvent, run_interaction};
use clemini::events;
use clemini::logging::OutputSink;
use clemini::tools::{self, CleminiToolService};

const DEFAULT_MODEL: &str = "gemini-3-flash-preview";

/// Initialize logging by ensuring the log directory exists.
/// Human-readable logs go through log_event().
pub fn init_logging() {
    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".clemini/logs");

    let _ = std::fs::create_dir_all(&log_dir);
}

// Re-export logging module to enable `crate::logging::` imports in mcp.rs.
// This wrapper is needed because mcp.rs uses `crate::logging::log_event` paths,
// and we can't use `clemini::logging` directly as `crate::` in the binary crate.
pub(crate) mod logging {
    pub use clemini::logging::*;
}

/// Writes to log files only (for MCP mode)
pub struct FileSink;

impl OutputSink for FileSink {
    fn emit(&self, message: &str) {
        log_event_to_file(message, true);
    }
    fn emit_line(&self, message: &str) {
        log_event_to_file(message, false);
    }
}

/// Writes to stderr AND log files (for REPL mode)
pub struct TerminalSink;

impl OutputSink for TerminalSink {
    fn emit(&self, message: &str) {
        // Print message with blank line after for visual separation
        if message.is_empty() {
            println!();
        } else {
            println!("{}\n", message);
        }
        log_event_to_file(message, true);
    }
    fn emit_line(&self, message: &str) {
        // Print message without adding newline (message already contains its own newlines)
        if message.is_empty() {
            println!();
        } else {
            print!("{}", message);
        }
        log_event_to_file(message, false);
    }
}

/// Log to file only (skip terminal output even with TerminalSink)
pub fn log_to_file(message: &str) {
    log_event_to_file(message, true);
}

fn log_event_to_file(message: &str, with_block_separator: bool) {
    // Skip logging during tests unless explicitly enabled
    if !logging::is_logging_enabled() {
        return;
    }

    colored::control::set_override(true);

    // Write to the stable log location: clemini.log.YYYY-MM-DD
    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".clemini/logs");
    let _ = std::fs::create_dir_all(&log_dir);

    let today = chrono::Local::now().format("%Y-%m-%d");
    let log_path = log_dir.join(format!("clemini.log.{}", today));

    let _ = write_to_log_file(&log_path, message, with_block_separator);

    // Also write to CLEMINI_LOG if set (backwards compat)
    if let Ok(path) = std::env::var("CLEMINI_LOG") {
        let _ = write_to_log_file(PathBuf::from(path), message, with_block_separator);
    }
}

fn write_to_log_file(
    path: impl Into<PathBuf>,
    rendered: &str,
    with_block_separator: bool,
) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.into())?;

    // Write content lines
    if rendered.is_empty() {
        writeln!(file)?;
    } else {
        for line in rendered.lines() {
            writeln!(file, "{}", line)?;
        }
    }
    // Add blank line after for visual separation between blocks
    if with_block_separator {
        writeln!(file)?;
    }
    Ok(())
}

const SYSTEM_PROMPT: &str = include_str!("system_prompt.md");

fn expand_tilde(path_str: &str) -> PathBuf {
    if path_str.starts_with('~') {
        home::home_dir()
            .map(|h| h.join(path_str.trim_start_matches("~/").trim_start_matches('~')))
            .unwrap_or_else(|| PathBuf::from(path_str))
    } else {
        PathBuf::from(path_str)
    }
}

fn default_allowed_paths() -> Vec<String> {
    vec!["~/.clemini".to_string(), "~/Documents/projects".to_string()]
}

#[derive(Deserialize)]
struct Config {
    model: Option<String>,
    bash_timeout: Option<u64>,
    #[serde(default = "default_allowed_paths")]
    allowed_paths: Vec<String>,
    /// Maximum extra retries after initial failure. Default 2 = 3 total attempts.
    max_extra_retries: Option<u32>,
    retry_delay_base_secs: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: None,
            bash_timeout: None,
            allowed_paths: default_allowed_paths(),
            max_extra_retries: None,
            retry_delay_base_secs: None,
        }
    }
}

fn load_config() -> Config {
    home::home_dir()
        .map(|mut p| {
            p.push(".clemini");
            p.push("config.toml");
            p
        })
        .filter(|p| p.exists())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tilde() {
        let home = home::home_dir().expect("Home dir should exist");
        assert_eq!(expand_tilde("~/.clemini"), home.join(".clemini"));
        assert_eq!(
            expand_tilde("~/Documents/projects"),
            home.join("Documents/projects")
        );
        assert_eq!(expand_tilde("/tmp"), PathBuf::from("/tmp"));
    }

    #[test]
    fn test_config_defaults() {
        let config = Config::default();
        assert_eq!(
            config.allowed_paths,
            vec!["~/.clemini", "~/Documents/projects"]
        );
        assert!(config.model.is_none());
        assert!(config.bash_timeout.is_none());
    }

    #[test]
    fn test_config_deserialization_defaults() {
        let toml_str = r#"
            model = "test-model"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.model, Some("test-model".to_string()));
        assert_eq!(
            config.allowed_paths,
            vec!["~/.clemini", "~/Documents/projects"]
        );
    }

    #[test]
    fn test_config_deserialization_override() {
        let toml_str = r#"
            allowed_paths = ["/etc", "/var"]
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.allowed_paths, vec!["/etc", "/var"]);
    }

    #[test]
    fn test_handle_builtin_command_basic() {
        let cwd = PathBuf::from("/test/cwd");
        let model = "test-model";

        assert_eq!(
            handle_builtin_command("/version", model, &cwd),
            Some(format!("clemini v{} | {}", env!("CARGO_PKG_VERSION"), model))
        );
        assert_eq!(
            handle_builtin_command("/v", model, &cwd),
            Some(format!("clemini v{} | {}", env!("CARGO_PKG_VERSION"), model))
        );
        assert_eq!(
            handle_builtin_command("/model", model, &cwd),
            Some(model.to_string())
        );
        assert_eq!(
            handle_builtin_command("/m", model, &cwd),
            Some(model.to_string())
        );
        assert_eq!(
            handle_builtin_command("/pwd", model, &cwd),
            Some(cwd.display().to_string())
        );
        assert_eq!(
            handle_builtin_command("/cwd", model, &cwd),
            Some(cwd.display().to_string())
        );
        assert_eq!(handle_builtin_command("/unknown", model, &cwd), None);
        assert_eq!(handle_builtin_command("not a command", model, &cwd), None);
    }

    #[test]
    fn test_run_shell_command_capture() {
        // Test successful command
        let out = if cfg!(target_os = "windows") {
            run_shell_command_capture("echo hello")
        } else {
            run_shell_command_capture("echo hello")
        };
        assert_eq!(out, "hello");

        // Test failing command
        let out = if cfg!(target_os = "windows") {
            run_shell_command_capture("dir non_existent_file_12345")
        } else {
            run_shell_command_capture("ls non_existent_file_12345")
        };
        assert!(out.contains("exit code"));

        // Test empty command
        assert_eq!(handle_builtin_command("!", "model", &PathBuf::from(".")), None);
        assert_eq!(handle_builtin_command("!  ", "model", &PathBuf::from(".")), None);
    }

    #[test]
    fn test_run_git_command_capture() {
        let temp_dir = tempfile::tempdir().unwrap();
        let repo_path = temp_dir.path();

        // Helper to run git in the temp repo
        let run_git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(repo_path)
                .status()
                .unwrap();
        };

        // Initialize a git repo
        run_git(&["init", "-b", "main"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "Test User"]);

        // Test with no commits - git log returns error 128 on empty repo
        let out = run_git_command_capture_in_dir(&["log", "--oneline"], "no commits", repo_path);
        assert!(out.contains("error"));

        // Test status on empty repo (should be success but empty with --short)
        let out = run_git_command_capture_in_dir(&["status", "--short"], "clean working directory", repo_path);
        assert_eq!(out, "[clean working directory]");

        // Add a file and commit
        std::fs::write(repo_path.join("test.txt"), "hello").unwrap();
        run_git(&["add", "test.txt"]);
        run_git(&["commit", "-m", "initial commit"]);

        // Test with commits
        let out = run_git_command_capture_in_dir(&["log", "--oneline"], "no commits", repo_path);
        assert!(out.contains("initial commit"));

        // Test diff
        std::fs::write(repo_path.join("test.txt"), "world").unwrap();
        let out = run_git_command_capture_in_dir(&["diff"], "no diff", repo_path);
        assert!(out.contains("-hello"));
        assert!(out.contains("+world"));
    }

    /// Helper for testing git commands in a specific directory
    fn run_git_command_capture_in_dir(args: &[&str], empty_msg: &str, dir: &std::path::Path) -> String {
        match std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
        {
            Ok(o) => {
                if o.status.success() {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    if stdout.is_empty() {
                        format!("[{empty_msg}]")
                    } else {
                        stdout.trim().to_string()
                    }
                } else {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    format!("[git {} error: {}]", args[0], stderr.trim())
                }
            }
            Err(e) => format!("[failed to run git {}: {}]", args[0], e),
        }
    }

    // Note: Logging tests moved to src/logging.rs since they test lib functionality
}

#[derive(Parser)]
#[command(name = "clemini")]
#[command(version)]
#[command(about = "A Gemini-powered coding CLI")]
struct Args {
    /// Initial prompt to run (non-interactive mode)
    #[arg(short, long)]
    prompt: Option<String>,

    /// Read prompt from a file
    #[arg(short, long)]
    file: Option<std::path::PathBuf>,

    /// Working directory
    #[arg(short = 'C', long, default_value = ".")]
    cwd: String,

    /// Model to use
    #[arg(short, long)]
    model: Option<String>,

    /// Timeout for bash commands in seconds
    #[arg(long)]
    timeout: Option<u64>,

    /// Start as an MCP server (stdio mode)
    #[arg(long)]
    mcp_server: bool,

    /// Use HTTP transport for MCP server (requires --mcp-server)
    #[arg(long)]
    http: bool,

    /// HTTP port for MCP server (requires --http)
    #[arg(long, default_value_t = 8080)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    let args = Args::parse();
    let config = load_config();

    let model = args
        .model
        .or(config.model)
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    let bash_timeout = args.timeout.or(config.bash_timeout).unwrap_or(120);

    let api_key = env::var("GEMINI_API_KEY")
        .map_err(|e| anyhow::anyhow!("GEMINI_API_KEY environment variable not set: {}", e))?;
    let client = Client::new(api_key.clone());

    let cwd = std::fs::canonicalize(&args.cwd)?;

    // Resolve allowed paths
    let mut allowed_paths = Vec::new();
    // Always allowed: CWD and tmp
    allowed_paths.push(cwd.clone());
    allowed_paths.push(PathBuf::from("/tmp"));
    #[cfg(target_os = "macos")]
    allowed_paths.push(PathBuf::from("/private/tmp"));

    // Add paths from config (which includes defaults if not specified)
    for path_str in config.allowed_paths {
        allowed_paths.push(expand_tilde(&path_str));
    }

    let tool_service = Arc::new(CleminiToolService::new(
        cwd.clone(),
        bash_timeout,
        args.mcp_server,
        allowed_paths,
        api_key.clone(),
    ));
    // Note: events_tx is set per-interaction via tool_service.set_events_tx()

    let mut system_prompt = SYSTEM_PROMPT.to_string();
    if let Ok(claude_md) = std::fs::read_to_string(cwd.join("CLAUDE.md")) {
        let claude_md = claude_md.trim();
        if !claude_md.is_empty() {
            system_prompt.push_str("\n\n## Project Context\n\n");
            system_prompt.push_str(claude_md);
        }
    }

    let retry_config = agent::RetryConfig {
        max_extra_retries: config.max_extra_retries.unwrap_or(2),
        retry_delay_base: std::time::Duration::from_secs(config.retry_delay_base_secs.unwrap_or(1)),
    };

    // MCP server mode - handle early before consuming stdin or printing banner
    if args.mcp_server {
        logging::set_output_sink(Arc::new(FileSink));
        let mcp_server = Arc::new(mcp::McpServer::new(
            client,
            tool_service,
            model,
            system_prompt,
            retry_config,
        ));
        if args.http {
            mcp_server.run_http(args.port).await?;
        } else {
            mcp_server.run_stdio().await?;
        }
        return Ok(());
    }

    eprintln!(
        "{} v{} | {} | {}",
        "clemini".bold(),
        env!("CARGO_PKG_VERSION").cyan(),
        model.green(),
        cwd.display().to_string().yellow()
    );
    eprintln!(
        "{} Remember to take breaks during development!",
        "💡".yellow()
    );
    eprintln!();

    let mut piped_input = String::new();
    if !io::stdin().is_terminal() {
        io::stdin().read_to_string(&mut piped_input)?;
    }
    let piped_input = piped_input.trim();

    let mut user_prompt = args.prompt;
    if let Some(file_path) = args.file {
        let file_content = std::fs::read_to_string(file_path)?;
        user_prompt = Some(match user_prompt {
            Some(p) => format!("{p}\n---\n{file_content}"),
            None => file_content,
        });
    }

    let combined_prompt = if !piped_input.is_empty() {
        if let Some(p) = user_prompt {
            Some(format!("{piped_input}\n---\n{p}"))
        } else {
            Some(piped_input.to_string())
        }
    } else {
        user_prompt
    };

    if let Some(prompt) = combined_prompt {
        logging::set_output_sink(Arc::new(TerminalSink));
        // Non-interactive mode: run single prompt
        let cancellation_token = CancellationToken::new();
        let ct_clone = cancellation_token.clone();
        ctrlc::set_handler(move || {
            eprintln!("\n{}", "[ctrl-c received]".yellow());
            ct_clone.cancel();
        })
        .ok();

        // Create channel for agent events
        let (events_tx, mut events_rx) = mpsc::channel::<AgentEvent>(100);

        // Spawn task to handle events using EventHandler
        let event_handler = tokio::spawn(async move {
            let mut handler = events::TerminalEventHandler::new();
            while let Some(event) = events_rx.recv().await {
                events::dispatch_event(&mut handler, &event);
            }
        });

        // Set events_tx for tools to emit output through the event system
        tool_service.set_events_tx(Some(events_tx.clone()));

        run_interaction(
            &client,
            &tool_service,
            &prompt,
            None,
            &model,
            &system_prompt,
            events_tx,
            cancellation_token,
            retry_config,
        )
        .await?;

        // Clear events_tx after interaction
        tool_service.set_events_tx(None);

        // Wait for event handler to finish
        let _ = event_handler.await;
    } else {
        // Interactive REPL mode
        logging::set_output_sink(Arc::new(TerminalSink));
        run_plain_repl(
            &client,
            &tool_service,
            cwd,
            &model,
            system_prompt,
            retry_config,
        )
        .await?;
    }

    Ok(())
}

/// Plain text REPL
async fn run_plain_repl(
    client: &Client,
    tool_service: &Arc<CleminiToolService>,
    cwd: std::path::PathBuf,
    model: &str,
    system_prompt: String,
    retry_config: agent::RetryConfig,
) -> Result<()> {
    let mut last_interaction_id: Option<String> = None;

    loop {
        eprint!("> ");
        io::stderr().flush()?;

        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            break; // EOF
        }
        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        if input == "/quit" || input == "/exit" || input == "/q" {
            break;
        }

        if input == "/clear" || input == "/c" {
            last_interaction_id = None;
            eprintln!("[conversation cleared]");
            continue;
        }

        if input == "/help" || input == "/h" {
            print_help();
            continue;
        }

        // Handle other commands
        if let Some(response) = handle_builtin_command(input, model, &cwd) {
            eprintln!("{response}");
            continue;
        }

        let cancellation_token = CancellationToken::new();
        let ct_clone = cancellation_token.clone();
        ctrlc::set_handler(move || {
            eprintln!("\n{}", "[ctrl-c received]".yellow());
            ct_clone.cancel();
        })
        .ok();

        // Create channel for agent events
        let (events_tx, mut events_rx) = mpsc::channel::<AgentEvent>(100);

        // Spawn task to handle events using EventHandler
        let event_handler = tokio::spawn(async move {
            let mut handler = events::TerminalEventHandler::new();
            while let Some(event) = events_rx.recv().await {
                events::dispatch_event(&mut handler, &event);
            }
        });

        // Set events_tx for tools to emit output through the event system
        tool_service.set_events_tx(Some(events_tx.clone()));

        match run_interaction(
            client,
            tool_service,
            input,
            last_interaction_id.as_deref(),
            model,
            &system_prompt,
            events_tx,
            cancellation_token,
            retry_config,
        )
        .await
        {
            Ok(result) => {
                last_interaction_id = result.id.clone();
            }
            Err(e) => {
                eprintln!("\n{}", format!("[error: {e}]").bright_red());
            }
        }

        // Clear events_tx after interaction
        tool_service.set_events_tx(None);

        // Wait for event handler to finish
        let _ = event_handler.await;
    }

    Ok(())
}

fn handle_builtin_command(input: &str, model: &str, cwd: &std::path::Path) -> Option<String> {
    match input {
        "/version" | "/v" => Some(format!(
            "clemini v{} | {}",
            env!("CARGO_PKG_VERSION"),
            model
        )),
        "/model" | "/m" => Some(model.to_string()),
        "/pwd" | "/cwd" => Some(cwd.display().to_string()),
        "/diff" | "/d" => Some(run_git_command_capture(&["diff"], "no uncommitted changes")),
        "/status" | "/s" => Some(run_git_command_capture(
            &["status", "--short"],
            "clean working directory",
        )),
        "/log" | "/l" => Some(run_git_command_capture(
            &["log", "--oneline", "-5"],
            "no commits found",
        )),
        "/branch" | "/b" => Some(run_git_command_capture(&["branch"], "no branches found")),
        _ if input.starts_with('!') => {
            let cmd = input.strip_prefix('!').unwrap().trim();
            if cmd.is_empty() {
                None
            } else {
                Some(run_shell_command_capture(cmd))
            }
        }
        _ => None,
    }
}

fn get_help_text() -> String {
    [
        "Commands:",
        "  /q, /quit, /exit  Exit the REPL",
        "  /c, /clear        Clear conversation history",
        "  /v, /version      Show version and model",
        "  /m, /model        Show model name",
        "  /pwd, /cwd        Show current working directory",
        "  /d, /diff         Show git diff",
        "  /s, /status       Show git status",
        "  /l, /log          Show git log",
        "  /b, /branch       Show git branches",
        "  /h, /help         Show this help message",
        "",
        "Controls:",
        "  Ctrl-C            Cancel current operation",
        "  Ctrl-D            Quit",
        "",
        "Shell escape:",
        "  ! <command>       Run a shell command directly",
    ]
    .join("\n")
}

fn print_help() {
    eprintln!("{}", get_help_text());
}

fn run_git_command_capture(args: &[&str], empty_msg: &str) -> String {
    match std::process::Command::new("git").args(args).output() {
        Ok(o) => {
            if o.status.success() {
                let stdout = String::from_utf8_lossy(&o.stdout);
                if stdout.is_empty() {
                    format!("[{empty_msg}]")
                } else {
                    stdout.trim().to_string()
                }
            } else {
                let stderr = String::from_utf8_lossy(&o.stderr);
                format!("[git {} error: {}]", args[0], stderr.trim())
            }
        }
        Err(e) => format!("[failed to run git {}: {}]", args[0], e),
    }
}

fn run_shell_command_capture(command: &str) -> String {
    let output = if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", command])
            .output()
    } else {
        std::process::Command::new("sh")
            .args(["-c", command])
            .output()
    };

    match output {
        Ok(o) => {
            let mut result = String::new();
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            if !stdout.is_empty() {
                result.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str(&stderr);
            }
            if !o.status.success() {
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str(&format!("[exit code: {:?}]", o.status.code()));
            }
            result.trim().to_string()
        }
        Err(e) => format!("[failed to run command: {}]", e),
    }
}

// =============================================================================
// Tests for output formatting and logging
// =============================================================================

#[cfg(test)]
mod output_tests {
    use super::*;
    use serde_json::json;

    /// ToolExecuting events should format as: ┌─ <tool_name> <args>
    #[test]
    fn test_tool_executing_format() {
        let args = json!({"file_path": "src/main.rs", "limit": 100});
        let formatted = events::format_tool_args("read_file", &args);

        // Args should be formatted as key=value pairs
        assert!(formatted.contains("file_path="));
        assert!(formatted.contains("limit=100"));
    }

    // =========================================
    // Output spacing contract tests
    // =========================================

    /// write_to_log_file with with_block_separator=true adds trailing blank line
    #[test]
    fn test_write_to_log_file_with_blank_line() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("test.log");

        write_to_log_file(&log_path, "hello", true).unwrap();

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            content, "hello\n\n",
            "emit() should add trailing blank line"
        );
    }

    /// write_to_log_file with with_block_separator=false does NOT add trailing blank line
    #[test]
    fn test_write_to_log_file_without_blank_line() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("test.log");

        write_to_log_file(&log_path, "hello", false).unwrap();

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            content, "hello\n",
            "emit_line() should NOT add trailing blank line"
        );
    }

    /// Multiple emit_line calls produce consecutive lines without gaps
    #[test]
    fn test_emit_line_consecutive_calls() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("test.log");

        write_to_log_file(&log_path, "line1", false).unwrap();
        write_to_log_file(&log_path, "line2", false).unwrap();
        write_to_log_file(&log_path, "line3", false).unwrap();

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            content, "line1\nline2\nline3\n",
            "consecutive emit_line() calls should not have gaps"
        );
    }

    /// emit() after emit_line() creates proper block separation
    #[test]
    fn test_emit_after_emit_line_creates_separation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("test.log");

        // Simulate: tool executing (emit_line), tool output (emit_line), tool result (emit)
        write_to_log_file(&log_path, "┌─ tool", false).unwrap(); // emit_line
        write_to_log_file(&log_path, "  output", false).unwrap(); // emit_line
        write_to_log_file(&log_path, "└─ tool", true).unwrap(); // emit (ends block)

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            content, "┌─ tool\n  output\n└─ tool\n\n",
            "tool block should end with blank line for separation"
        );
    }
}
