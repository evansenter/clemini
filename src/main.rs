use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use futures_util::StreamExt;
use genai_rs::{CallableFunction, Client, Content, StreamChunk, ToolService};
use ratatui::DefaultTerminal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Instant;
use termimad::MadSkin;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};
use tui_textarea::{Input, TextArea};

mod diff;
mod mcp;
mod tools;
mod tui;

use tools::CleminiToolService;

const DEFAULT_MODEL: &str = "gemini-3-flash-preview";
const CONTEXT_WINDOW_LIMIT: u32 = 1_000_000;

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

        let _ = run_interaction(
            &client,
            &tool_service,
            &prompt,
            None,
            &model,
            args.stream,
            None,
            &system_prompt,
            cancellation_token,
            false, // not TUI mode
        )
        .await?;
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
#[allow(dead_code)] // StreamChunk reserved for future streaming text updates
enum AppEvent {
    StreamChunk(String),
    ToolProgress(InteractionProgress),
    InteractionComplete(Result<InteractionResult>),
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

        match run_interaction(
            client,
            tool_service,
            input,
            last_interaction_id.as_deref(),
            model,
            stream_output,
            None,
            &system_prompt,
            cancellation_token,
            false, // not TUI mode
        )
        .await
        {
            Ok(result) => {
                last_interaction_id = result.id;
            }
            Err(e) => {
                eprintln!("\n{}", format!("[error: {e}]").bright_red());
            }
        }
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
    stream_output: bool,
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
                                    stream_output,
                                    progress_fn,
                                    &system_prompt,
                                    cancellation_token,
                                    true, // TUI mode
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
                        app.append_to_chat(&text);
                    }
                    AppEvent::ToolProgress(progress) => {
                        if progress.status == "executing" {
                            app.set_activity(tui::Activity::Executing(progress.tool.clone()));
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

fn check_context_window(total_tokens: u32) {
    let ratio = f64::from(total_tokens) / f64::from(CONTEXT_WINDOW_LIMIT);
    if ratio > 0.95 {
        eprintln!(
            "{}",
            format!(
                "WARNING: Context window usage is at {:.1}% ({}/{} tokens). Please use /clear to reset history.",
                ratio * 100.0,
                total_tokens,
                CONTEXT_WINDOW_LIMIT
            )
            .bright_red()
            .bold()
        );
    } else if ratio > 0.80 {
        eprintln!(
            "{}",
            format!(
                "WARNING: Context window usage is at {:.1}% ({}/{} tokens).",
                ratio * 100.0,
                total_tokens,
                CONTEXT_WINDOW_LIMIT
            )
            .yellow()
            .bold()
        );
    }
}

fn flush_response(
    response_text: &mut String,
    skin: &MadSkin,
    stream_output: bool,
    force_newline: bool,
    tui_mode: bool,
) {
    if !response_text.is_empty() {
        // Log to file only - don't duplicate to terminal since we're rendering it
        log_to_file(&format!("> {}", response_text.trim()));

        // In TUI mode, text is already sent through the channel - don't print to terminal
        if !tui_mode {
            if stream_output {
                let width = termimad::terminal_size().0.max(20);
                let mut visual_lines = 0;
                for line in response_text.split('\n') {
                    let len = line.chars().count();
                    if len == 0 {
                        visual_lines += 1;
                    } else {
                        visual_lines += (len as u16).div_ceil(width);
                    }
                }
                for i in 0..visual_lines {
                    if i == 0 {
                        print!("\r\x1B[2K");
                    } else {
                        print!("\x1B[F\x1B[2K");
                    }
                }
                let _ = io::stdout().flush();
            }
            skin.print_text(response_text);
        }
        response_text.clear();
    } else if force_newline && !tui_mode {
        println!();
    }
}

#[derive(Debug, Clone)]
pub struct InteractionResult {
    pub id: Option<String>,
    pub response: String,
    pub context_size: u32,
    pub total_tokens: u32,
    pub tool_calls: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct InteractionProgress {
    pub tool: String,
    pub status: String, // "executing" or "completed"
    pub args: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

fn format_tool_args(args: &Value) -> String {
    let Some(obj) = args.as_object() else {
        return String::new();
    };

    let mut parts = Vec::new();
    for (k, v) in obj {
        let val_str = match v {
            Value::String(s) => {
                let trimmed = s.replace('\n', " ");
                if trimmed.len() > 80 {
                    format!("\"{}...\"", &trimmed[..77])
                } else {
                    format!("\"{trimmed}\"")
                }
            }
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => "null".to_string(),
            _ => "...".to_string(),
        };
        parts.push(format!("{k}={val_str}"));
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!("{} ", parts.join(" "))
    }
}

/// Rough token estimate: ~4 chars per token
fn estimate_tokens(value: &Value) -> u32 {
    (value.to_string().len() / 4) as u32
}

fn format_tool_result(
    name: &str,
    duration: std::time::Duration,
    estimated_tokens: u32,
    has_error: bool,
) -> String {
    let error_suffix = if has_error {
        " ERROR".bright_red().bold().to_string()
    } else {
        String::new()
    };
    let elapsed_secs = duration.as_secs_f32();

    let duration_str = if elapsed_secs < 0.001 {
        format!("{:.3}s", elapsed_secs)
    } else {
        format!("{:.2}s", elapsed_secs)
    };

    format!(
        "[{}] {}, ~{} tok{}",
        name.cyan(),
        duration_str.yellow(),
        estimated_tokens,
        error_suffix
    )
}

struct ToolExecutionResult {
    results: Vec<Content>,
    cancelled: bool,
}

async fn execute_tools(
    tool_service: &Arc<CleminiToolService>,
    accumulated_function_calls: &[(Option<String>, String, Value)],
    progress_fn: &Option<Arc<dyn Fn(InteractionProgress) + Send + Sync>>,
    tool_calls: &mut Vec<String>,
    cancellation_token: &CancellationToken,
) -> ToolExecutionResult {
    let mut results = Vec::new();

    for (call_id, call_name, call_args) in accumulated_function_calls {
        if cancellation_token.is_cancelled() {
            return ToolExecutionResult {
                results,
                cancelled: true,
            };
        }

        log_event(&format!(
            "{} {} {}",
            "CALL".magenta().bold(),
            call_name.purple(),
            format_tool_args(call_args).trim().dimmed()
        ));

        if let Some(cb) = progress_fn {
            cb(InteractionProgress {
                tool: call_name.to_string(),
                status: "executing".to_string(),
                args: call_args.clone(),
                duration_ms: None,
            });
        }

        let start = Instant::now();
        let result: Value = match tool_service.execute(call_name, call_args.clone()).await {
            Ok(v) => v,
            Err(e) => {
                // Return error as JSON so Gemini can see it and retry
                serde_json::json!({"error": e.to_string()})
            }
        };
        let duration = start.elapsed();

        tool_calls.push(call_name.to_string());
        let has_error = result.get("error").is_some();
        let estimated_tokens = estimate_tokens(&result);

        if let Some(cb) = progress_fn {
            cb(InteractionProgress {
                tool: call_name.to_string(),
                status: "completed".to_string(),
                args: call_args.clone(),
                duration_ms: Some(duration.as_millis() as u64),
            });
        }

        let formatted = format_tool_result(call_name, duration, estimated_tokens, has_error);
        log_event(&formatted);

        if has_error && let Some(error_msg) = result.get("error").and_then(|e: &Value| e.as_str()) {
            let error_detail = format!("  └─ {}: {}", "error".red(), error_msg.dimmed());
            log_event(&error_detail);
        }

        results.push(Content::function_result(
            call_name.to_string(),
            call_id.clone().expect("Function call ID required"),
            result,
        ));
    }

    ToolExecutionResult {
        results,
        cancelled: false,
    }
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub async fn run_interaction(
    client: &Client,
    tool_service: &Arc<CleminiToolService>,
    input: &str,
    previous_interaction_id: Option<&str>,
    model: &str,
    stream_output: bool,
    progress_fn: Option<Arc<dyn Fn(InteractionProgress) + Send + Sync>>,
    system_prompt: &str,
    cancellation_token: CancellationToken,
    tui_mode: bool,
) -> Result<InteractionResult> {
    let functions: Vec<_> = tool_service
        .tools()
        .iter()
        .map(|t: &Arc<dyn CallableFunction>| t.declaration())
        .collect();

    // Build the interaction - system instruction must be sent on every turn
    // (it's NOT inherited via previousInteractionId per genai-rs docs)
    let mut interaction = client
        .interaction()
        .with_model(model)
        .add_functions(functions.clone())
        .with_system_instruction(system_prompt)
        .with_content(vec![Content::text(input)]);

    if let Some(prev_id) = previous_interaction_id {
        interaction = interaction.with_previous_interaction(prev_id);
    }

    let mut stream = Box::pin(interaction.create_stream());

    let mut last_id = previous_interaction_id.map(String::from);
    let mut current_context_size: u32 = 0;
    let mut total_tokens: u32 = 0;
    let mut tool_calls: Vec<String> = Vec::new();
    let mut response_text = String::new();
    let mut full_response = String::new();
    let skin = MadSkin::default();

    const MAX_ITERATIONS: usize = 100;
    for _ in 0..MAX_ITERATIONS {
        let mut response: Option<genai_rs::InteractionResponse> = None;
        let mut accumulated_function_calls: Vec<(Option<String>, String, Value)> = Vec::new();
        response_text.clear();

        while let Some(event) = stream.next().await {
            // Check for cancellation at each iteration
            if cancellation_token.is_cancelled() {
                eprintln!("{}", "[cancelled]".yellow());
                return Ok(InteractionResult {
                    id: last_id,
                    response: full_response,
                    context_size: current_context_size,
                    total_tokens,
                    tool_calls,
                });
            }

            match event {
                Ok(event) => match &event.chunk {
                    StreamChunk::Delta(content) => {
                        if let Some(text) = content.as_text() {
                            if stream_output {
                                emit_streaming(text);
                            }
                            response_text.push_str(text);
                            full_response.push_str(text);
                        }
                        // Accumulate function calls from Delta chunks (streaming doesn't put them in Complete)
                        if let Content::FunctionCall { id, name, args } = content {
                            accumulated_function_calls.push((
                                id.clone(),
                                name.clone(),
                                args.clone(),
                            ));
                        }
                    }
                    StreamChunk::Complete(resp) => {
                        response = Some(resp.clone());
                    }
                    _ => {}
                },
                Err(e) => {
                    let err_msg = e.to_string();
                    log_event_raw(
                        &format!("\n[stream error: {err_msg}]")
                            .bright_red()
                            .to_string(),
                    );
                    return Err(anyhow::anyhow!(err_msg));
                }
            }
        }

        let resp = response.ok_or_else(|| anyhow::anyhow!("Stream ended without completion"))?;
        last_id = resp.id.clone();

        // Update token count
        if let Some(usage) = &resp.usage {
            let turn_tokens = usage.total_tokens.unwrap_or_else(|| {
                usage.total_input_tokens.unwrap_or(0) + usage.total_output_tokens.unwrap_or(0)
            });
            if turn_tokens > 0 {
                current_context_size = turn_tokens;
                total_tokens = turn_tokens;
            }
        }

        // Use accumulated function calls from Delta chunks (streaming mode doesn't populate Complete.outputs)
        if accumulated_function_calls.is_empty() {
            // Render final text
            flush_response(&mut response_text, &skin, stream_output, true, tui_mode);
            break;
        }

        // Process function calls (accumulated from Delta chunks)
        flush_response(&mut response_text, &skin, stream_output, false, tui_mode);
        full_response.clear(); // Clear accumulated text before tools as we'll only return text after final tool

        let tool_result = execute_tools(
            tool_service,
            &accumulated_function_calls,
            &progress_fn,
            &mut tool_calls,
            &cancellation_token,
        )
        .await;

        if tool_result.cancelled {
            eprintln!("{}", "[cancelled]".yellow());
            return Ok(InteractionResult {
                id: last_id,
                response: full_response,
                context_size: current_context_size,
                total_tokens,
                tool_calls,
            });
        }

        let results = tool_result.results;

        // Create new stream for the next turn
        stream = Box::pin(
            client
                .interaction()
                .with_model(model)
                .with_previous_interaction(last_id.as_ref().unwrap())
                .with_system_instruction(system_prompt)
                .with_content(results)
                .create_stream(),
        );
    }

    // Render any remaining text (e.g., if stream ended abruptly or on error)
    flush_response(&mut response_text, &skin, stream_output, false, tui_mode);

    if current_context_size > 0 {
        check_context_window(current_context_size);
    }

    Ok(InteractionResult {
        id: last_id,
        response: full_response,
        context_size: current_context_size,
        total_tokens,
        tool_calls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn test_format_tool_args_empty() {
        assert_eq!(format_tool_args(&json!({})), "");
        assert_eq!(format_tool_args(&json!(null)), "");
        assert_eq!(format_tool_args(&json!("not an object")), "");
    }

    #[test]
    fn test_format_tool_args_types() {
        let args = json!({
            "bool": true,
            "num": 42,
            "null": null,
            "str": "hello"
        });
        let formatted = format_tool_args(&args);
        // serde_json::Map is sorted by key
        assert_eq!(formatted, "bool=true null=null num=42 str=\"hello\" ");
    }

    #[test]
    fn test_format_tool_args_complex_types() {
        let args = json!({
            "arr": [1, 2],
            "obj": {"a": 1}
        });
        let formatted = format_tool_args(&args);
        assert_eq!(formatted, "arr=... obj=... ");
    }

    #[test]
    fn test_format_tool_args_truncation() {
        let long_str = "a".repeat(100);
        let args = json!({"long": long_str});
        let formatted = format_tool_args(&args);
        let expected_val = format!("\"{}...\"", "a".repeat(77));
        assert_eq!(formatted, format!("long={} ", expected_val));
    }

    #[test]
    fn test_format_tool_args_newlines() {
        let args = json!({"text": "hello\nworld"});
        let formatted = format_tool_args(&args);
        assert_eq!(formatted, "text=\"hello world\" ");
    }

    #[test]
    fn test_estimate_tokens() {
        // ~4 chars per token
        assert_eq!(estimate_tokens(&json!("hello")), 1); // "hello" = 7 chars / 4 = 1
        assert_eq!(estimate_tokens(&json!({"key": "value"})), 3); // {"key":"value"} = 15 chars / 4 = 3
    }

    #[test]
    fn test_format_tool_result_duration() {
        colored::control::set_override(false);

        // < 1ms (100us) -> 3 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_micros(100), 10, false),
            "[test] 0.000s, ~10 tok"
        );

        // < 1ms (900us) -> 3 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_micros(900), 10, false),
            "[test] 0.001s, ~10 tok"
        );

        // >= 1ms (1.1ms) -> 2 decimals (shows 0.00s due to threshold)
        assert_eq!(
            format_tool_result("test", Duration::from_micros(1100), 10, false),
            "[test] 0.00s, ~10 tok"
        );

        // >= 1ms (20ms) -> 2 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_millis(20), 10, false),
            "[test] 0.02s, ~10 tok"
        );

        // >= 1ms (1450ms) -> 2 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_millis(1450), 10, false),
            "[test] 1.45s, ~10 tok"
        );

        colored::control::unset_override();
    }

    #[test]
    fn test_format_tool_result_error() {
        colored::control::set_override(false);

        let res = format_tool_result("test", Duration::from_millis(10), 25, true);
        assert_eq!(res, "[test] 0.01s, ~25 tok ERROR");

        let res = format_tool_result("test", Duration::from_millis(10), 25, false);
        assert_eq!(res, "[test] 0.01s, ~25 tok");

        colored::control::unset_override();
    }
}
