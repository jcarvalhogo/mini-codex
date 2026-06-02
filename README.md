# mini-codex
Mini Codex is a Rust prototype of a local coding agent powered by Ollama. It currently focuses on `deepseek-r1:14b` as the default local model and supports shell execution, file tools, patching, workspace mode, plan-first flow, diffs, summaries, and JSONL logs to explore a lightweight Codex-like agent loop for local models.

## Model Profiles

Mini Codex does not only switch model names. It loads a model profile that can customize prompts, plan cleanup, tool-result follow-up text, and model-specific output normalization.

The current profile is `deepseek-r1`, selected for models that start with `deepseek-r1`. Other models fall back to the `generic` profile until a dedicated profile is added.

While waiting for a model response, the CLI prints `model thinking... done` so long-running local inference does not look frozen.

## Run

```bash
cargo run -- --workspace /path/to/workspace
```

The default model is `deepseek-r1:14b`. You can override it with:

```bash
MINI_CODEX_MODEL=qwen3-coder cargo run -- --workspace /path/to/workspace
```

For faster local iteration:

```bash
MINI_CODEX_AUTO_APPROVE=1 \
MINI_CODEX_PLAN_FIRST=1 \
cargo run -- --workspace /tmp/mini-codex-workspace
```
