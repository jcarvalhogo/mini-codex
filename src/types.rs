use serde::Deserialize;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChatMessage {
    pub(crate) role: String,
    pub(crate) content: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ToolCall {
    pub(crate) tool: String,
    #[serde(default)]
    pub(crate) cmd: String,
    #[serde(default)]
    pub(crate) cwd: String,
    #[serde(default)]
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) content: String,
    #[serde(default)]
    pub(crate) old: String,
    #[serde(default)]
    pub(crate) new: String,
}

#[derive(Debug)]
pub(crate) struct ToolOutcome {
    pub(crate) result: String,
    pub(crate) changed_file: Option<ChangedFile>,
    pub(crate) command: Option<ExecutedCommand>,
}

#[derive(Debug)]
pub(crate) struct ChangedFile {
    pub(crate) action: &'static str,
    pub(crate) path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct ExecutedCommand {
    pub(crate) cmd: String,
    pub(crate) cwd: PathBuf,
    pub(crate) success: bool,
}

#[derive(Debug)]
pub(crate) struct AppConfig {
    pub(crate) workspace: PathBuf,
    pub(crate) session_log_path: PathBuf,
}
