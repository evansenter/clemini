use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use futures_util::StreamExt;
use genai_rs::{AutoFunctionStreamChunk, Client, Content};
use serde_json::Value;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use std::env;
use std::io::{self, IsTerminal, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use termimad::MadSkin;

mod tools;

use tools::CleminiToolService;

const DEFAULT_MODEL: &str = "gemini-3-flash-preview";
const CONTEXT_WINDOW_LIMIT: u32 = 1_000_000;

const SYSTEM_PROMPT: &str = r#"You are clemini, a coding assistant that helps users with software engineering tasks.

## Core Principles
- Be concise. Focus on getting things done. Avoid long explanations unless asked.
- Read files before editing them. Never guess at file contents.
- Verify your changes work (run `cargo check`, tests, etc.) before considering a task complete.

## Tool Selection
- `glob` - Find files by pattern: `**/*.rs`, `src/**/*.ts`
- `grep` - Search file contents with regex. Use `(?i)` for case-insensitive.
- `read_file` - Read specific files you know exist.
- `edit` - Surgical string replacement. `old_string` must match EXACTLY and uniquely. Re-read the file if it fails.
- `write_file` - Create new files or completely rewrite existing ones.
- `bash` - Run shell commands. Use for: git, builds, tests, `ls`, complex pipelines.
- `web_search` - Search the web via DuckDuckGo for current information.
- `web_fetch` - Fetch content from a specific URL.
- `ask_user` - **Use this when uncertain.** Ask clarifying questions rather than guessing wrong.
- `todo_write` - **Use this for complex multi-step tasks.** Track progress visibly.

## When to Ask vs. Proceed
- Multiple valid approaches? → Ask which the user prefers.
- Ambiguous requirements? → Ask for clarification.
- Found multiple matches? → Ask which one they meant.
- Simple, obvious task? → Just do it.

## Task Management
For tasks with 3+ steps, use `todo_write` to:
- Break work into trackable items
- Show progress to the user
- Ensure nothing is forgotten

## Quality Gates
- After writing code: run `cargo check` or equivalent
- After fixing bugs: verify the fix works
- Before finishing: ensure all changes compile/pass

## Anti-patterns to Avoid
- Creating temporary helper scripts (use existing tools instead)
- Editing files you haven't read
- Making changes without verifying they work
- Long explanations when action is needed
- Guessing when you could ask

## Self-Improvement
When you encounter recurring issues or discover better patterns:
- Update THIS system instruction (in src/main.rs SYSTEM_PROMPT) with general guidance
- Keep additions concise and actionable
- Only add guidance that applies broadly, not task-specific notes
"#;

#[derive(serde::Deserialize, Default)]
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

    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY environment variable not set");
    let client = Client::new(api_key);

    let cwd = std::fs::canonicalize(&args.cwd)?;
    let tool_service = Arc::new(CleminiToolService::new(cwd.clone(), bash_timeout));

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
        let _ = run_interaction(&client, &tool_service, &prompt, None, &model, args.stream).await?;
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
                        "Estimated context size: {}/{} tokens ({:.1}%)",
                        last_estimated_context_size,
                        CONTEXT_WINDOW_LIMIT,
                        (f64::from(last_estimated_context_size) / f64::from(CONTEXT_WINDOW_LIMIT))
                            * 100.0
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
                    println!("  Total Tokens:  {}", total_session_tokens);
                    println!(
                        "  Context usage: {}/{} tokens ({:.1}%)",
                        last_estimated_context_size,
                        CONTEXT_WINDOW_LIMIT,
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
                    eprintln!("  /t, /tokens       Show estimated context size");
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
                )
                .await
                {
                    Ok(result) => {
                        last_interaction_id = result.id;
                        last_estimated_context_size = result.context_size;
                        total_interactions += 1;
                        total_tool_calls += result.tool_calls;
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
        if !stream_output {
            skin.print_text(response_text);
        }
        response_text.clear();
        println!();
    } else if force_newline {
        println!();
    }
}

struct InteractionResult {
    id: Option<String>,
    context_size: u32,
    total_tokens: u32,
    tool_calls: u32,
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

#[allow(clippy::too_many_lines)]
async fn run_interaction(
    client: &Client,
    tool_service: &Arc<CleminiToolService>,
    input: &str,
    previous_interaction_id: Option<&str>,
    model: &str,
    stream_output: bool,
) -> Result<InteractionResult> {
    // Build the interaction - system instruction must be sent on every turn
    // (it's NOT inherited via previousInteractionId per genai-rs docs)
    let interaction = client
        .interaction()
        .with_model(model)
        .with_tool_service(tool_service.clone())
        .with_system_instruction(SYSTEM_PROMPT)
        .with_content(vec![Content::text(input)])
        .with_max_function_call_loops(100);

    let mut stream = if let Some(prev_id) = previous_interaction_id {
        interaction
            .with_previous_interaction(prev_id)
            .create_stream_with_auto_functions()
    } else {
        interaction.create_stream_with_auto_functions()
    };

    let mut last_id: Option<String> = None;
    let mut current_context_size: u32 = 0;
    let mut total_tokens: u32 = 0;
    let mut tool_calls: u32 = 0;
    let mut response_text = String::new();
    let skin = MadSkin::default();
    let mut spinner: Option<Spinner> = None;

    while let Some(event) = stream.next().await {
        match event {
            Ok(event) => match &event.chunk {
                AutoFunctionStreamChunk::Delta(content) => {
                    if let Some(text) = content.as_text() {
                        if stream_output {
                            print!("{text}");
                            io::stdout().flush()?;
                        }
                        response_text.push_str(text);
                    }
                }
                AutoFunctionStreamChunk::ExecutingFunctions(resp) => {
                    // Capture interaction ID early for conversation continuity
                    last_id.clone_from(&resp.id);

                    // Render any text before tool execution
                    flush_response(&mut response_text, &skin, stream_output, false);

                    // Update token count from the response that triggered function calls
                    if let Some(usage) = &resp.usage {
                        let turn_tokens = usage.total_input_tokens.unwrap_or(0)
                            + usage.total_output_tokens.unwrap_or(0);
                        current_context_size = turn_tokens;
                        total_tokens += turn_tokens;
                    }

                    // Start spinner
                    stop_spinner(&mut spinner).await;
                    spinner = Some(Spinner::start());
                }
                AutoFunctionStreamChunk::FunctionResults(results) => {
                    // Stop spinner
                    stop_spinner(&mut spinner).await;

                    // Calculate tokens added by function results.
                    // Note: This is a crude estimate (approx. 4 chars per token).
                    let mut tokens_added: u32 = 0;
                    for result in results {
                        tool_calls += 1;
                        let result_str = result.result.to_string();
                        tokens_added += u32::try_from(result_str.len() / 4).unwrap_or(u32::MAX); // ~4 chars per token
                    }
                    current_context_size += tokens_added;
                    // We don't add to total_tokens here because the next turn's input usage will include these.

                    // Log each result with timing and tokens
                    for result in results {
                        let has_error = result.result.get("error").is_some();
                        let error_suffix = if has_error {
                            " ERROR".bright_red().bold().to_string()
                        } else {
                            String::new()
                        };
                        let elapsed_secs = result.duration.as_secs_f32();

                        eprintln!(
                            "[{}] {}{}, {:.1}k tokens (+{}){}",
                            result.name.cyan(),
                            format_tool_args(&result.args).dimmed(),
                            format!("{:.1}s", elapsed_secs).yellow(),
                            f64::from(current_context_size) / 1000.0,
                            tokens_added,
                            error_suffix
                        );
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
                        total_tokens += total_in + total_out;
                        eprintln!(
                            "[{}→{} tok]",
                            total_in,
                            total_out
                        );
                    }
                }
                AutoFunctionStreamChunk::MaxLoopsReached(_) => {
                    eprintln!("\n{}", "[max tool loops reached]".bright_red());
                }
                _ => {}
            },
            Err(e) => {
                eprintln!("\n{}", format!("[stream error: {e}]").bright_red());
                break;
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
        context_size: current_context_size,
        total_tokens,
        tool_calls,
    })
}
