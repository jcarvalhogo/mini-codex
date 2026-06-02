# Mini Codex Progress

## Current Status

The mini-codex prototype can run a local Ollama model as a small coding agent.
The current default model is `deepseek-r1:14b` through Ollama.
The latest successful end-to-end test used a clean workspace and asked the agent
to create, build, and run a Rust project from scratch.

Test request:

```text
Crie um projeto Rust do zero chamado final-agent. Ele deve imprimir "final test from mini-codex". Use write_file para criar final-agent/Cargo.toml e final-agent/src/main.rs. Depois rode cargo run usando shell com cwd final-agent.
```

Validated result:

```text
stdout:
final test from mini-codex
```

The agent successfully:

- produced an implementation plan;
- created `final-agent/Cargo.toml`;
- created `final-agent/src/main.rs`;
- ran `cargo run` with `cwd = "final-agent"`;
- captured stdout, stderr, and exit status;
- printed a turn summary;
- printed changed files.

## Implemented Capabilities

- Ollama chat integration through `/api/chat`.
- DeepSeek R1 14B default model through `deepseek-r1:14b`.
- Model profile layer with DeepSeek R1-specific prompt handling, plan cleanup,
  tool-result continuation, and write-file content normalization.
- Active model and model profile are recorded in the session log.
- Tool-result follow-up now warns models not to repeat successful setup commands
  or failed commands unchanged, and suggests robust shell/Python parsing for HTML.
- Tool-result follow-up now tells the model to stop calling tools when successful
  stdout already contains the requested answer.
- Configurable model override with `MINI_CODEX_MODEL`.
- Optional auto-approval with `MINI_CODEX_AUTO_APPROVE=1`.
- Optional plan-first mode with `MINI_CODEX_PLAN_FIRST=1`.
- Workspace selection with `--workspace <dir>` or `-w <dir>`.
- Tool parsing for:
  - raw JSON tool calls;
  - `<tool_call>...</tool_call>`;
  - fenced JSON blocks;
  - `<write_file path="...">...</write_file>`;
  - XML-style `<read_file .../>`, `<list_dir .../>`, and `<shell .../>`.
- Tools:
  - `shell`;
  - `read_file`;
  - `write_file`;
  - `patch_file`;
  - `list_dir`.
- Auto-read preview before overwriting an existing file.
- Diff display before `write_file` and `patch_file`.
- Changed-files summary.
- Command execution summary.
- Thinking feedback while waiting for Ollama responses.
- Unsupported-tool feedback when a model invents tools such as `BeautifulSoup`.
- Fenced `bash`/`sh`/`shell` blocks are parsed as shell tool calls for model tolerance.
- JSON tool calls inside fenced shell blocks are not duplicated as shell commands.
- Lenient shell JSON parsing handles unescaped quotes inside generated commands.
- Unsupported fenced JSON tool payloads no longer crash the session.
- JSONL session log at:

```text
<workspace>/.mini-codex/session.jsonl
```

## Observations From The Final Test

The final test was successful, but a few improvement points remain:

- Older prompt guidance allowed generated Rust projects to use `edition = "2021"` instead of `2024`.
- Older prompt guidance allowed `write_file` content with a leading blank line.
- The simple diff is line-by-line and not a true unified diff.
- Auto-approval is useful for fast iteration, but it should remain opt-in.
- The model sometimes mixes prose, Markdown, and tool calls; parser tolerance helped a lot.

## Recommended Next Steps

1. Expand the model profile layer:
   - add a profile trait or richer hooks if enum-style profiles become too small;
   - add a Qwen coder profile for comparison;
   - compare profile behavior across the same smoke-test tasks.

2. Add a config file, for example `.mini-codex.toml`, so model, workspace,
   auto-approval, and plan-first mode do not need environment variables.

3. Add stronger workspace safety:
   - canonicalize tool paths after joining;
   - reject symlink escapes;
   - keep all reads and writes inside the configured workspace.

4. Replace `patch_file` with a real unified-diff `apply_patch` tool.

5. Improve tool result logging:
   - log the exact tool call before execution;
   - include approval mode;
   - include command duration.

6. Add a final task report command or automatic final report:

```text
Summary:
- Created final-agent
- Ran cargo run successfully

Changed files:
- final-agent/Cargo.toml
- final-agent/src/main.rs
```

7. Add tests for parser behavior:
   - raw JSON;
   - fenced JSON;
   - XML-style tools;
   - multiple tool calls;
   - write_file blocks.

8. Add a safer project-generation workflow:
   - prefer `edition = "2024"` for new Rust projects;
   - avoid leading blank lines in generated files;
   - run formatting after generation when requested.

## Last Known Good Command

```bash
cd /home/jcarvalho/development/pessoal/ia/codex/codex-rs/mini-codex
source "$HOME/.cargo/env"
MINI_CODEX_MODEL=deepseek-r1:14b \
MINI_CODEX_AUTO_APPROVE=1 \
MINI_CODEX_PLAN_FIRST=1 \
cargo run -- --workspace /tmp/mini-codex-final-test
```
