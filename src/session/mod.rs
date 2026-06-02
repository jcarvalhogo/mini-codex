use crate::model_profiles::ModelProfile;
use crate::types::AppConfig;
use crate::types::ToolOutcome;
use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use std::io::Write;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

pub(crate) fn init_session_log(
    config: &AppConfig,
    model: &str,
    model_profile: &ModelProfile,
) -> Result<()> {
    if let Some(parent) = config.session_log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    append_session_event(
        config,
        "session_start",
        json!({
            "workspace": config.workspace,
            "model": model,
            "model_profile": {
                "id": model_profile.id,
                "display_name": model_profile.display_name,
            },
        }),
    )
}

pub(crate) fn log_tool_outcomes(config: &AppConfig, outcomes: &[ToolOutcome]) -> Result<()> {
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

pub(crate) fn append_session_event(
    config: &AppConfig,
    event_type: &str,
    data: serde_json::Value,
) -> Result<()> {
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
