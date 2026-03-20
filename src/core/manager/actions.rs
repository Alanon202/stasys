use std::time::Duration;
use eyre::Result;
use tokio::process::Command;
use std::process::Stdio;
use crate::config::model::{IdleActionBlock, IdleAction};
use crate::log::log_message;

#[derive(Debug, Clone)]
pub enum ActionRequest {
    RunCommand(String),
    Skip(String),
}

/// Prepare action for execution
pub async fn prepare_action(action: &IdleActionBlock) -> Vec<ActionRequest> {
    let cmd = action.command.clone();
    match action.kind {
        IdleAction::Suspend => {
            if !cmd.trim().is_empty() {
                vec![ActionRequest::RunCommand(cmd)]
            } else {
                vec![]
            }
        }
        IdleAction::LockScreen => {
            // Check the actual lock process, not the trigger command
            let probe_cmd = if let Some(ref lock_cmd) = action.lock_command {
                // If lock_command is set, check for that process
                lock_cmd
            } else {
                // Otherwise check for the regular command
                &action.command
            };
            
            if is_process_running(probe_cmd).await {
                log_message("Lockscreen already running, skipping action.");
                vec![ActionRequest::Skip(probe_cmd.to_string())]
            } else {
                vec![ActionRequest::RunCommand(action.command.clone())]
            }
        }
        _ => {
            if cmd.trim().is_empty() {
                vec![]
            } else {
                vec![ActionRequest::RunCommand(cmd)]
            }
        }
    }
}

/// Run a shell command silently (log to /tmp/stasys.log)
pub async fn run_command_silent(cmd: &str) -> Result<()> {
    let log_file = "/tmp/stasys.log";
    let fut = async {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!("{cmd} >> {log_file} 2>&1"))
            .envs(std::env::vars())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let status = child.wait().await?;
        if !status.success() {
            eyre::bail!("Command '{}' exited with status {:?}", cmd, status.code());
        }
        Ok::<(), eyre::Report>(())
    };
    tokio::time::timeout(Duration::from_secs(30), fut).await??;
    Ok(())
}

/// Run a command detached (e.g., lock screen) and return its PID
pub async fn run_command_detached(command: &str) -> Result<u32, Box<dyn std::error::Error>> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return Err("Empty command".into());
    }
    let child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .envs(std::env::vars())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let pid = child.id().ok_or("Failed to get child PID")?;
    Ok(pid)
}

/// Check if a process matching `cmd` is running (by name)
pub async fn is_process_running(cmd: &str) -> bool {
    if cmd.trim().is_empty() {
        return false;
    }
    let first_word = cmd.split_whitespace().next().unwrap_or("");
    if first_word.is_empty() {
        return false;
    }
    let binary_name = std::path::Path::new(first_word)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(first_word);

    // Use procfs to scan /proc directly instead of spawning pgrep
    // This eliminates PID churn and reduces CPU/memory overhead
    if let Ok(processes) = procfs::process::all_processes() {
        for proc_result in processes {
            if let Ok(proc) = proc_result {
                // Check stat.comm (process name)
                if let Ok(stat) = proc.stat() {
                    if stat.comm == binary_name {
                        return true;
                    }
                }
                // Fallback to cmdline for full path matching
                if let Ok(cmdline) = proc.cmdline() {
                    if cmdline.iter().any(|arg| arg.contains(binary_name)) {
                        return true;
                    }
                }
            }
        }
    }
    false
}
