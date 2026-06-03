use anyhow::Context;
use anyhow::Result;
use std::io::Write;
use std::path::PathBuf;

pub(crate) fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim_end().to_string())
}

pub(crate) fn approve_or_prompt(auto_approve: bool, label: &str) -> Result<bool> {
    if auto_approve {
        println!("auto-approved");
        return Ok(true);
    }

    let approval = prompt(label)?;
    Ok(matches!(approval.trim(), "y" | "Y" | "yes" | "YES"))
}

pub(crate) fn env_flag_or(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(default)
}

pub(crate) fn parse_workspace_arg() -> Result<PathBuf> {
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
