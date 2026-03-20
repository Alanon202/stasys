use std::sync::Arc;
use tokio::sync::Mutex;

use crate::{
    core::manager::{helpers::{run_action, trigger_pre_suspend}, Manager},
    log::log_message,
};

/// Helper function to strip ac. or battery. prefix from action names
fn strip_action_prefix(name: &str) -> &str {
    name.strip_prefix("ac.")
        .or_else(|| name.strip_prefix("battery."))
        .unwrap_or(name)
}

pub async fn trigger_action_by_name(manager: Arc<Mutex<Manager>>, name: &str) -> Result<String, String> {
    let normalized = name.replace('_', "-").to_lowercase();
    let mut mgr = manager.lock().await;

    if normalized == "pre-suspend" || normalized == "presuspend" {
        trigger_pre_suspend(&mut mgr).await;
        return Ok("pre_suspend".to_string());
    }

    // Check if user is explicitly targeting a specific block (e.g., "ac.dim" or "battery.suspend")
    let (target_block, search_name) = if normalized.starts_with("ac.") {
        (Some("ac"), normalized.strip_prefix("ac.").unwrap())
    } else if normalized.starts_with("battery.") {
        (Some("battery"), normalized.strip_prefix("battery.").unwrap())
    } else {
        (None, normalized.as_str())
    };

    // Determine which block to search
    let block = if let Some(explicit_block) = target_block {
        // User explicitly specified ac. or battery.
        match explicit_block {
            "ac" => &mgr.state.ac_actions,
            "battery" => &mgr.state.battery_actions,
            _ => &mgr.state.default_actions,
        }
    } else if !mgr.state.ac_actions.is_empty() || !mgr.state.battery_actions.is_empty() {
        // Auto-detect based on current power state
        match mgr.state.on_battery() {
            Some(true) => &mgr.state.battery_actions,
            Some(false) => &mgr.state.ac_actions,
            None => &mgr.state.default_actions,
        }
    } else {
        &mgr.state.default_actions
    };

    let action_opt = block.iter().find(|a| {
        let kind_name = format!("{:?}", a.kind).to_lowercase().replace('_', "-");
        let kind_name_no_hyphen = kind_name.replace('-', "");
        let search_name_no_hyphen = search_name.replace('-', "");
        let stripped_name = strip_action_prefix(&a.name).to_lowercase();
        let stripped_name_no_hyphen = stripped_name.replace('-', "");
        
        kind_name == search_name 
            || kind_name_no_hyphen == search_name_no_hyphen
            || stripped_name == search_name 
            || stripped_name_no_hyphen == search_name_no_hyphen
            || a.name.to_lowercase() == search_name
    });

    let action = match action_opt {
        Some(a) => a.clone(),
        None => {
            let mut available: Vec<String> = block.iter()
                .map(|a| strip_action_prefix(&a.name).to_string())
                .collect();
            if mgr.state.pre_suspend_command.is_some() {
                available.push("pre_suspend".to_string());
            }
            available.sort();
            return Err(format!(
                "Action '{}' not found. Available actions: {}",
                name,
                available.join(", ")
            ));
        }
    };

    log_message(&format!("Action triggered: '{}'", strip_action_prefix(&action.name)));
    let is_lock = matches!(action.kind, crate::config::model::IdleAction::LockScreen);

    if is_lock {
        if action.command.contains("loginctl lock-session") {
            log_message("Lock uses loginctl lock-session, triggering it via IPC");
            if let Err(e) = crate::core::manager::actions::run_command_detached(&action.command).await {
                return Err(format!("Failed to trigger lock: {}", e));
            }
        } else {
            mgr.state.lock_state.is_locked = true;
            mgr.state.lock_state.post_advanced = false;
            mgr.state.lock_state.command = Some(action.command.clone());
            mgr.state.lock_notify.notify_one();

            run_action(&mut mgr, &action).await;
            mgr.advance_past_lock().await;
            mgr.state.notify.notify_one();
        }
    } else {
        run_action(&mut mgr, &action).await;
    }

    Ok(strip_action_prefix(&action.name).to_string())
}

pub async fn list_available_actions(manager: Arc<Mutex<Manager>>) -> Vec<String> {
    let mgr = manager.lock().await;
    let mut actions = mgr
        .state
        .default_actions
        .iter()
        .map(|a| strip_action_prefix(&a.name).to_string())
        .collect::<Vec<_>>();

    if mgr.state.pre_suspend_command.is_some() {
        actions.push("pre_suspend".to_string());
    }

    actions.sort();
    actions
}

pub async fn switch_profile(manager: Arc<Mutex<Manager>>, profile_name: &str) -> Result<String, String> {
    use std::fs;
    use crate::config;

    // Handle "list" command
    if profile_name == "list" {
        let profiles_dir = dirs::home_dir()
            .map(|mut p| {
                p.push(".config/stasys/profiles");
                p
            });

        if let Some(dir) = profiles_dir {
            if let Ok(entries) = fs::read_dir(&dir) {
                let mut profiles: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .filter_map(|e| {
                        e.path()
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .map(|s| s.to_string())
                    })
                    .collect();
                profiles.sort();

                if profiles.is_empty() {
                    return Ok("No profiles found. Create .rune files in ~/.config/stasys/profiles/".to_string());
                }

                return Ok(format!("Available profiles: {}", profiles.join(", ")));
            }
        }

        return Ok("No profiles directory found".to_string());
    }

    // Handle "cycle" command
    let actual_profile = if profile_name == "cycle" {
        // Get current profile
        let current = dirs::home_dir()
            .map(|mut p| {
                p.push(".config/stasys/active_profile");
                p
            })
            .and_then(|path| fs::read_to_string(&path).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "none".to_string());

        // Get all available profiles
        let profiles_dir = dirs::home_dir()
            .map(|mut p| {
                p.push(".config/stasys/profiles");
                p
            });

        let mut profiles = vec!["none".to_string()];
        if let Some(dir) = profiles_dir {
            if let Ok(entries) = fs::read_dir(&dir) {
                let mut profile_names: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .filter_map(|e| {
                        e.path()
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .map(|s| s.to_string())
                    })
                    .collect();
                profile_names.sort();
                profiles.extend(profile_names);
            }
        }

        // Find current and get next
        if let Some(pos) = profiles.iter().position(|p| p == &current) {
            profiles[(pos + 1) % profiles.len()].clone()
        } else {
            "none".to_string()
        }
    } else {
        profile_name.to_string()
    };

    let mut mgr = manager.lock().await;

    // Handle profile switch
    let config_path = if actual_profile == "none" || actual_profile.is_empty() {
        // Use base config
        let path = dirs::home_dir()
            .map(|mut p| {
                p.push(".config/stasys/stasys.rune");
                p
            });

        if path.as_ref().map(|p| p.exists()).unwrap_or(false) {
            path
        } else {
            return Err("Base config not found at ~/.config/stasys/stasys.rune".to_string());
        }
    } else {
        // Use profile config
        let path = dirs::home_dir()
            .map(|mut p| {
                p.push(".config/stasys/profiles");
                p.push(format!("{}.rune", actual_profile));
                p
            });

        if path.as_ref().map(|p| p.exists()).unwrap_or(false) {
            path
        } else {
            return Err(format!("Profile '{}' not found. Create ~/.config/stasys/profiles/{}.rune", actual_profile, actual_profile));
        }
    };

    // Load the new config
    let new_cfg = match config::parser::load_config_from_path(config_path.as_ref().unwrap()) {
        Ok(cfg) => cfg,
        Err(e) => return Err(format!("Failed to load config: {}", e)),
    };

    // Apply the new config
    mgr.state.update_from_config(&new_cfg).await;
    mgr.recheck_media().await;
    mgr.trigger_instant_actions().await;

    // Save active profile
    if let Some(mut config_dir) = dirs::home_dir() {
        config_dir.push(".config/stasys/active_profile");
        let _ = fs::write(&config_dir, &actual_profile);
    }

    let profile_display = if actual_profile.is_empty() || actual_profile == "none" {
        "base config"
    } else {
        &actual_profile
    };

    Ok(format!("Switched to profile: {}", profile_display))
}
