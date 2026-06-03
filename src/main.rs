use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use std::collections::HashMap;

mod cli;
mod llm;
mod model_profiles;
mod session;
mod tools;
mod types;

use cli::env_flag_or;
use cli::parse_workspace_arg;
use cli::prompt;
use llm::ollama::call_ollama;
use model_profiles::ModelProfile;
use model_profiles::profile_for_model;
use session::append_session_event;
use session::init_session_log;
use session::log_tool_outcomes;
use tools::format_tool_results;
use tools::parser::deferred_command_feedback;
use tools::parser::looks_like_deferred_command_instructions;
use tools::parser::looks_like_unsupported_tool_request;
use tools::parser::parse_tool_calls;
use tools::parser::unsupported_tool_feedback;
use tools::print_tool_results;
use tools::print_turn_summary;
use tools::run_tool_calls;
use tools::tool_result_guidance;
use types::AppConfig;
use types::ChatMessage;
use types::ToolOutcome;

const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434/api/chat";
const MAX_TOOL_TURNS: usize = 12;

const BASE_SYSTEM_PROMPT: &str = r#"
You are Mini Codex, a local coding assistant.

You can either answer normally or request one or more tool calls.

Available tool:
- shell: execute a local shell command after user approval. It accepts optional cwd.
- read_file: read a UTF-8 file inside the current workspace.
- write_file: create or overwrite a UTF-8 file inside the current workspace after user approval.
- patch_file: replace an exact text fragment in a UTF-8 file after user approval.
- list_dir: list a directory inside the current workspace.

When you need a command, respond with exactly this format and no extra text:

<tool_call>
{"tool":"shell","cmd":"pwd"}
</tool_call>

<tool_call>
{"tool":"shell","cmd":"cargo build","cwd":"hello-agent"}
</tool_call>

Other examples:

<tool_call>
{"tool":"read_file","path":"README.md"}
</tool_call>

For writing files, prefer this raw-content format:

<write_file path="hello.txt">
hello
</write_file>

<tool_call>
{"tool":"list_dir","path":"."}
</tool_call>

Short XML-style tool calls are also accepted:

<read_file path="README.md"/>
<list_dir path="."/>
<shell cmd="cargo run" cwd="hello-agent"/>

<tool_call>
{"tool":"patch_file","path":"src/main.rs","old":"hello","new":"hello from mini-codex"}
</tool_call>

Rules:
- Do not claim you executed a command unless a tool result was provided.
- If a tool result is empty, say it is empty. Do not invent files, output, or command results.
- Prefer read_file, write_file, patch_file, and list_dir for file operations.
- For small edits to existing files, use patch_file instead of write_file.
- Prefer shell with cwd over commands that start with cd.
- Do not use cd in shell commands; cwd does not persist between tool calls.
- Do not create empty files with touch before writing them. Use write_file with the final content.
- Prefer small, specific commands.
- Do not use destructive commands.
- When creating Rust projects, use edition = "2024", write Cargo.toml and src/main.rs, then use shell to run cargo build with cwd. For simple apps, prefer standard-library Rust with no external crates. Do not add web frameworks such as hyper, axum, or actix unless the user explicitly asks for an HTTP server or API.
- When creating React projects, prefer a minimal Vite app. If the user gives a project name, write every file under that directory and run shell commands with cwd set to that directory. Write package.json directly with dev and build scripts; do not use npm init -y. Write index.html, vite.config.js, src/main.jsx, src/App.jsx, and optionally src/App.css. In index.html include <div id="root"></div> and use <script type="module" src="/src/main.jsx"></script>. Then use shell to run npm install and npm run build with cwd. Do not run npm run dev unless the user explicitly asks you to start a dev server.
- If you can answer without a tool, answer directly.
"#;

#[tokio::main]
async fn main() -> Result<()> {
    let model = std::env::var("MINI_CODEX_MODEL")
        .unwrap_or_else(|_| model_profiles::DEFAULT_MODEL.to_string());
    let model_profile = profile_for_model(&model)?;
    let ollama_url =
        std::env::var("MINI_CODEX_OLLAMA_URL").unwrap_or_else(|_| DEFAULT_OLLAMA_URL.to_string());
    let auto_approve = env_flag_or("MINI_CODEX_AUTO_APPROVE", true);
    let plan_first = env_flag_or("MINI_CODEX_PLAN_FIRST", true);
    let workspace = parse_workspace_arg()?.canonicalize().with_context(|| {
        "failed to resolve workspace; create it first or pass an existing directory".to_string()
    })?;
    let config = AppConfig {
        session_log_path: workspace.join(".mini-codex").join("session.jsonl"),
        workspace,
    };
    init_session_log(&config, &model, &model_profile).with_context(|| {
        format!(
            "failed to initialize session log at {}",
            config.session_log_path.display()
        )
    })?;

    let client = reqwest::Client::new();
    let mut messages = vec![ChatMessage {
        role: "system".to_string(),
        content: model_profile.system_prompt(BASE_SYSTEM_PROMPT),
    }];

    print_startup(&config, &model, &model_profile, auto_approve, plan_first);

    loop {
        let input = prompt("› ")?;
        if input.trim().is_empty() {
            continue;
        }
        if matches!(input.trim(), "/exit" | "/quit") {
            break;
        }
        append_session_event(&config, "user", json!({ "content": input }))?;

        let direct_tool_calls = parse_tool_calls(&input, &model_profile)?;
        if !direct_tool_calls.is_empty() {
            let outcomes =
                run_tool_calls(&config, &model_profile, direct_tool_calls, auto_approve).await?;
            print_tool_results(&outcomes);
            print_turn_summary(&outcomes);
            log_tool_outcomes(&config, &outcomes)?;
            continue;
        }

        messages.push(ChatMessage {
            role: "user".to_string(),
            content: input.clone(),
        });

        run_agent_turn(
            &config,
            &client,
            &ollama_url,
            &model,
            &model_profile,
            &input,
            &mut messages,
            auto_approve,
            plan_first,
        )
        .await?;
    }

    Ok(())
}

fn print_startup(
    config: &AppConfig,
    model: &str,
    model_profile: &ModelProfile,
    auto_approve: bool,
    plan_first: bool,
) {
    println!("mini-codex");
    println!("model: {model}");
    println!(
        "model profile: {} ({})",
        model_profile.display_name, model_profile.id
    );
    println!("workspace: {}", config.workspace.display());
    println!("session log: {}", config.session_log_path.display());
    println!("auto approve: {auto_approve}");
    println!("plan first: {plan_first}");
    println!("type /exit to quit\n");
}

async fn run_agent_turn(
    config: &AppConfig,
    client: &reqwest::Client,
    ollama_url: &str,
    model: &str,
    model_profile: &ModelProfile,
    original_request: &str,
    messages: &mut Vec<ChatMessage>,
    auto_approve: bool,
    plan_first: bool,
) -> Result<()> {
    if plan_first {
        request_plan_first(config, client, ollama_url, model, model_profile, messages).await?;
    }

    let mut failed_command_counts = HashMap::new();
    for _ in 0..MAX_TOOL_TURNS {
        let answer = call_ollama(client, ollama_url, model, messages).await?;
        append_session_event(config, "assistant", json!({ "content": answer }))?;

        let tool_calls = parse_tool_calls(&answer, model_profile)?;
        if !tool_calls.is_empty() {
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: answer,
            });

            let outcomes = run_tool_calls(config, model_profile, tool_calls, auto_approve).await?;
            print_tool_results(&outcomes);
            log_tool_outcomes(config, &outcomes)?;
            let tool_results = format_tool_results(&outcomes);
            let repeat_guidance = repeated_failed_command_guidance(
                &outcomes,
                &mut failed_command_counts,
                MAX_REPEATED_FAILED_COMMANDS,
            );
            let followup_guidance =
                format!("{}{}", tool_result_guidance(&outcomes), repeat_guidance);
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: format!(
                    "{}{}",
                    model_profile.tool_result_followup(original_request, &tool_results),
                    followup_guidance
                ),
            });
            print_turn_summary(&outcomes);
            if let Some(command) = repeated_failed_command_to_stop(
                &failed_command_counts,
                MAX_REPEATED_FAILED_COMMANDS,
            ) {
                println!(
                    "\nStopped after repeating the same failed command {MAX_REPEATED_FAILED_COMMANDS} times:\n{command}\n"
                );
                return Ok(());
            }
            continue;
        }

        if looks_like_unsupported_tool_request(&answer) {
            println!("\nModel requested an unsupported tool. Asking it to use Mini Codex tools.\n");
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: answer,
            });
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: unsupported_tool_feedback(),
            });
            continue;
        }

        if looks_like_deferred_command_instructions(original_request, &answer) {
            println!(
                "\nModel provided command instructions instead of running them. Asking it to use shell.\n"
            );
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: answer,
            });
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: deferred_command_feedback(original_request),
            });
            continue;
        }

        println!("\n{answer}\n");
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: answer,
        });
        return Ok(());
    }

    println!("\nStopped after {MAX_TOOL_TURNS} tool turns.\n");
    Ok(())
}

const MAX_REPEATED_FAILED_COMMANDS: usize = 3;

fn repeated_failed_command_guidance(
    outcomes: &[ToolOutcome],
    failed_command_counts: &mut HashMap<String, usize>,
    max_repeats: usize,
) -> String {
    let mut repeated = Vec::new();
    for command in outcomes
        .iter()
        .filter_map(|outcome| outcome.command.as_ref())
        .filter(|command| !command.success)
    {
        let count = failed_command_counts
            .entry(command.cmd.clone())
            .and_modify(|count| *count += 1)
            .or_insert(1);
        if *count >= 2 {
            repeated.push(format!("`{}` failed {count} times", command.cmd));
        }
    }

    if repeated.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nRepeated failure guidance:\n- Do not repeat the same failed command. {}\n- Fix the root cause first by reading or patching the relevant file, then run a different validation command. The turn will stop after {max_repeats} repeats of the same failed command.",
            repeated.join("; ")
        )
    }
}

fn repeated_failed_command_to_stop(
    failed_command_counts: &HashMap<String, usize>,
    max_repeats: usize,
) -> Option<&str> {
    failed_command_counts
        .iter()
        .find(|(_, count)| **count >= max_repeats)
        .map(|(command, _)| command.as_str())
}

async fn request_plan_first(
    config: &AppConfig,
    client: &reqwest::Client,
    ollama_url: &str,
    model: &str,
    model_profile: &ModelProfile,
    messages: &mut Vec<ChatMessage>,
) -> Result<()> {
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: model_profile.plan_prompt().to_string(),
    });

    let raw_plan = call_ollama(client, ollama_url, model, messages).await?;
    let plan = model_profile.sanitize_plan_response(&raw_plan);
    append_session_event(config, "plan", json!({ "content": plan }))?;
    println!("\nPlan:\n{plan}\n");
    messages.push(ChatMessage {
        role: "assistant".to_string(),
        content: plan,
    });
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: model_profile.execute_plan_prompt().to_string(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use types::ExecutedCommand;

    #[test]
    fn repeated_failed_command_guidance_warns_on_second_failure() {
        let mut counts = HashMap::new();
        let outcomes = vec![ToolOutcome {
            result: "failed".to_string(),
            changed_file: None,
            command: Some(ExecutedCommand {
                cmd: "npm install".to_string(),
                cwd: PathBuf::from("/tmp/project"),
                success: false,
            }),
        }];

        assert_eq!(
            repeated_failed_command_guidance(&outcomes, &mut counts, 3),
            ""
        );
        let guidance = repeated_failed_command_guidance(&outcomes, &mut counts, 3);

        assert!(guidance.contains("failed 2 times"));
        assert!(guidance.contains("Do not repeat the same failed command"));
    }

    #[test]
    fn repeated_failed_command_stops_at_limit() {
        let mut counts = HashMap::new();
        counts.insert("npm install".to_string(), 3);

        assert_eq!(
            repeated_failed_command_to_stop(&counts, 3),
            Some("npm install")
        );
    }
}
