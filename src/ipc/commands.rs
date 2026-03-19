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
        // Mark lock state and notify watcher
        mgr.state.lock_state.is_locked = true;
        mgr.state.lock_state.post_advanced = false;
        mgr.state.lock_state.command = Some(action.command.clone());
        mgr.state.lock_notify.notify_one();

        // Run the lock command
        run_action(&mut mgr, &action).await;

        // Mark as advanced past lock (this also resets timers and advances action_index)
        mgr.advance_past_lock().await;

        // Wake idle loop to recalculate timers
        mgr.state.notify.notify_one();
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
