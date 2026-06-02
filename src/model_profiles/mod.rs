mod deepseek;

use crate::types::ToolCall;
use anyhow::Result;
use std::borrow::Cow;
use std::path::Path;

pub(crate) const DEFAULT_MODEL: &str = deepseek::DEFAULT_MODEL;

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

pub(crate) fn profile_for_model(model: &str) -> Result<ModelProfile> {
    if deepseek::supports_model(model) {
        return Ok(deepseek::profile());
    }

    anyhow::bail!(
        "unsupported model `{}`. Mini Codex currently supports only models with specialized profiles: {}",
        model,
        supported_models().join(", ")
    );
}

pub(crate) fn supported_models() -> Vec<&'static str> {
    vec![deepseek::SUPPORTED_MODEL_PREFIX]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_deepseek_profile() {
        let profile = profile_for_model("deepseek-r1:14b").unwrap();

        assert_eq!(profile.id, "deepseek-r1");
    }

    #[test]
    fn rejects_models_without_specialized_profile() {
        let error = profile_for_model("qwen3-coder").unwrap_err().to_string();

        assert!(error.contains("unsupported model"));
        assert!(error.contains("deepseek-r1"));
    }
}
