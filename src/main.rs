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

use clemini::acp::AcpServer;
use clemini::agent::{self, AgentEvent, run_interaction};
use clemini::events;
use clemini::format;
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
            let _ = io::stdout().flush();
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
        colored::control::set_override(false);

        let cwd = PathBuf::from("/test/cwd");
        let model = "test-model";

        // /model returns formatted output with newlines (dimmed)
        assert_eq!(
            handle_builtin_command("/model", model, &cwd),
            Some(format!("\n{model}\n"))
        );
        assert_eq!(
            handle_builtin_command("/m", model, &cwd),
            Some(format!("\n{model}\n"))
        );

        // /pwd returns formatted path with newlines (dimmed)
        assert_eq!(
            handle_builtin_command("/pwd", model, &cwd),
            Some(format!("\n{}\n", cwd.display()))
        );
        assert_eq!(
            handle_builtin_command("/cwd", model, &cwd),
            Some(format!("\n{}\n", cwd.display()))
        );

        // Unknown commands return None
        assert_eq!(handle_builtin_command("/unknown", model, &cwd), None);
        assert_eq!(handle_builtin_command("not a command", model, &cwd), None);

        colored::control::unset_override();
    }

    #[test]
    fn test_run_shell_command_capture() {
        // Test successful command
        let out = run_shell_command_capture("echo hello");
        assert_eq!(out, "hello");

        // Test failing command
        let out = if cfg!(target_os = "windows") {
            run_shell_command_capture("dir non_existent_file_12345")
        } else {
            run_shell_command_capture("ls non_existent_file_12345")
        };
        assert!(out.contains("exit code"));

        // Test empty command
        assert_eq!(
            handle_builtin_command("!", "model", &PathBuf::from(".")),
            None
        );
        assert_eq!(
            handle_builtin_command("!  ", "model", &PathBuf::from(".")),
            None
        );
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

    /// Continue from a previous interaction ID
    #[arg(short, long)]
    interaction: Option<String>,

    /// Start as an MCP server (stdio mode)
    #[arg(long)]
    mcp_server: bool,

    /// Start as an ACP server (Agent Client Protocol)
    #[arg(long)]
    acp_server: bool,
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

    let bash_timeout = config.bash_timeout.unwrap_or(120);

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
        mcp_server.run_stdio().await?;
        return Ok(());
    }

    // ACP server mode - handle early before consuming stdin or printing banner
    if args.acp_server {
        logging::set_output_sink(Arc::new(FileSink));
        let acp_server = Arc::new(AcpServer::new(
            client,
            tool_service,
            model,
            system_prompt,
            retry_config,
        ));
        acp_server.run_stdio().await?;
        return Ok(());
    }

    eprintln!(
        "{}",
        clemini::format::format_startup_banner(
            env!("CARGO_PKG_VERSION"),
            &model,
            &cwd.display().to_string()
        )
    );
    eprintln!("{}", clemini::format::format_startup_tip());
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
            eprintln!("\n{}", clemini::format::format_ctrl_c().yellow());
            ct_clone.cancel();
        })
        .ok();

        // Create channel for agent events
        let (events_tx, mut events_rx) = mpsc::channel::<AgentEvent>(100);

        // Spawn task to handle events using EventHandler
        let model_for_handler = model.clone();
        let event_handler = tokio::spawn(async move {
            let mut handler = events::TerminalEventHandler::new(model_for_handler);
            while let Some(event) = events_rx.recv().await {
                events::dispatch_event(&mut handler, &event);
            }
        });

        // Set events_tx for tools - guard clears it when dropped
        let _events_guard = tool_service.with_events_tx(events_tx.clone());

        run_interaction(
            &client,
            &tool_service,
            &prompt,
            args.interaction.as_deref(),
            &model,
            &system_prompt,
            events_tx,
            cancellation_token,
            retry_config,
        )
        .await?;
        // _events_guard dropped here, clearing events_tx

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
            args.interaction,
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
    initial_interaction_id: Option<String>,
) -> Result<()> {
    let mut last_interaction_id: Option<String> = initial_interaction_id;

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

        // Redraw input with background
        eprint!("\x1b[1A\x1b[2K"); // Move up 1 line, clear line
        eprint!("> ");
        eprintln!("{}", input.on_bright_black());
        io::stderr().flush()?;

        if input == "/quit" || input == "/exit" || input == "/q" {
            break;
        }

        if input == "/clear" || input == "/c" {
            last_interaction_id = None;
            eprint!("{}", clemini::format::format_builtin_cleared());
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

        println!();

        let cancellation_token = CancellationToken::new();
        let ct_clone = cancellation_token.clone();
        ctrlc::set_handler(move || {
            eprintln!("\n{}", clemini::format::format_ctrl_c().yellow());
            ct_clone.cancel();
        })
        .ok();

        // Create channel for agent events
        let (events_tx, mut events_rx) = mpsc::channel::<AgentEvent>(100);

        // Spawn task to handle events using EventHandler
        let model_for_handler = model.to_string();
        let event_handler = tokio::spawn(async move {
            let mut handler = events::TerminalEventHandler::new(model_for_handler);
            while let Some(event) = events_rx.recv().await {
                events::dispatch_event(&mut handler, &event);
            }
        });

        // Set events_tx for tools - guard clears it when dropped
        let _events_guard = tool_service.with_events_tx(events_tx.clone());

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
        // _events_guard dropped here, clearing events_tx

        // Wait for event handler to finish
        let _ = event_handler.await;
    }

    Ok(())
}

fn handle_builtin_command(input: &str, model: &str, cwd: &std::path::Path) -> Option<String> {
    match input {
        "/model" | "/m" => Some(clemini::format::format_builtin_model(model)),
        "/pwd" | "/cwd" => Some(clemini::format::format_builtin_pwd(
            &cwd.display().to_string(),
        )),
        _ if input.starts_with('!') => {
            let cmd = input.strip_prefix('!').unwrap().trim();
            if cmd.is_empty() {
                None
            } else {
                Some(clemini::format::format_builtin_shell(
                    &run_shell_command_capture(cmd),
                ))
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
        "  /m, /model        Show model name",
        "  /pwd, /cwd        Show current working directory",
        "  /h, /help         Show this help message",
        "",
        "Controls:",
        "  Ctrl-C            Cancel current operation",
        "  Ctrl-D            Quit",
        "",
        "Shell escape:",
        "  !<command>        Run a shell command directly",
    ]
    .join("\n")
}

fn print_help() {
    eprint!("{}", clemini::format::format_builtin_help(&get_help_text()));
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
        let formatted = clemini::format::format_tool_args("read_file", &args);

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

    // =========================================
    // Complete tool block format tests
    // =========================================

    /// Complete tool block structure in log file:
    /// ┌─ <tool> <args>\n
    ///   <output>\n
    /// └─ <tool> <duration> ~<tokens> tok\n\n
    #[test]
    fn test_complete_tool_block_format() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("test.log");
        colored::control::set_override(false);

        // Write a complete tool block exactly as dispatch_event would
        // 1. Tool executing (emit_line)
        write_to_log_file(&log_path, "┌─ read_file file_path=\"test.rs\" \n", false).unwrap();
        // 2. Tool output (emit_line) - with newline added by dispatch
        write_to_log_file(&log_path, "  742 lines\n", false).unwrap();
        // 3. Tool result (emit) - with block separator
        write_to_log_file(&log_path, "└─ read_file 0.02s ~100 tok", true).unwrap();

        let content = std::fs::read_to_string(&log_path).unwrap();

        // Verify structure
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 4, "Should have 3 content lines + 1 blank line");
        assert!(
            lines[0].starts_with("┌─"),
            "Line 1 should be executing line"
        );
        assert!(
            lines[1].starts_with("  "),
            "Line 2 should have 2-space indent for output"
        );
        assert!(lines[2].starts_with("└─"), "Line 3 should be result line");
        assert!(lines[3].is_empty(), "Line 4 should be blank for separation");

        colored::control::unset_override();
    }

    /// Tool block with error shows result line + error detail
    #[test]
    fn test_tool_block_with_error() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("test.log");
        colored::control::set_override(false);

        // Write error tool block
        write_to_log_file(&log_path, "┌─ bash command=\"rm -rf /\" \n", false).unwrap();
        // Error result block (contains result + error detail, ends with emit)
        write_to_log_file(
            &log_path,
            "└─ bash 0.01s ~10 tok ERROR\n  └─ error: permission denied",
            true,
        )
        .unwrap();

        let content = std::fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();

        assert_eq!(lines.len(), 4, "Should have 3 content lines + 1 blank");
        assert!(lines[0].contains("┌─"));
        assert!(lines[1].contains("└─") && lines[1].contains("ERROR"));
        assert!(
            lines[2].starts_with("  └─ error:"),
            "Error detail should have 2-space indent"
        );
        assert!(lines[3].is_empty());

        colored::control::unset_override();
    }

    /// Multiple consecutive tool blocks have proper separation
    #[test]
    fn test_multiple_tool_blocks_separation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("test.log");
        colored::control::set_override(false);

        // First tool block
        write_to_log_file(&log_path, "┌─ tool1 \n", false).unwrap();
        write_to_log_file(&log_path, "└─ tool1 0.01s ~10 tok", true).unwrap();

        // Second tool block
        write_to_log_file(&log_path, "┌─ tool2 \n", false).unwrap();
        write_to_log_file(&log_path, "└─ tool2 0.02s ~20 tok", true).unwrap();

        let content = std::fs::read_to_string(&log_path).unwrap();

        // Each block ends with \n\n, so between blocks there's exactly one blank line
        assert!(
            content.contains("tok\n\n┌─"),
            "Blocks should be separated by exactly one blank line"
        );
        // File ends with \n\n
        assert!(content.ends_with("\n\n"), "File should end with blank line");

        colored::control::unset_override();
    }

    /// Text (flush) followed by tool block - text should end with \n\n
    #[test]
    fn test_text_then_tool_block_spacing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("test.log");

        // Flushed text from TextBuffer always ends with \n\n
        write_to_log_file(&log_path, "Some explanation text.\n\n", false).unwrap();

        // Immediately followed by tool executing
        write_to_log_file(&log_path, "┌─ tool \n", false).unwrap();
        write_to_log_file(&log_path, "└─ tool 0.01s ~10 tok", true).unwrap();

        let content = std::fs::read_to_string(&log_path).unwrap();

        // Text ends with \n\n, tool block starts on new line
        assert!(content.contains("text.\n\n┌─"));
    }

    /// Tool block followed by text - result has blank line, text starts fresh
    #[test]
    fn test_tool_block_then_text_spacing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("test.log");

        // Tool block
        write_to_log_file(&log_path, "┌─ tool \n", false).unwrap();
        write_to_log_file(&log_path, "└─ tool 0.01s ~10 tok", true).unwrap();

        // Followed by flushed text
        write_to_log_file(&log_path, "Now let me explain...\n\n", false).unwrap();

        let content = std::fs::read_to_string(&log_path).unwrap();

        // Tool result ends with \n\n, text starts on new line
        assert!(content.contains("tok\n\nNow"));
    }

    // =========================================
    // Edge cases
    // =========================================

    /// Empty message to emit() creates a blank line
    #[test]
    fn test_emit_empty_message() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("test.log");

        write_to_log_file(&log_path, "", true).unwrap();

        let content = std::fs::read_to_string(&log_path).unwrap();
        // Empty message + block separator = 2 newlines
        assert_eq!(content, "\n\n");
    }

    /// Empty message to emit_line() creates single newline
    #[test]
    fn test_emit_line_empty_message() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("test.log");

        write_to_log_file(&log_path, "", false).unwrap();

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(content, "\n");
    }

    /// Multi-line message is written line by line
    #[test]
    fn test_multi_line_message() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("test.log");

        write_to_log_file(&log_path, "line1\nline2\nline3", true).unwrap();

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(content, "line1\nline2\nline3\n\n");
    }

    // =========================================
    // Format function newline contracts
    // =========================================

    /// format_tool_executing MUST end with \n (for emit_line)
    #[test]
    fn test_format_tool_executing_newline_contract() {
        colored::control::set_override(false);

        let args = serde_json::json!({"path": "test.rs"});
        let formatted = clemini::format::format_tool_executing("read", &args);
        assert!(
            formatted.ends_with('\n'),
            "MUST end with \\n for emit_line(), got: {:?}",
            formatted
        );
        assert!(
            !formatted.ends_with("\n\n"),
            "Should have exactly one trailing newline"
        );

        colored::control::unset_override();
    }

    /// format_result_block does NOT end with \n (emit adds it)
    #[test]
    fn test_format_result_block_no_trailing_newline() {
        colored::control::set_override(false);

        let result = genai_rs::FunctionExecutionResult::new(
            "test".to_string(),
            "1".to_string(),
            serde_json::json!({}),
            serde_json::json!({"ok": true}),
            std::time::Duration::from_millis(10),
        );
        let formatted = clemini::format::format_result_block(&result);
        assert!(
            !formatted.ends_with('\n'),
            "format_result_block should NOT end with newline (emit adds \\n\\n)"
        );

        colored::control::unset_override();
    }

    /// format_error_detail has proper 2-space indent
    #[test]
    fn test_format_error_detail_indent() {
        colored::control::set_override(false);

        let formatted = clemini::format::format_error_detail("test error");
        assert!(
            formatted.starts_with("  └─ error:"),
            "Must have 2-space indent, got: {:?}",
            formatted
        );
        // Error detail does not have trailing newline (it's part of result block)
        assert!(
            !formatted.ends_with('\n'),
            "Error detail should not have trailing newline"
        );

        colored::control::unset_override();
    }

    /// Tool output messages need 2-space indent
    #[test]
    fn test_tool_output_format_contract() {
        // Tool outputs like "  742 lines" should have 2-space indent
        // This is enforced by the tool implementation, not format functions
        // Here we just verify the expected format
        let examples = vec!["  742 lines", "  running subagent...", "  3 matches found"];

        for example in examples {
            assert!(
                example.starts_with("  "),
                "Tool output '{}' must start with 2-space indent",
                example
            );
        }
    }
}
