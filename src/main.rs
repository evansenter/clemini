use anyhow::Result;
use clap::Parser;
use futures_util::StreamExt;
use genai_rs::{AutoFunctionStreamChunk, Client, Content};
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use std::env;
use std::io::{self, Write};
use std::sync::Arc;

mod tools;

use tools::CleminiToolService;

const MODEL: &str = "gemini-3-flash-preview";

const SYSTEM_PROMPT: &str = r#"You are clemini, a coding assistant that helps users with software engineering tasks.

You have access to tools for reading files, writing files, and executing bash commands.
Use these tools to help users accomplish their goals.

When working on tasks:
1. Read relevant files to understand the context
2. Make changes using the write tool
3. Run commands to verify your changes work

Be concise in your responses. Focus on getting things done."#;

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
    eprintln!("Model: {}", MODEL);
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

                rl.add_history_entry(input)?;

                match run_interaction(client, tool_service, input, last_interaction_id.as_deref())
                    .await
                {
                    Ok(new_id) => {
                        last_interaction_id = new_id;
                    }
                    Err(e) => {
                        eprintln!("\n[error: {}]", e);
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                eprintln!("[interrupted]");
                continue;
            }
            Err(ReadlineError::Eof) => {
                break;
            }
            Err(err) => {
                eprintln!("[readline error: {}]", err);
                break;
            }
        }
    }

    Ok(())
}

async fn run_interaction(
    client: &Client,
    tool_service: &Arc<CleminiToolService>,
    input: &str,
    previous_interaction_id: Option<&str>,
) -> Result<Option<String>> {
    // Due to genai-rs's typestate pattern, we need separate code paths for first turn vs continuation
    let mut stream = if let Some(prev_id) = previous_interaction_id {
        // Continuation turn - chain to previous interaction
        client
            .interaction()
            .with_model(MODEL)
            .with_tool_service(tool_service.clone())
            .with_previous_interaction(prev_id)
            .with_content(vec![Content::text(input)])
            .create_stream_with_auto_functions()
    } else {
        // First turn - include system instruction
        client
            .interaction()
            .with_model(MODEL)
            .with_tool_service(tool_service.clone())
            .with_system_instruction(SYSTEM_PROMPT)
            .with_content(vec![Content::text(input)])
            .create_stream_with_auto_functions()
    };

    let mut last_id: Option<String> = None;

    while let Some(event) = stream.next().await {
        match event {
            Ok(event) => match &event.chunk {
                AutoFunctionStreamChunk::Delta(content) => {
                    if let Some(text) = content.as_text() {
                        print!("{}", text);
                        io::stdout().flush()?;
                    }
                }
                AutoFunctionStreamChunk::ExecutingFunctions(resp) => {
                    let calls = resp.function_calls();
                    for call in &calls {
                        eprintln!("\n[executing: {}]", call.name);
                    }
                }
                AutoFunctionStreamChunk::FunctionResults(results) => {
                    for result in results {
                        // Check if result contains an error by inspecting the JSON
                        if let Some(err) = result.result.get("error") {
                            eprintln!("[tool error: {}]", err);
                        }
                    }
                }
                AutoFunctionStreamChunk::Complete(resp) => {
                    last_id = resp.id.clone();
                    println!();

                    // Log token usage
                    if let Some(usage) = &resp.usage {
                        eprintln!(
                            "[tokens: {} in, {} out]",
                            usage.total_input_tokens.unwrap_or(0),
                            usage.total_output_tokens.unwrap_or(0)
                        );
                    }
                }
                AutoFunctionStreamChunk::MaxLoopsReached(_) => {
                    eprintln!("[max tool loops reached]");
                }
                _ => {}
            },
            Err(e) => {
                eprintln!("\n[stream error: {}]", e);
                break;
            }
        }
    }

    Ok(last_id)
}
