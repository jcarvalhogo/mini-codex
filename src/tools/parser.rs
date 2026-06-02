use crate::model_profiles::ModelProfile;
use crate::types::ToolCall;
use anyhow::Context;
use anyhow::Result;

pub(crate) fn parse_tool_calls(
    answer: &str,
    model_profile: &ModelProfile,
) -> Result<Vec<ToolCall>> {
    let mut tool_calls = Vec::new();

    tool_calls.extend(parse_write_file_blocks(answer, model_profile)?);
    tool_calls.extend(parse_xml_style_tool_calls(answer)?);
    tool_calls.extend(parse_tagged_tool_call_blocks(answer)?);
    tool_calls.extend(parse_fenced_tool_calls(answer)?);
    tool_calls.extend(parse_fenced_shell_tool_calls(answer));

    if !tool_calls.is_empty() {
        return Ok(tool_calls);
    }

    tool_calls.extend(parse_line_delimited_json_tool_calls(answer)?);
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

pub(crate) fn looks_like_unsupported_tool_request(answer: &str) -> bool {
    let lower = answer.to_lowercase();
    (lower.contains("tool call") || lower.contains("tool_calls"))
        && (lower.contains("beautifulsoup")
            || lower.contains("```python")
            || lower.contains("\"parser\"")
            || lower.contains("unsupported tool")
            || lower.contains("library will help"))
}

pub(crate) fn unsupported_tool_feedback() -> String {
    "The previous response requested an unsupported tool. Mini Codex only supports shell, read_file, write_file, patch_file, and list_dir. If parsing HTML or structured text is needed, use the shell tool to run an available command or script. Output only the next supported tool call if more work is needed.".to_string()
}

fn parse_line_delimited_json_tool_calls(answer: &str) -> Result<Vec<ToolCall>> {
    let mut tool_calls = Vec::new();
    for line in answer
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if !line.starts_with('{') || !line.ends_with('}') || !line.contains("\"tool\"") {
            continue;
        }

        if let Ok(tool_call) = serde_json::from_str::<ToolCall>(line) {
            tool_calls.push(tool_call);
        } else if let Some(tool_call) = parse_lenient_shell_json_tool_call(line) {
            tool_calls.push(tool_call);
        }
    }
    Ok(tool_calls)
}

fn parse_lenient_shell_json_tool_call(line: &str) -> Option<ToolCall> {
    let prefix = r#"{"tool":"shell","cmd":""#;
    let command = line
        .strip_prefix(prefix)?
        .strip_suffix(r#""}"#)?
        .replace(r#"\""#, r#"""#)
        .replace(r#"\\"#, r#"\"#);

    Some(ToolCall {
        tool: "shell".to_string(),
        cmd: command,
        cwd: String::new(),
        path: String::new(),
        content: String::new(),
        old: String::new(),
        new: String::new(),
    })
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

fn parse_write_file_blocks(answer: &str, model_profile: &ModelProfile) -> Result<Vec<ToolCall>> {
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
        let content = model_profile
            .normalize_write_file_block_content(
                &rest[content_start..content_start + close_relative],
            )
            .to_string();
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
            if let Ok(tool_call) = serde_json::from_str::<ToolCall>(fenced_body) {
                tool_calls.push(tool_call);
            }
        }
        rest = &rest[fenced_body_start + relative_fence_end + "```".len()..];
    }
    Ok(tool_calls)
}

fn parse_fenced_shell_tool_calls(answer: &str) -> Vec<ToolCall> {
    let mut tool_calls = Vec::new();
    let mut rest = answer;
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
        let fenced_body = rest[fenced_body_start..fenced_body_start + relative_fence_end].trim();
        if matches!(language.as_str(), "bash" | "sh" | "shell")
            && !fenced_body.is_empty()
            && !looks_like_json_tool_call(fenced_body)
        {
            tool_calls.push(ToolCall {
                tool: "shell".to_string(),
                cmd: fenced_body.to_string(),
                cwd: String::new(),
                path: String::new(),
                content: String::new(),
                old: String::new(),
                new: String::new(),
            });
        }
        rest = &rest[fenced_body_start + relative_fence_end + "```".len()..];
    }
    tool_calls
}

fn looks_like_json_tool_call(input: &str) -> bool {
    let trimmed = input.trim();
    trimmed.starts_with('{') && trimmed.ends_with('}') && trimmed.contains("\"tool\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_profiles::profile_for_model;

    #[test]
    fn detects_beautifulsoup_as_unsupported_tool_request() {
        let answer = r#"To parse the HTML file, the next tool needed would be:

**Tool Call:**
```python
BeautifulSoup
```
"#;

        assert!(looks_like_unsupported_tool_request(answer));
    }

    #[test]
    fn plain_answer_is_not_an_unsupported_tool_request() {
        assert!(!looks_like_unsupported_tool_request(
            "The command succeeded and no further steps are needed."
        ));
    }

    #[test]
    fn parses_multiple_line_delimited_json_tool_calls() {
        let profile = profile_for_model("deepseek-r1:14b").unwrap();
        let answer = r#"{"tool":"shell","cmd":"curl \"https://example.com\" --output page.html"}
{"tool":"shell","cmd":"grep -o '<h1.*>' page.html"}
"#;

        let tool_calls = parse_tool_calls(answer, &profile).unwrap();

        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0].tool, "shell");
        assert!(tool_calls[0].cmd.contains("curl"));
        assert!(tool_calls[1].cmd.contains("grep"));
    }

    #[test]
    fn parses_lenient_shell_json_tool_call_with_unescaped_inner_quotes() {
        let profile = profile_for_model("deepseek-r1:14b").unwrap();
        let answer = r#"{"tool":"shell","cmd":"grep -o '<h1.*class=\"firstHeading\">.*</h1>' page.html; egrep '(?<=<h1 class="firstHeading">)(.*)(?=</h1>)' page.html"}"#;

        let tool_calls = parse_tool_calls(answer, &profile).unwrap();

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].tool, "shell");
        assert!(tool_calls[0].cmd.contains("firstHeading"));
        assert!(tool_calls[0].cmd.contains("egrep"));
    }

    #[test]
    fn parses_fenced_bash_as_shell_tool_call() {
        let profile = profile_for_model("deepseek-r1:14b").unwrap();
        let answer = r#"Here is the tool call:
```bash
curl -o page.html https://example.com
```
"#;

        let tool_calls = parse_tool_calls(answer, &profile).unwrap();

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].tool, "shell");
        assert_eq!(tool_calls[0].cmd, "curl -o page.html https://example.com");
    }

    #[test]
    fn fenced_bash_json_tool_call_is_not_duplicated_as_shell() {
        let profile = profile_for_model("deepseek-r1:14b").unwrap();
        let answer = r#"Here is the tool call:
```bash
{"tool":"read_file","path":"page.html"}
```
"#;

        let tool_calls = parse_tool_calls(answer, &profile).unwrap();

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].tool, "read_file");
        assert_eq!(tool_calls[0].path, "page.html");
    }

    #[test]
    fn malformed_fenced_tool_calls_json_does_not_crash_parser() {
        let profile = profile_for_model("deepseek-r1:14b").unwrap();
        let answer = r#"```json
{
  "tool_calls": [
    {
      "tool": "parser",
      "args": {
        "command": "parser.sh --process-html wikipedia.html"
      }
    }
  ]
}
```"#;

        let tool_calls = parse_tool_calls(answer, &profile).unwrap();

        assert!(tool_calls.is_empty());
        assert!(looks_like_unsupported_tool_request(answer));
    }
}
