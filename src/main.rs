use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use futures_util::StreamExt;
use genai_rs::Client;
use ratatui::DefaultTerminal;
use serde::Deserialize;
use std::env;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, OnceLock};
use termimad::MadSkin;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};
use tui_textarea::{Input, TextArea};

mod agent;
mod diff;
mod events;
mod mcp;
mod tools;
mod tui;

use agent::{AgentEvent, InteractionProgress, InteractionResult, run_interaction};
use tools::CleminiToolService;

const DEFAULT_MODEL: &str = "gemini-3-flash-preview";

/// Initialize tracing for structured JSON logs only.
/// Human-readable logs go through log_event() instead.
pub fn init_logging() {
    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".clemini/logs");

    let _ = std::fs::create_dir_all(&log_dir);

    // JSON layer: clemini.json.YYYY-MM-DD
    let json_file = tracing_appender::rolling::daily(&log_dir, "clemini.json");
    let (json_writer, json_guard) = tracing_appender::non_blocking(json_file);
    let json_layer = fmt::layer().json().with_writer(json_writer);

    Box::leak(Box::new(json_guard));

    tracing_subscriber::registry()
        .with(json_layer)
        .with(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();
}

static SKIN: LazyLock<MadSkin> = LazyLock::new(|| {
    let mut skin = MadSkin::default();
    for h in &mut skin.headers {
        h.align = termimad::Alignment::Left;
    }
    skin
});

pub trait OutputSink: Send + Sync {
    fn emit(&self, message: &str, render_markdown: bool);
    /// Emit streaming text (no newline, no markdown). Used for model response streaming.
    fn emit_streaming(&self, text: &str);
}

/// Writes to log files only (current behavior of log_event)
pub struct FileSink;

impl OutputSink for FileSink {
    fn emit(&self, message: &str, render_markdown: bool) {
        log_event_to_file(message, render_markdown);
    }
    fn emit_streaming(&self, _text: &str) {
        // No terminal to stream to
    }
}

/// Writes to stderr AND log files (for REPL mode)
pub struct TerminalSink;

impl OutputSink for TerminalSink {
    fn emit(&self, message: &str, render_markdown: bool) {
        if render_markdown {
            // term_text includes trailing newline, use eprint to avoid doubling
            eprint!("{}", SKIN.term_text(message));
        } else {
            eprintln!("{}", message);
        }
        log_event_to_file(message, render_markdown);
    }
    fn emit_streaming(&self, text: &str) {
        print!("{text}");
        let _ = io::stdout().flush();
    }
}

/// Message types for TUI output channel
#[derive(Debug)]
pub enum TuiMessage {
    /// Complete line/message (uses append_to_chat)
    Line(String),
    /// Streaming text chunk (uses append_streaming)
    Streaming(String),
}

/// Channel for TUI output - global sender that TuiSink writes to
static TUI_OUTPUT_TX: OnceLock<mpsc::UnboundedSender<TuiMessage>> = OnceLock::new();

/// Set the TUI output channel sender
pub fn set_tui_output_channel(tx: mpsc::UnboundedSender<TuiMessage>) {
    let _ = TUI_OUTPUT_TX.set(tx);
}

/// Writes to TUI buffer (via channel) AND log files - no termimad, no stderr
pub struct TuiSink;

impl OutputSink for TuiSink {
    fn emit(&self, message: &str, _render_markdown: bool) {
        // Send to TUI via channel (no termimad rendering - just plain text with ANSI colors)
        if let Some(tx) = TUI_OUTPUT_TX.get() {
            let _ = tx.send(TuiMessage::Line(message.to_string()));
        }
        // Also log to file (without markdown rendering to avoid termimad formatting issues)
        log_event_to_file(message, false);
    }
    fn emit_streaming(&self, text: &str) {
        // Send streaming text to TUI via channel
        if let Some(tx) = TUI_OUTPUT_TX.get() {
            let _ = tx.send(TuiMessage::Streaming(text.to_string()));
        }
    }
}

static OUTPUT_SINK: OnceLock<Arc<dyn OutputSink>> = OnceLock::new();

pub fn set_output_sink(sink: Arc<dyn OutputSink>) {
    let _ = OUTPUT_SINK.set(sink);
}

/// Log to human-readable file with ANSI colors preserved
/// Uses same naming as rolling::daily: clemini.log.YYYY-MM-DD
pub fn log_event(message: &str) {
    if let Some(sink) = OUTPUT_SINK.get() {
        sink.emit(message, true);
    }
    // No fallback - OUTPUT_SINK is always set in production before logging.
    // Skipping prevents test pollution of shared log files.
}

/// Log without markdown rendering (for protocol messages with long content)
pub fn log_event_raw(message: &str) {
    if let Some(sink) = OUTPUT_SINK.get() {
        sink.emit(message, false);
    }
    // No fallback - see log_event comment
}

/// Log to file only (skip terminal output even with TerminalSink)
pub fn log_to_file(message: &str) {
    log_event_to_file(message, true);
}

/// Emit streaming text (for model response streaming)
pub fn emit_streaming(text: &str) {
    if let Some(sink) = OUTPUT_SINK.get() {
        sink.emit_streaming(text);
    }
}

fn log_event_to_file(message: &str, render_markdown: bool) {
    colored::control::set_override(true);

    // Optionally render markdown (can wrap long lines)
    let rendered = if render_markdown {
        SKIN.term_text(message).to_string()
    } else {
        message.to_string()
    };

    // Write to the stable log location: clemini.log.YYYY-MM-DD
    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".clemini/logs");
    let _ = std::fs::create_dir_all(&log_dir);

    let today = chrono::Local::now().format("%Y-%m-%d");
    let log_path = log_dir.join(format!("clemini.log.{}", today));

    let _ = write_to_log_file(&log_path, &rendered);

    // Also write to CLEMINI_LOG if set (backwards compat)
    if let Ok(path) = std::env::var("CLEMINI_LOG") {
        let _ = write_to_log_file(PathBuf::from(path), &rendered);
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
            let timestamp = format!("[{}]", chrono::Local::now().format("%H:%M:%S%.3f")).cyan();
            writeln!(file, "{} {}", timestamp, line)?;
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
- "Let me fetch the issue to understand the requirements..."
- "Reading the file to see the current implementation..."
- "I'll update the function to handle this edge case..."

This is NOT optional. Users need to follow your thought process. One line per step, output text BEFORE calling tools.

## Tools
- `read_file(file_path, offset?, limit?)` - Read files. Use `limit: 100` for first read. If `truncated: true`, continue with `offset`.
- `edit(file_path, old_string, new_string, replace_all?)` - Surgical string replacement. Params are TOP-LEVEL, not nested in operations.
- `write_file(file_path, contents)` - Create new files or completely overwrite existing ones.
- `glob` - Find files by pattern: `**/*.rs`, `src/**/*.ts`
- `grep` - Search file contents. **Always prefer this over `bash grep`.** Use `context: N` for surrounding lines.
- `bash` - Shell commands: git, builds, tests. For GitHub, use `gh`: `gh issue view 34`, `gh pr view`.
- `ask_user` - **Use when uncertain.** Ask clarifying questions rather than guessing.
- `todo_write` - **ALWAYS use for multi-step tasks.** If a task has 2+ steps, needs planning, or involves a list of requirements, create todos FIRST. Update status as you work—mark in_progress before starting, completed when done. Users rely on this for visibility into your progress.
- `web_search` / `web_fetch` - Get current information from the web.

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

## Self-Improvement
When you discover patterns that would help future tasks:
- Update this system prompt (in `src/main.rs` SYSTEM_PROMPT) with the guidance
- Keep additions concise and broadly applicable
- This helps you get better over time
"#;

#[derive(Deserialize, Default)]
struct Config {
    model: Option<String>,
    bash_timeout: Option<u64>,
    allowed_paths: Option<Vec<String>>,
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
    let client = Client::new(api_key);

    let cwd = std::fs::canonicalize(&args.cwd)?;

    // Resolve allowed paths
    let mut allowed_paths = Vec::new();
    if let Some(config_paths) = config.allowed_paths {
        for path_str in config_paths {
            let path = if path_str.starts_with('~') {
                home::home_dir()
                    .map(|h| h.join(path_str.trim_start_matches("~/").trim_start_matches('~')))
                    .unwrap_or_else(|| PathBuf::from(&path_str))
            } else {
                PathBuf::from(&path_str)
            };
            allowed_paths.push(path);
        }
    } else {
        // Default allowed paths: cwd + home dirs + tmp
        let home = home::home_dir().expect("Failed to get home directory");
        allowed_paths.push(cwd.clone());
        allowed_paths.push(home.join(".clemini"));
        allowed_paths.push(home.join("Documents/projects"));
        allowed_paths.push(PathBuf::from("/tmp"));
        #[cfg(target_os = "macos")]
        allowed_paths.push(PathBuf::from("/private/tmp"));
    }

    let tool_service = Arc::new(CleminiToolService::new(
        cwd.clone(),
        bash_timeout,
        args.mcp_server,
        allowed_paths,
    ));

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
        set_output_sink(Arc::new(FileSink));
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
        set_output_sink(Arc::new(TerminalSink));
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

        let result = run_interaction(
            &client,
            &tool_service,
            &prompt,
            None,
            &model,
            &system_prompt,
            events_tx,
            None,
            cancellation_token,
        )
        .await?;

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
            set_output_sink(Arc::new(TerminalSink));
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
    ToolProgress(InteractionProgress),
    InteractionComplete(Result<InteractionResult>),
    ContextWarning(String),
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

        match run_interaction(
            client,
            tool_service,
            input,
            last_interaction_id.as_deref(),
            model,
            &system_prompt,
            events_tx,
            None,
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
    set_output_sink(Arc::new(TuiSink));

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

                                // Spawn task to convert AgentEvents to AppEvents
                                let app_tx = tx.clone();
                                tokio::spawn(async move {
                                    while let Some(event) = events_rx.recv().await {
                                        match event {
                                            AgentEvent::TextDelta(text) => {
                                                let _ = app_tx.try_send(AppEvent::StreamChunk(text));
                                            }
                                            AgentEvent::ContextWarning { percentage, .. } => {
                                                let msg = if percentage > 95.0 {
                                                    format!(
                                                        "WARNING: Context window at {:.1}%. Use /clear to reset.",
                                                        percentage
                                                    )
                                                } else {
                                                    format!("WARNING: Context window at {:.1}%.", percentage)
                                                };
                                                let _ = app_tx.try_send(AppEvent::ContextWarning(msg));
                                            }
                                            // Other events handled via progress_fn or final result
                                            _ => {}
                                        }
                                    }
                                });

                                let progress_tx = tx.clone();
                                let progress_fn: Option<Arc<dyn Fn(InteractionProgress) + Send + Sync>> =
                                    Some(Arc::new(move |progress: InteractionProgress| {
                                        let _ = progress_tx.try_send(AppEvent::ToolProgress(progress));
                                    }));

                                let result = run_interaction(
                                    &client,
                                    &tool_service,
                                    &input,
                                    prev_id.as_deref(),
                                    &model,
                                    &system_prompt,
                                    events_tx,
                                    progress_fn,
                                    cancellation_token,
                                )
                                .await;

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
                    AppEvent::ToolProgress(progress) => {
                        if progress.status == "executing" {
                            app.set_activity(tui::Activity::Executing(progress.tool.clone()));
                            // Display tool call in chat
                            let args_str = agent::format_tool_args(&progress.args);
                            let msg = format!(
                                "{} {} {}",
                                "🔧".dimmed(),
                                progress.tool.cyan(),
                                args_str.dimmed()
                            );
                            app.append_to_chat(&msg);
                        } else if progress.status == "completed" {
                            // Tool completed - show duration if available
                            if let Some(duration_ms) = progress.duration_ms {
                                let duration_str = if duration_ms < 1000 {
                                    format!("{}ms", duration_ms)
                                } else {
                                    format!("{:.1}s", duration_ms as f64 / 1000.0)
                                };
                                let msg = format!(
                                    "  {} {}",
                                    "└─".dimmed(),
                                    duration_str.dimmed()
                                );
                                app.append_to_chat(&msg);
                            }
                            app.append_to_chat(""); // Single blank line after tool completes
                        }
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
                    TuiMessage::Streaming(text) => app.append_streaming(&text),
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
    use genai_rs::{FunctionExecutionResult, OwnedFunctionCallInfo};
    use serde_json::json;
    use std::time::Duration;

    // =========================================
    // Event formatting tests
    // These verify the formatting used across all UI modes
    // =========================================

    /// ToolExecuting events should format as: 🔧 <tool_name> <args>
    #[test]
    fn test_tool_executing_format() {
        let args = json!({"file_path": "src/main.rs", "limit": 100});
        let formatted = agent::format_tool_args(&args);

        // Args should be formatted as key=value pairs
        assert!(formatted.contains("file_path="));
        assert!(formatted.contains("limit=100"));
    }

    /// ToolResult events should format with duration and token estimate
    #[test]
    fn test_tool_result_format() {
        colored::control::set_override(false);

        let formatted =
            agent::format_tool_result("read_file", Duration::from_millis(25), 150, false);

        assert!(formatted.contains("[read_file]"));
        assert!(formatted.contains("0.02s") || formatted.contains("0.03s")); // timing can vary
        assert!(formatted.contains("~150 tok"));

        colored::control::unset_override();
    }

    /// ToolResult errors should include ERROR suffix
    #[test]
    fn test_tool_result_error_format() {
        colored::control::set_override(false);

        let formatted =
            agent::format_tool_result("write_file", Duration::from_millis(10), 50, true);

        assert!(formatted.contains("[write_file]"));
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
        app.append_to_chat("🔧 grep pattern=\"test\""); // Line-based, adds new line

        assert_eq!(app.chat_lines().len(), 3);
        assert_eq!(app.chat_lines()[0], "Let me search.");
        assert_eq!(app.chat_lines()[1], ""); // empty from \n
        assert!(app.chat_lines()[2].contains("grep"));
    }

    /// ToolResult should use line-based output (own line)
    #[test]
    fn test_tool_result_requires_line_output() {
        let mut app = tui::App::new("test");

        app.append_to_chat("🔧 read_file path=\"test.rs\"");
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
        app.append_to_chat("🔧 grep pattern=\"fn main\"");

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
        assert_eq!(app.chat_lines()[3], "🔧 grep pattern=\"fn main\"");
        assert_eq!(app.chat_lines()[4], "[grep] 0.01s, ~50 tok");
        assert_eq!(app.chat_lines()[5], "");
        assert_eq!(app.chat_lines()[6], "Found it in src/main.rs");
    }
}
