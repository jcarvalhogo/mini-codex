use crate::ToolCall;
use std::borrow::Cow;
use std::path::Path;

pub(crate) const DEFAULT_MODEL: &str = "deepseek-r1:14b";

#[derive(Clone, Copy, Debug)]
pub(crate) struct ModelProfile {
    pub(crate) id: &'static str,
    pub(crate) display_name: &'static str,
    system_prompt_addendum: &'static str,
    plan_prompt: &'static str,
    execute_plan_prompt: &'static str,
    tool_result_followup: &'static str,
    sanitize_plan_response: fn(&str) -> String,
    normalize_write_file_block_content: fn(&str) -> Cow<'_, str>,
    normalize_tool_call: fn(ToolCall, &Path) -> ToolCall,
}

impl ModelProfile {
    pub(crate) fn system_prompt(&self, base_prompt: &str) -> String {
        format!(
            "{}\n\n{}",
            base_prompt.trim(),
            self.system_prompt_addendum.trim()
        )
    }

    pub(crate) fn plan_prompt(&self) -> &'static str {
        self.plan_prompt
    }

    pub(crate) fn execute_plan_prompt(&self) -> &'static str {
        self.execute_plan_prompt
    }

    pub(crate) fn tool_result_followup(
        &self,
        original_request: &str,
        tool_results: &str,
    ) -> String {
        format!(
            "Original request:\n{}\n\nTool results:\n{}\n\n{}",
            original_request, tool_results, self.tool_result_followup
        )
    }

    pub(crate) fn sanitize_plan_response(&self, plan: &str) -> String {
        (self.sanitize_plan_response)(plan)
    }

    pub(crate) fn normalize_write_file_block_content<'a>(&self, content: &'a str) -> Cow<'a, str> {
        (self.normalize_write_file_block_content)(content)
    }

    pub(crate) fn normalize_tool_call(&self, tool_call: ToolCall, workspace: &Path) -> ToolCall {
        (self.normalize_tool_call)(tool_call, workspace)
    }
}

pub(crate) fn profile_for_model(model: &str) -> ModelProfile {
    if model.starts_with("deepseek-r1") {
        deepseek_r1()
    } else {
        generic()
    }
}

fn deepseek_r1() -> ModelProfile {
    ModelProfile {
        id: "deepseek-r1",
        display_name: "DeepSeek R1",
        system_prompt_addendum: r#"
DeepSeek R1 model handling:
- Keep reasoning out of the final answer and put only the requested tool call in a tool-call response.
- If a tool is needed, output the tool call only. Do not wrap it in prose, Markdown commentary, or explanations.
- After a tool result, continue with the next required tool call until the user request is fully complete.
- Do not provide shell commands as instructions when the user asked you to run them. Use the shell tool.
- Do not invent tools. Libraries such as BeautifulSoup are not tools; use shell to run a command or script when parsing is needed.
- When writing files, do not add leading blank lines unless the user explicitly asked for them.
"#,
        plan_prompt: "First provide a short implementation plan. Do not call tools yet. Do not include tool-call syntax in the plan.",
        execute_plan_prompt: "Now execute the plan using tools. Do not repeat the plan. If a step requires a tool, output only the next tool call.",
        tool_result_followup: "Continue the original request. If any remaining step requires a tool, output only the next tool call.",
        sanitize_plan_response: sanitize_deepseek_plan_response,
        normalize_write_file_block_content: normalize_deepseek_write_file_block_content,
        normalize_tool_call: normalize_deepseek_tool_call,
    }
}

fn generic() -> ModelProfile {
    ModelProfile {
        id: "generic",
        display_name: "Generic local model",
        system_prompt_addendum: r#"
Generic model handling:
- If a tool is needed, output a supported tool call.
- After tool results, continue until the original user request is complete.
- Do not invent tools. Only shell, read_file, write_file, patch_file, and list_dir are available.
"#,
        plan_prompt: "First provide a short implementation plan. Do not call tools yet.",
        execute_plan_prompt: "Now execute the plan using tools. Do not repeat the plan.",
        tool_result_followup: "Continue the original request. If any remaining step requires a tool, output the next tool call.",
        sanitize_plan_response: sanitize_generic_plan_response,
        normalize_write_file_block_content: normalize_generic_write_file_block_content,
        normalize_tool_call: normalize_generic_tool_call,
    }
}

fn sanitize_generic_plan_response(plan: &str) -> String {
    plan.trim().to_string()
}

fn sanitize_deepseek_plan_response(plan: &str) -> String {
    let plan = remove_tagged_block(plan, "<tool_call>", "</tool_call>");
    let plan = remove_tagged_block(&plan, "<write_file", "</write_file>");
    let plan = remove_fenced_json_tool_calls(&plan);
    let plan = remove_fenced_shell_tool_calls(&plan);
    let plan = plan
        .lines()
        .filter(|line| !is_standalone_tool_call_line(line))
        .collect::<Vec<_>>()
        .join("\n");
    plan.trim().to_string()
}

fn remove_tagged_block(input: &str, start_marker: &str, end_marker: &str) -> String {
    let mut output = String::new();
    let mut rest = input;
    while let Some(start) = rest.find(start_marker) {
        output.push_str(&rest[..start]);
        let after_start = &rest[start..];
        let Some(end_relative) = after_start.find(end_marker) else {
            output.push_str(after_start);
            return output;
        };
        rest = &after_start[end_relative + end_marker.len()..];
    }
    output.push_str(rest);
    output
}

fn remove_fenced_json_tool_calls(input: &str) -> String {
    let mut output = String::new();
    let mut rest = input;
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
        if fenced_body.starts_with('{') && serde_json::from_str::<ToolCall>(fenced_body).is_ok() {
            output.push_str(&rest[..fence_start]);
            rest = &rest[fenced_body_start + relative_fence_end + "```".len()..];
        } else {
            let keep_end = fenced_body_start + relative_fence_end + "```".len();
            output.push_str(&rest[..keep_end]);
            rest = &rest[keep_end..];
        }
    }
    output.push_str(rest);
    output
}

fn remove_fenced_shell_tool_calls(input: &str) -> String {
    remove_fenced_blocks_by_language(input, &["bash", "sh", "shell"])
}

fn remove_fenced_blocks_by_language(input: &str, languages: &[&str]) -> String {
    let mut output = String::new();
    let mut rest = input;
    while let Some(fence_start) = rest.find("```") {
        let after_opening_fence = &rest[fence_start + "```".len()..];
        let Some(first_newline) = after_opening_fence.find('\n') else {
            break;
        };
        let language = after_opening_fence[..first_newline].trim().to_lowercase();
        let fenced_body_start = fence_start + "```".len() + first_newline + 1;
        let Some(relative_fence_end) = rest[fenced_body_start..].find("```") else {
            break;
        };
        if languages.iter().any(|allowed| language == *allowed) {
            output.push_str(&rest[..fence_start]);
            rest = &rest[fenced_body_start + relative_fence_end + "```".len()..];
        } else {
            let keep_end = fenced_body_start + relative_fence_end + "```".len();
            output.push_str(&rest[..keep_end]);
            rest = &rest[keep_end..];
        }
    }
    output.push_str(rest);
    output
}

fn is_standalone_tool_call_line(line: &str) -> bool {
    let trimmed = line.trim();
    matches!(
        trimmed,
        "<read_file" | "<list_dir" | "<shell" | "<write_file" | "<tool_call>"
    ) || serde_json::from_str::<ToolCall>(trimmed).is_ok()
        || ["<read_file", "<list_dir", "<shell"]
            .iter()
            .any(|prefix| trimmed.starts_with(prefix) && trimmed.ends_with("/>"))
}

fn normalize_generic_write_file_block_content(content: &str) -> Cow<'_, str> {
    Cow::Borrowed(content)
}

fn normalize_deepseek_write_file_block_content(content: &str) -> Cow<'_, str> {
    content
        .strip_prefix("\r\n")
        .or_else(|| content.strip_prefix('\n'))
        .map(Cow::Borrowed)
        .unwrap_or_else(|| Cow::Borrowed(content))
}

fn normalize_generic_tool_call(tool_call: ToolCall, _workspace: &Path) -> ToolCall {
    tool_call
}

fn normalize_deepseek_tool_call(mut tool_call: ToolCall, workspace: &Path) -> ToolCall {
    if tool_call.tool == "shell"
        && !tool_call.cwd.trim().is_empty()
        && !workspace.join(tool_call.cwd.trim()).exists()
    {
        tool_call.cwd.clear();
    }

    tool_call
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_write_file_block_content_drops_structural_leading_newline() {
        let profile = deepseek_r1();

        assert_eq!(
            profile.normalize_write_file_block_content("\nhello\n"),
            "hello\n"
        );
    }

    #[test]
    fn deepseek_write_file_block_content_drops_structural_leading_windows_newline() {
        let profile = deepseek_r1();

        assert_eq!(
            profile.normalize_write_file_block_content("\r\nhello\r\n"),
            "hello\r\n"
        );
    }

    #[test]
    fn deepseek_write_file_block_content_preserves_inline_content() {
        let profile = deepseek_r1();

        assert_eq!(
            profile.normalize_write_file_block_content("hello\n"),
            "hello\n"
        );
    }

    #[test]
    fn deepseek_plan_sanitizer_removes_tool_syntax_from_deepseek_style_plan() {
        let profile = deepseek_r1();
        let plan = r#"**Implementation Plan:**

1. Create a file.

<write_file path="hello.txt">
deepseek smoke test
</write_file>

{"tool":"shell","cmd":"cat hello.txt"}
"#;

        let sanitized = profile.sanitize_plan_response(plan);

        assert!(sanitized.contains("Implementation Plan"));
        assert!(!sanitized.contains("<write_file"));
        assert!(!sanitized.contains("deepseek smoke test"));
        assert!(!sanitized.contains(r#""tool":"shell""#));
    }

    #[test]
    fn deepseek_plan_sanitizer_removes_fenced_shell_commands() {
        let profile = deepseek_r1();
        let plan = r#"**Implementation Plan:**

Download the page:

```bash
curl -o page.html https://example.com
```

Then parse it.
"#;

        let sanitized = profile.sanitize_plan_response(plan);

        assert!(sanitized.contains("Implementation Plan"));
        assert!(sanitized.contains("Then parse it"));
        assert!(!sanitized.contains("```bash"));
        assert!(!sanitized.contains("curl -o page.html"));
    }

    #[test]
    fn generic_profile_keeps_plan_text_unchanged_except_outer_whitespace() {
        let profile = generic();

        assert_eq!(profile.sanitize_plan_response("  plan\n"), "plan");
    }

    #[test]
    fn tool_result_followup_keeps_original_request_visible() {
        let profile = deepseek_r1();

        let followup = profile.tool_result_followup(
            "Extract the page title",
            "tool_call_1:\nwrote_file: page.html",
        );

        assert!(followup.contains("Original request:"));
        assert!(followup.contains("Extract the page title"));
        assert!(followup.contains("Tool results:"));
        assert!(followup.contains("wrote_file: page.html"));
    }

    #[test]
    fn deepseek_tool_call_drops_missing_shell_cwd() {
        let profile = deepseek_r1();
        let tool_call = ToolCall {
            tool: "shell".to_string(),
            cmd: "cat hello.txt".to_string(),
            cwd: "hello-agent".to_string(),
            path: String::new(),
            content: String::new(),
            old: String::new(),
            new: String::new(),
        };

        let normalized = profile.normalize_tool_call(tool_call, Path::new("/tmp"));

        assert_eq!(normalized.cwd, "");
    }
}
