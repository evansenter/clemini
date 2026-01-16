use anyhow::Result;
use clap::Parser;
use futures_util::StreamExt;
use genai_rs::{AutoFunctionStreamChunk, Client, Content};
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use std::env;
use std::sync::Arc;
use termimad::MadSkin;

mod tools;

use tools::CleminiToolService;

const MODEL: &str = "gemini-3-flash-preview";

const SYSTEM_PROMPT: &str = r"You are clemini, a coding assistant that helps users with software engineering tasks.

You have access to tools for reading files, writing files, and executing bash commands.
Use these tools to help users accomplish their goals.

Guidelines:
- Be efficient with tool calls. Prefer fewer, well-chosen calls over many small ones.
- Tool Choice:
    - Use `glob` for finding files by name patterns (e.g., `**/*.rs`).
    - Use `grep` for searching text within files. Supports regex; use `(?i)` for case-insensitivity.
    - Use `bash` with `find`, `ls`, or other CLI utilities for more complex exploration or system tasks. `bash` is also useful for `grep -C` to see context.
    - Use `read_file` to read the content of specific files.
    - Use `edit` for surgical string replacements in existing files (preferred for small changes). The `old_string` must match EXACTLY and uniquely. If it fails, re-read the file to ensure you have the correct text and whitespace.
    - Use `write_file` for creating new files or completely rewriting existing ones.
    - Avoid creating temporary helper scripts (e.g. Python scripts for text processing). Use existing tools and shell commands instead.
- Codebase Exploration:
    - Start with high-level commands like `ls -F` or `bash` with `find . -maxdepth 2`.
    - Read only the files most relevant to the task.
- Error Handling:
    - If a tool returns an error, analyze it and try an alternative approach.
    - For example, if `read_file` fails because a file doesn't exist, use `ls` or `glob` to find the correct path.
- Be extremely concise in your responses. Focus on getting things done. Avoid long explanations unless necessary.
- Before editing or overwriting a file, ensure you have read its current content to understand the context.
";

#[derive(Parser)]
#[command(name = "clemini")]
#[command(version)]
#[command(about = "A Gemini-powered coding CLI")]
struct Args {
    /// Initial prompt to run (non-interactive mode)
    #[arg(short, long)]
    prompt: Option<String>,

    /// Working directory
    #[arg(short = 'C', long, default_value = ".")]
    cwd: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY environment variable not set");
    let client = Client::new(api_key);

    let cwd = std::fs::canonicalize(&args.cwd)?;
    let tool_service = Arc::new(CleminiToolService::new(cwd.clone()));

    eprintln!("clemini v{}", env!("CARGO_PKG_VERSION"));
    eprintln!("Working directory: {}", cwd.display());
    eprintln!("Model: {MODEL}");
    eprintln!();

    if let Some(prompt) = args.prompt {
        // Non-interactive mode: run single prompt
        run_interaction(&client, &tool_service, &prompt, None).await?;
    } else {
        // Interactive REPL mode
        run_repl(&client, &tool_service).await?;
    }

    Ok(())
}

async fn run_repl(client: &Client, tool_service: &Arc<CleminiToolService>) -> Result<()> {
    let mut rl = DefaultEditor::new()?;
    let mut last_interaction_id: Option<String> = None;

    loop {
        let readline = rl.readline("> ");
        match readline {
            Ok(line) => {
                let input = line.trim();
                if input.is_empty() {
                    continue;
                }

                if input == "/quit" || input == "/exit" {
                    break;
                }

                if input == "/clear" {
                    last_interaction_id = None;
                    eprintln!("[conversation cleared]");
                    continue;
                }

                if input == "/version" {
                    eprintln!("clemini v{}", env!("CARGO_PKG_VERSION"));
                    eprintln!("Model: {MODEL}");
                    continue;
                }

                if input == "/diff" {
                    run_git_command(&["diff"], "no uncommitted changes");
                    continue;
                }

                if input == "/status" {
                    run_git_command(&["status", "--short"], "clean working directory");
                    continue;
                }

                if input == "/log" {
                    run_git_command(&["log", "--oneline", "-5"], "no commits found");
                    continue;
                }

                if input == "/help" {
                    eprintln!("Commands:");
                    eprintln!("  /quit, /exit  Exit the REPL");
                    eprintln!("  /clear        Clear conversation history");
                    eprintln!("  /version      Show version and model");
                    eprintln!("  /diff         Show uncommitted git changes");
                    eprintln!("  /status       Show git status");
                    eprintln!("  /log          Show last 5 git commits");
                    eprintln!("  /help         Show this help message");
                    eprintln!();
                    eprintln!("Tools:");
                    eprintln!("  read_file     Read file contents");
                    eprintln!("  write_file    Create/overwrite files");
                    eprintln!("  edit          Surgical string replacement");
                    eprintln!("  bash          Run shell commands");
                    eprintln!("  glob          Find files by pattern");
                    eprintln!("  grep          Search text in files");
                    eprintln!();
                    continue;
                }

                rl.add_history_entry(input)?;

                match run_interaction(client, tool_service, input, last_interaction_id.as_deref())
                    .await
                {
                    Ok(new_id) => {
                        last_interaction_id = new_id;
                    }
                    Err(e) => {
                        eprintln!("\n[error: {e}]");
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

#[allow(clippy::too_many_lines)]
async fn run_interaction(
    client: &Client,
    tool_service: &Arc<CleminiToolService>,
    input: &str,
    previous_interaction_id: Option<&str>,
) -> Result<Option<String>> {
    // Build the interaction - system instruction must be sent on every turn
    // (it's NOT inherited via previousInteractionId per genai-rs docs)
    let mut stream = if let Some(prev_id) = previous_interaction_id {
        // Continuation turn - chain to previous interaction
        client
            .interaction()
            .with_model(MODEL)
            .with_tool_service(tool_service.clone())
            .with_previous_interaction(prev_id)
            .with_system_instruction(SYSTEM_PROMPT)
            .with_content(vec![Content::text(input)])
            .with_max_function_call_loops(100)
            .create_stream_with_auto_functions()
    } else {
        // First turn
        client
            .interaction()
            .with_model(MODEL)
            .with_tool_service(tool_service.clone())
            .with_system_instruction(SYSTEM_PROMPT)
            .with_content(vec![Content::text(input)])
            .with_max_function_call_loops(100)
            .create_stream_with_auto_functions()
    };

    let mut last_id: Option<String> = None;
    let mut estimated_context_size: u32 = 0;
    let mut response_text = String::new();
    let skin = MadSkin::default();

    while let Some(event) = stream.next().await {
        match event {
            Ok(event) => match &event.chunk {
                AutoFunctionStreamChunk::Delta(content) => {
                    if let Some(text) = content.as_text() {
                        response_text.push_str(text);
                    }
                }
                AutoFunctionStreamChunk::ExecutingFunctions(resp) => {
                    // Capture interaction ID early for conversation continuity
                    last_id.clone_from(&resp.id);

                    // Render any text before tool execution
                    if !response_text.is_empty() {
                        skin.print_text(&response_text);
                        response_text.clear();
                        println!();
                    }

                    // Update token count from the response that triggered function calls
                    if let Some(usage) = &resp.usage {
                        estimated_context_size = usage.total_input_tokens.unwrap_or(0)
                            + usage.total_output_tokens.unwrap_or(0);
                    }
                }
                AutoFunctionStreamChunk::FunctionResults(results) => {
                    // Calculate tokens added by function results.
                    // Note: This is a crude estimate (approx. 4 chars per token).
                    let mut tokens_added: u32 = 0;
                    for result in results {
                        let result_str = result.result.to_string();
                        tokens_added += u32::try_from(result_str.len() / 4).unwrap_or(u32::MAX); // ~4 chars per token
                    }
                    estimated_context_size += tokens_added;

                    // Log each result with timing and tokens
                    for result in results {
                        let has_error = result.result.get("error").is_some();
                        let error_suffix = if has_error { " ERROR" } else { "" };
                        let elapsed_secs = result.duration.as_secs_f32();

                        eprintln!(
                            "[{}] {:.1}s, {:.1}k tokens (+{}){}",
                            result.name,
                            elapsed_secs,
                            f64::from(estimated_context_size) / 1000.0,
                            tokens_added,
                            error_suffix
                        );
                    }
                }
                AutoFunctionStreamChunk::Complete(resp) => {
                    last_id.clone_from(&resp.id);

                    // Render accumulated text as markdown
                    if !response_text.is_empty() {
                        skin.print_text(&response_text);
                        response_text.clear();
                    }
                    println!();

                    // Log final token usage
                    if let Some(usage) = &resp.usage {
                        let total_in = usage.total_input_tokens.unwrap_or(0);
                        let total_out = usage.total_output_tokens.unwrap_or(0);
                        eprintln!(
                            "[total: {:.1}k tokens ({} in + {} out)]",
                            f64::from(total_in + total_out) / 1000.0,
                            total_in,
                            total_out
                        );
                    }
                }
                AutoFunctionStreamChunk::MaxLoopsReached(_) => {
                    eprintln!("\n[max tool loops reached]");
                }
                _ => {}
            },
            Err(e) => {
                eprintln!("\n[stream error: {e}]");
                break;
            }
        }
    }

    // Render any remaining text (e.g., if stream ended abruptly or on error)
    if !response_text.is_empty() {
        skin.print_text(&response_text);
        println!();
    }

    Ok(last_id)
}
