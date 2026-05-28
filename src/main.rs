use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::process::Command;

const DEFAULT_MODEL: &str = "qwen3-coder";
const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434/api/chat";

const SYSTEM_PROMPT: &str = r#"
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
- Prefer small, specific commands.
- Do not use destructive commands.
- When creating Rust projects, write Cargo.toml and src/main.rs, then ask to run cargo build with cwd.
- If you can answer without a tool, answer directly.
"#;

#[derive(Clone, Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    message: ChatResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct ToolCall {
    tool: String,
    #[serde(default)]
    cmd: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    old: String,
    #[serde(default)]
    new: String,
}

#[derive(Debug)]
struct ToolOutcome {
    result: String,
    changed_file: Option<ChangedFile>,
    command: Option<ExecutedCommand>,
}

#[derive(Debug)]
struct ChangedFile {
    action: &'static str,
    path: PathBuf,
}

#[derive(Debug)]
struct ExecutedCommand {
    cmd: String,
    cwd: PathBuf,
    success: bool,
}

#[derive(Debug)]
struct AppConfig {
    workspace: PathBuf,
    session_log_path: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let model = std::env::var("MINI_CODEX_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let ollama_url =
        std::env::var("MINI_CODEX_OLLAMA_URL").unwrap_or_else(|_| DEFAULT_OLLAMA_URL.to_string());
    let auto_approve = env_flag("MINI_CODEX_AUTO_APPROVE");
    let plan_first = env_flag("MINI_CODEX_PLAN_FIRST");
    let workspace = parse_workspace_arg()?.canonicalize().with_context(|| {
        "failed to resolve workspace; create it first or pass an existing directory".to_string()
    })?;
    let config = AppConfig {
        session_log_path: workspace.join(".mini-codex").join("session.jsonl"),
        workspace,
    };
    init_session_log(&config).with_context(|| {
        format!(
            "failed to initialize session log at {}",
            config.session_log_path.display()
        )
    })?;

    let client = reqwest::Client::new();
    let mut messages = vec![ChatMessage {
        role: "system".to_string(),
        content: SYSTEM_PROMPT.trim().to_string(),
    }];

    println!("mini-codex");
    println!("model: {model}");
    println!("workspace: {}", config.workspace.display());
    println!("session log: {}", config.session_log_path.display());
    println!("auto approve: {auto_approve}");
    println!("plan first: {plan_first}");
    println!("type /exit to quit\n");

    loop {
        let input = prompt("› ")?;
        if input.trim().is_empty() {
            continue;
        }
        if matches!(input.trim(), "/exit" | "/quit") {
            break;
        }
        append_session_event(&config, "user", json!({ "content": input }))?;

        let direct_tool_calls = parse_tool_calls(&input)?;
        if !direct_tool_calls.is_empty() {
            let outcomes = run_tool_calls(&config, direct_tool_calls, auto_approve).await?;
            print_tool_results(&outcomes);
            print_turn_summary(&outcomes);
            log_tool_outcomes(&config, &outcomes)?;
            continue;
        }

        messages.push(ChatMessage {
            role: "user".to_string(),
            content: input,
        });

        run_agent_turn(
            &config,
            &client,
            &ollama_url,
            &model,
            &mut messages,
            auto_approve,
            plan_first,
        )
        .await?;
    }

    Ok(())
}

async fn run_agent_turn(
    config: &AppConfig,
    client: &reqwest::Client,
    ollama_url: &str,
    model: &str,
    messages: &mut Vec<ChatMessage>,
    auto_approve: bool,
    plan_first: bool,
) -> Result<()> {
    if plan_first {
        request_plan_first(config, client, ollama_url, model, messages).await?;
    }

    for _ in 0..6 {
        let answer = call_ollama(client, ollama_url, model, messages).await?;
        append_session_event(config, "assistant", json!({ "content": answer }))?;

        let tool_calls = parse_tool_calls(&answer)?;
        if !tool_calls.is_empty() {
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: answer,
            });

            let outcomes = run_tool_calls(config, tool_calls, auto_approve).await?;
            print_tool_results(&outcomes);
            log_tool_outcomes(config, &outcomes)?;
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: format!("Tool results:\n{}", format_tool_results(&outcomes)),
            });
            print_turn_summary(&outcomes);
            continue;
        }

        println!("\n{answer}\n");
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: answer,
        });
        return Ok(());
    }

    println!("\nStopped after too many tool calls.\n");
    Ok(())
}

async fn request_plan_first(
    config: &AppConfig,
    client: &reqwest::Client,
    ollama_url: &str,
    model: &str,
    messages: &mut Vec<ChatMessage>,
) -> Result<()> {
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: "First provide a short implementation plan. Do not call tools yet.".to_string(),
    });

    let plan = call_ollama(client, ollama_url, model, messages).await?;
    append_session_event(config, "plan", json!({ "content": plan }))?;
    println!("\nPlan:\n{plan}\n");
    messages.push(ChatMessage {
        role: "assistant".to_string(),
        content: plan,
    });
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: "Now execute the plan using tools. Do not repeat the plan.".to_string(),
    });
    Ok(())
}

async fn call_ollama(
    client: &reqwest::Client,
    ollama_url: &str,
    model: &str,
    messages: &[ChatMessage],
) -> Result<String> {
    let request = ChatRequest {
        model: model.to_string(),
        messages: messages.to_vec(),
        stream: false,
    };

    let response = client
        .post(ollama_url)
        .json(&request)
        .send()
        .await
        .context("failed to call Ollama")?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Ollama returned {status}: {body}");
    }

    let response = response
        .json::<ChatResponse>()
        .await
        .context("failed to parse Ollama response")?;

    Ok(response.message.content.trim().to_string())
}

fn parse_tool_calls(answer: &str) -> Result<Vec<ToolCall>> {
    let mut tool_calls = Vec::new();

    tool_calls.extend(parse_write_file_blocks(answer)?);
    tool_calls.extend(parse_xml_style_tool_calls(answer)?);
    tool_calls.extend(parse_tagged_tool_call_blocks(answer)?);
    tool_calls.extend(parse_fenced_tool_calls(answer)?);

    if !tool_calls.is_empty() {
        return Ok(tool_calls);
    }

    if let Ok(tool_call) = serde_json::from_str::<ToolCall>(answer.trim()) {
        return Ok(vec![tool_call]);
    }

    if let Some(tool_call) = parse_malformed_write_file_call(answer)? {
        return Ok(vec![tool_call]);
    }

    Ok(Vec::new())
}

fn parse_tagged_tool_call_blocks(answer: &str) -> Result<Vec<ToolCall>> {
    let mut tool_calls = Vec::new();
    let mut rest = answer;
    while let Some(start) = rest.find("<tool_call>") {
        let after_start = &rest[start + "<tool_call>".len()..];
        let Some(end) = after_start.find("</tool_call>") else {
            break;
        };
        let json = after_start[..end].trim();
        let tool_call = serde_json::from_str::<ToolCall>(json)
            .with_context(|| format!("malformed tool call: {json}"))?;
        tool_calls.push(tool_call);
        rest = &after_start[end + "</tool_call>".len()..];
    }
    Ok(tool_calls)
}

fn parse_write_file_blocks(answer: &str) -> Result<Vec<ToolCall>> {
    let mut tool_calls = Vec::new();
    let mut rest = answer;
    while let Some(start) = rest.find("<write_file") {
        let Some(open_end_relative) = rest[start..].find('>') else {
            break;
        };
        let open_end = start + open_end_relative;
        let opening_tag = &rest[start..=open_end];
        let path = extract_attr(opening_tag, "path")
            .with_context(|| format!("missing path in write_file tag: {opening_tag}"))?;

        let content_start = open_end + 1;
        let Some(close_relative) = rest[content_start..].find("</write_file>") else {
            break;
        };
        let content = rest[content_start..content_start + close_relative].to_string();
        tool_calls.push(ToolCall {
            tool: "write_file".to_string(),
            cmd: String::new(),
            cwd: String::new(),
            path,
            content,
            old: String::new(),
            new: String::new(),
        });
        rest = &rest[content_start + close_relative + "</write_file>".len()..];
    }
    Ok(tool_calls)
}

fn parse_xml_style_tool_calls(answer: &str) -> Result<Vec<ToolCall>> {
    let mut tool_calls = Vec::new();
    tool_calls.extend(parse_xml_style_named_tool_calls(answer, "read_file")?);
    tool_calls.extend(parse_xml_style_named_tool_calls(answer, "list_dir")?);
    tool_calls.extend(parse_xml_style_named_tool_calls(answer, "shell")?);
    Ok(tool_calls)
}

fn parse_xml_style_named_tool_calls(answer: &str, tool: &str) -> Result<Vec<ToolCall>> {
    let mut tool_calls = Vec::new();
    let mut rest = answer;
    let start_tag = format!("<{tool}");
    while let Some(start) = rest.find(&start_tag) {
        let Some(end_relative) = rest[start..].find("/>") else {
            break;
        };
        let tag = &rest[start..start + end_relative + "/>".len()];
        let path = extract_attr(tag, "path").unwrap_or_default();
        let cmd = extract_attr(tag, "cmd").unwrap_or_default();
        let cwd = extract_attr(tag, "cwd").unwrap_or_default();
        tool_calls.push(ToolCall {
            tool: tool.to_string(),
            cmd,
            cwd,
            path,
            content: String::new(),
            old: String::new(),
            new: String::new(),
        });
        rest = &rest[start + end_relative + "/>".len()..];
    }
    Ok(tool_calls)
}

fn extract_attr(tag: &str, attr: &str) -> Result<String> {
    let prefix = format!("{attr}=\"");
    let Some(start) = tag.find(&prefix) else {
        anyhow::bail!("attribute not found: {attr}");
    };
    let value_start = start + prefix.len();
    let Some(end) = tag[value_start..].find('"') else {
        anyhow::bail!("unterminated attribute: {attr}");
    };
    Ok(tag[value_start..value_start + end].to_string())
}

fn parse_malformed_write_file_call(answer: &str) -> Result<Option<ToolCall>> {
    let trimmed = answer.trim();
    if !trimmed.contains(r#""tool":"write_file""#) || !trimmed.contains(r#""content"="#) {
        return Ok(None);
    }

    let path = extract_json_string_field(trimmed, "path")
        .with_context(|| format!("missing path in malformed write_file call: {trimmed}"))?;
    let content_marker = r#""content"="#;
    let Some(content_start) = trimmed.find(content_marker) else {
        return Ok(None);
    };
    let raw_content = trimmed[content_start + content_marker.len()..].trim();
    let mut content = raw_content.strip_prefix('"').unwrap_or(raw_content);
    content = content.strip_suffix('}').unwrap_or(content);
    content = content.strip_suffix('"').unwrap_or(content);
    let content = content.replace("\\n", "\n").replace("\\\"", "\"");

    Ok(Some(ToolCall {
        tool: "write_file".to_string(),
        cmd: String::new(),
        cwd: String::new(),
        path,
        content,
        old: String::new(),
        new: String::new(),
    }))
}

fn extract_json_string_field(input: &str, field: &str) -> Result<String> {
    let prefix = format!(r#""{field}":"#);
    let Some(start) = input.find(&prefix) else {
        anyhow::bail!("field not found: {field}");
    };
    let value = &input[start + prefix.len()..];
    let Some(stripped) = value.strip_prefix('"') else {
        anyhow::bail!("field is not a string: {field}");
    };

    let mut escaped = false;
    let mut out = String::new();
    for ch in stripped.chars() {
        if escaped {
            match ch {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                other => {
                    out.push('\\');
                    out.push(other);
                }
            }
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Ok(out);
        } else {
            out.push(ch);
        }
    }

    anyhow::bail!("unterminated string field: {field}");
}

fn parse_fenced_tool_calls(answer: &str) -> Result<Vec<ToolCall>> {
    let mut tool_calls = Vec::new();
    let mut rest = answer;
    while let Some(fence_start) = rest.find("```") {
        let after_opening_fence = &rest[fence_start + "```".len()..];
        let Some(first_newline) = after_opening_fence.find('\n') else {
            break;
        };
        let fenced_body_start = fence_start + "```".len() + first_newline + 1;
        let Some(relative_fence_end) = rest[fenced_body_start..].find("```") else {
            break;
        };
        let fenced_body = rest[fenced_body_start..fenced_body_start + relative_fence_end].trim();
        if fenced_body.starts_with('{') && fenced_body.contains("\"tool\"") {
            let tool_call = serde_json::from_str::<ToolCall>(fenced_body)
                .with_context(|| format!("malformed fenced tool call: {fenced_body}"))?;
            tool_calls.push(tool_call);
        }
        rest = &rest[fenced_body_start + relative_fence_end + "```".len()..];
    }
    Ok(tool_calls)
}

async fn run_tool_calls(
    config: &AppConfig,
    tool_calls: Vec<ToolCall>,
    auto_approve: bool,
) -> Result<Vec<ToolOutcome>> {
    let mut outcomes = Vec::new();
    for (index, tool_call) in tool_calls.into_iter().enumerate() {
        let mut outcome = run_tool_call(config, tool_call, auto_approve)
            .await
            .with_context(|| format!("tool call {} failed", index + 1))?;
        outcome.result = format!("tool_call_{}:\n{}", index + 1, outcome.result);
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

async fn run_tool_call(
    config: &AppConfig,
    tool_call: ToolCall,
    auto_approve: bool,
) -> Result<ToolOutcome> {
    match tool_call.tool.as_str() {
        "shell" => run_shell(config, tool_call, auto_approve).await,
        "read_file" => read_file_tool(config, &tool_call.path).map(unchanged),
        "write_file" => write_file_tool(config, &tool_call.path, &tool_call.content, auto_approve),
        "patch_file" => {
            patch_file_tool(
                config,
                &tool_call.path,
                &tool_call.old,
                &tool_call.new,
                auto_approve,
            )
        }
        "list_dir" => list_dir_tool(config, &tool_call.path).map(unchanged),
        _ => anyhow::bail!("unsupported tool: {}", tool_call.tool),
    }
}

async fn run_shell(
    config: &AppConfig,
    tool_call: ToolCall,
    auto_approve: bool,
) -> Result<ToolOutcome> {
    validate_shell_command(&tool_call.cmd)?;
    let cwd = if tool_call.cwd.trim().is_empty() {
        config.workspace.clone()
    } else {
        workspace_path(config, &tool_call.cwd)?
    };

    println!("\nrequested shell command:");
    println!("  {}", tool_call.cmd);
    println!("cwd:");
    println!("  {}", cwd.display());
    if !approve_or_prompt(auto_approve, "run it? [y/N] ")? {
        return Ok(unchanged("User declined command.".to_string()));
    }

    let output = Command::new("bash")
        .arg("-lc")
        .arg(&tool_call.cmd)
        .current_dir(&cwd)
        .output()
        .await
        .context("failed to run shell command")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(ToolOutcome {
        result: format!(
            "cwd: {}\nexit_status: {}\nstdout:\n{}\nstderr:\n{}",
            cwd.display(),
            output.status,
            stdout,
            stderr
        ),
        changed_file: None,
        command: Some(ExecutedCommand {
            cmd: tool_call.cmd,
            cwd,
            success: output.status.success(),
        }),
    })
}

fn read_file_tool(config: &AppConfig, path: &str) -> Result<String> {
    let path = workspace_path(config, path)?;
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(format!("read_file: {}\n\n{}", path.display(), content))
}

fn write_file_tool(
    config: &AppConfig,
    path: &str,
    content: &str,
    auto_approve: bool,
) -> Result<ToolOutcome> {
    let path = workspace_path(config, path)?;
    let previous_content = match std::fs::read_to_string(&path) {
        Ok(content) => Some(content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
    };

    if let Some(previous_content) = &previous_content {
        println!("\nexisting file will be overwritten:");
        println!("  {}", path.display());
        println!("previous content preview:");
        println!("{}", preview(previous_content));
        println!("diff:");
        println!("{}", simple_diff(previous_content, content));
        if let Some(hint) = patch_hint_for_overwrite(previous_content, content, &path) {
            println!("{hint}");
        }
    } else {
        println!("\nnew file diff:");
        println!("{}", simple_diff("", content));
    }

    println!("\nrequested write file:");
    println!("  {}", path.display());
    println!("  {} bytes", content.len());
    if !approve_or_prompt(auto_approve, "write it? [y/N] ")? {
        return Ok(unchanged("User declined file write.".to_string()));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))?;
    let previous_summary = previous_content
        .as_deref()
        .map(|previous_content| {
            let patch_hint = patch_hint_for_overwrite(previous_content, content, &path)
                .map(|hint| format!("\n{hint}"))
                .unwrap_or_default();
            format!(
                "\nprevious_content_preview:\n{}{}",
                preview(previous_content),
                patch_hint
            )
        })
        .unwrap_or_default();
    Ok(ToolOutcome {
        result: format!("wrote_file: {}{}", path.display(), previous_summary),
        changed_file: Some(ChangedFile {
            action: "wrote",
            path,
        }),
        command: None,
    })
}

fn patch_file_tool(
    config: &AppConfig,
    path: &str,
    old: &str,
    new: &str,
    auto_approve: bool,
) -> Result<ToolOutcome> {
    if old.is_empty() {
        anyhow::bail!("patch_file old text must not be empty");
    }

    let path = workspace_path(config, path)?;
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if !content.contains(old) {
        anyhow::bail!("old text not found in {}", path.display());
    }

    println!("\nrequested patch file:");
    println!("  {}", path.display());
    println!("replace:");
    println!("{old}");
    println!("with:");
    println!("{new}");
    let updated = content.replacen(old, new, 1);
    println!("diff:");
    println!("{}", simple_diff(&content, &updated));
    if !approve_or_prompt(auto_approve, "apply patch? [y/N] ")? {
        return Ok(unchanged("User declined file patch.".to_string()));
    }

    std::fs::write(&path, updated).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(ToolOutcome {
        result: format!("patched_file: {}", path.display()),
        changed_file: Some(ChangedFile {
            action: "patched",
            path,
        }),
        command: None,
    })
}

fn list_dir_tool(config: &AppConfig, path: &str) -> Result<String> {
    let path = workspace_path(config, path)?;
    let mut entries = Vec::new();
    for entry in
        std::fs::read_dir(&path).with_context(|| format!("failed to list {}", path.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let suffix = if file_type.is_dir() { "/" } else { "" };
        entries.push(format!("{}{}", entry.file_name().to_string_lossy(), suffix));
    }
    entries.sort();
    Ok(format!("list_dir: {}\n{}", path.display(), entries.join("\n")))
}

fn workspace_path(config: &AppConfig, path: &str) -> Result<PathBuf> {
    let relative = Path::new(path);
    if relative.is_absolute() {
        anyhow::bail!("absolute paths are not allowed: {path}");
    }
    if relative
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        anyhow::bail!("parent directory segments are not allowed: {path}");
    }

    Ok(config.workspace.join(relative))
}

fn validate_shell_command(cmd: &str) -> Result<()> {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        anyhow::bail!("empty shell command");
    }

    let forbidden = [
        "rm ", "rm\t", "sudo ", "mkfs", "dd ", "shutdown", "reboot", ":(){", "chmod -R",
        "chown -R", "> /dev/", "git reset --hard", "git checkout --",
    ];
    if forbidden.iter().any(|token| trimmed.contains(token)) {
        anyhow::bail!("refusing potentially destructive command: {trimmed}");
    }

    Ok(())
}

fn unchanged(result: String) -> ToolOutcome {
    ToolOutcome {
        result,
        changed_file: None,
        command: None,
    }
}

fn format_tool_results(outcomes: &[ToolOutcome]) -> String {
    outcomes
        .iter()
        .map(|outcome| outcome.result.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn print_tool_results(outcomes: &[ToolOutcome]) {
    println!("\nTool result:\n{}\n", format_tool_results(outcomes));
}

fn print_turn_summary(outcomes: &[ToolOutcome]) {
    let commands = outcomes
        .iter()
        .filter_map(|outcome| outcome.command.as_ref())
        .collect::<Vec<_>>();
    let changed_files = outcomes
        .iter()
        .filter_map(|outcome| outcome.changed_file.as_ref())
        .collect::<Vec<_>>();

    if commands.is_empty() && changed_files.is_empty() {
        return;
    }

    println!("Summary:");
    for changed_file in &changed_files {
        println!("- {} {}", changed_file.action, changed_file.path.display());
    }
    for command in &commands {
        let status = if command.success { "succeeded" } else { "failed" };
        println!(
            "- command `{}` {status} in {}",
            command.cmd,
            command.cwd.display()
        );
    }
    println!();

    if changed_files.is_empty() {
        return;
    }
    println!("Changed files:");
    for changed_file in changed_files {
        println!("- {} {}", changed_file.action, changed_file.path.display());
    }
    println!();
}

fn init_session_log(config: &AppConfig) -> Result<()> {
    if let Some(parent) = config.session_log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    append_session_event(
        config,
        "session_start",
        json!({
            "workspace": config.workspace,
        }),
    )
}

fn log_tool_outcomes(config: &AppConfig, outcomes: &[ToolOutcome]) -> Result<()> {
    append_session_event(
        config,
        "tool_results",
        json!({
            "results": outcomes.iter().map(|outcome| &outcome.result).collect::<Vec<_>>(),
            "changed_files": outcomes
                .iter()
                .filter_map(|outcome| outcome.changed_file.as_ref())
                .map(|changed_file| json!({
                    "action": changed_file.action,
                    "path": changed_file.path,
                }))
                .collect::<Vec<_>>(),
            "commands": outcomes
                .iter()
                .filter_map(|outcome| outcome.command.as_ref())
                .map(|command| json!({
                    "cmd": command.cmd,
                    "cwd": command.cwd,
                    "success": command.success,
                }))
                .collect::<Vec<_>>(),
        }),
    )
}

fn append_session_event(config: &AppConfig, event_type: &str, data: serde_json::Value) -> Result<()> {
    let event = json!({
        "ts": unix_timestamp_secs(),
        "type": event_type,
        "data": data,
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config.session_log_path)
        .with_context(|| format!("failed to open {}", config.session_log_path.display()))?;
    writeln!(file, "{event}")
        .with_context(|| format!("failed to write {}", config.session_log_path.display()))?;
    Ok(())
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn preview(content: &str) -> String {
    const MAX_CHARS: usize = 2000;
    let mut preview = content.chars().take(MAX_CHARS).collect::<String>();
    if content.chars().count() > MAX_CHARS {
        preview.push_str("\n... <truncated>");
    }
    if preview.trim().is_empty() {
        "<empty>".to_string()
    } else {
        preview
    }
}

fn simple_diff(before: &str, after: &str) -> String {
    const MAX_LINES: usize = 120;
    let before_lines = before.lines().collect::<Vec<_>>();
    let after_lines = after.lines().collect::<Vec<_>>();

    if before == after {
        return "  <no changes>".to_string();
    }

    let mut lines = Vec::new();
    lines.push("--- before".to_string());
    lines.push("+++ after".to_string());

    let max_len = before_lines.len().max(after_lines.len());
    for index in 0..max_len {
        if lines.len() >= MAX_LINES {
            lines.push("... <diff truncated>".to_string());
            break;
        }

        match (before_lines.get(index), after_lines.get(index)) {
            (Some(before), Some(after)) if before == after => {
                lines.push(format!("  {before}"));
            }
            (Some(before), Some(after)) => {
                lines.push(format!("- {before}"));
                lines.push(format!("+ {after}"));
            }
            (Some(before), None) => lines.push(format!("- {before}")),
            (None, Some(after)) => lines.push(format!("+ {after}")),
            (None, None) => {}
        }
    }

    lines.join("\n")
}

fn patch_hint_for_overwrite(previous: &str, next: &str, path: &Path) -> Option<String> {
    if previous == next || previous.is_empty() || next.is_empty() {
        return None;
    }

    let changed_chars = changed_char_count(previous, next);
    let larger_len = previous.chars().count().max(next.chars().count()).max(1);
    let small_absolute_change = changed_chars <= 200;
    let small_relative_change = changed_chars * 100 <= larger_len * 25;
    if small_absolute_change || small_relative_change {
        Some(format!(
            "hint: this looks like a small edit to {}. Prefer patch_file next time.",
            path.display()
        ))
    } else {
        None
    }
}

fn changed_char_count(previous: &str, next: &str) -> usize {
    let previous_chars = previous.chars().collect::<Vec<_>>();
    let next_chars = next.chars().collect::<Vec<_>>();

    let prefix_len = previous_chars
        .iter()
        .zip(&next_chars)
        .take_while(|(left, right)| left == right)
        .count();

    let mut suffix_len = 0;
    while suffix_len + prefix_len < previous_chars.len()
        && suffix_len + prefix_len < next_chars.len()
        && previous_chars[previous_chars.len() - 1 - suffix_len]
            == next_chars[next_chars.len() - 1 - suffix_len]
    {
        suffix_len += 1;
    }

    let previous_changed = previous_chars.len().saturating_sub(prefix_len + suffix_len);
    let next_changed = next_chars.len().saturating_sub(prefix_len + suffix_len);
    previous_changed.max(next_changed)
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim_end().to_string())
}

fn approve_or_prompt(auto_approve: bool, label: &str) -> Result<bool> {
    if auto_approve {
        println!("auto-approved");
        return Ok(true);
    }

    let approval = prompt(label)?;
    Ok(matches!(approval.trim(), "y" | "Y" | "yes" | "YES"))
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn parse_workspace_arg() -> Result<PathBuf> {
    let mut args = std::env::args().skip(1);
    let mut workspace = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workspace" | "-w" => {
                let Some(value) = args.next() else {
                    anyhow::bail!("{arg} requires a directory");
                };
                workspace = Some(PathBuf::from(value));
            }
            "--help" | "-h" => {
                println!("Usage: mini-codex [--workspace DIR]");
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }

    workspace
        .or_else(|| std::env::current_dir().ok())
        .context("failed to determine workspace")
}
