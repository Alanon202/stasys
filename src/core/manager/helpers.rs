use std::fs;
use std::path::Path;
use tokio::process::Command;

use crate::log::{log_error_message, log_message};

use crate::{
    config::model::IdleActionBlock, 
    core::manager::{
        actions::{is_process_running, prepare_action, run_command_detached, run_command_silent, ActionRequest}, 
        state::ManagerState, Manager,
    }
};

#[derive(Clone, Debug)]
struct BrightnessState {
    value: u32,
    max_brightness: u32,
    device: String,
}

pub async fn capture_brightness(state: &mut ManagerState) -> Result<(), std::io::Error> {
    // Try sysfs method first
    if let Some(sys_brightness) = capture_sysfs_brightness() {
        log_message(&format!("Captured brightness via sysfs: {}/{} on device '{}'", 
            sys_brightness.value, sys_brightness.max_brightness, sys_brightness.device));

        // Store the full u32 value - don't truncate!
        state.previous_brightness = Some(sys_brightness.value);
        state.max_brightness = Some(sys_brightness.max_brightness);
        state.brightness_device = Some(sys_brightness.device);
        return Ok(());
    }

    // Fallback to brightnessctl
    log_message("Falling back to brightnessctl for brightness capture");
    match Command::new("brightnessctl").arg("get").output().await {
        Ok(out) if out.status.success() => {
            let val = String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse::<u32>()
                .unwrap_or(0);
            state.previous_brightness = Some(val);
            log_message(&format!("Captured brightness via brightnessctl: {}", val));
        }
        Ok(out) => {
            log_error_message(&format!("brightnessctl get failed: {:?}", out.status));
        }
        Err(e) => {
            log_error_message(&format!("Failed to execute brightnessctl: {}", e));
        }
    }

    Ok(())
}

pub async fn restore_brightness(state: &mut ManagerState) -> Result<(), std::io::Error> {
    if let Some(level) = state.previous_brightness {
        log_message(&format!("Attempting to restore brightness to {}", level));

        // Try sysfs restore first if we have device info
        if let (Some(device), Some(_max)) = (&state.brightness_device, state.max_brightness) {
            if restore_sysfs_brightness_to_device(device, level).is_ok() {
                log_message("Brightness restored via sysfs");
                state.previous_brightness = None;
                state.max_brightness = None;
                state.brightness_device = None;
                return Ok(());
            }
        }

        // Fallback to generic sysfs restore
        if restore_sysfs_brightness(level).is_ok() {
            log_message("Brightness restored via sysfs (generic)");
        } else {
            log_message("Falling back to brightnessctl for brightness restore");
            if let Err(e) = Command::new("brightnessctl")
                .arg("set")
                .arg(level.to_string())
                .output()
                .await
            {
                log_error_message(&format!("Failed to restore brightness: {}", e));
            }
        }

        // Reset stored brightness
        state.previous_brightness = None;
        state.max_brightness = None;
        state.brightness_device = None;
    }
    Ok(())
}

fn capture_sysfs_brightness() -> Option<BrightnessState> {
    let base = Path::new("/sys/class/backlight");
    let device_entry = fs::read_dir(base).ok()?.next()?;
    let device = device_entry.ok()?.file_name().to_string_lossy().to_string();

    let current = fs::read_to_string(base.join(&device).join("brightness")).ok()?;
    let max = fs::read_to_string(base.join(&device).join("max_brightness")).ok()?;
    
    Some(BrightnessState {
        value: current.trim().parse().ok()?,
        max_brightness: max.trim().parse().ok()?,
        device,
    })
}

fn restore_sysfs_brightness_to_device(device: &str, value: u32) -> Result<(), std::io::Error> {
    let base = Path::new("/sys/class/backlight");
    let path = base.join(device).join("brightness");
    fs::write(&path, value.to_string())?;
    Ok(())
}

fn restore_sysfs_brightness(value: u32) -> Result<(), std::io::Error> {
    let base = Path::new("/sys/class/backlight");

    let entry = fs::read_dir(base)
        .ok()
        .and_then(|mut it| it.next())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "No backlight device found"))??;

    let device = entry.file_name().to_string_lossy().to_string();
    let path = base.join(device).join("brightness");
    fs::write(&path, value.to_string())?;

    Ok(())
}

pub fn wake_idle_tasks(state: &ManagerState) {
    state.notify.notify_waiters();
}

// Getters and Setters
pub fn update_lock_state(state: &mut ManagerState, locked: bool) {
    state.lock_state.is_locked = locked;
}

pub fn get_compositor_manager(state: &mut ManagerState) -> bool {
    state.compositor_managed
}

pub fn set_compositor_manager(state: &mut ManagerState, value: bool) {
    state.compositor_managed = value;
}

pub fn get_manual_inhibit(state: &mut ManagerState) -> bool {
    state.manually_paused
}

pub async fn set_manual_inhibit(mgr: &mut Manager, inhibit: bool) {
    if inhibit {
        // Enable manual pause
        mgr.pause(true).await;
        mgr.state.manually_paused = true;
    } else {
        // Disable manual pause
        mgr.resume(true).await;
        mgr.state.manually_paused = false;
    }
}

pub async fn run_action(mgr: &mut Manager, action: &IdleActionBlock) {
    log_message(&format!(
        "Action triggered: name=\"{}\" kind={:?} timeout={} command=\"{}\"",
        action.name, action.kind, action.timeout, action.command
    ));

    // For loginctl lock actions, just trigger the D-Bus signal
    // The LoginctlLock event handler manages lock state
    if matches!(action.kind, crate::config::model::IdleAction::LockScreen) {
        if action.command.contains("loginctl lock-session") {
            log_message("Lock uses loginctl lock-session, triggering it (state will be managed by loginctl event)");
            if let Err(e) = crate::core::manager::actions::run_command_detached(&action.command).await {
                log_message(&format!("Failed to run loginctl lock-session: {}", e));
            }
            return;
        }

        if mgr.state.lock_state.is_locked {
            log_message("Lock screen action skipped: already locked");
            return;
        }
    }

    // Brightness capture
    if matches!(action.kind, crate::config::model::IdleAction::Brightness) && mgr.state.previous_brightness.is_none() {
        let _ = capture_brightness(&mut mgr.state).await;
    }

    if matches!(action.kind, crate::config::model::IdleAction::LockScreen) {
        mgr.state.lock_state.is_locked = true;
        mgr.state.lock_notify.notify_one();
        log_message("Lock screen action triggered, notifying lock watcher");
    }

    // Handle pre-suspend for Suspend actions
    if matches!(action.kind, crate::config::model::IdleAction::Suspend) {
        if let Some(cfg) = &mgr.state.cfg {
            if let Some(ref cmd) = cfg.pre_suspend_command {
                log_message(&format!("Running pre-suspend command: {}", cmd));
                let should_wait = match run_command_detached(cmd).await {
                    Ok(pid) => {
                        log_message(&format!("Pre-suspend command started with PID {}", pid));
                        true
                    }
                    Err(e) => {
                        log_error_message(&format!("Pre-suspend command failed: {}", e));
                        true
                    }
                };
                // Wait 500ms before proceeding to suspend
                if should_wait {
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
            }
        }
    }

    let requests = prepare_action(action).await;
    for req in requests {
        match req {
            ActionRequest::RunCommand(cmd) => {
                run_command_for_action(mgr, action, cmd).await;
            }
            ActionRequest::Skip(_) => {}
        }
    }
}

pub async fn run_command_for_action(mgr: &mut Manager, action: &IdleActionBlock, cmd: String) {
    let is_lock = matches!(action.kind, crate::config::model::IdleAction::LockScreen);
    if is_lock {
        // Determine which command to actually run
        let lock_cmd = if action.lock_command.is_some() {
            // If lock_command is set, run loginctl and then the lock_command
            log_message("Running loginctl lock-session (lock_command is configured)");
            match run_command_detached("loginctl lock-session").await {
                Ok(_) => log_message("loginctl lock-session executed"),
                Err(e) => log_error_message(&format!("Failed to run loginctl lock-session: {}", e)),
            }
            
            // Then run the actual lock_command
            action.lock_command.as_ref().unwrap().clone()
        } else {
            // No lock_command set, just run the regular command (hyprlock, etc.)
            cmd
        };

        match run_command_detached(&lock_cmd).await {
            Ok(pid) => {
                mgr.state.lock_state.pid = Some(pid);
                mgr.state.lock_state.is_locked = true;
                log_message(&format!("Lock screen started with PID {}", pid));
            }
            Err(e) => log_message(&format!("Failed to run lock command '{}': {}", lock_cmd, e)),
        }
    } else {
        let spawned = tokio::spawn(async move {
            if let Err(e) = run_command_silent(&cmd).await {
                log_message(&format!("Failed to run command '{}': {}", cmd, e));
            }
        });
        mgr.spawned_tasks.push(spawned);
    }
}

pub async fn lock_still_active(state: &ManagerState) -> bool {
    if let Some(cmd) = &state.lock_state.command {
        is_process_running(cmd).await
    } else {
        false
    }
}

pub async fn trigger_all_idle_actions(mgr: &mut Manager) {
    use crate::config::model::IdleAction;

    let block_name = if !mgr.state.ac_actions.is_empty() || !mgr.state.battery_actions.is_empty() {
        match mgr.state.on_battery() {
            Some(true) => "battery",
            Some(false) => "ac",
            None => "default",
        }
    } else {
        "default"
    };

    // Clone the actions so we don't borrow mgr mutably while iterating
    let actions_to_trigger: Vec<IdleActionBlock> = match block_name {
        "ac" => mgr.state.ac_actions.clone(),
        "battery" => mgr.state.battery_actions.clone(),
        "default" => mgr.state.default_actions.clone(),
        _ => unreachable!(),
    };

    if actions_to_trigger.is_empty() {
        log_message("No actions defined to trigger");
        return;
    }

    log_message(&format!("Triggering all idle actions for '{}'", block_name));

    for action in actions_to_trigger {
        // Skip lockscreen if already locked
        if matches!(action.kind, IdleAction::LockScreen) && mgr.state.lock_state.is_locked {
            log_message("Skipping lock action: already locked");
            continue;
        }

        log_message(&format!("Triggering idle action '{}'", action.name));
        run_action(mgr, &action).await;
    }

    // Now update `last_triggered` after all actions are done
    let now = std::time::Instant::now();
    let actions_mut: &mut Vec<IdleActionBlock> = match block_name {
        "ac" => &mut mgr.state.ac_actions,
        "battery" => &mut mgr.state.battery_actions,
        "default" => &mut mgr.state.default_actions,
        _ => unreachable!(),
    };

    for a in actions_mut.iter_mut() {
        a.last_triggered = Some(now);
    }

    mgr.state.action_index = actions_mut.len().saturating_sub(1);
    log_message("All idle actions triggered manually");
}

pub async fn incr_active_inhibitor(mgr: &mut Manager) {
    let prev = mgr.state.active_inhibitor_count;
    mgr.state.active_inhibitor_count = prev.saturating_add(1);
    let now = mgr.state.active_inhibitor_count;

    if prev == 0 {
        if !mgr.state.manually_paused {
            mgr.state.paused = true;
            log_message(&format!(
                "Inhibitor registered (count: {} → {}): first inhibitor active → idle timers paused",
                prev, now
            ));
        } else {
            log_message(&format!(
                "Inhibitor registered (count: {} → {}): manual pause already active",
                prev, now
            ));
        }
    } else {
        log_message(&format!(
            "Inhibitor registered (count: {} → {})",
            prev, now
        ));
    }

    // wake idle task so it can recalc next timeout (if needed)
    mgr.state.notify.notify_one();
}

pub async fn decr_active_inhibitor(mgr: &mut Manager) {
    let prev = mgr.state.active_inhibitor_count;

    if prev == 0 {
        log_message("decr_active_inhibitor called but count already 0 (possible mismatch)");
        return;
    }

    mgr.state.active_inhibitor_count = prev.saturating_sub(1);
    let now = mgr.state.active_inhibitor_count;

    if now == 0 {
        if !mgr.state.manually_paused {
            mgr.state.paused = false;
            log_message(&format!(
                "Inhibitor removed (count: {} → {}): no more inhibitors → idle timers resumed",
                prev, now
            ));

            // fire resume commands queued (if any)
            mgr.fire_resume_queue().await;
        } else {
            log_message(&format!(
                "Inhibitor removed (count: {} → {}): manual pause still active, timers remain paused",
                prev, now
            ));
        }

        // wake idle task so timeouts will be recalculated right away
        mgr.state.notify.notify_one();
    } else {
        log_message(&format!(
            "Inhibitor removed (count: {} → {})",
            prev, now
        ));
    }
}

pub async fn trigger_pre_suspend(mgr: &mut Manager) {
    if let Some(cmd) = &mgr.state.pre_suspend_command {
        log_message(&format!("Running pre-suspend command: {}", cmd));

        // Wait for it to finish (synchronous)
        match run_command_silent(cmd).await {
            Ok(_) => log_message("Pre-suspend command finished"),
            Err(e) => log_message(&format!("Pre-suspend command failed: {}", e)),
        }
    }
}
