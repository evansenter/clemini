use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use futures_util::StreamExt;
use genai_rs::{AutoFunctionStreamChunk, Client, Content};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use std::env;
use std::io::{self, IsTerminal, Read, Write};
use std::time::Instant;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use termimad::MadSkin;

mod tools;
mod mcp;

use tools::CleminiToolService;

const DEFAULT_MODEL: &str = "gemini-3-flash-preview";
const CONTEXT_WINDOW_LIMIT: u32 = 1_000_000;

pub fn log_event(message: &str) {
    colored::control::set_override(true);
    if let Ok(path) = std::env::var("CLEMINI_LOG")
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let now = std::time::SystemTime::now();
        let datetime: chrono::DateTime<chrono::Local> = now.into();
        let timestamp = datetime.format("%H:%M:%S%.3f").to_string();
        use std::io::Write;
        let _ = writeln!(file, "[{}] {}", timestamp, message);
    }
}

const SYSTEM_PROMPT: &str = r#"You are clemini, a coding assistant. Be concise. Get things done.

## Workflow
1. **Understand** - Read files before editing. Never guess at contents.
2. **Plan** - For complex tasks, briefly state your approach before implementing.
3. **Execute** - Make changes, narrating each step in one line.
4. **Verify** - Run tests/checks. Compilation passing ≠ working code.

## Communication
Narrate your work with brief status updates:
- "Let me read the config to understand the current setup..."
- "I'll update the function to handle the edge case..."
- "Running tests to verify the fix..."
Keep it to one line per step. This helps users follow along.

## Tools
- `read_file` - Read files. Use `offset`/`limit` for large files (e.g., `offset: 100, limit: 50`).
- `edit` - Replace specific strings. Use `replace_all: true` for renaming across a file.
- `write_file` - Create new files or completely overwrite existing ones.
- `glob` - Find files by pattern: `**/*.py`, `src/**/*.ts`, `**/test_*.js`
- `grep` - Search contents with regex. Use `context: 3` to show surrounding lines.
- `bash` - Shell commands: git, builds, tests, package managers, pipelines.
- `ask_user` - When uncertain, ask. Better to clarify than guess wrong.
- `todo_write` - For 3+ step tasks, track progress visibly so nothing is forgotten.
- `web_search` / `web_fetch` - Get current information from the web.

## Verification
After changes, verify they work:
- Python: `pytest`, `python -m py_compile`
- Rust: `cargo check`, `cargo test`
- JavaScript/TypeScript: `npm test`, `tsc --noEmit`
- General: run the relevant test suite or try the changed functionality

## Refactoring
- Passing syntax/type checks ≠ working code. Test affected functionality.
- Timeouts during testing usually mean broken code, not network issues.
- For unfamiliar APIs, read source/docs first. If unavailable, ask the user.
- Before declaring complete, run tests that exercise changed code paths.

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
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config = load_config();

    let model = args
        .model
        .or(config.model)
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    let bash_timeout = args.timeout.or(config.bash_timeout).unwrap_or(60);

    let api_key = env::var("GEMINI_API_KEY")
        .map_err(|e| anyhow::anyhow!("GEMINI_API_KEY environment variable not set: {}", e))?;
    let client = Client::new(api_key);

    let cwd = std::fs::canonicalize(&args.cwd)?;
    let tool_service = Arc::new(CleminiToolService::new(cwd.clone(), bash_timeout));

    // MCP server mode - handle early before consuming stdin or printing banner
    if args.mcp_server {
        let mcp_server = Arc::new(mcp::McpServer::new(client, tool_service, model));
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
    eprintln!("{} Remember to take breaks during development!", "💡".yellow());
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
        // Non-interactive mode: run single prompt
        let _ = run_interaction(&client, &tool_service, &prompt, None, &model, args.stream, None).await?;
    } else {
        // Interactive REPL mode
        run_repl(&client, &tool_service, cwd, &model, args.stream).await?;
    }

    Ok(())
}

async fn run_repl(
    client: &Client,
    tool_service: &Arc<CleminiToolService>,
    cwd: std::path::PathBuf,
    model: &str,
    stream_output: bool,
) -> Result<()> {
    let mut rl = DefaultEditor::new()?;

    let history_path = home::home_dir().map(|mut p| {
        p.push(".clemini_history");
        p
    });

    if let Some(ref path) = history_path {
        let _ = rl.load_history(path);
    }

    let mut last_interaction_id: Option<String> = None;
    let mut last_estimated_context_size: u32 = 0;

    let session_start = std::time::Instant::now();
    let mut total_interactions = 0;
    let mut total_tool_calls = 0;
    let mut total_session_tokens = 0;

    loop {
        let readline = rl.readline("> ");
        match readline {
            Ok(line) => {
                let input = line.trim();
                if input.is_empty() {
                    continue;
                }

                if input == "/quit" || input == "/exit" || input == "/q" {
                    break;
                }

                if input == "/clear" || input == "/c" {
                    last_interaction_id = None;
                    last_estimated_context_size = 0;
                    eprintln!("[conversation cleared]");
                    continue;
                }

                if input == "/version" || input == "/v" {
                    println!(
                        "clemini v{} | {}",
                        env!("CARGO_PKG_VERSION").cyan(),
                        model.green()
                    );
                    continue;
                }

                if input == "/model" || input == "/m" {
                    println!("{model}");
                    continue;
                }

                if input == "/pwd" || input == "/cwd" {
                    println!("{}", cwd.display().to_string().yellow());
                    continue;
                }

                if input == "/diff" || input == "/d" {
                    run_git_command(&["diff"], "no uncommitted changes");
                    continue;
                }

                if input == "/status" || input == "/s" {
                    run_git_command(&["status", "--short"], "clean working directory");
                    continue;
                }

                if input == "/log" || input == "/l" {
                    run_git_command(&["log", "--oneline", "-5"], "no commits found");
                    continue;
                }

                if input == "/branch" || input == "/b" {
                    run_git_command(&["branch"], "no branches found");
                    continue;
                }

                if input == "/tokens" || input == "/t" {
                    println!(
                        "Context usage: {}/{} tokens ({:.1}%)",
                        last_estimated_context_size.to_string().yellow(),
                        CONTEXT_WINDOW_LIMIT.to_string().dimmed(),
                        (f64::from(last_estimated_context_size) / f64::from(CONTEXT_WINDOW_LIMIT))
                            * 100.0
                    );
                    println!(
                        "Session total: {} tokens",
                        total_session_tokens.to_string().cyan()
                    );
                    continue;
                }

                if input == "/stats" {
                    let elapsed = session_start.elapsed();
                    let mins = elapsed.as_secs() / 60;
                    let secs = elapsed.as_secs() % 60;
                    println!("{}", "Session Statistics:".bold().underline());
                    println!("  Uptime:        {}m {}s", mins, secs);
                    println!("  Model:         {}", model.green());
                    println!("  Interactions:  {}", total_interactions);
                    println!("  Tool Calls:    {}", total_tool_calls);
                    println!("  Total Tokens:  {}", total_session_tokens.to_string().cyan());
                    println!(
                        "  Context usage: {}/{} tokens ({:.1}%)",
                        last_estimated_context_size.to_string().yellow(),
                        CONTEXT_WINDOW_LIMIT.to_string().dimmed(),
                        (f64::from(last_estimated_context_size) / f64::from(CONTEXT_WINDOW_LIMIT))
                            * 100.0
                    );
                    continue;
                }

                if input == "/help" || input == "/h" {
                    eprintln!("Commands:");
                    eprintln!("  /q, /quit, /exit  Exit the REPL");
                    eprintln!("  /c, /clear        Clear conversation history");
                    eprintln!("  /v, /version      Show version and model");
                    eprintln!("  /m, /model        Show model name");
                    eprintln!("  /t, /tokens       Show token usage statistics");
                    eprintln!("  /stats            Show session statistics");
                    eprintln!("  /pwd, /cwd        Show current working directory");
                    eprintln!("  /d, /diff         Show git diff");
                    eprintln!("  /s, /status       Show git status");
                    eprintln!("  /l, /log          Show git log");
                    eprintln!("  /b, /branch       Show git branches");
                    eprintln!("  /h, /help         Show this help message");
                    eprintln!();
                    eprintln!("Shell escape:");
                    eprintln!("  ! <command>       Run a shell command directly");
                    eprintln!();
                    eprintln!("Tools:");
                    eprintln!("  read_file         Read file contents");
                    eprintln!("  write_file        Create/overwrite files");
                    eprintln!("  edit              Surgical string replacement");
                    eprintln!("  bash              Run shell commands");
                    eprintln!("  glob              Find files by pattern");
                    eprintln!("  grep              Search text in files");
                    eprintln!("  web_search        Search the web");
                    eprintln!("  web_fetch         Fetch web content");
                    eprintln!();
                    continue;
                }

                if let Some(cmd) = input.strip_prefix('!') {
                    let cmd = cmd.trim();
                    if !cmd.is_empty() {
                        rl.add_history_entry(input)?;
                        run_shell_command(cmd);
                    }
                    continue;
                }

                rl.add_history_entry(input)?;

                match run_interaction(
                    client,
                    tool_service,
                    input,
                    last_interaction_id.as_deref(),
                    model,
                    stream_output,
                    None,
                )
                .await
                {
                    Ok(result) => {
                        last_interaction_id = result.id;
                        last_estimated_context_size = result.context_size;
                        total_interactions += 1;
                        total_tool_calls += result.tool_calls.len() as u32;
                        total_session_tokens += result.total_tokens;
                    }
                    Err(e) => {
                        eprintln!("\n{}", format!("[error: {e}]").bright_red());
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                eprintln!("[interrupted]");
            }
            Err(ReadlineError::Eof) => {
                break;
            }
            Err(err) => {
                eprintln!("[readline error: {err}]");
                break;
            }
        }
    }

    if let Some(ref path) = history_path {
        rl.save_history(path)?;
    }

    Ok(())
}

fn run_git_command(args: &[&str], empty_msg: &str) {
    let output = std::process::Command::new("git").args(args).output();

    match output {
        Ok(o) => {
            if o.status.success() {
                let stdout = String::from_utf8_lossy(&o.stdout);
                if stdout.is_empty() {
                    eprintln!("[{empty_msg}]");
                } else {
                    println!("{stdout}");
                }
            } else {
                let stderr = String::from_utf8_lossy(&o.stderr);
                eprintln!("[git {} error: {}]", args[0], stderr.trim());
            }
        }
        Err(e) => {
            eprintln!("[failed to run git {}: {}]", args[0], e);
        }
    }
}

fn run_shell_command(command: &str) {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", command]);
        c
    };

    match cmd.status() {
        Ok(status) => {
            if let Some(code) = status.code().filter(|_| !status.success()) {
                eprintln!("[process exited with code: {code}]");
            }
        }
        Err(e) => {
            eprintln!("[failed to run command: {e}]");
        }
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

struct Spinner {
    handle: tokio::task::JoinHandle<()>,
    stop: Arc<AtomicBool>,
}

impl Spinner {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let handle = tokio::spawn(async move {
            let chars = ['|', '/', '-', '\\'];
            let mut i = 0;
            while !stop_clone.load(Ordering::SeqCst) {
                eprint!("\r{}", chars[i]);
                let _ = io::stderr().flush();
                i = (i + 1) % chars.len();
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
            // Clear spinner
            eprint!("\r\x1B[K");
            let _ = io::stderr().flush();
        });
        Self { handle, stop }
    }

    async fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.handle.await;
    }
}

async fn stop_spinner(spinner: &mut Option<Spinner>) {
    if let Some(s) = spinner.take() {
        s.stop().await;
    }
}

fn flush_response(response_text: &mut String, skin: &MadSkin, stream_output: bool, force_newline: bool) {
    if !response_text.is_empty() {
        log_event(&format!("> {}", response_text.trim()));
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
        response_text.clear();
        println!();
    } else if force_newline {
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
    let Some(obj) = args.as_object() else { return String::new() };

    let mut parts = Vec::new();
    for (k, v) in obj {
        let val_str = match v {
            Value::String(s) => {
                let trimmed = s.replace('\n', " ");
                if trimmed.len() > 40 {
                    format!("\"{}...\"", &trimmed[..37])
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

fn format_tool_result(
    name: &str,
    args: &Value,
    duration: std::time::Duration,
    token_count: u32,
    has_error: bool,
) -> String {
    let error_suffix = if has_error {
        " ERROR".bright_red().bold().to_string()
    } else {
        String::new()
    };
    let elapsed_secs = duration.as_secs_f32();
    let args_str = format_tool_args(args);

    let duration_str = if elapsed_secs < 0.001 {
        format!("{:.3}s", elapsed_secs)
    } else {
        format!("{:.2}s", elapsed_secs)
    };

    let tokens_str = if token_count == 0 {
        "—".to_string()
    } else if token_count < 1000 {
        format!("{} tokens", token_count)
    } else {
        format!("{:.1}k tokens", f64::from(token_count) / 1000.0)
    };

    format!(
        "[{}] {}{}, {}{}",
        name.cyan(),
        args_str.dimmed(),
        duration_str.yellow(),
        tokens_str,
        error_suffix
    )
}

#[allow(clippy::too_many_lines)]
pub async fn run_interaction(
    client: &Client,
    tool_service: &Arc<CleminiToolService>,
    input: &str,
    previous_interaction_id: Option<&str>,
    model: &str,
    stream_output: bool,
    progress_fn: Option<Arc<dyn Fn(InteractionProgress) + Send + Sync>>,
) -> Result<InteractionResult> {
    // Build the interaction - system instruction must be sent on every turn
    // (it's NOT inherited via previousInteractionId per genai-rs docs)
    let mut interaction = client
        .interaction()
        .with_model(model)
        .with_tool_service(tool_service.clone())
        .with_system_instruction(SYSTEM_PROMPT)
        .with_content(vec![Content::text(input)])
        .with_max_function_call_loops(100);

    if let Some(prev_id) = previous_interaction_id {
        interaction = interaction.with_previous_interaction(prev_id);
    }

    static CANCELLED: AtomicBool = AtomicBool::new(false);
    CANCELLED.store(false, Ordering::SeqCst); // Reset for each interaction

    ctrlc::set_handler(move || {
        eprintln!("\n{}", "[ctrl-c received]".yellow());
        CANCELLED.store(true, Ordering::SeqCst);
    }).ok();

    let mut stream = Box::pin(interaction.create_stream_with_auto_functions());

    let mut last_id = previous_interaction_id.map(String::from);
    let mut current_context_size: u32 = 0;
    let mut total_tokens: u32 = 0;
    let mut tool_start_time: Option<Instant> = None;
    let mut tool_calls: Vec<String> = Vec::new();
    let mut response_text = String::new();
    let mut full_response = String::new();
    let skin = MadSkin::default();
    let mut spinner: Option<Spinner> = None;

    while let Some(event) = stream.next().await {
        // Check for cancellation at each iteration
        if CANCELLED.load(Ordering::SeqCst) {
            eprintln!("{}", "[cancelled]".yellow());
            break;
        }
        match event {
            Ok(event) => match &event.chunk {
                AutoFunctionStreamChunk::Delta(content) => {
                    if let Some(text) = content.as_text() {
                        if stream_output {
                            print!("{text}");
                            io::stdout().flush()?;
                        }
                        response_text.push_str(text);
                        full_response.push_str(text);
                    }
                }
                AutoFunctionStreamChunk::ExecutingFunctions(resp) => {
                    tool_start_time = Some(Instant::now());
                    // Capture interaction ID early for conversation continuity
                    last_id.clone_from(&resp.id);

                    for call in resp.function_calls() {
                        log_event(&format!(
                            "{} {} {}",
                            "CALL".magenta().bold(),
                            call.name.purple(),
                            format_tool_args(call.args).trim().dimmed()
                        ));
                        if let Some(ref cb) = progress_fn {
                            cb(InteractionProgress {
                                tool: call.name.to_string(),
                                status: "executing".to_string(),
                                args: call.args.clone(),
                                duration_ms: None,
                            });
                        }
                    }

                    // Render any text before tool execution
                    flush_response(&mut response_text, &skin, stream_output, false);

                    // Update token count from the response that triggered function calls
                    if let Some(usage) = &resp.usage {
                        let turn_tokens = usage.total_tokens.unwrap_or_else(|| {
                            usage.total_input_tokens.unwrap_or(0)
                                + usage.total_output_tokens.unwrap_or(0)
                        });
                        if turn_tokens > 0 {
                            current_context_size = turn_tokens;
                            total_tokens = turn_tokens;
                        }
                    }

                    // Start spinner if not an interactive tool
                    stop_spinner(&mut spinner).await;
                    let has_interactive = resp.function_calls().iter().any(|c| c.name == "ask_user");
                    if !has_interactive {
                        spinner = Some(Spinner::start());
                    }
                }
                AutoFunctionStreamChunk::FunctionResults(results) => {
                    // Stop spinner
                    stop_spinner(&mut spinner).await;

                    let manual_duration = tool_start_time.map(|t| t.elapsed()).unwrap_or_default();

                    for result in results {
                        tool_calls.push(result.name.clone());

                        let has_error = result.result.get("error").is_some();

                        let duration = if result.duration.as_nanos() == 0 {
                            manual_duration
                        } else {
                            result.duration
                        };

                        if let Some(ref cb) = progress_fn {
                            cb(InteractionProgress {
                                tool: result.name.clone(),
                                status: "completed".to_string(),
                                args: result.args.clone(),
                                duration_ms: Some(duration.as_millis() as u64),
                            });
                        }

                        let formatted = format_tool_result(
                            &result.name,
                            &result.args,
                            duration,
                            current_context_size,
                            has_error,
                        );
                        log_event(&formatted);
                        eprintln!("{formatted}");
                    }
                }
                AutoFunctionStreamChunk::Complete(resp) => {
                    last_id.clone_from(&resp.id);

                    // Render accumulated text as markdown
                    flush_response(&mut response_text, &skin, stream_output, true);

                    // Log final token usage
                    if let Some(usage) = &resp.usage {
                        let total_in = usage.total_input_tokens.unwrap_or(0);
                        let total_out = usage.total_output_tokens.unwrap_or(0);
                        current_context_size = total_in + total_out;
                        total_tokens = current_context_size;
                        eprintln!("[{}→{} tok]", total_in, total_out);
                    }
                }
                AutoFunctionStreamChunk::MaxLoopsReached(_) => {
                    eprintln!("\n{}", "[max tool loops reached]".bright_red());
                }
                _ => {}
            },
            Err(e) => {
                let err_msg = e.to_string();
                eprintln!("\n{}", format!("[stream error: {err_msg}]").bright_red());
                return Err(anyhow::anyhow!(err_msg));
            }
        }
    }

    // Render any remaining text (e.g., if stream ended abruptly or on error)
    flush_response(&mut response_text, &skin, stream_output, false);

    // Cleanup spinner if it's still running
    stop_spinner(&mut spinner).await;

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
