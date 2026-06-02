use crate::cli::approve_or_prompt;
use crate::model_profiles::ModelProfile;
use crate::types::AppConfig;
use crate::types::ChangedFile;
use crate::types::ExecutedCommand;
use crate::types::ToolCall;
use crate::types::ToolOutcome;
use anyhow::Context;
use anyhow::Result;
use std::path::Path;
use std::path::PathBuf;
use tokio::process::Command;

pub(crate) async fn run_tool_calls(
    config: &AppConfig,
    model_profile: &ModelProfile,
    tool_calls: Vec<ToolCall>,
    auto_approve: bool,
) -> Result<Vec<ToolOutcome>> {
    let mut outcomes = Vec::new();
    for (index, tool_call) in tool_calls.into_iter().enumerate() {
        let tool_call = model_profile.normalize_tool_call(tool_call, &config.workspace);
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
        "patch_file" => patch_file_tool(
            config,
            &tool_call.path,
            &tool_call.old,
            &tool_call.new,
            auto_approve,
        ),
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
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write {}", path.display()))?;
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

    std::fs::write(&path, updated)
        .with_context(|| format!("failed to write {}", path.display()))?;
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
    Ok(format!(
        "list_dir: {}\n{}",
        path.display(),
        entries.join("\n")
    ))
}

fn workspace_path(config: &AppConfig, path: &str) -> Result<PathBuf> {
    let requested = Path::new(path);
    if requested.is_absolute() {
        let canonical = requested
            .canonicalize()
            .with_context(|| format!("failed to resolve {path}"))?;
        if !canonical.starts_with(&config.workspace) {
            anyhow::bail!("absolute path escapes workspace: {path}");
        }
        return Ok(canonical);
    }
    if requested
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        anyhow::bail!("parent directory segments are not allowed: {path}");
    }

    Ok(config.workspace.join(requested))
}

fn validate_shell_command(cmd: &str) -> Result<()> {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        anyhow::bail!("empty shell command");
    }

    let forbidden = [
        "rm ",
        "rm\t",
        "sudo ",
        "mkfs",
        "dd ",
        "shutdown",
        "reboot",
        ":(){",
        "chmod -R",
        "chown -R",
        "> /dev/",
        "git reset --hard",
        "git checkout --",
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

pub(crate) fn format_tool_results(outcomes: &[ToolOutcome]) -> String {
    outcomes
        .iter()
        .map(|outcome| outcome.result.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(crate) fn print_tool_results(outcomes: &[ToolOutcome]) {
    println!("\nTool result:\n{}\n", format_tool_results(outcomes));
}

pub(crate) fn tool_result_guidance(outcomes: &[ToolOutcome]) -> String {
    let commands = outcomes
        .iter()
        .filter_map(|outcome| outcome.command.as_ref())
        .collect::<Vec<_>>();
    if commands.is_empty() {
        return String::new();
    }

    let mut guidance = Vec::new();
    if commands.iter().any(|command| command.success) {
        guidance.push("Do not repeat successful setup commands unless their output is missing or the file changed.");
    }
    if outcomes.iter().any(|outcome| {
        outcome
            .command
            .as_ref()
            .is_some_and(|command| command.success)
            && stdout_has_content(&outcome.result)
    }) {
        guidance.push("If stdout already contains the requested answer, stop calling tools and provide the final answer from that output.");
    }
    if commands.iter().any(|command| !command.success) {
        guidance.push("One or more commands failed. Do not repeat failed commands unchanged; choose a different supported approach.");
    }
    if commands.iter().any(|command| {
        command.cmd.contains(".html")
            || command.cmd.contains("<h1")
            || command.cmd.contains("grep")
            || command.cmd.contains("sed")
    }) {
        guidance.push("For HTML parsing, prefer shell with python3 and standard-library parsing or a single robust command instead of fragile multi-line grep/sed patterns.");
    }

    if guidance.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nExecution guidance:\n{}",
            guidance
                .into_iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

fn stdout_has_content(tool_result: &str) -> bool {
    let Some(stdout_start) = tool_result.find("stdout:\n") else {
        return false;
    };
    let stdout = &tool_result[stdout_start + "stdout:\n".len()..];
    let stdout = stdout
        .split_once("\nstderr:")
        .map(|(stdout, _)| stdout)
        .unwrap_or(stdout);
    !stdout.trim().is_empty()
}

pub(crate) fn print_turn_summary(outcomes: &[ToolOutcome]) {
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
        let status = if command.success {
            "succeeded"
        } else {
            "failed"
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_guidance_warns_after_failed_html_shell_command() {
        let outcomes = vec![ToolOutcome {
            result: "failed".to_string(),
            changed_file: None,
            command: Some(ExecutedCommand {
                cmd: "grep -P '<h1.*?>' page.html".to_string(),
                cwd: PathBuf::from("/tmp/workspace"),
                success: false,
            }),
        }];

        let guidance = tool_result_guidance(&outcomes);

        assert!(guidance.contains("Do not repeat failed commands unchanged"));
        assert!(guidance.contains("For HTML parsing"));
        assert!(guidance.contains("python3"));
    }

    #[test]
    fn tool_result_guidance_stops_when_successful_stdout_has_answer() {
        let outcomes = vec![ToolOutcome {
            result: "cwd: /tmp/workspace\nexit_status: exit status: 0\nstdout:\ntitle>Artificial intelligence in marketing - Wikipedia\nstderr:\n".to_string(),
            changed_file: None,
            command: Some(ExecutedCommand {
                cmd: "grep -Eo '<title>.*</title>' page.html".to_string(),
                cwd: PathBuf::from("/tmp/workspace"),
                success: true,
            }),
        }];

        let guidance = tool_result_guidance(&outcomes);

        assert!(guidance.contains("stdout already contains the requested answer"));
        assert!(guidance.contains("stop calling tools"));
    }

    #[test]
    fn stdout_has_content_detects_non_empty_stdout_section() {
        assert!(stdout_has_content(
            "cwd: /tmp\nexit_status: exit status: 0\nstdout:\nanswer\nstderr:\n"
        ));
        assert!(!stdout_has_content(
            "cwd: /tmp\nexit_status: exit status: 0\nstdout:\n\nstderr:\n"
        ));
    }

    #[test]
    fn tool_result_guidance_is_empty_without_commands() {
        let outcomes = vec![ToolOutcome {
            result: "read_file".to_string(),
            changed_file: None,
            command: None,
        }];

        assert_eq!(tool_result_guidance(&outcomes), "");
    }

    #[test]
    fn workspace_path_accepts_absolute_path_inside_workspace() {
        let config = AppConfig {
            workspace: PathBuf::from("/tmp/mini-codex-test-workspace"),
            session_log_path: PathBuf::from(
                "/tmp/mini-codex-test-workspace/.mini-codex/session.jsonl",
            ),
        };
        std::fs::create_dir_all(&config.workspace).unwrap();

        let resolved = workspace_path(&config, "/tmp/mini-codex-test-workspace").unwrap();

        assert_eq!(resolved, config.workspace);
    }

    #[test]
    fn workspace_path_rejects_absolute_path_outside_workspace() {
        let config = AppConfig {
            workspace: PathBuf::from("/tmp/mini-codex-test-workspace"),
            session_log_path: PathBuf::from(
                "/tmp/mini-codex-test-workspace/.mini-codex/session.jsonl",
            ),
        };
        std::fs::create_dir_all(&config.workspace).unwrap();

        let error = workspace_path(&config, "/tmp").unwrap_err().to_string();

        assert!(error.contains("escapes workspace"));
    }
}
