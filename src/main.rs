use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use futures_util::StreamExt;
use genai_rs::{Client, FunctionExecutionResult, OwnedFunctionCallInfo};
use ratatui::DefaultTerminal;
use serde::Deserialize;
use serde_json::Value;
use std::env;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use termimad::MadSkin;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tui_textarea::{Input, TextArea};

mod mcp;
mod tui;

use clemini::agent::{self, AgentEvent, InteractionResult, run_interaction};
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
        log_event_to_file(message);
    }
}

/// Writes to stderr AND log files (for REPL mode)
pub struct TerminalSink;

impl OutputSink for TerminalSink {
    fn emit(&self, message: &str) {
        if message.is_empty() {
            eprintln!();
        } else {
            eprintln!("{}", message);
        }
        log_event_to_file(message);
    }
}

/// Message types for TUI output channel
#[derive(Debug)]
pub enum TuiMessage {
    /// Complete line/message (uses append_to_chat)
    Line(String),
}

/// Channel for TUI output - global sender that TuiSink writes to
static TUI_OUTPUT_TX: OnceLock<mpsc::UnboundedSender<TuiMessage>> = OnceLock::new();

/// Set the TUI output channel sender
pub fn set_tui_output_channel(tx: mpsc::UnboundedSender<TuiMessage>) {
    let _ = TUI_OUTPUT_TX.set(tx);
}

/// Writes to TUI buffer (via channel) AND log files
pub struct TuiSink;

impl OutputSink for TuiSink {
    fn emit(&self, message: &str) {
        // Send to TUI via channel
        if let Some(tx) = TUI_OUTPUT_TX.get() {
            let _ = tx.send(TuiMessage::Line(message.to_string()));
        }
        log_event_to_file(message);
    }
}

/// Log to file only (skip terminal output even with TerminalSink)
pub fn log_to_file(message: &str) {
    log_event_to_file(message);
}

fn log_event_to_file(message: &str) {
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

    let _ = write_to_log_file(&log_path, message);

    // Also write to CLEMINI_LOG if set (backwards compat)
    if let Ok(path) = std::env::var("CLEMINI_LOG") {
        let _ = write_to_log_file(PathBuf::from(path), message);
    }
}

fn write_to_log_file(path: impl Into<PathBuf>, rendered: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.into())?;

    let rendered = rendered.trim_end();
    if rendered.is_empty() {
        writeln!(file)?;
    } else {
        for line in rendered.lines() {
            writeln!(file, "{}", line)?;
        }
    }
    Ok(())
}

const SYSTEM_PROMPT: &str = r#"You are clemini, a coding assistant. Be concise. Get things done.

## Workflow
1. **Understand** - Read files before editing. Never guess at contents.
   - See `#N` or `issue N`? Fetch it: `gh issue view N`
   - See `PR #N` or pull request reference? Fetch it: `gh pr view N`
   - Always look up references you don't already know about
2. **Plan** - For complex tasks, briefly state your approach before implementing.
3. **Execute** - Make changes. Output narration BEFORE each tool call.
4. **Verify** - Run tests/checks. Compilation passing ≠ working code.

## Communication Style
**ALWAYS narrate your work.** Before each tool call, output a brief status update explaining what you're about to do and why:
- Let me fetch the issue to understand the requirements...
- Reading the file to see the current implementation...
- I'll update the function to handle this edge case...

This is NOT optional. Users need to follow your thought process. One line per step, output text BEFORE calling tools. Do NOT wrap your narration in quotes.

## Tools

All tools return JSON. Success responses have relevant data fields. Errors have `{"error": "message", "error_code": "CODE"}`.

### File Operations
- `read_file(file_path, offset?, limit?)` - Read file contents with line numbers. Default limit is 2000 lines. If `truncated: true`, continue with `offset`.
- `edit(file_path, old_string, new_string, replace_all?)` - Surgical string replacement. Use for precise changes to existing files.
- `write_file(file_path, content, backup?)` - Create new files or completely overwrite. Use `edit` for modifications, `write_file` only for new files or full rewrites.

### Search
- `glob(pattern, directory?, sort?)` - Find files by pattern: `**/*.rs`, `src/**/*.ts`. Use for locating files.
- `grep(pattern, directory?, type?, output_mode?)` - Search file contents with regex. **Always prefer over `bash grep`.** Use for searching within files.

### Execution
- `bash(command, description?, confirmed?, run_in_background?, working_directory?)` - Shell commands: git, builds, tests. Destructive commands (rm, sudo, git push --force) return `{needs_confirmation: true}` - explain to the user what needs approval and wait. After user approves in conversation, retry with `confirmed: true`. Use `run_in_background: true` for long-running commands. For GitHub, use `gh`: `gh issue view 34`.
- `kill_shell(task_id)` - Kill a background bash task. Pass the `task_id` returned by `bash` with `run_in_background: true`.

### Interaction
- `ask_user(question, options?)` - **Use when uncertain.** Ask clarifying questions rather than guessing.
- `todo_write(todos)` - **ALWAYS use for multi-step tasks.** Create todos FIRST for tasks with 2+ steps. Each todo needs: `content` (imperative: "Run tests"), `activeForm` (continuous: "Running tests"), `status` (pending/in_progress/completed). Update as you work.

### Web
- `web_search(query)` - Search the web via DuckDuckGo.
- `web_fetch(url, prompt?)` - Fetch a URL. Use `prompt` to extract specific information.

## Verification
After changes, verify they work:
- Python: `pytest`, `python -m py_compile`
- Rust: `cargo check`, `cargo test`
- JavaScript/TypeScript: `npm test`, `tsc --noEmit`
- General: run the relevant test suite or try the changed functionality

## Refactoring
- Passing syntax/type checks ≠ working code. Test the specific feature you changed.
- Timeouts during testing usually mean broken code, not network issues (default bash timeout is 30s).
- For unfamiliar APIs, read source/docs first. If unavailable, ask the user.
- Before declaring complete, verify the changed functionality works end-to-end.

## Judgment
- Multiple valid approaches → Ask user preference.
- Ambiguous requirements → Ask for clarification.
- Simple, obvious task → Just do it.

## Avoid
- Editing files you haven't read
- Scope creep (adding unrequested features)
- Long explanations when action is needed
- Declaring success without functional verification
- Over-reaching: If asked to "remove unused X" and X IS used, report back—don't decide to remove the usage too
- Changing behavior beyond what was requested (removing a constant ≠ removing its functionality)

## Self-Improvement
When you discover patterns that would help future tasks:
- Update this system prompt (in `src/main.rs` SYSTEM_PROMPT) with the guidance
- Keep additions concise and broadly applicable
- This helps you get better over time
"#;

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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: None,
            bash_timeout: None,
            allowed_paths: default_allowed_paths(),
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

    /// Stream raw text output (non-interactive mode)
    #[arg(long)]
    stream: bool,

    /// Start as an MCP server (stdio mode)
    #[arg(long)]
    mcp_server: bool,

    /// Use HTTP transport for MCP server (requires --mcp-server)
    #[arg(long)]
    http: bool,

    /// HTTP port for MCP server (requires --http)
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// Disable TUI and use plain text mode
    #[arg(long)]
    no_tui: bool,
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

    let bash_timeout = args.timeout.or(config.bash_timeout).unwrap_or(30);

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

    // MCP server mode - handle early before consuming stdin or printing banner
    if args.mcp_server {
        logging::set_output_sink(Arc::new(FileSink));
        let mcp_server = Arc::new(mcp::McpServer::new(
            client,
            tool_service,
            model,
            system_prompt,
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

        // Spawn task to handle streaming events using EventHandler
        let stream_enabled = args.stream;
        let event_handler = tokio::spawn(async move {
            let mut handler = events::TerminalEventHandler::new(stream_enabled);
            while let Some(event) = events_rx.recv().await {
                events::dispatch_event(&mut handler, &event);
            }
        });

        // Set events_tx for tools to emit output through the event system
        tool_service.set_events_tx(Some(events_tx.clone()));

        let result = run_interaction(
            &client,
            &tool_service,
            &prompt,
            None,
            &model,
            &system_prompt,
            events_tx,
            cancellation_token,
        )
        .await?;

        // Clear events_tx after interaction
        tool_service.set_events_tx(None);

        // Wait for event handler to finish
        let _ = event_handler.await;

        // In non-streaming mode, render the final response
        if !args.stream && !result.response.is_empty() {
            let skin = MadSkin::default();
            skin.print_text(&result.response);
        }
    } else {
        // Interactive REPL mode
        // Use TUI if terminal and --no-tui not specified
        let use_tui = !args.no_tui && io::stderr().is_terminal();
        // Set output sink based on mode (TUI sets its own sink, plain REPL uses TerminalSink)
        if !use_tui {
            logging::set_output_sink(Arc::new(TerminalSink));
        }
        // TUI mode always needs streaming for incremental updates
        let stream_output = use_tui || args.stream;
        run_repl(
            &client,
            &tool_service,
            cwd,
            &model,
            stream_output,
            system_prompt,
            use_tui,
        )
        .await?;
    }

    Ok(())
}

/// Events from the async interaction task
enum AppEvent {
    StreamChunk(String),
    ToolExecuting {
        name: String,
        args: Value,
    },
    ToolCompleted {
        name: String,
        duration_ms: u64,
        tokens: u32,
        has_error: bool,
    },
    InteractionComplete(Result<InteractionResult>),
    ContextWarning(String),
    ToolOutput(String),
}

/// Convert AgentEvent to AppEvents for the TUI.
/// Returns a Vec because ToolExecuting can produce multiple AppEvents.
/// Note: This function is only used in tests; actual conversion happens in TuiEventHandler.
#[cfg(test)]
fn convert_agent_event_to_app_events(event: &AgentEvent) -> Vec<AppEvent> {
    match event {
        AgentEvent::TextDelta(text) => vec![AppEvent::StreamChunk(text.clone())],
        AgentEvent::ToolExecuting(calls) => calls
            .iter()
            .map(|call| AppEvent::ToolExecuting {
                name: call.name.clone(),
                args: call.args.clone(),
            })
            .collect(),
        AgentEvent::ToolResult(result) => vec![AppEvent::ToolCompleted {
            name: result.name.clone(),
            duration_ms: result.duration.as_millis() as u64,
            tokens: events::estimate_tokens(&result.args) + events::estimate_tokens(&result.result),
            has_error: result.is_error(),
        }],
        AgentEvent::ContextWarning(warning) => {
            vec![AppEvent::ContextWarning(events::format_context_warning(
                warning.percentage(),
            ))]
        }
        // Complete and Cancelled are handled differently (via join handle result)
        AgentEvent::Complete { .. } | AgentEvent::Cancelled => vec![],
        AgentEvent::ToolOutput(output) => vec![AppEvent::ToolOutput(output.clone())],
    }
}

/// TUI-specific event handler that sends AppEvents via channel.
/// Also logs events to file for `make logs` visibility.
struct TuiEventHandler {
    app_tx: mpsc::Sender<AppEvent>,
    text_buffer: events::TextBuffer,
}

impl TuiEventHandler {
    fn new(app_tx: mpsc::Sender<AppEvent>) -> Self {
        Self {
            app_tx,
            text_buffer: events::TextBuffer::new(),
        }
    }
}

impl events::EventHandler for TuiEventHandler {
    fn on_text_delta(&mut self, text: &str) {
        self.text_buffer.push(text);
    }

    fn on_tool_executing(&mut self, call: &OwnedFunctionCallInfo) {
        // Flush buffer before tool output (normalizes to \n\n for spacing)
        // Logging is handled by dispatch_event() after this method returns
        if let Some(rendered) = self.text_buffer.flush() {
            let _ = self
                .app_tx
                .try_send(AppEvent::StreamChunk(rendered.clone()));
            events::write_to_streaming_log(&rendered);
        }
        let _ = self.app_tx.try_send(AppEvent::ToolExecuting {
            name: call.name.clone(),
            args: call.args.clone(),
        });
    }

    fn on_tool_result(&mut self, result: &FunctionExecutionResult) {
        // Logging is handled by dispatch_event() after this method returns
        let tokens =
            events::estimate_tokens(&result.args) + events::estimate_tokens(&result.result);
        let _ = self.app_tx.try_send(AppEvent::ToolCompleted {
            name: result.name.clone(),
            duration_ms: result.duration.as_millis() as u64,
            tokens,
            has_error: result.is_error(),
        });
    }

    fn on_context_warning(&mut self, warning: &clemini::agent::ContextWarning) {
        let msg = events::format_context_warning(warning.percentage());
        let _ = self.app_tx.try_send(AppEvent::ContextWarning(msg.clone()));
        // Logging is handled by dispatch_event() after this method returns
    }

    fn on_complete(
        &mut self,
        _interaction_id: Option<&str>,
        _response: &genai_rs::InteractionResponse,
    ) {
        // Flush any remaining buffered text (normalizes to \n\n)
        if let Some(rendered) = self.text_buffer.flush() {
            let _ = self
                .app_tx
                .try_send(AppEvent::StreamChunk(rendered.clone()));
            events::write_to_streaming_log(&rendered);
        }
    }

    fn on_tool_output(&mut self, output: &str) {
        let _ = self
            .app_tx
            .try_send(AppEvent::ToolOutput(output.to_string()));
        // Logging is handled by dispatch_event() after this method returns
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_repl(
    client: &Client,
    tool_service: &Arc<CleminiToolService>,
    cwd: std::path::PathBuf,
    model: &str,
    stream_output: bool,
    system_prompt: String,
    use_tui: bool,
) -> Result<()> {
    if use_tui {
        run_tui_repl(
            client,
            tool_service,
            cwd,
            model,
            stream_output,
            system_prompt,
        )
        .await
    } else {
        run_plain_repl(
            client,
            tool_service,
            cwd,
            model,
            stream_output,
            system_prompt,
        )
        .await
    }
}

/// Plain text REPL for non-TTY or --no-tui mode
#[allow(clippy::too_many_arguments)]
async fn run_plain_repl(
    client: &Client,
    tool_service: &Arc<CleminiToolService>,
    cwd: std::path::PathBuf,
    model: &str,
    stream_output: bool,
    system_prompt: String,
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

        // Spawn task to handle streaming events using EventHandler
        let stream_enabled = stream_output;
        let event_handler = tokio::spawn(async move {
            let mut handler = events::TerminalEventHandler::new(stream_enabled);
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
        )
        .await
        {
            Ok(result) => {
                last_interaction_id = result.id.clone();
                // In non-streaming mode, render the final response
                if !stream_output && !result.response.is_empty() {
                    let skin = MadSkin::default();
                    skin.print_text(&result.response);
                }
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

/// TUI REPL with ratatui
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_tui_repl(
    client: &Client,
    tool_service: &Arc<CleminiToolService>,
    cwd: std::path::PathBuf,
    model: &str,
    stream_output: bool,
    system_prompt: String,
) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = run_tui_event_loop(
        &mut terminal,
        client,
        tool_service,
        cwd,
        model,
        stream_output,
        system_prompt,
    )
    .await;
    ratatui::restore();
    result
}

/// Create a configured TextArea for TUI input
fn create_textarea_with_content(content: Option<&str>) -> TextArea<'static> {
    use ratatui::style::Style;
    let mut textarea = match content {
        Some(text) => TextArea::new(vec![text.to_string()]),
        None => TextArea::default(),
    };
    textarea.set_block(
        ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .title(" Input (Enter to send, Ctrl-D to quit) "),
    );
    // Remove underline from cursor line (default has underline)
    textarea.set_cursor_line_style(Style::default());
    textarea
}

/// Create a configured TextArea for TUI input (empty)
fn create_textarea() -> TextArea<'static> {
    create_textarea_with_content(None)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_tui_event_loop(
    terminal: &mut DefaultTerminal,
    client: &Client,
    tool_service: &Arc<CleminiToolService>,
    cwd: std::path::PathBuf,
    model: &str,
    _stream_output: bool, // Always streams via channel in TUI mode
    system_prompt: String,
) -> Result<()> {
    // Set up TUI output channel (for OutputSink -> TUI)
    let (tui_tx, mut tui_rx) = mpsc::unbounded_channel::<TuiMessage>();
    set_tui_output_channel(tui_tx);
    logging::set_output_sink(Arc::new(TuiSink));

    let mut app = tui::App::new(model);
    let mut textarea = create_textarea();

    let mut event_stream = EventStream::new();
    let (tx, mut rx) = mpsc::channel::<AppEvent>(100);

    let mut last_interaction_id: Option<String> = None;
    let mut history: Vec<String> = Vec::new();
    let mut history_index: Option<usize> = None;

    // Load history from file
    if let Some(history_path) = home::home_dir().map(|p| p.join(".clemini_history"))
        && let Ok(content) = std::fs::read_to_string(&history_path)
    {
        history = content.lines().map(String::from).collect();
    }

    loop {
        // Render
        terminal.draw(|frame| {
            tui::render(frame, &app, textarea.lines().len() as u16);
            let input_area = tui::ui::get_input_area(frame, textarea.lines().len() as u16);
            frame.render_widget(&textarea, input_area);
        })?;

        // Handle events
        tokio::select! {
            // Keyboard input
            Some(Ok(event)) = event_stream.next() => {
                if let Event::Key(key) = event {
                    // Check for quit
                    if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) {
                        break;
                    }

                    // Check for cancel during streaming
                    if key.code == KeyCode::Esc && app.activity().is_busy() {
                        app.cancel();
                        app.append_to_chat(&"[cancelled]".yellow().to_string());
                        app.set_activity(tui::Activity::Idle);
                        continue;
                    }

                    // Handle Enter to submit
                    if key.code == KeyCode::Enter && !app.activity().is_busy() {
                        let input: String = textarea.lines().join("\n");
                        let input = input.trim();

                        if !input.is_empty() {
                            // Add to history
                            history.push(input.to_string());
                            history_index = None;

                            // Save to history file
                            if let Some(history_path) = home::home_dir().map(|p| p.join(".clemini_history")) {
                                let _ = std::fs::write(&history_path, history.join("\n"));
                            }

                            // Check for quit command
                            if input == "/quit" || input == "/exit" || input == "/q" {
                                break;
                            }

                            // Check for clear command
                            if input == "/clear" || input == "/c" {
                                last_interaction_id = None;
                                app.clear_chat();
                                app.estimated_tokens = 0;
                                textarea = create_textarea();
                                continue;
                            }

                            // Check for help command
                            if input == "/help" || input == "/h" {
                                app.append_to_chat(&get_help_text());
                                textarea = create_textarea();
                                continue;
                            }

                            // Handle other builtin commands
                            if let Some(response) = handle_builtin_command(input, model, &cwd) {
                                app.append_to_chat(&response);
                                textarea = create_textarea();
                                continue;
                            }

                            // Show user input in chat
                            app.append_to_chat(&format!("> {}", input.cyan()));
                            app.append_to_chat("");

                            // Start interaction
                            app.set_activity(tui::Activity::Streaming);
                            app.reset_cancellation();

                            let tx = tx.clone();
                            let client = client.clone();
                            let tool_service = tool_service.clone();
                            let input = input.to_string();
                            let prev_id = last_interaction_id.clone();
                            let model = model.to_string();
                            let system_prompt = system_prompt.clone();
                            let cancellation_flag = app.cancellation_flag();

                            tokio::spawn(async move {
                                let cancellation_token = CancellationToken::new();
                                let ct_clone = cancellation_token.clone();

                                // Watch for cancellation flag
                                let cancel_flag = cancellation_flag.clone();
                                tokio::spawn(async move {
                                    loop {
                                        if cancel_flag.load(std::sync::atomic::Ordering::SeqCst) {
                                            ct_clone.cancel();
                                            break;
                                        }
                                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                                    }
                                });

                                // Create channel for agent events
                                let (events_tx, mut events_rx) = mpsc::channel::<AgentEvent>(100);

                                // Spawn task to handle AgentEvents via TuiEventHandler
                                let app_tx = tx.clone();
                                tokio::spawn(async move {
                                    let mut handler = TuiEventHandler::new(app_tx);
                                    while let Some(event) = events_rx.recv().await {
                                        events::dispatch_event(&mut handler, &event);
                                    }
                                });

                                // Set events_tx for tools to emit output through the event system
                                tool_service.set_events_tx(Some(events_tx.clone()));

                                let result = run_interaction(
                                    &client,
                                    &tool_service,
                                    &input,
                                    prev_id.as_deref(),
                                    &model,
                                    &system_prompt,
                                    events_tx,
                                    cancellation_token,
                                )
                                .await;

                                // Clear events_tx after interaction
                                tool_service.set_events_tx(None);

                                let _ = tx.send(AppEvent::InteractionComplete(result)).await;
                            });

                            // Clear input
                            textarea = create_textarea();
                        }
                        continue;
                    }

                    // History navigation
                    if key.code == KeyCode::Up && !app.activity().is_busy() {
                        if !history.is_empty() {
                            let new_index = match history_index {
                                None => history.len().saturating_sub(1),
                                Some(i) => i.saturating_sub(1),
                            };
                            history_index = Some(new_index);
                            textarea = create_textarea_with_content(Some(&history[new_index]));
                        }
                        continue;
                    }

                    if key.code == KeyCode::Down && !app.activity().is_busy() {
                        if let Some(i) = history_index {
                            if i + 1 < history.len() {
                                history_index = Some(i + 1);
                                textarea = create_textarea_with_content(Some(&history[i + 1]));
                            } else {
                                history_index = None;
                                textarea = create_textarea();
                            }
                        }
                        continue;
                    }

                    // Scroll chat with Page Up/Down
                    if key.code == KeyCode::PageUp {
                        app.scroll_up(10);
                        continue;
                    }
                    if key.code == KeyCode::PageDown {
                        app.scroll_down(10);
                        continue;
                    }

                    // Pass other keys to textarea
                    if !app.activity().is_busy() {
                        textarea.input(Input::from(key));
                    }
                }
            }

            // Events from interaction task
            Some(event) = rx.recv() => {
                match event {
                    AppEvent::StreamChunk(text) => {
                        app.append_streaming(&text);
                    }
                    AppEvent::ToolExecuting { name, args } => {
                        app.set_activity(tui::Activity::Executing(name.clone()));
                        // Display tool call in chat - use same format as log output
                        app.append_to_chat(&events::format_tool_executing(&name, &args));
                    }
                    AppEvent::ToolCompleted { name, duration_ms, tokens, has_error } => {
                        // Tool completed - use same format as log output
                        let duration = std::time::Duration::from_millis(duration_ms);
                        let msg = events::format_tool_result(&name, duration, tokens, has_error);
                        app.append_to_chat(&msg);
                        app.append_to_chat(""); // Single blank line after tool completes
                    }
                    AppEvent::InteractionComplete(result) => {
                        app.set_activity(tui::Activity::Idle);
                        match result {
                            Ok(result) => {
                                last_interaction_id = result.id;
                                app.update_stats(result.context_size, result.tool_calls.len());
                            }
                            Err(e) => {
                                app.append_to_chat(&format!("[error: {}]", e).bright_red().to_string());
                            }
                        }
                    }
                    AppEvent::ContextWarning(msg) => {
                        app.append_to_chat("");
                        app.append_to_chat(&msg.bright_red().bold().to_string());
                        app.append_to_chat("");
                    }
                    AppEvent::ToolOutput(output) => {
                        app.append_to_chat(&output);
                    }
                }
            }

            // TUI output messages from OutputSink (via TuiSink)
            Some(message) = tui_rx.recv() => {
                match message {
                    TuiMessage::Line(text) => {
                        app.append_to_chat(&text);
                        // Add blank line after so streaming starts on new line
                        app.append_to_chat("");
                    }
                }
            }
        }
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
        "Navigation:",
        "  Up/Down           History navigation",
        "  PageUp/PageDown   Scroll chat",
        "  Esc               Cancel current operation",
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
// Tests for event handling consistency
// =============================================================================
// These tests document and verify how AgentEvents should be handled across
// all UI modes (non-interactive, plain REPL, TUI).

#[cfg(test)]
mod event_handling_tests {
    use super::*;
    use crate::events::EventHandler;
    use serde_json::json;
    use std::time::Duration;

    // =========================================
    // Event formatting tests
    // These verify the formatting used across all UI modes
    // =========================================

    /// ToolExecuting events should format as: ┌─ <tool_name> <args>
    #[test]
    fn test_tool_executing_format() {
        let args = json!({"file_path": "src/main.rs", "limit": 100});
        let formatted = events::format_tool_args("read_file", &args);

        // Args should be formatted as key=value pairs
        assert!(formatted.contains("file_path="));
        assert!(formatted.contains("limit=100"));
    }

    /// ToolResult events should format with duration and token estimate
    #[test]
    fn test_tool_result_format() {
        colored::control::set_override(false);

        let formatted =
            events::format_tool_result("read_file", Duration::from_millis(25), 150, false);

        assert!(formatted.contains("└─ read_file"));
        assert!(formatted.contains("0.02s") || formatted.contains("0.03s")); // timing can vary
        assert!(formatted.contains("~150 tok"));

        colored::control::unset_override();
    }

    /// ToolResult errors should include ERROR suffix
    #[test]
    fn test_tool_result_error_format() {
        colored::control::set_override(false);

        let formatted =
            events::format_tool_result("write_file", Duration::from_millis(10), 50, true);

        assert!(formatted.contains("└─ write_file"));
        assert!(formatted.contains("ERROR"));

        colored::control::unset_override();
    }

    /// ContextWarning should format with percentage
    #[test]
    fn test_context_warning_format_normal() {
        let percentage = 85.5;
        let msg = format!("WARNING: Context window at {:.1}%.", percentage);

        assert!(msg.contains("85.5%"));
        assert!(!msg.contains("/clear")); // Not critical yet
    }

    /// ContextWarning at critical level should suggest /clear
    #[test]
    fn test_context_warning_format_critical() {
        let percentage = 96.0;
        let msg = format!(
            "WARNING: Context window at {:.1}%. Use /clear to reset.",
            percentage
        );

        assert!(msg.contains("96.0%"));
        assert!(msg.contains("/clear")); // Critical level
    }

    // =========================================
    // Event handling contract tests
    // Document what each event type should produce
    // =========================================

    /// TextDelta should use streaming output (append to current line)
    /// NOT line-based output (which would break sentences)
    #[test]
    fn test_text_delta_requires_streaming() {
        // This test documents the contract: TextDelta MUST use streaming/append
        // because text arrives in chunks that form sentences.
        //
        // WRONG: append_to_chat("I'll") then append_to_chat(" search") = 2 lines
        // RIGHT: append_streaming("I'll") then append_streaming(" search") = 1 line

        let mut app = tui::App::new("test");
        app.append_streaming("I'll");
        app.append_streaming(" search");

        assert_eq!(app.chat_lines().len(), 1);
        assert_eq!(app.chat_lines()[0], "I'll search");
    }

    /// ToolExecuting should use line-based output (own line)
    #[test]
    fn test_tool_executing_requires_line_output() {
        // Tool calls should appear on their own line, not appended to streaming text
        let mut app = tui::App::new("test");

        app.append_streaming("Let me search.\n"); // Creates 2 lines: "Let me search." and ""
        app.append_to_chat("┌─ grep pattern=\"test\""); // Line-based, adds new line

        assert_eq!(app.chat_lines().len(), 3);
        assert_eq!(app.chat_lines()[0], "Let me search.");
        assert_eq!(app.chat_lines()[1], ""); // empty from \n
        assert!(app.chat_lines()[2].contains("grep"));
    }

    /// ToolResult should use line-based output (own line)
    #[test]
    fn test_tool_result_requires_line_output() {
        let mut app = tui::App::new("test");

        app.append_to_chat("┌─ read_file path=\"test.rs\"");
        app.append_to_chat("[read_file] 0.02s, ~100 tok");

        assert_eq!(app.chat_lines().len(), 2);
        assert!(app.chat_lines()[1].contains("read_file"));
    }

    // =========================================
    // Integration pattern tests
    // Verify the full streaming → tool → streaming flow
    // =========================================

    /// Complete flow: streaming text, tool call, tool result, more streaming
    #[test]
    fn test_full_event_flow_pattern() {
        let mut app = tui::App::new("test");

        // Model starts streaming response
        app.append_streaming("I'll search for ");
        app.append_streaming("the function.\n\n");

        // Tool executes (line-based)
        app.append_to_chat("┌─ grep pattern=\"fn main\"");

        // Tool result (line-based)
        app.append_to_chat("[grep] 0.01s, ~50 tok");
        app.append_to_chat(""); // Blank line after tool

        // Model continues streaming
        app.append_streaming("Found it in ");
        app.append_streaming("src/main.rs");

        // Verify structure
        assert_eq!(app.chat_lines().len(), 7);
        assert_eq!(app.chat_lines()[0], "I'll search for the function.");
        assert_eq!(app.chat_lines()[1], ""); // from \n\n
        assert_eq!(app.chat_lines()[2], ""); // from \n\n
        assert_eq!(app.chat_lines()[3], "┌─ grep pattern=\"fn main\"");
        assert_eq!(app.chat_lines()[4], "[grep] 0.01s, ~50 tok");
        assert_eq!(app.chat_lines()[5], "");
        assert_eq!(app.chat_lines()[6], "Found it in src/main.rs");
    }

    // =========================================
    // AgentEvent to AppEvent conversion tests
    // =========================================

    #[test]
    fn test_convert_text_delta() {
        let event = AgentEvent::TextDelta("Hello world".to_string());
        let app_events = convert_agent_event_to_app_events(&event);

        assert_eq!(app_events.len(), 1);
        match &app_events[0] {
            AppEvent::StreamChunk(text) => assert_eq!(text, "Hello world"),
            _ => panic!("Expected StreamChunk"),
        }
    }

    #[test]
    fn test_convert_tool_executing_single() {
        use genai_rs::OwnedFunctionCallInfo;

        let call = OwnedFunctionCallInfo {
            id: Some("call-1".to_string()),
            name: "read_file".to_string(),
            args: json!({"file_path": "test.txt"}),
        };
        let event = AgentEvent::ToolExecuting(vec![call]);
        let app_events = convert_agent_event_to_app_events(&event);

        assert_eq!(app_events.len(), 1);
        match &app_events[0] {
            AppEvent::ToolExecuting { name, args } => {
                assert_eq!(name, "read_file");
                assert_eq!(args["file_path"], "test.txt");
            }
            _ => panic!("Expected ToolExecuting"),
        }
    }

    #[test]
    fn test_convert_tool_executing_multiple() {
        use genai_rs::OwnedFunctionCallInfo;

        let calls = vec![
            OwnedFunctionCallInfo {
                id: Some("call-1".to_string()),
                name: "glob".to_string(),
                args: json!({"pattern": "*.rs"}),
            },
            OwnedFunctionCallInfo {
                id: Some("call-2".to_string()),
                name: "grep".to_string(),
                args: json!({"pattern": "fn main"}),
            },
        ];
        let event = AgentEvent::ToolExecuting(calls);
        let app_events = convert_agent_event_to_app_events(&event);

        assert_eq!(app_events.len(), 2);
        match &app_events[0] {
            AppEvent::ToolExecuting { name, .. } => assert_eq!(name, "glob"),
            _ => panic!("Expected ToolExecuting"),
        }
        match &app_events[1] {
            AppEvent::ToolExecuting { name, .. } => assert_eq!(name, "grep"),
            _ => panic!("Expected ToolExecuting"),
        }
    }

    #[test]
    fn test_convert_tool_result() {
        use genai_rs::FunctionExecutionResult;

        let result = FunctionExecutionResult::new(
            "bash".to_string(),
            "call-1".to_string(),
            json!({"command": "ls"}),
            json!({"output": "file.txt"}),
            Duration::from_millis(150),
        );
        let event = AgentEvent::ToolResult(result);
        let app_events = convert_agent_event_to_app_events(&event);

        assert_eq!(app_events.len(), 1);
        match &app_events[0] {
            AppEvent::ToolCompleted {
                name,
                duration_ms,
                tokens,
                has_error,
            } => {
                assert_eq!(name, "bash");
                assert_eq!(*duration_ms, 150);
                assert!(*tokens > 0); // Should have some token estimate
                assert!(!has_error); // Successful result
            }
            _ => panic!("Expected ToolCompleted"),
        }
    }

    #[test]
    fn test_convert_context_warning() {
        let event =
            AgentEvent::ContextWarning(clemini::agent::ContextWarning::new(900_000, 1_000_000));
        let app_events = convert_agent_event_to_app_events(&event);

        assert_eq!(app_events.len(), 1);
        match &app_events[0] {
            AppEvent::ContextWarning(msg) => {
                assert!(msg.contains("90.0%"));
            }
            _ => panic!("Expected ContextWarning"),
        }
    }

    #[test]
    fn test_convert_complete_returns_empty() {
        use genai_rs::{InteractionResponse, InteractionStatus};

        let response = InteractionResponse {
            id: Some("test-id".to_string()),
            model: None,
            agent: None,
            input: vec![],
            outputs: vec![],
            status: InteractionStatus::Completed,
            usage: None,
            tools: None,
            grounding_metadata: None,
            url_context_metadata: None,
            previous_interaction_id: None,
            created: None,
            updated: None,
        };
        let event = AgentEvent::Complete {
            interaction_id: Some("test-id".to_string()),
            response: Box::new(response),
        };
        let app_events = convert_agent_event_to_app_events(&event);

        assert!(
            app_events.is_empty(),
            "Complete should not produce AppEvents"
        );
    }

    #[test]
    fn test_convert_cancelled_returns_empty() {
        let event = AgentEvent::Cancelled;
        let app_events = convert_agent_event_to_app_events(&event);

        assert!(
            app_events.is_empty(),
            "Cancelled should not produce AppEvents"
        );
    }

    // =========================================
    // TuiEventHandler tests
    // =========================================

    #[tokio::test]
    async fn test_tui_handler_text_delta_buffers_until_flush() {
        logging::disable_logging();

        let (tx, mut rx) = mpsc::channel::<AppEvent>(10);
        let mut handler = TuiEventHandler::new(tx);

        // Text is buffered until event boundary (tool executing, complete)
        handler.on_text_delta("Hello\n");

        // Nothing sent yet - text is buffered
        assert!(rx.try_recv().is_err(), "Expected no event until flush");

        // Flush happens at on_complete
        use genai_rs::{InteractionResponse, InteractionStatus};
        let response = InteractionResponse {
            id: Some("test-id".to_string()),
            model: None,
            agent: None,
            input: vec![],
            outputs: vec![],
            status: InteractionStatus::Completed,
            usage: None,
            tools: None,
            grounding_metadata: None,
            url_context_metadata: None,
            previous_interaction_id: None,
            created: None,
            updated: None,
        };
        handler.on_complete(Some("test-id"), &response);

        // Now the buffered text is sent
        let event = rx.try_recv().unwrap();
        match event {
            AppEvent::StreamChunk(text) => assert!(text.contains("Hello")),
            _ => panic!("Expected StreamChunk"),
        }
    }

    #[tokio::test]
    async fn test_tui_handler_tool_executing_sends_app_event() {
        let (tx, mut rx) = mpsc::channel::<AppEvent>(10);
        let mut handler = TuiEventHandler::new(tx);

        let call = OwnedFunctionCallInfo {
            name: "read_file".to_string(),
            args: json!({"file_path": "test.rs"}),
            id: None,
        };
        handler.on_tool_executing(&call);

        // With empty buffer, only ToolExecuting is sent (no blank line antipattern)
        let event = rx.try_recv().unwrap();
        match event {
            AppEvent::ToolExecuting { name, args } => {
                assert_eq!(name, "read_file");
                assert_eq!(args["file_path"], "test.rs");
            }
            _ => panic!("Expected ToolExecuting"),
        }
    }

    #[tokio::test]
    async fn test_tui_handler_tool_result_sends_app_event() {
        let (tx, mut rx) = mpsc::channel::<AppEvent>(10);
        let mut handler = TuiEventHandler::new(tx);

        let result = FunctionExecutionResult::new(
            "bash".to_string(),
            "call-1".to_string(),
            json!({"command": "ls"}),
            json!({"output": "file.txt"}),
            Duration::from_millis(150),
        );
        handler.on_tool_result(&result);

        let event = rx.try_recv().unwrap();
        match event {
            AppEvent::ToolCompleted {
                name,
                duration_ms,
                tokens,
                has_error,
            } => {
                assert_eq!(name, "bash");
                assert_eq!(duration_ms, 150);
                assert!(tokens > 0); // tokens computed from args + result
                assert!(!has_error);
            }
            _ => panic!("Expected ToolCompleted"),
        }
    }

    #[tokio::test]
    async fn test_tui_handler_context_warning_sends_app_event() {
        let (tx, mut rx) = mpsc::channel::<AppEvent>(10);
        let mut handler = TuiEventHandler::new(tx);

        handler.on_context_warning(&clemini::agent::ContextWarning::new(855_000, 1_000_000));

        let event = rx.try_recv().unwrap();
        match event {
            AppEvent::ContextWarning(msg) => {
                assert!(msg.contains("85.5%"));
            }
            _ => panic!("Expected ContextWarning"),
        }
    }
}
